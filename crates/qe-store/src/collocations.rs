//! 从 StarDict 词典条目里抽「相关词组」段。
//!
//! 朗道英汉字典在词条末尾会列常用搭配，形如：
//!
//! ```text
//! n. 目的, 缘故, 理由
//! 相关词组:
//!   for safety's sake
//!   for the sake of
//!   for art's sake
//! ```
//!
//! 这一段是**词典编者列出来的固定搭配**，不是统计出来的共现，可以放心当作
//! 搭配数据源。抽取时只认这个显式标记，其余行原样保留。
//!
//! [`split`] 把一条条目拆成「剩余释义」和「搭配列表」两半。合并多本词典时必须
//! 先拆再合，否则搭配行会被当成义项塞进 `translation`。

use qe_core::model::Collocation;

/// 段起始标记。兼容全角冒号和「相关短语」这类近义写法。
const MARKERS: &[&str] = &["相关词组", "相关短语", "常用词组", "词组"];

/// 一条词典条目的拆分结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Split {
    /// 去掉搭配段之后剩下的释义文本。
    pub senses: String,
    /// 搭配段里抽出的词组。
    pub phrases: Vec<Collocation>,
}

/// 把整条释义拆成「释义」和「搭配」。没有标记时 `phrases` 为空、
/// `senses` 与输入等价（只做行尾空白清理）。
pub fn split(text: &str) -> Split {
    let mut senses: Vec<&str> = Vec::new();
    let mut phrases: Vec<Collocation> = Vec::new();
    let mut inside = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if !inside {
            if is_marker(trimmed) {
                inside = true;
            } else {
                senses.push(line);
            }
            continue;
        }

        // 段结束：空行、方括号标签行，或者下一条释义。
        if trimmed.is_empty() || trimmed.starts_with('【') || looks_like_sense(trimmed) {
            inside = false;
            if !trimmed.is_empty() {
                senses.push(line);
            }
            continue;
        }

        // 有的词条会重复写一次标记（一个词性一段搭配）；遇到就继续收集，
        // 不要把 `相关词组:` 当成一条释义塞回去。
        if is_marker(trimmed) {
            continue;
        }

        let candidate = clean_phrase(trimmed);
        if !looks_like_phrase(&candidate) {
            // 不是搭配的杂行，结束这一段，并把它留作释义。
            inside = false;
            senses.push(line);
            continue;
        }

        if !phrases
            .iter()
            .any(|existing| existing.phrase.eq_ignore_ascii_case(&candidate))
        {
            // 词典不给中文释义，这里留空；卡片只显示搭配本身。
            phrases.push(Collocation::new(candidate, ""));
        }
    }

    Split {
        senses: senses.join("\n").trim().to_string(),
        phrases,
    }
}

/// 只要搭配部分。
pub fn extract(text: &str) -> Vec<Collocation> {
    split(text).phrases
}

/// `相关词组:` / `相关词组：` / `[词组]` 都认。
fn is_marker(line: &str) -> bool {
    let core = line.trim_start_matches('[').trim_end_matches(']').trim();
    let core = core.trim_end_matches([':', '：']).trim();
    MARKERS.contains(&core)
}

/// 词典用 `...` 表示「如此类推」，`?` 表示「疑问句式」；两者都只是排版符号。
fn clean_phrase(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('*')
        .trim()
        .trim_end_matches(is_trailing_punctuation)
        .trim()
        .to_string()
}

fn is_trailing_punctuation(ch: char) -> bool {
    matches!(ch, '.' | '…' | '?' | '!' | ',' | ';' | ':')
}

/// 搭配至少两个词、只含 ASCII；含中文说明的行不是搭配。
fn looks_like_phrase(phrase: &str) -> bool {
    if phrase.len() < 4 || phrase.len() > 48 {
        return false;
    }
    let mut letters = 0;
    for ch in phrase.chars() {
        if ch.is_ascii_alphabetic() {
            letters += 1;
        } else if !(ch.is_ascii_whitespace()
            || ch == '\''
            || ch == '-'
            || ch == '/'
            || ch == '('
            || ch == ')')
        {
            return false;
        }
    }
    letters > 0 && phrase.contains(' ')
}

/// `n. 释义`、`vt. 释义`、`a. 释义` —— 进入下一段释义的标志。
///
/// 必须要求「缩写 + 点 + 后面跟中文」：只认词性表会把 `art for art's sake`
/// 里的 `art` 当成冠词，从而提前截断整个搭配段。
fn looks_like_sense(line: &str) -> bool {
    let Some((head, rest)) = line.split_once(' ') else {
        return false;
    };
    if !head.ends_with('.') {
        return false;
    }
    let pos = head.trim_end_matches('.');
    matches!(
        pos,
        "n" | "v"
            | "vt"
            | "vi"
            | "a"
            | "ad"
            | "adj"
            | "adv"
            | "prep"
            | "conj"
            | "pron"
            | "num"
            | "art"
            | "int"
            | "aux"
            | "abbr"
    ) && rest.chars().any(|ch| !ch.is_ascii())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases(text: &str) -> Vec<String> {
        extract(text).into_iter().map(|c| c.phrase).collect()
    }

    #[test]
    fn pulls_the_marked_block() {
        let text = "n. 目的, 缘故, 理由\n相关词组:\n  for safety's sake\n  for the sake of";
        assert_eq!(phrases(text), vec!["for safety's sake", "for the sake of"]);
    }

    #[test]
    fn keeps_the_senses_and_drops_the_block() {
        let text = "n. 目的, 缘故, 理由\n相关词组:\n  for safety's sake\n  for the sake of";
        assert_eq!(split(text).senses, "n. 目的, 缘故, 理由");
    }

    #[test]
    fn stops_at_the_next_sense_and_keeps_it() {
        let text = "相关词组:\n  to bank sth up\nvt. 存款";
        let result = split(text);
        assert_eq!(result.senses, "vt. 存款");
        let got: Vec<&str> = result.phrases.iter().map(|c| c.phrase.as_str()).collect();
        assert_eq!(got, vec!["to bank sth up"]);
    }

    #[test]
    fn stops_at_a_blank_line() {
        let text = "相关词组:\n  to bank sth up\n\n  unrelated note";
        assert_eq!(phrases(text), vec!["to bank sth up"]);
    }

    #[test]
    fn drops_the_trailing_ellipsis() {
        let text = "相关词组:\n  to bank with...";
        assert_eq!(phrases(text), vec!["to bank with"]);
    }

    #[test]
    fn skips_chinese_glue_lines() {
        // 混合编码污染时会出现「搭配+下一条释义」粘在一行的情况，这种要丢掉。
        let text = "相关词组:\n  be abhorrent to sbn. 憎恶者";
        assert!(phrases(text).is_empty(), "含中文的不是纯搭配");
    }

    #[test]
    fn no_marker_means_no_phrases() {
        assert!(phrases("vt. 检查，检验；视察").is_empty());
        assert!(phrases("").is_empty());
        assert_eq!(split("vt. 检查，检验；视察").senses, "vt. 检查，检验；视察");
    }

    #[test]
    fn accepts_fullwidth_colon_and_bracket_form() {
        assert_eq!(phrases("相关词组：\n  bank on sb"), vec!["bank on sb"]);
        assert_eq!(phrases("[词组]\n  bank on sb"), vec!["bank on sb"]);
    }

    #[test]
    fn a_repeated_marker_keeps_collecting() {
        // 一个词性一段搭配时标记会出现两次；第二次不能被当成释义。
        let text = "相关词组:\n  bank on sb\n相关词组:\n  bank with sb";
        let result = split(text);
        assert_eq!(
            result.phrases.iter().map(|c| c.phrase.as_str()).collect::<Vec<_>>(),
            vec!["bank on sb", "bank with sb"]
        );
        assert_eq!(result.senses, "", "the marker must not leak into senses");
    }

    #[test]
    fn dedupes_repeated_phrases() {
        let text = "相关词组:\n  bank on sb\n  bank on sb";
        assert_eq!(phrases(text).len(), 1);
    }

    #[test]
    fn a_phrase_starting_with_a_pos_word_does_not_end_the_block() {
        // `art for art's sake` 的 `art` 是冠词缩写，但它是一条搭配。
        let text = "相关词组:\n  for the sake of\n  art for art's sake\n  for any sake";
        assert_eq!(
            phrases(text),
            vec!["for the sake of", "art for art's sake", "for any sake"]
        );
    }

    #[test]
    fn a_question_mark_does_not_end_the_block() {
        let text = "相关词组:\n  What's the matter with...?\n  a matter of course";
        assert_eq!(
            phrases(text),
            vec!["What's the matter with", "a matter of course"]
        );
    }
}
