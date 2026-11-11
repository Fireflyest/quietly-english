//! 阅读模块的纯逻辑：文本切分、选词、阅读会话模型。
//!
//! 这一层不碰数据库也不碰终端，只做三件事：
//!
//! 1. 把一段导入的文本切成段落（和可选的逐段译文）；
//! 2. 把段落切成可点击的词，保留原始字符偏移，供高亮使用；
//! 3. 记住「用户标了哪些词」以及读到哪一段。
//!
//! **不判断哪个词是生词。** 生词由用户自己选：标注动作是显式的，模块只负责
//! 把用户选中的词整理成可查询、可加进词书的形式。

use crate::model::Word;

/// 阅读模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadMode {
    /// 全篇阅读：整篇连续渲染，靠滚动翻看。
    #[default]
    Full,
    /// 精读：一次一段，段下有译文、生词和笔记。
    Intensive,
}

impl ReadMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadMode::Full => "full",
            ReadMode::Intensive => "intensive",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "full" | "reading" | "read" | "全篇" => Some(ReadMode::Full),
            "intensive" | "close" | "精读" => Some(ReadMode::Intensive),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ReadMode::Full => "full",
            ReadMode::Intensive => "intensive",
        }
    }
}

/// 一篇已经导入的文本。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub id: i64,
    pub title: String,
    /// 来源说明，例如 `Project Gutenberg #74`。
    pub source: Option<String>,
    /// 授权，例如 `public domain` / `CC BY-SA 4.0`。
    pub license: Option<String>,
    /// 原文，段落之间用空行分隔。
    pub body: String,
    /// 逐段译文，段落数与 `body` 一致；没有译文时为空。
    pub translation: String,
}

impl Text {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: 0,
            title: title.into(),
            source: None,
            license: None,
            body: body.into(),
            translation: String::new(),
        }
    }

    /// 原文段落。
    pub fn paragraphs(&self) -> Vec<&str> {
        split_paragraphs(&self.body)
    }

    /// 译文段落；数量可能与原文不一致（用户只译了一部分）。
    pub fn translations(&self) -> Vec<&str> {
        split_paragraphs(&self.translation)
    }

    /// 第 `index` 段的译文，没有就返回 `None`。
    pub fn translation_at(&self, index: usize) -> Option<&str> {
        let paragraphs = self.translations();
        paragraphs
            .get(index)
            .copied()
            .filter(|line| !line.trim().is_empty())
    }

    pub fn paragraph_count(&self) -> usize {
        self.paragraphs().len()
    }
}

/// 切段：空行分隔，连续的空白行合并，行内换行保留为空格。
///
/// 这样 `\n\n`、`\r\n\r\n` 和「一行一段 + 空行」三种写法都能正确读出段数。
pub fn split_paragraphs(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;

    for (offset, line) in line_offsets(body) {
        if line.trim().is_empty() {
            if let Some(from) = start.take() {
                let paragraph = body[from..offset].trim();
                if !paragraph.is_empty() {
                    out.push(paragraph);
                }
            }
        } else if start.is_none() {
            start = Some(offset);
        }
    }
    if let Some(from) = start {
        let paragraph = body[from..].trim();
        if !paragraph.is_empty() {
            out.push(paragraph);
        }
    }
    out
}

/// 逐行返回 `(行首字节偏移, 行内容)`。
fn line_offsets(body: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    body.split('\n').map(move |line| {
        let here = offset;
        // `split` 不包含分隔符，所以要自己补回 1 字节。
        offset += line.len() + 1;
        (here, line.trim_end_matches('\r'))
    })
}

/// 段落里的一个词，带它在段落中的字节范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {    /// 原样大小写（用于显示与高亮定位）。
    pub raw: String,
    /// 小写形式（用于查词）。
    pub lower: String,
    /// 在段落字符串中的字节范围。
    pub start: usize,
    pub end: usize,
}

impl Token {
    /// 是否值得查词：至少两个字母，且不是纯标点。
    pub fn is_word(&self) -> bool {
        self.raw.chars().filter(|ch| ch.is_alphabetic()).count() >= 2
    }
}

/// 把段落切成 token。
///
/// 一个 token 是「连续的字母、撇号、连字符」：
///
/// ```text
/// don't        -> don't
/// well-known   -> well-known
/// animals'     -> animals'
/// ```
///
/// 撇号只在**词内**连接（`don't`、`animals'`），句首引号 `'Tis` 也保留，
/// 但 `"word"` 两侧的引号不属于词。
pub fn tokenize(paragraph: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let bytes = paragraph.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let ch = paragraph[index..].chars().next().unwrap_or(' ');
        if !is_word_char(ch) {
            index += ch.len_utf8();
            continue;
        }
        let start = index;
        let mut end = index;
        for (offset, ch) in paragraph[index..].char_indices() {
            if !is_word_char(ch) {
                break;
            }
            end = index + offset + ch.len_utf8();
        }
        let raw = paragraph[start..end].to_string();
        // 去掉首尾的撇号与连字符：`'Tis` 保留内部，但 `word-` 的尾巴不要。
        let trimmed = raw.trim_matches(|ch| ch == '\'' || ch == '-');
        if !trimmed.is_empty() {
            let lead = raw.len() - raw.trim_start_matches(['\'', '-']).len();
            out.push(Token {
                lower: trimmed.to_ascii_lowercase(),
                raw: trimmed.to_string(),
                start: start + lead,
                end: start + lead + trimmed.len(),
            });
        }
        index = end.max(start + 1);
    }

    out
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphabetic() || ch == '\'' || ch == '-'
}

/// 把段落切成 `(文本, 是否标记)` 片段，供高亮渲染。
///
/// 未被标记的部分保持原样（含标点与空格），被标记的词单独成段，渲染层只需
/// 给 `true` 的片段换颜色，不用重新做断词。
pub fn highlight_segments(
    paragraph: &str,
    is_marked: impl Fn(&str) -> bool,
) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut cursor = 0;

    for token in tokenize(paragraph) {
        if token.start > cursor {
            push_segment(&mut out, &paragraph[cursor..token.start], false);
        }
        let marked = token.is_word() && is_marked(&token.lower);
        push_segment(&mut out, &paragraph[token.start..token.end], marked);
        cursor = token.end;
    }
    if cursor < paragraph.len() {
        push_segment(&mut out, &paragraph[cursor..], false);
    }
    out
}

/// 相邻同色片段合成一段，渲染时 span 更少，也便于测试断言。
fn push_segment(out: &mut Vec<(String, bool)>, text: &str, marked: bool) {
    if text.is_empty() {
        return;
    }
    if let Some((last, last_marked)) = out.last_mut() {
        if *last_marked == marked {
            last.push_str(text);
            return;
        }
    }
    out.push((text.to_string(), marked));
}

/// 用户标注过的一个词。
#[derive(Debug, Clone)]
pub struct Marked {
    pub lemma: String,
    /// 出现在第几段（从 0 开始）。
    pub paragraph: usize,
    /// 词条；库里没有这个词时为 `None`。
    pub word: Option<Word>,
}

/// 一次阅读会话。
#[derive(Debug, Clone)]
pub struct ReadingSession {
    pub text: Text,
    pub mode: ReadMode,
    /// 当前段（精读）/ 起始位置（全篇）。
    pub paragraph: usize,
    /// 用户标记的词，按标注顺序。
    pub marked: Vec<Marked>,
    /// 精读模式下当前选中的第几个生词（配合数字键）。
    pub cursor: Option<usize>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 打开过几个词条。
    pub opened: u32,
    /// 收进词书几个词。
    pub added: u32,
    /// 消息在 `App::messages` 里的位置，用于原地刷新。
    pub message: Option<usize>,
}

impl ReadingSession {
    pub fn new(text: Text, mode: ReadMode, paragraph: usize) -> Self {
        Self {
            text,
            mode,
            paragraph,
            marked: Vec::new(),
            cursor: None,
            started_at: chrono::Utc::now(),
            opened: 0,
            added: 0,
            message: None,
        }
    }

    pub fn total(&self) -> usize {
        self.text.paragraph_count()
    }

    pub fn current(&self) -> Option<&str> {
        self.text.paragraphs().get(self.paragraph).copied()
    }

    pub fn current_translation(&self) -> Option<&str> {
        self.text.translation_at(self.paragraph)
    }

    /// 当前段的所有可查词（去重，按出现顺序）。
    pub fn current_tokens(&self) -> Vec<Token> {
        let Some(paragraph) = self.current() else {
            return Vec::new();
        };
        let mut seen: Vec<String> = Vec::new();
        tokenize(paragraph)
            .into_iter()
            .filter(|token| token.is_word())
            .filter(|token| {
                if seen.contains(&token.lower) {
                    false
                } else {
                    seen.push(token.lower.clone());
                    true
                }
            })
            .collect()
    }

    pub fn next(&mut self) -> bool {
        if self.paragraph + 1 >= self.total() {
            return false;
        }
        self.paragraph += 1;
        self.cursor = None;
        true
    }

    pub fn prev(&mut self) -> bool {
        if self.paragraph == 0 {
            return false;
        }
        self.paragraph -= 1;
        self.cursor = None;
        true
    }

    pub fn go_to(&mut self, index: usize) -> bool {
        if index >= self.total() {
            return false;
        }
        self.paragraph = index;
        self.cursor = None;
        true
    }

    pub fn is_marked(&self, lemma: &str) -> bool {
        self.marked
            .iter()
            .any(|marked| marked.lemma.eq_ignore_ascii_case(lemma))
    }

    /// 标记一个词。重复标记返回 `false`。
    pub fn mark(&mut self, lemma: &str, word: Option<Word>) -> bool {
        let lemma = lemma.trim().to_ascii_lowercase();
        if lemma.is_empty() || self.is_marked(&lemma) {
            return false;
        }
        self.marked.push(Marked {
            lemma,
            paragraph: self.paragraph,
            word,
        });
        true
    }

    /// 取消标记。返回是否真的删掉了。
    pub fn unmark(&mut self, lemma: &str) -> bool {
        let lemma = lemma.trim().to_ascii_lowercase();
        let before = self.marked.len();
        self.marked
            .retain(|marked| !marked.lemma.eq_ignore_ascii_case(&lemma));
        self.marked.len() != before
    }

    /// 进度百分比（用于状态栏）。
    pub fn progress(&self) -> u32 {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        (((self.paragraph + 1) * 100) / total) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_paragraphs_on_blank_lines() {
        let body = "First paragraph.\nStill first.\n\nSecond one.\n\n\nThird.";
        let paragraphs = split_paragraphs(body);
        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[0], "First paragraph.\nStill first.");
        assert_eq!(paragraphs[1], "Second one.");
        assert_eq!(paragraphs[2], "Third.");
    }

    #[test]
    fn handles_crlf_and_leading_blank_lines() {
        let body = "\r\n\r\nAlpha.\r\n\r\nBeta.\r\n";
        assert_eq!(split_paragraphs(body), vec!["Alpha.", "Beta."]);
    }

    #[test]
    fn tokenize_keeps_contractions_together() {
        // 撇号连接词内成分要保留；但所有格尾巴（`animals'`）属于语法符号，
        // 去掉才能查到 `animals` 这个词条。
        let tokens: Vec<String> = tokenize("Don't stop, animals' rights well-known")
            .into_iter()
            .map(|t| t.raw)
            .collect();
        assert_eq!(
            tokens,
            vec!["Don't", "stop", "animals", "rights", "well-known"]
        );
    }

    #[test]
    fn tokenize_reports_byte_ranges_inside_the_paragraph() {
        let paragraph = "A tyranny begins.";
        let token = tokenize(paragraph)
            .into_iter()
            .find(|t| t.lower == "tyranny")
            .expect("tyranny");
        assert_eq!(&paragraph[token.start..token.end], "tyranny");
    }

    #[test]
    fn tokenize_drops_surrounding_quotes() {
        let tokens = tokenize("\"word\" and (`paren`)").into_iter().map(|t| t.raw).collect::<Vec<_>>();
        assert_eq!(tokens, vec!["word", "and", "paren"]);
    }

    #[test]
    fn short_tokens_are_not_words() {
        let tokens = tokenize("a I an ox");
        let words: Vec<&str> = tokens
            .iter()
            .filter(|t| t.is_word())
            .map(|t| t.raw.as_str())
            .collect();
        assert_eq!(words, vec!["an", "ox"]);
    }

    #[test]
    fn session_navigation_stays_in_bounds() {
        let text = Text::new("t", "One.\n\nTwo.\n\nThree.");
        let mut session = ReadingSession::new(text, ReadMode::Intensive, 0);
        assert_eq!(session.total(), 3);
        assert!(session.next());
        assert!(session.next());
        assert!(!session.next(), "cannot go past the last paragraph");
        assert!(session.prev());
        assert_eq!(session.paragraph, 1);
        assert!(session.go_to(2));
        assert!(!session.go_to(9));
    }

    #[test]
    fn marking_is_idempotent_and_removable() {
        let text = Text::new("t", "One.");
        let mut session = ReadingSession::new(text, ReadMode::Full, 0);
        assert!(session.mark("Tyranny", None));
        assert!(!session.mark("tyranny", None), "already marked");
        assert!(session.is_marked("TYRANNY"));
        assert!(session.unmark("Tyranny"));
        assert!(!session.is_marked("tyranny"));
    }

    #[test]
    fn translations_line_up_by_paragraph() {
        let mut text = Text::new("t", "One.\n\nTwo.\n\nThree.");
        text.translation = "一。\n\n二。".to_string();
        assert_eq!(text.translation_at(0), Some("一。"));
        assert_eq!(text.translation_at(1), Some("二。"));
        assert_eq!(text.translation_at(2), None, "translation may be partial");
    }

    #[test]
    fn current_tokens_are_deduplicated_in_order() {
        let text = Text::new("t", "the cat saw the cat and the dog");
        let session = ReadingSession::new(text, ReadMode::Intensive, 0);
        let words: Vec<String> = session
            .current_tokens()
            .into_iter()
            .map(|t| t.lower)
            .collect();
        assert_eq!(words, vec!["the", "cat", "saw", "and", "dog"]);
    }

    #[test]
    fn mode_parses_both_languages() {
        assert_eq!(ReadMode::parse("full"), Some(ReadMode::Full));
        assert_eq!(ReadMode::parse("精读"), Some(ReadMode::Intensive));
        assert_eq!(ReadMode::parse("nope"), None);
    }

    #[test]
    fn highlight_segments_round_trip_the_original_text() {
        let paragraph = "Tyranny begins, and the claim is made.";
        let segments = highlight_segments(paragraph, |word| word == "tyranny");
        let rebuilt: String = segments.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(rebuilt, paragraph, "nothing may be dropped or reordered");

        let marked: Vec<&str> = segments
            .iter()
            .filter(|(_, hit)| *hit)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(marked, vec!["Tyranny"], "only the chosen word is highlighted");
    }

    #[test]
    fn highlight_segments_with_nothing_marked_is_one_plain_run() {
        let segments = highlight_segments("All quiet.", |_| false);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].0, "All quiet.");
        assert!(!segments[0].1);
    }
}
