//! 存储端口（trait）。
//!
//! 应用层只依赖这个 trait，`qe-store` 提供 SQLite 实现。
//! 未来要换成别的后端（例如本地 JSON、Postgres），只需新增实现。

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};

use crate::model::{
    Book, BookProgress, DailyStats, DictReport, Example, ImportPlan, ImportReport, Quiz, Rating,
    ReadingLog, ReadingStats, SessionLog, SrsCard, StudyCard, Word,
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
    /// 记一次评分。`lemma` 是跨库键：词书与卡片存它而不是 `words.id`，
    /// 这样换一份词典也不会把进度挂到别的词上。
    fn record_review(
        &mut self,
        lemma: &str,
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
        lemma: &str,
        book_id: i64,
        after: &SrsCard,
        rating: Rating,
        now: DateTime<Utc>,
        is_new: bool,
    ) -> Result<()>;
    fn record_session(&mut self, log: &SessionLog) -> Result<()>;

    // ---- 卡片控制 ----
    /// 把一张卡推迟到之后（bury）。
    fn postpone(&mut self, lemma: &str, until: DateTime<Utc>) -> Result<()>;
    /// 恢复到之前的记忆状态；`None` 表示删除卡片（撤销新学）。
    fn restore_card(&mut self, lemma: &str, card: Option<&SrsCard>) -> Result<()>;
    /// 删除该词最新的一条评分流水。
    fn drop_last_review(&mut self, lemma: &str) -> Result<()>;
    /// 把词条加入词书（已存在则忽略）。返回是否新增。
    fn add_to_book(&mut self, book_id: i64, word: &Word) -> Result<bool>;

    // ---- 查询 ----
    fn word(&self, lemma: &str) -> Result<Option<Word>>;
    /// 按单词或词根片段搜索，`needle` 不区分大小写。
    fn search(&self, needle: &str, limit: usize) -> Result<Vec<Word>>;
    /// 所有词书里出现过的词（小写），用于给例句挑选候选。
    fn deck_lemmas(&self) -> Result<Vec<String>>;

    // ---- 阅读 ----
    /// 保存（或更新）一篇文章，按标题去重。返回文本 id。
    fn save_text(&mut self, text: &crate::reading::Text) -> Result<i64>;
    /// 列出所有已导入的文本，最近导入的在前。
    fn texts(&self) -> Result<Vec<crate::reading::Text>>;
    /// 已导入文本的标题，用于 `/read` 的参数补全（不读正文）。
    fn text_titles(&self) -> Result<Vec<String>> {
        Ok(self.texts()?.into_iter().map(|text| text.title).collect())
    }
    /// 按标题取文本（不区分大小写）。
    fn text_by_title(&self, title: &str) -> Result<Option<crate::reading::Text>>;
    /// 删除一篇文章及其进度、标注、测验。
    fn delete_text(&mut self, id: i64) -> Result<()>;
    /// 上次读到哪一段、用的哪种模式。
    fn reading_progress(&self, text_id: i64) -> Result<Option<(usize, crate::reading::ReadMode)>>;
    /// 记住进度。
    fn save_reading_progress(
        &mut self,
        text_id: i64,
        paragraph: usize,
        mode: crate::reading::ReadMode,
    ) -> Result<()>;
    /// 用户标记过哪些词（`text_id = 0` 表示不限文章）。
    fn reading_marks(&self, text_id: i64) -> Result<Vec<String>>;
    /// 标记一个词；已存在返回 `false`。
    fn add_reading_mark(&mut self, text_id: i64, lemma: &str, paragraph: usize) -> Result<bool>;
    /// 取消标记；不存在返回 `false`。
    fn remove_reading_mark(&mut self, text_id: i64, lemma: &str) -> Result<bool>;
    /// 写一条阅读日志（结束阅读时调用）。
    fn record_reading(&mut self, log: &ReadingLog) -> Result<()>;
    /// 当天的阅读统计。
    fn daily_reading(&self, day: NaiveDate) -> Result<ReadingStats>;
    /// 某篇文章的测验题。
    fn quizzes(&self, text_id: i64) -> Result<Vec<Quiz>>;
    /// 写入测验题（按 `(text_id, paragraph, prompt)` 去重）。返回新增条数。
    fn add_quiz(&mut self, quiz: &Quiz) -> Result<bool>;
    /// 记录一次作答。
    fn record_attempt(&mut self, quiz_id: i64, correct: bool) -> Result<()>;

    // ---- 统计 ----
    fn daily(&self, day: NaiveDate) -> Result<DailyStats>;
    fn streak(&self) -> Result<u32>;
    fn upcoming(&self, from: NaiveDate, days: usize) -> Result<Vec<(NaiveDate, u32)>>;
    fn learned_today(&self, book_id: i64) -> Result<u32>;

    // ---- 设置 ----
    fn setting(&self, key: &str) -> Result<Option<String>>;
    fn set_setting(&self, key: &str, value: &str) -> Result<()>;
    /// 数据文件清单：`(用途, 显示路径, 字节数)`。
    ///
    /// 学习库和词典库是两个文件，`/db` 与 `quietly db` 都从这里取，
    /// 免得上层再去关心具体实现里的路径字段。
    fn db_files(&self) -> Vec<(String, String, u64)>;
    // ---- 导入 ----
    fn import_plan(&mut self, plan: &ImportPlan) -> Result<ImportReport>;
    fn add_examples(&mut self, examples: &[(String, Example)]) -> Result<u32>;
    /// 导入常用词组表（`lemma<TAB>phrase<TAB>gloss`），只更新已有词条。
    /// 返回 `(覆盖词数, 新增搭配数)`。
    fn import_phrases(&mut self, path: &Path) -> Result<(u32, u32)>;
    /// 给一个词追加一条搭配；词不存在时返回 `false`。
    fn add_collocation(&mut self, lemma: &str, phrase: &str, gloss: &str) -> Result<bool>;
    /// 给缺释义的搭配补中文：直接在 `words` 里查该搭配自己的条目。
    ///
    /// 从词典抽出来的搭配只有英文（`相关词组:` 段不带翻译），但 `for the sake of`
    /// 这类短语在 ECDICT 里本身就是词条，查一下就拿到了。返回补上的条数。
    fn resolve_collocation_glosses(&mut self) -> Result<u32>;
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
    fn import_stardict(&mut self, path: &Path) -> Result<DictReport> {
        self.import_stardict_with(path, false, &mut |_| {})
    }

    /// 同上。`merge` 为真时把新义项追加到已有释义后，而不是覆盖；
    /// 依次导入多本词典时用它才能逐本累加。
    fn import_stardict_with(
        &mut self,
        path: &Path,
        merge: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<DictReport>;
}
