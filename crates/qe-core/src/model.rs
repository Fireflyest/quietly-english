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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordRoot {
    pub part: String,
    pub gloss: String,
    pub kind: RootKind,
}

impl WordRoot {
    pub fn new(part: impl Into<String>, gloss: impl Into<String>, kind: RootKind) -> Self {
        Self {
            part: part.into(),
            gloss: gloss.into(),
            kind,
        }
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
}

impl Word {
    pub fn new(lemma: impl Into<String>, translation: impl Into<String>) -> Self {
        Self {
            lemma: lemma.into(),
            translation: translation.into(),
            ..Default::default()
        }
    }

    /// 优先使用词库中人工录入的词根，缺失时退回词缀表推测。
    /// 返回 `(roots, guessed)`。
    pub fn roots_or_guess(&self) -> (Vec<WordRoot>, bool) {
        if !self.roots.is_empty() {
            return (self.roots.clone(), false);
        }
        (crate::affix::decompose(&self.lemma), true)
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
