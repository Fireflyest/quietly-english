//! 核心数据模型。

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// 词根成分的类型，决定 TUI 里显示的前缀符号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    Prefix,
    Root,
    Suffix,
}

impl RootKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RootKind::Prefix => "prefix",
            RootKind::Root => "root",
            RootKind::Suffix => "suffix",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RootKind::Prefix => "prefix",
            RootKind::Root => "root",
            RootKind::Suffix => "suffix",
        }
    }

    pub fn marker(self) -> &'static str {
        match self {
            RootKind::Prefix => "◂",
            RootKind::Root => "◆",
            RootKind::Suffix => "▸",
        }
    }

    pub fn parse(raw: &str) -> RootKind {
        match raw.trim().to_ascii_lowercase().as_str() {
            "prefix" | "pre" | "前缀" => RootKind::Prefix,
            "suffix" | "suf" | "后缀" => RootKind::Suffix,
            _ => RootKind::Root,
        }
    }
}

/// 单词的一个构词成分，例如 `ab- / 远离 / 前缀`。
///
/// `source` records where the breakdown came from (a dictionary entry, a
/// Wiktionary etymology template, a textbook). Roots are never inferred by code:
/// if a word has no sourced analysis on file, the card shows nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordRoot {
    pub part: String,
    pub gloss: String,
    pub kind: RootKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl WordRoot {
    pub fn new(part: impl Into<String>, gloss: impl Into<String>, kind: RootKind) -> Self {
        Self {
            part: part.into(),
            gloss: gloss.into(),
            kind,
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

/// 例句，中文翻译可选（导入 Tatoeba 时可能只有英文）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Example {
    pub en: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
}

impl Example {
    pub fn new(en: impl Into<String>, zh: Option<String>) -> Self {
        Self {
            en: en.into(),
            zh,
        }
    }
}

/// 常用词组 / 固定搭配，例如 `for the sake of → 为了`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collocation {
    pub phrase: String,
    #[serde(default)]
    pub gloss: String,
}

impl Collocation {
    pub fn new(phrase: impl Into<String>, gloss: impl Into<String>) -> Self {
        Self {
            phrase: phrase.into(),
            gloss: gloss.into(),
        }
    }
}

/// 一个单词的完整条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Word {
    pub id: i64,
    pub lemma: String,
    #[serde(default)]
    pub phonetic: Option<String>,
    #[serde(default)]
    pub translation: String,
    #[serde(default)]
    pub definition: Option<String>,
    #[serde(default)]
    pub roots: Vec<WordRoot>,
    #[serde(default)]
    pub examples: Vec<Example>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 常用词组 / 固定搭配。
    #[serde(default)]
    pub collocations: Vec<Collocation>,
    /// 英文释义，来自 Wiktionary，用来补 ECDICT 缺失的生僻义。
    #[serde(default)]
    pub glosses: Vec<crate::text::Gloss>,
}

impl Word {
    pub fn new(lemma: impl Into<String>, translation: impl Into<String>) -> Self {
        Self {
            lemma: lemma.into(),
            translation: translation.into(),
            ..Default::default()
        }
    }

    /// Sourced morpheme breakdown, straight from the word record.
    ///
    /// Deliberately no fallback: guessing a split from a built-in affix list
    /// produces plausible-looking but wrong analyses (`surgeon` → `geo`), so an
    /// empty list means "no etymology on file".
    pub fn roots(&self) -> &[WordRoot] {
        &self.roots
    }

    /// Structured senses, split out of `translation` when no richer source supplied
    /// them.
    ///
    /// Keeping this derived means the 3.2M rows imported before senses existed still
    /// get the multi-sense card without a re-import.
    pub fn senses(&self) -> crate::text::ParsedSenses {
        crate::text::parse_senses(&self.translation)
    }
}

/// 词书元信息。
#[derive(Debug, Clone, Default)]
pub struct Book {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub description: String,
}

/// 词书学习进度。
#[derive(Debug, Clone, Copy, Default)]
pub struct BookProgress {
    pub total: u32,
    pub learned: u32,
    pub due: u32,
}

/// 卡片所处阶段（与 Anki 的 new/learning/review/relearning 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    New,
    Learning,
    Review,
    Relearning,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::New => "new",
            Phase::Learning => "learning",
            Phase::Review => "review",
            Phase::Relearning => "relearning",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::New => "new",
            Phase::Learning => "learning",
            Phase::Review => "review",
            Phase::Relearning => "relearning",
        }
    }

    pub fn parse(raw: &str) -> Phase {
        match raw.trim().to_ascii_lowercase().as_str() {
            "learning" => Phase::Learning,
            "review" => Phase::Review,
            "relearning" => Phase::Relearning,
            _ => Phase::New,
        }
    }
}

/// 用户对一张卡的评价。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}

impl Rating {
    pub fn key(self) -> char {
        match self {
            Rating::Again => '1',
            Rating::Hard => '2',
            Rating::Good => '3',
            Rating::Easy => '4',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Rating::Again => "Again",
            Rating::Hard => "Hard",
            Rating::Good => "Good",
            Rating::Easy => "Easy",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Rating::Again => "forgot it",
            Rating::Hard => "barely recalled",
            Rating::Good => "recalled it",
            Rating::Easy => "too easy",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Rating::Again => "again",
            Rating::Hard => "hard",
            Rating::Good => "good",
            Rating::Easy => "easy",
        }
    }

    pub fn parse(raw: &str) -> Option<Rating> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "again" | "1" | "forgot" => Some(Rating::Again),
            "hard" | "2" => Some(Rating::Hard),
            "good" | "3" | "known" => Some(Rating::Good),
            "easy" | "4" => Some(Rating::Easy),
            _ => None,
        }
    }

    pub fn all() -> [Rating; 4] {
        [Rating::Again, Rating::Hard, Rating::Good, Rating::Easy]
    }
}

/// 一张卡片的记忆状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrsCard {
    pub phase: Phase,
    pub step: u8,
    pub ease: f64,
    pub interval_days: u32,
    pub reps: u32,
    pub lapses: u32,
    pub due_at: DateTime<Utc>,
    #[serde(default)]
    pub last_review_at: Option<DateTime<Utc>>,
}

impl SrsCard {
    /// 待学新卡：还没有落库的进度，due 用当前时间占位。
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            phase: Phase::New,
            step: 0,
            ease: 2.5,
            interval_days: 0,
            reps: 0,
            lapses: 0,
            due_at: now,
            last_review_at: None,
        }
    }
}

/// 学习队列里的一张卡：单词 + 可选的历史状态。
#[derive(Debug, Clone)]
pub struct StudyCard {
    pub word: Word,
    pub card: Option<SrsCard>,
    pub is_new: bool,
}

impl StudyCard {
    pub fn new_word(word: Word) -> Self {
        Self {
            word,
            card: None,
            is_new: true,
        }
    }

    pub fn review(word: Word, card: SrsCard) -> Self {
        Self {
            word,
            card: Some(card),
            is_new: false,
        }
    }
}

/// 某一天的统计。
#[derive(Debug, Clone, Default)]
pub struct DailyStats {
    pub day: Option<NaiveDate>,
    pub new_learned: u32,
    pub reviews: u32,
    pub again: u32,
    pub hard: u32,
    pub good: u32,
    pub easy: u32,
    pub seconds: u32,
}

impl DailyStats {
    pub fn minutes(&self) -> u32 {
        (self.seconds + 30) / 60
    }

    pub fn total_graded(&self) -> u32 {
        self.again + self.hard + self.good + self.easy
    }
}

/// 一次阅读的记录。
#[derive(Debug, Clone)]
pub struct ReadingLog {
    pub text_id: i64,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    /// 看过几段。
    pub paragraphs: u32,
    /// 打开过几个词条。
    pub words_opened: u32,
    /// 收进词书几个词。
    pub words_added: u32,
}

/// 当天的阅读统计。
#[derive(Debug, Clone, Default)]
pub struct ReadingStats {
    pub day: Option<NaiveDate>,
    /// 几篇文章（去重）。
    pub texts: u32,
    pub paragraphs: u32,
    pub seconds: u32,
    pub words_opened: u32,
    pub words_added: u32,
    /// 这天做了几道题，答对几道。
    pub quizzes: u32,
    pub correct: u32,
}

impl ReadingStats {
    pub fn minutes(&self) -> u32 {
        (self.seconds + 30) / 60
    }

    pub fn is_empty(&self) -> bool {
        self.paragraphs == 0 && self.seconds == 0 && self.quizzes == 0
    }
}

/// 阅读理解题。
///
/// `source` 必须写明出处（原文段落、语料库或用户自备文件）；没有出处的题目
/// 不写入——凭空生成的题目无法核对对错。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quiz {
    pub id: i64,
    pub text_id: i64,
    /// 题目基于第几段（从 0 开始）。
    pub paragraph: usize,
    pub prompt: String,
    pub answer: String,
    /// 选择题的选项；为空表示填空/简答。
    pub choices: Vec<String>,
    pub source: String,
}

impl Quiz {
    pub fn new(prompt: impl Into<String>, answer: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            answer: answer.into(),
            ..Default::default()
        }
    }

    pub fn choices(mut self, choices: Vec<String>) -> Self {
        self.choices = choices;
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn at_paragraph(mut self, paragraph: usize) -> Self {
        self.paragraph = paragraph;
        self
    }

    pub fn is_choice(&self) -> bool {
        self.choices.len() >= 2
    }
}

/// 一次学习会话的日志。
#[derive(Debug, Clone)]
pub struct SessionLog {
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub seconds: u32,
    pub new_count: u32,
    pub review_count: u32,
}

/// 导入词书的结果。
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub books: Vec<(String, u32)>,
    pub inserted: u32,
    pub updated: u32,
    pub skipped: u32,
}

impl ImportReport {
    pub fn merge(&mut self, other: ImportReport) {
        self.inserted += other.inserted;
        self.updated += other.updated;
        self.skipped += other.skipped;
        for (slug, count) in other.books {
            if let Some(entry) = self.books.iter_mut().find(|(s, _)| s == &slug) {
                entry.1 += count;
            } else {
                self.books.push((slug, count));
            }
        }
    }
}

/// 导入 StarDict 词典的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DictReport {
    /// 新建的词条数。
    pub inserted: u32,
    /// 已存在、被覆盖或补全的词条数。
    pub updated: u32,
    /// 其中因为合并而真的多出义项的词条数；不带 `--merge` 时恒为 0。
    ///
    /// 这个数字是判断「这本词典值不值得叠」的唯一依据：如果叠完几乎不涨，
    /// 说明它的内容已经被库里的词典覆盖了。
    pub grown: u32,
    /// 从「相关词组」段抽出的搭配条数。
    pub phrases: u32,
    /// 其中通过查词库补上了中文释义的条数。
    pub phrases_translated: u32,
}

/// 一次性导入到某个词书的计划，由解析层产出、存储层消费。
#[derive(Debug, Clone)]
pub struct ImportPlan {
    pub book_slug: String,
    pub book_name: String,
    pub description: String,
    pub words: Vec<Word>,
}

impl ImportPlan {
    pub fn new(slug: impl Into<String>, name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            book_slug: slug.into(),
            book_name: name.into(),
            description: description.into(),
            words: Vec::new(),
        }
    }
}
