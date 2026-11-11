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
    /// 所有词书里出现过的词（小写），用于给例句挑选候选。
    fn deck_lemmas(&self) -> Result<Vec<String>>;

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
    /// 导入常用词组表（`lemma<TAB>phrase<TAB>gloss`），只更新已有词条。
    /// 返回 `(覆盖词数, 新增搭配数)`。
    fn import_phrases(&mut self, path: &Path) -> Result<(u32, u32)>;
    /// 给一个词追加一条搭配；词不存在时返回 `false`。
    fn add_collocation(&mut self, lemma: &str, phrase: &str, gloss: &str) -> Result<bool>;
    /// 按文件后缀与表头自动选择导入器（ECDICT / JSON / CSV / TXT）。
    fn import_file(&mut self, path: &Path) -> Result<ImportReport>;
    /// 导入有出处的词源数据（kaikki/Wiktionary JSONL 或 TSV），只更新已有词条。
    /// 返回 `(已更新词数, 词素数)`。
    fn import_etymology(&mut self, path: &Path) -> Result<(u32, u32)> {
        self.import_etymology_with(path, &mut |_| {})
    }

    /// 同上，但每扫过 10 万行回调一次已扫描行数。
    fn import_etymology_with(
        &mut self,
        path: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(u32, u32)>;

    /// 导入 StarDict 词典（ECDICT 的 `ecdict-stardict` 包），写入 `words` 表。
    /// 返回 `(新增, 更新)`。
    fn import_stardict(&mut self, path: &Path) -> Result<(u32, u32)> {
        self.import_stardict_with(path, false, &mut |_| {})
    }

    /// 同上。`merge` 为真时把新义项追加到已有释义后，而不是覆盖；
    /// 依次导入多本词典时用它才能逐本累加。
    fn import_stardict_with(
        &mut self,
        path: &Path,
        merge: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(u32, u32)>;
}
