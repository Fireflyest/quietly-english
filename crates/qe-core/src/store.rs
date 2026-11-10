//! 存储端口（trait）。
//!
//! 应用层只依赖这个 trait，`qe-store` 提供 SQLite 实现。
//! 未来要换成别的后端（例如本地 JSON、Postgres），只需新增实现。

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};

use crate::model::{
    Book, BookProgress, DailyStats, Example, ImportPlan, ImportReport, Rating, SessionLog, SrsCard,
    StudyCard, Word,
};

pub trait Store {
    // ---- 词书 ----
    fn list_books(&self) -> Result<Vec<Book>>;
    fn book_by_slug(&self, slug: &str) -> Result<Option<Book>>;
    fn current_book(&self) -> Result<Option<Book>>;
    fn set_current_book(&self, slug: &str) -> Result<()>;
    fn progress(&self, book_id: i64) -> Result<BookProgress>;

    // ---- 学习队列 ----
    fn due_cards(&self, book_id: i64, now: DateTime<Utc>, limit: usize) -> Result<Vec<StudyCard>>;
    fn new_cards(&self, book_id: i64, limit: usize) -> Result<Vec<Word>>;
    fn record_review(
        &mut self,
        word_id: i64,
        book_id: i64,
        before: Option<&SrsCard>,
        after: &SrsCard,
        rating: Rating,
        now: DateTime<Utc>,
        is_new: bool,
    ) -> Result<()>;
    /// Replace the most recent review for a word (re-selecting 1-4 on the same card).
    /// Counters must not be incremented again.
    fn amend_review(
        &mut self,
        word_id: i64,
        book_id: i64,
        after: &SrsCard,
        rating: Rating,
        now: DateTime<Utc>,
        is_new: bool,
    ) -> Result<()>;
    fn record_session(&mut self, log: &SessionLog) -> Result<()>;

    // ---- 卡片控制 ----
    /// 把一张卡推迟到之后（bury）。
    fn postpone(&mut self, word_id: i64, until: DateTime<Utc>) -> Result<()>;
    /// 恢复到之前的记忆状态；`None` 表示删除卡片（撤销新学）。
    fn restore_card(&mut self, word_id: i64, card: Option<&SrsCard>) -> Result<()>;
    /// 删除该词最新的一条评分流水。
    fn drop_last_review(&mut self, word_id: i64) -> Result<()>;
    /// 把词条加入词书（已存在则忽略）。返回是否新增。
    fn add_to_book(&mut self, book_id: i64, word: &Word) -> Result<bool>;

    // ---- 查询 ----
    fn word(&self, lemma: &str) -> Result<Option<Word>>;
    /// 按单词或词根片段搜索，`needle` 不区分大小写。
    fn search(&self, needle: &str, limit: usize) -> Result<Vec<Word>>;

    // ---- 统计 ----
    fn daily(&self, day: NaiveDate) -> Result<DailyStats>;
    fn streak(&self) -> Result<u32>;
    fn upcoming(&self, from: NaiveDate, days: usize) -> Result<Vec<(NaiveDate, u32)>>;
    fn learned_today(&self, book_id: i64) -> Result<u32>;

    // ---- 设置 ----
    fn setting(&self, key: &str) -> Result<Option<String>>;
    fn set_setting(&self, key: &str, value: &str) -> Result<()>;

    // ---- 导入 ----
    fn import_plan(&mut self, plan: &ImportPlan) -> Result<ImportReport>;
    fn add_examples(&mut self, examples: &[(String, Example)]) -> Result<u32>;
    /// 按文件后缀与表头自动选择导入器（ECDICT / JSON / CSV / TXT）。
    fn import_file(&mut self, path: &Path) -> Result<ImportReport>;
}
