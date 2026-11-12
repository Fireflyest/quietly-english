//! SQLite 适配器。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use qe_core::model::{
    Book, BookProgress, Collocation, DailyStats, DictReport, Example, ImportPlan, ImportReport,
    Phase, Quiz, Rating, ReadingLog, ReadingStats, SessionLog, SrsCard, StudyCard, Word,
};
use qe_core::store::Store;
use qe_core::time;

/// 词典表。它单独放在 `quietly-words.db` 里：327 万词条会让整个文件涨到 500 MB+，
/// 而学习数据只有几 MB。分开之后学习数据可以进 git，词典随时能从下载包重建。
///
/// 跨库外键在 SQLite 里不生效，所以 `book_words` / `cards` / `reviews` 存的是
/// `lemma`（v6 起）而不是 `dict.words.id`：词典重建时 id 会按导入顺序重排，
/// 存 id 的词书会静默指向别的词。`quietly doctor` 负责校验引用是否还有对应词条。
const SCHEMA_WORDS: &str = r#"
CREATE TABLE IF NOT EXISTS dict.words (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    lemma       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    phonetic    TEXT,
    translation TEXT NOT NULL DEFAULT '',
    definition  TEXT,
    roots       TEXT NOT NULL DEFAULT '[]',
    examples    TEXT NOT NULL DEFAULT '[]',
    tags        TEXT NOT NULL DEFAULT '[]',
    source      TEXT NOT NULL DEFAULT ''
);
"#;

/// 学习数据。不含 `words`，也不带指向它的外键。
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS books (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    slug        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    builtin     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS book_words (
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    word_id INTEGER NOT NULL,
    rank    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (book_id, word_id)
);

CREATE TABLE IF NOT EXISTS cards (
    word_id        INTEGER PRIMARY KEY,
    book_id        INTEGER,
    phase          TEXT NOT NULL DEFAULT 'new',
    step           INTEGER NOT NULL DEFAULT 0,
    ease           REAL NOT NULL DEFAULT 2.5,
    interval_days  INTEGER NOT NULL DEFAULT 0,
    reps           INTEGER NOT NULL DEFAULT 0,
    lapses         INTEGER NOT NULL DEFAULT 0,
    due_at         TEXT,
    due_ts         INTEGER,
    last_review_at TEXT
);

CREATE TABLE IF NOT EXISTS reviews (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    word_id         INTEGER NOT NULL,
    at              TEXT NOT NULL,
    day             TEXT NOT NULL,
    rating          TEXT NOT NULL,
    phase_after     TEXT NOT NULL,
    interval_after  INTEGER NOT NULL DEFAULT 0,
    is_new          INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sessions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    day         TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    ended_at    TEXT NOT NULL,
    seconds     INTEGER NOT NULL DEFAULT 0,
    new_count   INTEGER NOT NULL DEFAULT 0,
    review_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_cards_due ON cards(due_ts);
CREATE INDEX IF NOT EXISTS idx_book_words_book ON book_words(book_id);
CREATE INDEX IF NOT EXISTS idx_reviews_day ON reviews(day);
CREATE INDEX IF NOT EXISTS idx_reviews_word ON reviews(word_id);
"#;

const WORD_COLS: &str = "w.id, w.lemma, w.phonetic, w.translation, w.definition, w.roots, w.examples, w.tags, w.collocations, w.glosses";

/// v4：阅读模块。
///
/// `texts.translation` 与 `body` 用同一套段落切分，所以两者按空行对齐；
/// 译文可以只写前几段（未译的段落留空），读取时按索引取。
const SCHEMA_V4: &str = r#"
CREATE TABLE IF NOT EXISTS texts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    title       TEXT NOT NULL UNIQUE,
    source      TEXT,
    license     TEXT,
    body        TEXT NOT NULL,
    translation TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reading_progress (
    text_id     INTEGER PRIMARY KEY REFERENCES texts(id) ON DELETE CASCADE,
    paragraph   INTEGER NOT NULL DEFAULT 0,
    mode        TEXT NOT NULL DEFAULT 'full',
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reading_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    text_id     INTEGER NOT NULL,
    day         TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    ended_at    TEXT NOT NULL,
    seconds     INTEGER NOT NULL DEFAULT 0,
    paragraphs  INTEGER NOT NULL DEFAULT 0,
    words_opened INTEGER NOT NULL DEFAULT 0,
    words_added  INTEGER NOT NULL DEFAULT 0
);

-- 用户手动标注的词。`text_id = 0` 表示「阅读中随手标记、不归属某篇文章」。
CREATE TABLE IF NOT EXISTS reading_marks (
    text_id     INTEGER NOT NULL,
    lemma       TEXT NOT NULL COLLATE NOCASE,
    paragraph   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (text_id, lemma)
);

-- 读后测验。题目必须带出处，`source` 指回具体语料或用户自备文件。
CREATE TABLE IF NOT EXISTS quizzes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    text_id     INTEGER NOT NULL,
    paragraph   INTEGER NOT NULL DEFAULT 0,
    prompt      TEXT NOT NULL,
    answer      TEXT NOT NULL,
    choices     TEXT NOT NULL DEFAULT '[]',
    source      TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS quiz_attempts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    quiz_id     INTEGER NOT NULL,
    day         TEXT NOT NULL,
    at          TEXT NOT NULL,
    correct     INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_reading_log_day ON reading_log(day);
CREATE INDEX IF NOT EXISTS idx_quizzes_text ON quizzes(text_id);
"#;

/// v6：词书、卡片、复习流水改用 `lemma` 作为跨库键。
///
/// 词典库里 `words.id` 是 AUTOINCREMENT，换一份词典或换导入顺序就会变，
/// 而 `lemma` 在两个库里都唯一且稳定。迁移由 [`SqliteStore::rekey_on_lemma`] 完成，
/// 这里只是新布局的说明。
pub struct SqliteStore {
    conn: Connection,
    path: PathBuf,
    /// 词典库文件（`quietly-words.db`），与 `path` 同目录。
    words_path: PathBuf,
}

/// `quietly doctor` 的结果。
#[derive(Debug, Default, Clone)]
pub struct DoctorReport {
    /// 词典库里的词条数。
    pub words: u64,
    /// 每本词书的词数：`(slug, 条数)`。
    pub decks: Vec<(String, u64)>,
    /// 一条词都没有的词书。
    pub empty_decks: Vec<String>,
    /// 词书引用了、词典里却没有的词：`(slug, lemma)`。
    pub missing_in_decks: Vec<(String, String)>,
    /// 卡片引用了、词典里却没有的词。
    pub missing_cards: Vec<String>,
    /// 复习流水里引用不到词条的条数。
    pub missing_reviews: u64,
    /// `--fix` 清掉的悬空引用条数；没加 `--fix` 时恒为 0。
    pub fixed: u64,
}

impl DoctorReport {
    /// 引用是否完整（空词书只算提示，不算错误）。
    pub fn is_clean(&self) -> bool {
        self.missing_in_decks.is_empty() && self.missing_cards.is_empty() && self.missing_reviews == 0
    }

    /// 悬空引用总条数。
    pub fn orphans(&self) -> u64 {
        self.missing_in_decks.len() as u64 + self.missing_cards.len() as u64 + self.missing_reviews
    }
}

impl SqliteStore {
    /// 词典库文件名。学习库叫 `quietly.db`，它叫 `quietly-words.db`。
    fn words_file_for(core: &Path) -> PathBuf {
        let stem = core
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "quietly".to_string());
        core.with_file_name(format!("{stem}-words.db"))
    }

    /// 默认位置：`%APPDATA%/quietly-english`（Windows）或 `~/.local/share/quietly-english`。
    /// 可用环境变量 `QUIETLY_HOME` 覆盖。
    pub fn open_default() -> Result<Self> {
        let dir = if let Some(custom) = std::env::var_os("QUIETLY_HOME") {
            PathBuf::from(custom)
        } else {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("quietly-english")
        };
        Self::open_at(dir.join("quietly.db"))
    }

    pub fn open_at(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("无法创建数据目录 {}", parent.display()))?;
            }
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("无法打开数据库 {}", path.display()))?;
        let _: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap_or_else(|_| "wal".to_string());
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        // 词典单独一个文件。ATTACH 让 `dict.words` 能和学习表共用同一个连接和
        // 事务，跨库 JOIN 也照常可用。
        let words_path = Self::words_file_for(&path);
        conn.execute(
            "ATTACH DATABASE ?1 AS dict",
            [words_path.to_string_lossy().as_ref()],
        )
        .with_context(|| format!("无法打开词典库 {}", words_path.display()))?;
        conn.execute_batch(SCHEMA_WORDS)?;

        let mut store = Self {
            conn,
            path,
            words_path,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 词典库文件的路径。`,quietly doctor` 用它算体积。
    pub fn words_path(&self) -> &Path {
        &self.words_path
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
            self.conn.execute_batch("PRAGMA user_version = 1")?;
        }
        if version < 4 {
            // 阅读模块。文本与译文都存原文，段落切分在读取时做，所以只需要一张表。
            self.conn.execute_batch(SCHEMA_V4)?;
            self.conn.execute_batch("PRAGMA user_version = 4")?;
        }
        // 词典库的列不跟主库版本号走：老库和新库都要补齐这两列，补之前先看
        // 实际列名，所以重复执行也不会出错。
        self.ensure_words_columns()?;
        if version < 5 {
            // 把 `words` 从学习库里搬出去，换成独立的词典库文件。
            //
            // 老库里 `book_words` / `cards` 带指向 `words` 的外键，跨库外键在 SQLite
            // 里不生效，所以这两张表要重建一次。它们只有几万行，很快。
            self.conn.execute_batch("PRAGMA foreign_keys = OFF")?;
            let moved = self.move_words_out()?;
            self.conn.execute_batch("PRAGMA foreign_keys = ON")?;
            self.conn.execute_batch("PRAGMA user_version = 5")?;
            if moved {
                // DROP TABLE 只把页标记为空闲，文件不会自己变小。VACUUM 把 500MB
                // 的词典页从学习库里真正收回去，之后主库只剩几 MB。
                self.conn.execute_batch("VACUUM")?;
            }
        }
        if version < 6 {
            // 词书、卡片、复习流水从 `word_id` 换成 `lemma`。
            self.rekey_on_lemma()?;
        }
        Ok(())
    }

    /// 词典库的列结构补齐。
    ///
    /// `ALTER TABLE ... ADD COLUMN` 带常量默认值时是纯元数据操作，327 万行也是瞬间。
    /// 先查实际列名，所以反复调用安全。
    fn ensure_words_columns(&mut self) -> Result<()> {
        let mut existing: Vec<String> = Vec::new();
        {
            let mut stmt = self.conn.prepare("PRAGMA dict.table_info(words)")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            for row in rows {
                existing.push(row?);
            }
        }
        for (name, decl) in [
            ("collocations", "TEXT NOT NULL DEFAULT '[]'"),
            ("glosses", "TEXT NOT NULL DEFAULT '[]'"),
        ] {
            if !existing.iter().any(|col| col == name) {
                self.conn.execute_batch(&format!(
                    "ALTER TABLE dict.words ADD COLUMN {name} {decl};"
                ))?;
            }
        }
        Ok(())
    }

    /// v5 → v6：词书、卡片、复习流水从 `word_id` 改成 `lemma`。
    ///
    /// 词典库的 `id` 是 AUTOINCREMENT，换一份词典或换导入顺序就会变；存 id 时
    /// 词书会静默指向别的词（原来的 `alpha` 变成 `gamma`）。`lemma` 在两个库里
    /// 都唯一，换词典后依然指向同一个词。
    ///
    /// 词典里已经找不到的词条会在迁移中丢弃——它们本来就无法显示，
    /// 之后可以用 `quietly doctor` 看到引用是否完整。
    fn rekey_on_lemma(&mut self) -> Result<()> {
        // 老库可能是手工建的，缺表比缺列常见；缺哪张就直接建新布局。
        let exists = |conn: &Connection, name: &str| -> Result<bool> {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM main.sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        };
        let has_book_words = exists(&self.conn, "book_words")?;
        let has_cards = exists(&self.conn, "cards")?;
        let has_reviews = exists(&self.conn, "reviews")?;

        let mut sql = String::new();
        if has_book_words {
            sql.push_str(
                "CREATE TABLE book_words_lemma (
                    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
                    lemma   TEXT NOT NULL COLLATE NOCASE,
                    rank    INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (book_id, lemma)
                 );
                 INSERT OR IGNORE INTO book_words_lemma(book_id, lemma, rank)
                    SELECT bw.book_id, w.lemma, bw.rank
                    FROM book_words bw JOIN dict.words w ON w.id = bw.word_id;
                 DROP TABLE book_words;
                 ALTER TABLE book_words_lemma RENAME TO book_words;",
            );
        } else {
            sql.push_str(
                "CREATE TABLE book_words (
                    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
                    lemma   TEXT NOT NULL COLLATE NOCASE,
                    rank    INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (book_id, lemma)
                 );",
            );
        }
        sql.push_str("CREATE INDEX IF NOT EXISTS idx_book_words_book ON book_words(book_id);");

        const CARDS_V6: &str = "
            lemma          TEXT NOT NULL COLLATE NOCASE PRIMARY KEY,
            book_id        INTEGER,
            phase          TEXT NOT NULL DEFAULT 'new',
            step           INTEGER NOT NULL DEFAULT 0,
            ease           REAL NOT NULL DEFAULT 2.5,
            interval_days  INTEGER NOT NULL DEFAULT 0,
            reps           INTEGER NOT NULL DEFAULT 0,
            lapses         INTEGER NOT NULL DEFAULT 0,
            due_at         TEXT,
            due_ts         INTEGER,
            last_review_at TEXT";
        if has_cards {
            sql.push_str(&format!(
                "CREATE TABLE cards_lemma ({CARDS_V6});
                 INSERT OR IGNORE INTO cards_lemma
                    (lemma, book_id, phase, step, ease, interval_days, reps, lapses,
                     due_at, due_ts, last_review_at)
                    SELECT w.lemma, c.book_id, c.phase, c.step, c.ease, c.interval_days,
                           c.reps, c.lapses, c.due_at, c.due_ts, c.last_review_at
                    FROM cards c JOIN dict.words w ON w.id = c.word_id;
                 DROP TABLE cards;
                 ALTER TABLE cards_lemma RENAME TO cards;"
            ));
        } else {
            sql.push_str(&format!("CREATE TABLE cards ({CARDS_V6});"));
        }
        sql.push_str("CREATE INDEX IF NOT EXISTS idx_cards_due ON cards(due_ts);");

        const REVIEWS_V6: &str = "
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            lemma           TEXT NOT NULL COLLATE NOCASE,
            at              TEXT NOT NULL,
            day             TEXT NOT NULL,
            rating          TEXT NOT NULL,
            phase_after     TEXT NOT NULL,
            interval_after  INTEGER NOT NULL DEFAULT 0,
            is_new          INTEGER NOT NULL DEFAULT 0";
        if has_reviews {
            sql.push_str(&format!(
                "CREATE TABLE reviews_lemma ({REVIEWS_V6});
                 INSERT INTO reviews_lemma
                    (id, lemma, at, day, rating, phase_after, interval_after, is_new)
                    SELECT r.id, w.lemma, r.at, r.day, r.rating, r.phase_after,
                           r.interval_after, r.is_new
                    FROM reviews r JOIN dict.words w ON w.id = r.word_id;
                 DROP TABLE reviews;
                 ALTER TABLE reviews_lemma RENAME TO reviews;"
            ));
        } else {
            sql.push_str(&format!("CREATE TABLE reviews ({REVIEWS_V6});"));
        }
        sql.push_str(
            "CREATE INDEX IF NOT EXISTS idx_reviews_day ON reviews(day);
             CREATE INDEX IF NOT EXISTS idx_reviews_lemma ON reviews(lemma);
             PRAGMA user_version = 6;",
        );

        self.conn.execute_batch("PRAGMA foreign_keys = OFF")?;
        let tx = self.conn.transaction()?;
        tx.execute_batch(&sql)?;
        tx.commit()?;
        self.conn.execute_batch("PRAGMA foreign_keys = ON")?;
        Ok(())
    }

    /// 老布局（`words` 在学习库里）迁移到新布局。
    /// 老布局（`words` 在学习库里）迁移到新布局。返回是否真的搬过。
    fn move_words_out(&mut self) -> Result<bool> {
        let has_old: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM main.sqlite_master WHERE type = 'table' AND name = 'words'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)?;
        if !has_old {
            return Ok(false);
        }

        let tx = self.conn.transaction()?;
        // 逐列搬，老库可能还没有 collocations / glosses。
        let cols: Vec<String> = tx
            .prepare("PRAGMA main.table_info(words)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let wanted = [
            "id",
            "lemma",
            "phonetic",
            "translation",
            "definition",
            "roots",
            "examples",
            "tags",
            "source",
            "collocations",
            "glosses",
        ];
        let present: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|name| cols.iter().any(|col| col == name))
            .collect();
        let list = present.join(", ");
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO dict.words({list}) SELECT {list} FROM main.words;"
        ))?;

        // 重建两张带外键的子表，去掉指向 words 的约束。
        tx.execute_batch(
            "ALTER TABLE book_words RENAME TO book_words_old;
             CREATE TABLE book_words (
                book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
                word_id INTEGER NOT NULL,
                rank    INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (book_id, word_id)
             );
             INSERT INTO book_words(book_id, word_id, rank)
                SELECT book_id, word_id, rank FROM book_words_old;
             DROP TABLE book_words_old;
             CREATE INDEX IF NOT EXISTS idx_book_words_book ON book_words(book_id);

             ALTER TABLE cards RENAME TO cards_old;
             CREATE TABLE cards (
                word_id        INTEGER PRIMARY KEY,
                book_id        INTEGER,
                phase          TEXT NOT NULL DEFAULT 'new',
                step           INTEGER NOT NULL DEFAULT 0,
                ease           REAL NOT NULL DEFAULT 2.5,
                interval_days  INTEGER NOT NULL DEFAULT 0,
                reps           INTEGER NOT NULL DEFAULT 0,
                lapses         INTEGER NOT NULL DEFAULT 0,
                due_at         TEXT,
                due_ts         INTEGER,
                last_review_at TEXT
             );
             INSERT INTO cards SELECT * FROM cards_old;
             DROP TABLE cards_old;
             CREATE INDEX IF NOT EXISTS idx_cards_due ON cards(due_ts);

             DROP TABLE main.words;",
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// 流式导入 ECDICT（约 77 万词）。
    ///
    /// 所有词条进入 `words` 表供查词；带 `tag` 的词按标签进入对应词书。
    pub fn import_ecdict(&mut self, path: &Path) -> Result<ImportReport> {
        let file = fs::File::open(path)
            .with_context(|| format!("无法打开 {}", path.display()))?;
        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(std::io::BufReader::new(file));

        let headers = reader.headers()?.clone();
        let col = |name: &str| headers.iter().position(|h| h.eq_ignore_ascii_case(name));
        let word_idx = col("word").context("ECDICT 缺少 word 列")?;
        let phonetic_idx = col("phonetic");
        let definition_idx = col("definition");
        let translation_idx = col("translation");
        let tag_idx = col("tag");
        let pos_idx = col("pos");

        let tags: [(&'static str, &'static str, &'static str); 8] = [
            ("zk", "Junior High Core", "Junior high / entrance exam vocabulary (ECDICT)"),
            ("gk", "Senior High Core", "Senior high / gaokao vocabulary (ECDICT)"),
            ("cet4", "CET-4 Core", "College English Test Band 4 (ECDICT)"),
            ("cet6", "CET-6 Core", "College English Test Band 6 (ECDICT)"),
            ("ky", "Postgraduate Core", "Postgraduate entrance exam vocabulary (ECDICT)"),
            ("toefl", "TOEFL Core", "TOEFL high-frequency vocabulary (ECDICT)"),
            ("ielts", "IELTS Core", "IELTS high-frequency vocabulary (ECDICT)"),
            ("gre", "GRE Core", "GRE high-frequency vocabulary (ECDICT)"),
        ];

        let tx = self.conn.transaction()?;
        let mut book_ids: HashMap<&'static str, i64> = HashMap::new();
        let mut ranks: HashMap<&'static str, i64> = HashMap::new();
        let mut report = ImportReport::default();

        for record in reader.records() {
            let record = record?;
            let lemma = record.get(word_idx).unwrap_or("").trim();
            if lemma.is_empty() || lemma.len() > 64 {
                report.skipped += 1;
                continue;
            }
            let phonetic = phonetic_idx.and_then(|i| non_empty(record.get(i)));
            let definition = definition_idx.and_then(|i| non_empty(record.get(i)));
            let translation = translation_idx
                .and_then(|i| record.get(i))
                .unwrap_or("")
                .trim()
                .to_string();
            let pos = pos_idx.and_then(|i| non_empty(record.get(i))).unwrap_or_default();
            let translation = if translation.is_empty() {
                pos
            } else if pos.is_empty() {
                translation
            } else {
                format!("{pos} {translation}")
            };

            let (_, inserted) = upsert_word_tx(
                &tx,
                lemma,
                phonetic.as_deref(),
                &translation,
                definition.as_deref(),
                "[]",
                "[]",
                "[]",
                "ecdict",
            )?;
            if inserted {
                report.inserted += 1;
            } else {
                report.updated += 1;
            }

            if let Some(tag_text) = tag_idx.and_then(|i| record.get(i)) {
                for token in tag_text.split_whitespace() {
                    let needle = token.to_ascii_lowercase();
                    let Some(entry) = tags
                        .iter()
                        .find(|entry| entry.0 == needle.as_str())
                        .copied()
                    else {
                        continue;
                    };
                    let (slug, name, description) = entry;

                    // 只有真的出现该标签时才建词书，避免导入后多出一堆空词书。
                    let book_id = match book_ids.get(slug) {
                        Some(id) => *id,
                        None => {
                            let id = ensure_book_tx(&tx, slug, name, description)?;
                            book_ids.insert(slug, id);
                            ranks.insert(slug, max_rank_tx(&tx, id)?);
                            id
                        }
                    };
                    let rank = ranks.entry(slug).or_insert(0);
                    tx.execute(
                        "INSERT OR IGNORE INTO book_words(book_id, lemma, rank) VALUES (?1, ?2, ?3)",
                        params![book_id, lemma, *rank],
                    )?;
                    *rank += 1;

                    match report.books.iter_mut().find(|(s, _)| s.as_str() == slug) {
                        Some(entry) => entry.1 += 1,
                        None => report.books.push((slug.to_string(), 1)),
                    }
                }
            }
        }
        tx.commit()?;
        Ok(report)
    }

    /// `quietly doctor`：核对词书、卡片、复习流水引用的词条还在不在词典里。
    ///
    /// 引用以 `lemma` 为准（v6 起），换词典不会错位；只有词典里根本没有这个词时
    /// 才算悬空。`fix` 为真时顺手清掉这些行——但词典为空时不动，否则一次误操作
    /// 就能把全部进度删光。
    pub fn doctor(&self, fix: bool) -> Result<DoctorReport> {
        let mut report = DoctorReport::default();
        report.words = self
            .conn
            .query_row("SELECT COUNT(*) FROM dict.words", [], |row| row.get(0))?;

        let mut stmt = self.conn.prepare(
            "SELECT b.slug, COUNT(bw.lemma)
             FROM books b LEFT JOIN book_words bw ON bw.book_id = b.id
             GROUP BY b.id ORDER BY b.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        for row in rows {
            let (slug, total) = row?;
            if total == 0 {
                report.empty_decks.push(slug.clone());
            }
            report.decks.push((slug, total));
        }

        let mut stmt = self.conn.prepare(
            "SELECT b.slug, bw.lemma
             FROM book_words bw JOIN books b ON b.id = bw.book_id
             WHERE NOT EXISTS (SELECT 1 FROM dict.words w WHERE w.lemma = bw.lemma)
             ORDER BY b.id, bw.rank",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            report.missing_in_decks.push(row?);
        }

        let mut stmt = self.conn.prepare(
            "SELECT lemma FROM cards
             WHERE NOT EXISTS (SELECT 1 FROM dict.words w WHERE w.lemma = cards.lemma)
             ORDER BY lemma",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            report.missing_cards.push(row?);
        }

        report.missing_reviews = self.conn.query_row(
            "SELECT COUNT(*) FROM reviews
             WHERE NOT EXISTS (SELECT 1 FROM dict.words w WHERE w.lemma = reviews.lemma)",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;

        if fix && report.words > 0 {
            let tx = self.conn.unchecked_transaction()?;
            let decks = tx.execute(
                "DELETE FROM book_words
                 WHERE NOT EXISTS (SELECT 1 FROM dict.words w WHERE w.lemma = book_words.lemma)",
                [],
            )?;
            let cards = tx.execute(
                "DELETE FROM cards
                 WHERE NOT EXISTS (SELECT 1 FROM dict.words w WHERE w.lemma = cards.lemma)",
                [],
            )?;
            let reviews = tx.execute(
                "DELETE FROM reviews
                 WHERE NOT EXISTS (SELECT 1 FROM dict.words w WHERE w.lemma = reviews.lemma)",
                [],
            )?;
            tx.commit()?;
            report.fixed = (decks + cards + reviews) as u64;
        }
        Ok(report)
    }
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn ensure_book_tx(tx: &Transaction<'_>, slug: &str, name: &str, description: &str) -> Result<i64> {
    tx.execute(
        "INSERT INTO books(slug, name, description) VALUES (?1, ?2, ?3)
         ON CONFLICT(slug) DO UPDATE SET
            name = excluded.name,
            description = CASE WHEN excluded.description = '' THEN books.description ELSE excluded.description END",
        params![slug, name, description],
    )?;
    let id = tx.query_row("SELECT id FROM books WHERE slug = ?1", [slug], |row| row.get(0))?;
    Ok(id)
}

fn max_rank_tx(tx: &Transaction<'_>, book_id: i64) -> Result<i64> {
    let rank: i64 = tx.query_row(
        "SELECT COALESCE(MAX(rank), 0) FROM book_words WHERE book_id = ?1",
        [book_id],
        |row| row.get(0),
    )?;
    Ok(rank)
}

#[allow(clippy::too_many_arguments)]
fn upsert_word_tx(
    tx: &Transaction<'_>,
    lemma: &str,
    phonetic: Option<&str>,
    translation: &str,
    definition: Option<&str>,
    roots: &str,
    examples: &str,
    tags: &str,
    source: &str,
) -> Result<(i64, bool)> {
    let existed: Option<i64> = tx
        .query_row("SELECT id FROM dict.words WHERE lemma = ?1", [lemma], |row| row.get(0))
        .optional()?;
    let id: i64 = tx.query_row(
        "INSERT INTO dict.words(lemma, phonetic, translation, definition, roots, examples, tags, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(lemma) DO UPDATE SET
            phonetic    = COALESCE(NULLIF(excluded.phonetic, ''), words.phonetic),
            translation = CASE WHEN excluded.translation = '' THEN words.translation ELSE excluded.translation END,
            definition  = COALESCE(NULLIF(excluded.definition, ''), words.definition),
            roots       = CASE WHEN excluded.roots = '[]' THEN words.roots ELSE excluded.roots END,
            examples    = CASE WHEN excluded.examples = '[]' THEN words.examples ELSE excluded.examples END,
            tags        = CASE WHEN excluded.tags = '[]' THEN words.tags ELSE excluded.tags END,
            source      = COALESCE(NULLIF(excluded.source, ''), words.source)
         RETURNING id",
        params![lemma, phonetic, translation, definition, roots, examples, tags, source],
        |row| row.get(0),
    )?;
    Ok((id, existed.is_none()))
}

/// 把抽出来的搭配并进已有词条。词条不存在就跳过——导入词典不该凭空造词。
fn add_collocations_tx(
    tx: &Transaction<'_>,
    lemma: &str,
    phrases: &[Collocation],
) -> Result<()> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT collocations FROM dict.words WHERE lemma = ?1",
            [lemma],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else { return Ok(()) };

    let mut list: Vec<Collocation> = serde_json::from_str(&raw).unwrap_or_default();
    let before = list.len();
    for phrase in phrases {
        if list
            .iter()
            .any(|existing| existing.phrase.eq_ignore_ascii_case(&phrase.phrase))
        {
            continue;
        }
        list.push(phrase.clone());
    }
    if list.len() == before {
        return Ok(());
    }
    tx.execute(
        "UPDATE dict.words SET collocations = ?1 WHERE lemma = ?2",
        params![serde_json::to_string(&list)?, lemma],
    )?;
    Ok(())
}

/// 查一个短语自己的词条，取出中文释义。
///
/// 只取第一条常用义，去掉词性缩写和方括号标签——卡片上的搭配行空间很小，
/// 放一整段多义项会挤成一坨。找不到就返回 `None`。
fn lookup_phrase_tx(tx: &Transaction<'_>, phrase: &str) -> Result<Option<String>> {
    let key = phrase.trim().to_lowercase();
    if key.is_empty() {
        return Ok(None);
    }
    let raw: Option<String> = tx
        .query_row(
            "SELECT translation FROM dict.words WHERE lemma = ?1",
            [key.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else { return Ok(None) };

    let parsed = qe_core::text::parse_senses(&raw);
    let Some(sense) = parsed
        .senses
        .iter()
        .find(|sense| sense.label.is_empty() && !sense.text.trim().is_empty())
    else {
        return Ok(None);
    };
    let text = strip_pos_prefix(sense.text.trim());
    // 太长的不适合塞进搭配行，宁可留空。
    if text.is_empty() || text.chars().count() > 40 {
        return Ok(None);
    }
    Ok(Some(text.to_string()))
}

/// 有些冷僻词性缩写（`na.`、`phr.`）不在 `POS_TABLE` 里，会跟着释义一起留下。
/// 搭配行只有一个字段，剥掉开头的 `<字母>.` 更干净。
fn strip_pos_prefix(text: &str) -> &str {
    let Some((head, rest)) = text.split_once(' ') else {
        return text;
    };
    let is_pos = head.ends_with('.')
        && head.len() <= 6
        && head[..head.len() - 1].chars().all(|ch| ch.is_ascii_alphabetic());
    if is_pos {
        rest.trim()
    } else {
        text
    }
}

fn decode_word(row: &Row<'_>) -> rusqlite::Result<Word> {    let roots_raw: String = row.get(5)?;
    let examples_raw: String = row.get(6)?;
    let tags_raw: String = row.get(7)?;
    let collocations_raw: String = row.get(8)?;
    let glosses_raw: String = row.get(9)?;
    Ok(Word {
        id: row.get(0)?,
        lemma: row.get(1)?,
        phonetic: row.get(2)?,
        translation: row.get(3)?,
        definition: row.get(4)?,
        roots: serde_json::from_str(&roots_raw).unwrap_or_default(),
        examples: serde_json::from_str(&examples_raw).unwrap_or_default(),
        tags: serde_json::from_str(&tags_raw).unwrap_or_default(),
        collocations: serde_json::from_str(&collocations_raw).unwrap_or_default(),
        glosses: serde_json::from_str(&glosses_raw).unwrap_or_default(),
    })
}

fn decode_card(row: &Row<'_>, offset: usize) -> rusqlite::Result<SrsCard> {
    let due_raw: Option<String> = row.get(offset + 6)?;
    let last_raw: Option<String> = row.get(offset + 7)?;
    Ok(SrsCard {
        phase: Phase::parse(&row.get::<_, String>(offset)?),
        step: row.get::<_, i64>(offset + 1)? as u8,
        ease: row.get(offset + 2)?,
        interval_days: row.get::<_, i64>(offset + 3)? as u32,
        reps: row.get::<_, i64>(offset + 4)? as u32,
        lapses: row.get::<_, i64>(offset + 5)? as u32,
        due_at: due_raw.as_deref().and_then(time::parse_rfc).unwrap_or_else(Utc::now),
        last_review_at: last_raw.as_deref().and_then(time::parse_rfc),
    })
}

fn book_from_row(row: &Row<'_>) -> rusqlite::Result<Book> {
    Ok(Book {
        id: row.get(0)?,
        slug: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
    })
}

fn parse_day(raw: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()
}

impl Store for SqliteStore {
    fn list_books(&self) -> Result<Vec<Book>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, slug, name, description FROM books ORDER BY id ASC")?;
        let rows = stmt.query_map([], book_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn book_by_slug(&self, slug: &str) -> Result<Option<Book>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, slug, name, description FROM books WHERE slug = ?1")?;
        let book = stmt
            .query_row([slug], book_from_row)
            .optional()?;
        Ok(book)
    }

    fn current_book(&self) -> Result<Option<Book>> {
        let slug = self.setting("current_book")?;
        match slug {
            Some(slug) => {
                if let Some(book) = self.book_by_slug(&slug)? {
                    return Ok(Some(book));
                }
                Ok(self.list_books()?.into_iter().next())
            }
            None => {
                let book = self.list_books()?.into_iter().next();
                if let Some(book) = &book {
                    self.set_current_book(&book.slug)?;
                }
                Ok(book)
            }
        }
    }

    fn set_current_book(&self, slug: &str) -> Result<()> {
        self.set_setting("current_book", slug)
    }

    fn progress(&self, book_id: i64) -> Result<BookProgress> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM book_words WHERE book_id = ?1",
            [book_id],
            |row| row.get(0),
        )?;
        let learned: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM book_words bw JOIN cards c ON c.lemma = bw.lemma
             WHERE bw.book_id = ?1",
            [book_id],
            |row| row.get(0),
        )?;
        let due: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM book_words bw JOIN cards c ON c.lemma = bw.lemma
             WHERE bw.book_id = ?1 AND c.due_ts IS NOT NULL AND c.due_ts <= ?2",
            params![book_id, Utc::now().timestamp()],
            |row| row.get(0),
        )?;
        Ok(BookProgress {
            total: total as u32,
            learned: learned as u32,
            due: due as u32,
        })
    }

    fn due_cards(&self, book_id: i64, now: DateTime<Utc>, limit: usize) -> Result<Vec<StudyCard>> {
        let sql = format!(
            "SELECT {WORD_COLS},
                    c.phase, c.step, c.ease, c.interval_days, c.reps, c.lapses, c.due_at, c.last_review_at
             FROM cards c
             JOIN dict.words w ON w.lemma = c.lemma
             JOIN book_words bw ON bw.lemma = c.lemma AND bw.book_id = ?1
             WHERE c.due_ts IS NOT NULL AND c.due_ts <= ?2
             ORDER BY c.due_ts ASC
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![book_id, now.timestamp(), limit as i64], |row| {
            let word = decode_word(row)?;
            // Card columns follow the word columns; keep this in step with WORD_COLS.
            let card = decode_card(row, 10)?;
            Ok(StudyCard::review(word, card))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn new_cards(&self, book_id: i64, limit: usize) -> Result<Vec<Word>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {WORD_COLS}
             FROM book_words bw
             JOIN dict.words w ON w.lemma = bw.lemma
             LEFT JOIN cards c ON c.lemma = bw.lemma
             WHERE bw.book_id = ?1 AND c.lemma IS NULL
             ORDER BY bw.rank ASC
             LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![book_id, limit as i64], decode_word)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn record_review(
        &mut self,
        lemma: &str,
        book_id: i64,
        _before: Option<&SrsCard>,
        after: &SrsCard,
        rating: Rating,
        now: DateTime<Utc>,
        is_new: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO cards(lemma, book_id, phase, step, ease, interval_days, reps, lapses, due_at, due_ts, last_review_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(lemma) DO UPDATE SET
                book_id = excluded.book_id,
                phase = excluded.phase,
                step = excluded.step,
                ease = excluded.ease,
                interval_days = excluded.interval_days,
                reps = excluded.reps,
                lapses = excluded.lapses,
                due_at = excluded.due_at,
                due_ts = excluded.due_ts,
                last_review_at = excluded.last_review_at",
            params![
                lemma,
                book_id,
                after.phase.as_str(),
                after.step as i64,
                after.ease,
                after.interval_days as i64,
                after.reps as i64,
                after.lapses as i64,
                after.due_at.to_rfc3339(),
                after.due_at.timestamp(),
                now.to_rfc3339(),
            ],
        )?;
        tx.execute(
            "INSERT INTO reviews(lemma, at, day, rating, phase_after, interval_after, is_new)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                lemma,
                now.to_rfc3339(),
                time::format_day(time::local_day(now)),
                rating.as_str(),
                after.phase.as_str(),
                after.interval_days as i64,
                if is_new { 1 } else { 0 },
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn amend_review(
        &mut self,
        lemma: &str,
        book_id: i64,
        after: &SrsCard,
        rating: Rating,
        now: DateTime<Utc>,
        is_new: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO cards(lemma, book_id, phase, step, ease, interval_days, reps, lapses, due_at, due_ts, last_review_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(lemma) DO UPDATE SET
                book_id = excluded.book_id,
                phase = excluded.phase,
                step = excluded.step,
                ease = excluded.ease,
                interval_days = excluded.interval_days,
                reps = excluded.reps,
                lapses = excluded.lapses,
                due_at = excluded.due_at,
                due_ts = excluded.due_ts,
                last_review_at = excluded.last_review_at",
            params![
                lemma,
                book_id,
                after.phase.as_str(),
                after.step as i64,
                after.ease,
                after.interval_days as i64,
                after.reps as i64,
                after.lapses as i64,
                after.due_at.to_rfc3339(),
                after.due_at.timestamp(),
                now.to_rfc3339(),
            ],
        )?;

        let latest: Option<i64> = tx
            .query_row(
                "SELECT id FROM reviews WHERE lemma = ?1 ORDER BY id DESC LIMIT 1",
                [lemma],
                |row| row.get(0),
            )
            .optional()?;

        match latest {
            Some(id) => {
                tx.execute(
                    "UPDATE reviews SET at = ?1, day = ?2, rating = ?3, phase_after = ?4,
                        interval_after = ?5, is_new = ?6 WHERE id = ?7",
                    params![
                        now.to_rfc3339(),
                        time::format_day(time::local_day(now)),
                        rating.as_str(),
                        after.phase.as_str(),
                        after.interval_days as i64,
                        if is_new { 1 } else { 0 },
                        id,
                    ],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO reviews(lemma, at, day, rating, phase_after, interval_after, is_new)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        lemma,
                        now.to_rfc3339(),
                        time::format_day(time::local_day(now)),
                        rating.as_str(),
                        after.phase.as_str(),
                        after.interval_days as i64,
                        if is_new { 1 } else { 0 },
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn record_session(&mut self, log: &SessionLog) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions(day, started_at, ended_at, seconds, new_count, review_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                time::format_day(time::local_day(log.ended_at)),
                log.started_at.to_rfc3339(),
                log.ended_at.to_rfc3339(),
                log.seconds as i64,
                log.new_count as i64,
                log.review_count as i64,
            ],
        )?;
        Ok(())
    }

    fn word(&self, lemma: &str) -> Result<Option<Word>> {
        let sql = format!("SELECT {WORD_COLS} FROM dict.words w WHERE w.lemma = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let word = stmt.query_row([lemma.trim()], decode_word).optional()?;
        Ok(word)
    }

    // ---------------------------------------------------------------- 阅读

    fn save_text(&mut self, text: &qe_core::reading::Text) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        // 按标题去重：重新导入同一篇时更新正文与译文，但保留已有进度与标注。
        self.conn.execute(
            "INSERT INTO texts(title, source, license, body, translation, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(title) DO UPDATE SET
                source = COALESCE(NULLIF(excluded.source, ''), texts.source),
                license = COALESCE(NULLIF(excluded.license, ''), texts.license),
                body = excluded.body,
                translation = CASE WHEN excluded.translation = '' THEN texts.translation
                                   ELSE excluded.translation END",
            params![
                text.title.trim(),
                text.source.as_deref(),
                text.license.as_deref(),
                text.body,
                text.translation,
                now,
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM texts WHERE title = ?1",
            [text.title.trim()],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    fn texts(&self) -> Result<Vec<qe_core::reading::Text>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, source, license, body, translation FROM texts
             ORDER BY id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(qe_core::reading::Text {
                id: row.get(0)?,
                title: row.get(1)?,
                source: row.get(2)?,
                license: row.get(3)?,
                body: row.get(4)?,
                translation: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn text_by_title(&self, title: &str) -> Result<Option<qe_core::reading::Text>> {
        let text = self
            .conn
            .query_row(
                "SELECT id, title, source, license, body, translation FROM texts
                 WHERE lower(title) = lower(?1)",
                [title.trim()],
                |row| {
                    Ok(qe_core::reading::Text {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        source: row.get(2)?,
                        license: row.get(3)?,
                        body: row.get(4)?,
                        translation: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    })
                },
            )
            .optional()?;
        Ok(text)
    }

    fn delete_text(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM quizzes WHERE text_id = ?1", [id])?;
        tx.execute("DELETE FROM reading_marks WHERE text_id = ?1", [id])?;
        tx.execute("DELETE FROM reading_log WHERE text_id = ?1", [id])?;
        tx.execute("DELETE FROM reading_progress WHERE text_id = ?1", [id])?;
        tx.execute("DELETE FROM texts WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    fn reading_progress(&self, text_id: i64) -> Result<Option<(usize, qe_core::reading::ReadMode)>> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT paragraph, mode FROM reading_progress WHERE text_id = ?1",
                [text_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(paragraph, mode)| {
            (
                paragraph.max(0) as usize,
                qe_core::reading::ReadMode::parse(&mode).unwrap_or_default(),
            )
        }))
    }

    fn save_reading_progress(
        &mut self,
        text_id: i64,
        paragraph: usize,
        mode: qe_core::reading::ReadMode,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO reading_progress(text_id, paragraph, mode, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(text_id) DO UPDATE SET
                paragraph = excluded.paragraph,
                mode = excluded.mode,
                updated_at = excluded.updated_at",
            params![
                text_id,
                paragraph as i64,
                mode.as_str(),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    fn reading_marks(&self, text_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT lemma FROM reading_marks WHERE text_id = ?1 ORDER BY created_at, lemma",
        )?;
        let rows = stmt.query_map([text_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn add_reading_mark(&mut self, text_id: i64, lemma: &str, paragraph: usize) -> Result<bool> {
        let lemma = lemma.trim().to_ascii_lowercase();
        if lemma.is_empty() {
            return Ok(false);
        }
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO reading_marks(text_id, lemma, paragraph, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![text_id, lemma, paragraph as i64, Utc::now().to_rfc3339()],
        )?;
        Ok(changed > 0)
    }

    fn remove_reading_mark(&mut self, text_id: i64, lemma: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM reading_marks WHERE text_id = ?1 AND lemma = ?2",
            params![text_id, lemma.trim()],
        )?;
        Ok(changed > 0)
    }

    fn record_reading(&mut self, log: &ReadingLog) -> Result<()> {
        let seconds = (log.ended_at - log.started_at)
            .num_seconds()
            .clamp(0, i64::from(u32::MAX)) as u32;
        self.conn.execute(
            "INSERT INTO reading_log(text_id, day, started_at, ended_at, seconds,
                                     paragraphs, words_opened, words_added)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                log.text_id,
                time::format_day(time::local_day(log.ended_at)),
                log.started_at.to_rfc3339(),
                log.ended_at.to_rfc3339(),
                seconds as i64,
                log.paragraphs as i64,
                log.words_opened as i64,
                log.words_added as i64,
            ],
        )?;
        Ok(())
    }

    fn daily_reading(&self, day: NaiveDate) -> Result<ReadingStats> {
        let key = time::format_day(day);
        let (texts, paragraphs, seconds, opened, added): (i64, i64, i64, i64, i64) = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT text_id), COALESCE(SUM(paragraphs), 0),
                        COALESCE(SUM(seconds), 0), COALESCE(SUM(words_opened), 0),
                        COALESCE(SUM(words_added), 0)
                 FROM reading_log WHERE day = ?1",
                [&key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?;
        let (quizzes, correct): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(correct), 0) FROM quiz_attempts WHERE day = ?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(ReadingStats {
            day: Some(day),
            texts: texts as u32,
            paragraphs: paragraphs as u32,
            seconds: seconds as u32,
            words_opened: opened as u32,
            words_added: added as u32,
            quizzes: quizzes as u32,
            correct: correct as u32,
        })
    }

    fn quizzes(&self, text_id: i64) -> Result<Vec<Quiz>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text_id, paragraph, prompt, answer, choices, source
             FROM quizzes WHERE text_id = ?1 ORDER BY paragraph, id",
        )?;
        let rows = stmt.query_map([text_id], |row| {
            let choices: String = row.get(5)?;
            Ok(Quiz {
                id: row.get(0)?,
                text_id: row.get(1)?,
                paragraph: row.get::<_, i64>(2)?.max(0) as usize,
                prompt: row.get(3)?,
                answer: row.get(4)?,
                choices: serde_json::from_str(&choices).unwrap_or_default(),
                source: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn add_quiz(&mut self, quiz: &Quiz) -> Result<bool> {
        // 没有出处的题目不写：无法核对对错的题不如不要。
        if quiz.prompt.trim().is_empty() || quiz.source.trim().is_empty() {
            return Ok(false);
        }
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO quizzes(text_id, paragraph, prompt, answer, choices, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                quiz.text_id,
                quiz.paragraph as i64,
                quiz.prompt.trim(),
                quiz.answer.trim(),
                serde_json::to_string(&quiz.choices)?,
                quiz.source.trim(),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(changed > 0)
    }

    fn record_attempt(&mut self, quiz_id: i64, correct: bool) -> Result<()> {
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO quiz_attempts(quiz_id, day, at, correct) VALUES (?1, ?2, ?3, ?4)",
            params![
                quiz_id,
                time::format_day(time::local_day(now)),
                now.to_rfc3339(),
                if correct { 1 } else { 0 },
            ],
        )?;
        Ok(())
    }

    fn postpone(&mut self, lemma: &str, until: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE cards SET due_at = ?1, due_ts = ?2 WHERE lemma = ?3",
            params![until.to_rfc3339(), until.timestamp(), lemma],
        )?;
        Ok(())
    }

    fn restore_card(&mut self, lemma: &str, card: Option<&SrsCard>) -> Result<()> {
        match card {
            Some(card) => {
                self.conn.execute(
                    "INSERT INTO cards(lemma, book_id, phase, step, ease, interval_days, reps, lapses, due_at, due_ts, last_review_at)
                     VALUES (?1, (SELECT book_id FROM cards WHERE lemma = ?1), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(lemma) DO UPDATE SET
                        phase = excluded.phase,
                        step = excluded.step,
                        ease = excluded.ease,
                        interval_days = excluded.interval_days,
                        reps = excluded.reps,
                        lapses = excluded.lapses,
                        due_at = excluded.due_at,
                        due_ts = excluded.due_ts,
                        last_review_at = excluded.last_review_at",
                    params![
                        lemma,
                        card.phase.as_str(),
                        card.step as i64,
                        card.ease,
                        card.interval_days as i64,
                        card.reps as i64,
                        card.lapses as i64,
                        card.due_at.to_rfc3339(),
                        card.due_at.timestamp(),
                        card.last_review_at.map(|t| t.to_rfc3339()),
                    ],
                )?;
            }
            None => {
                self.conn
                    .execute("DELETE FROM cards WHERE lemma = ?1", [lemma])?;
            }
        }
        Ok(())
    }

    fn drop_last_review(&mut self, lemma: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM reviews WHERE id = (
                SELECT id FROM reviews WHERE lemma = ?1 ORDER BY id DESC LIMIT 1
             )",
            [lemma],
        )?;
        Ok(())
    }

    fn add_to_book(&mut self, book_id: i64, word: &Word) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let roots = serde_json::to_string(&word.roots)?;
        let examples = serde_json::to_string(&word.examples)?;
        let tags = serde_json::to_string(&word.tags)?;
        let lemma = word.lemma.trim();
        // 词条必须在词典库里有一份，否则学习时 JOIN 不到它。
        upsert_word_tx(
            &tx,
            lemma,
            word.phonetic.as_deref(),
            word.translation.trim(),
            word.definition.as_deref(),
            &roots,
            &examples,
            &tags,
            "manual",
        )?;
        let rank = max_rank_tx(&tx, book_id)?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO book_words(book_id, lemma, rank) VALUES (?1, ?2, ?3)",
            params![book_id, lemma, rank],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    fn search(&self, needle: &str, limit: usize) -> Result<Vec<Word>> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {WORD_COLS} FROM dict.words w
             WHERE w.lemma LIKE ?1 OR w.translation LIKE ?1 OR w.roots LIKE ?1
             ORDER BY CASE WHEN lower(w.lemma) = lower(?2) THEN 0 WHEN w.lemma LIKE ?3 THEN 1 ELSE 2 END,
                      length(w.lemma) ASC, w.lemma ASC
             LIMIT ?4"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                format!("%{needle}%"),
                needle,
                format!("{needle}%"),
                limit as i64
            ],
            decode_word,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn deck_lemmas(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT lower(lemma) FROM book_words")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn daily(&self, day: NaiveDate) -> Result<DailyStats> {
        let key = time::format_day(day);
        let (reviews, new_learned, again, hard, good, easy): (i64, i64, i64, i64, i64, i64) =
            self.conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(is_new), 0),
                        COALESCE(SUM(rating = 'again'), 0),
                        COALESCE(SUM(rating = 'hard'), 0),
                        COALESCE(SUM(rating = 'good'), 0),
                        COALESCE(SUM(rating = 'easy'), 0)
                 FROM reviews WHERE day = ?1",
                [&key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
        let seconds: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(seconds), 0) FROM sessions WHERE day = ?1",
            [&key],
            |row| row.get(0),
        )?;
        Ok(DailyStats {
            day: Some(day),
            new_learned: new_learned as u32,
            reviews: reviews as u32,
            again: again as u32,
            hard: hard as u32,
            good: good as u32,
            easy: easy as u32,
            seconds: seconds as u32,
        })
    }

    fn streak(&self) -> Result<u32> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT day FROM reviews ORDER BY day DESC LIMIT 400")?;
        let days: Vec<NaiveDate> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok().and_then(|s| parse_day(&s)))
            .collect();
        if days.is_empty() {
            return Ok(0);
        }
        let today = time::today();
        let mut expected = if days.contains(&today) {
            today
        } else {
            today - ChronoDuration::days(1)
        };
        if !days.contains(&expected) {
            return Ok(0);
        }
        let mut count = 0;
        for day in days {
            if day == expected {
                count += 1;
                expected -= ChronoDuration::days(1);
            } else if day < expected {
                break;
            }
        }
        Ok(count)
    }

    fn upcoming(&self, from: NaiveDate, days: usize) -> Result<Vec<(NaiveDate, u32)>> {
        let horizon = from + ChronoDuration::days(days as i64);
        let mut stmt = self
            .conn
            .prepare("SELECT due_ts FROM cards WHERE due_ts IS NOT NULL AND due_ts <= ?1")?;
        let rows = stmt.query_map([time::day_start(horizon).timestamp()], |row| {
            row.get::<_, i64>(0)
        })?;
        let mut buckets: HashMap<NaiveDate, u32> = HashMap::new();
        for raw in rows {
            let ts = DateTime::<Utc>::from_timestamp(raw?, 0).unwrap_or_else(Utc::now);
            let day = time::local_day(ts).max(from);
            *buckets.entry(day).or_insert(0) += 1;
        }
        Ok((0..days as i64)
            .map(|i| {
                let day = from + ChronoDuration::days(i);
                let count = buckets.get(&day).copied().unwrap_or(0);
                (day, count)
            })
            .collect())
    }

    fn learned_today(&self, book_id: i64) -> Result<u32> {
        let key = time::format_day(time::today());
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM reviews r
             JOIN book_words bw ON bw.lemma = r.lemma AND bw.book_id = ?1
             WHERE r.day = ?2 AND r.is_new = 1",
            params![book_id, key],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    fn setting(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        Ok(value)
    }

    fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn db_files(&self) -> Vec<(String, String, u64)> {
        let size = |path: &Path| {
            fs::metadata(path)
                .map(|meta| meta.len())
                .unwrap_or(0)
        };
        vec![
            (
                "study".to_string(),
                self.path.display().to_string(),
                size(&self.path),
            ),
            (
                "words".to_string(),
                self.words_path.display().to_string(),
                size(&self.words_path),
            ),
        ]
    }

    fn import_plan(&mut self, plan: &ImportPlan) -> Result<ImportReport> {
        let tx = self.conn.transaction()?;
        // 同名词书复用已有 slug，避免 “CET-4 核心词” 与 “cet-4” 重复。
        let existing: Option<String> = tx
            .query_row(
                "SELECT slug FROM books WHERE lower(name) = lower(?1)",
                [&plan.book_name],
                |row| row.get(0),
            )
            .optional()?;
        let slug = existing.unwrap_or_else(|| plan.book_slug.clone());
        let book_id = ensure_book_tx(&tx, &slug, &plan.book_name, &plan.description)?;
        let mut rank = max_rank_tx(&tx, book_id)?;
        let mut report = ImportReport::default();

        for word in &plan.words {
            let lemma = word.lemma.trim();
            if lemma.is_empty() {
                report.skipped += 1;
                continue;
            }
            let roots = serde_json::to_string(&word.roots)?;
            let examples = serde_json::to_string(&word.examples)?;
            let tags = serde_json::to_string(&word.tags)?;
            let (_, inserted) = upsert_word_tx(
                &tx,
                lemma,
                word.phonetic.as_deref(),
                word.translation.trim(),
                word.definition.as_deref(),
                &roots,
                &examples,
                &tags,
                "import",
            )?;
            if inserted {
                report.inserted += 1;
            } else {
                report.updated += 1;
            }
            tx.execute(
                "INSERT OR IGNORE INTO book_words(book_id, lemma, rank) VALUES (?1, ?2, ?3)",
                params![book_id, lemma, rank],
            )?;
            rank += 1;
        }
        tx.commit()?;
        report.books.push((slug, plan.words.len() as u32));
        Ok(report)
    }

    fn import_file(&mut self, path: &Path) -> Result<ImportReport> {
        use crate::import::{self, FileKind};

        match import::inspect(path)? {
            FileKind::Ecdict => self.import_ecdict(path),
            FileKind::Json => {
                let plans = import::parse_json(path)?;
                let mut report = ImportReport::default();
                for plan in plans {
                    report.merge(self.import_plan(&plan)?);
                }
                Ok(report)
            }
            FileKind::WordsCsv => {
                let plans = import::parse_words_csv(path)?;
                let mut report = ImportReport::default();
                for plan in plans {
                    report.merge(self.import_plan(&plan)?);
                }
                Ok(report)
            }
            FileKind::ExamplesCsv => {
                let examples = import::parse_examples_csv(path)?;
                let added = self.add_examples(&examples)?;
                Ok(ImportReport {
                    inserted: 0,
                    updated: added,
                    skipped: examples.len() as u32 - added,
                    books: Vec::new(),
                })
            }
            FileKind::Plain => {
                let lemmas = import::parse_plain(path)?;
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("词表")
                    .to_string();
                let slug = import::slugify(&name);
                let mut plan = ImportPlan::new(slug, name, "从词表导入（释义需先导入词典）");
                let mut missing = 0u32;
                for lemma in lemmas {
                    match self.word(&lemma)? {
                        Some(word) => plan.words.push(word),
                        None => missing += 1,
                    }
                }
                let mut report = self.import_plan(&plan)?;
                report.skipped += missing;
                Ok(report)
            }
            FileKind::Etymology => {
                let (words, morphemes) = self.import_etymology(path)?;
                Ok(ImportReport {
                    inserted: 0,
                    updated: words,
                    // Reported as the number of morphemes attached; `books` is used
                    // here to surface it in the CLI summary.
                    skipped: 0,
                    books: vec![("morphemes".to_string(), morphemes)],
                })
            }
        }
    }

    fn import_stardict_with(
        &mut self,
        path: &Path,
        merge: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<DictReport> {
        use crate::{collocations, merge as merge_mod, stardict::Stardict};

        let mut dict = Stardict::open(path)?;
        let tx = self.conn.transaction()?;
        let mut report = DictReport::default();

        dict.for_each_word(progress, |word| {
            // 词条末尾的「相关词组」段属于搭配，不属于义项。必须先摘掉，
            // 否则合并时这些行会被当成新义项追加进 `translation`。
            let parts = collocations::split(&word.translation);

            // `merge` layers a second dictionary on top of the first: senses the
            // existing entry does not have are appended instead of replacing it.
            // Without it, importing 朗道 after ECDICT would wipe ECDICT's text.
            let mut translation = parts.senses.clone();
            if merge {
                if let Some(existing) = tx
                    .query_row(
                        "SELECT translation FROM dict.words WHERE lemma = ?1",
                        [word.lemma.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                {
                    let merged = merge_mod::merge_translations(&existing, &translation);
                    if merged != existing {
                        report.grown += 1;
                    }
                    translation = merged;
                }
            }

            let (_, is_new) = upsert_word_tx(
                &tx,
                &word.lemma,
                word.phonetic.as_deref(),
                &translation,
                None,
                "[]",
                "[]",
                "[]",
                "ecdict-stardict",
            )?;
            if is_new {
                report.inserted += 1;
            } else {
                report.updated += 1;
            }

            if !parts.phrases.is_empty() {
                report.phrases += parts.phrases.len() as u32;
                add_collocations_tx(&tx, &word.lemma, &parts.phrases)?;
            }
            Ok(())
        })?;

        tx.commit()?;

        // 搭配只有英文，趁词条都在库里补上中文（`for the sake of` 本身就是词条）。
        if report.phrases > 0 {
            report.phrases_translated = self.resolve_collocation_glosses()?;
        }
        Ok(report)
    }

    fn resolve_collocation_glosses(&mut self) -> Result<u32> {
        // 从词典抽出来的搭配只有英文，但短语本身往往就是词条：
        // `for the sake of` / `bank on` 在 ECDICT 里都有中文释义。
        // 一遍扫全表；有改动的行才写回。
        let pending: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT lemma, collocations FROM dict.words WHERE collocations != '[]'")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };

        let tx = self.conn.transaction()?;
        let mut filled = 0u32;
        for (lemma, raw) in pending {
            let mut list: Vec<Collocation> = serde_json::from_str(&raw).unwrap_or_default();
            let mut changed = false;
            for colloc in list.iter_mut() {
                // 已有的释义也可能带着漏网的词性缩写，顺手清一遍。
                let trimmed = colloc.gloss.trim().to_string();
                let cleaned = strip_pos_prefix(&trimmed).to_string();
                if cleaned != colloc.gloss {
                    colloc.gloss = cleaned;
                    changed = true;
                }
                if !colloc.gloss.trim().is_empty() {
                    continue;
                }
                let Some(gloss) = lookup_phrase_tx(&tx, &colloc.phrase)? else {
                    continue;
                };
                colloc.gloss = gloss;
                filled += 1;
                changed = true;
            }
            if changed {
                tx.execute(
                    "UPDATE dict.words SET collocations = ?1 WHERE lemma = ?2",
                    params![serde_json::to_string(&list)?, lemma],
                )?;
            }
        }
        tx.commit()?;
        Ok(filled)
    }

    fn import_phrases(&mut self, path: &Path) -> Result<(u32, u32)> {
        use crate::import;

        let rows = import::parse_phrases(path)?;
        let tx = self.conn.transaction()?;
        let mut words = 0u32;
        let mut added = 0u32;

        for (lemma, collocation) in rows {
            let raw: Option<String> = tx
                .query_row(
                    "SELECT collocations FROM dict.words WHERE lemma = ?1",
                    [lemma.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            // A phrases file must never invent vocabulary; it only enriches words we
            // already have.
            let Some(raw) = raw else { continue };

            let mut list: Vec<Collocation> = serde_json::from_str(&raw).unwrap_or_default();
            if list
                .iter()
                .any(|existing| existing.phrase.eq_ignore_ascii_case(&collocation.phrase))
            {
                continue;
            }
            list.push(collocation);
            tx.execute(
                "UPDATE dict.words SET collocations = ?1 WHERE lemma = ?2",
                params![serde_json::to_string(&list)?, lemma.as_str()],
            )?;
            words += 1;
            added += 1;
        }

        tx.commit()?;
        Ok((words, added))
    }

    fn add_collocation(&mut self, lemma: &str, phrase: &str, gloss: &str) -> Result<bool> {
        let lemma = lemma.trim();
        let phrase = phrase.trim();
        if lemma.is_empty() || phrase.is_empty() {
            return Ok(false);
        }
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT collocations FROM dict.words WHERE lemma = ?1",
                [lemma],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else { return Ok(false) };

        let mut list: Vec<Collocation> = serde_json::from_str(&raw).unwrap_or_default();
        if list
            .iter()
            .any(|existing| existing.phrase.eq_ignore_ascii_case(phrase))
        {
            return Ok(false);
        }
        list.push(Collocation::new(phrase, gloss.trim()));
        self.conn.execute(
            "UPDATE dict.words SET collocations = ?1 WHERE lemma = ?2",
            params![serde_json::to_string(&list)?, lemma],
        )?;
        Ok(true)
    }

    fn import_etymology_with(
        &mut self,
        path: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(u32, u32)> {
        use crate::etymology;

        // Only words we already have can be enriched; load them once instead of
        // hitting the database for every one of the million entries in the dump.
        let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = self.conn.prepare("SELECT lemma FROM dict.words")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for lemma in rows {
                existing.insert(lemma?.to_ascii_lowercase());
            }
        }

        let tx = self.conn.transaction()?;
        let mut words = 0u32;
        let mut morphemes = 0u32;
        let mut glosses = 0u32;

        if etymology::looks_like_jsonl(path) {
            // Pass one gathers the affixes' own glosses, pass two walks every word.
            let index = etymology::index_morphemes(path)?;
            etymology::for_each_analysis(path, &index, progress, |lemma, analysis| {
                if !existing.contains(&lemma.to_ascii_lowercase()) {
                    return Ok(());
                }
                if !analysis.roots.is_empty() {
                    tx.execute(
                        "UPDATE dict.words SET roots = ?1 WHERE lemma = ?2",
                        params![serde_json::to_string(&analysis.roots)?, lemma],
                    )?;
                    morphemes += analysis.roots.len() as u32;
                }
                if !analysis.glosses.is_empty() {
                    tx.execute(
                        "UPDATE dict.words SET glosses = ?1 WHERE lemma = ?2",
                        params![serde_json::to_string(&analysis.glosses)?, lemma],
                    )?;
                    glosses += analysis.glosses.len() as u32;
                }
                words += 1;
                Ok(())
            })?;
        } else {
            for (lemma, roots) in etymology::parse_roots_tsv(path)? {
                if !existing.contains(&lemma.to_ascii_lowercase()) {
                    continue;
                }
                let payload = serde_json::to_string(&roots)?;
                tx.execute(
                    "UPDATE dict.words SET roots = ?1 WHERE lemma = ?2",
                    params![payload, lemma],
                )?;
                words += 1;
                morphemes += roots.len() as u32;
            }
        }

        tx.commit()?;
        let _ = glosses;
        Ok((words, morphemes))
    }

    fn add_examples(&mut self, examples: &[(String, Example)]) -> Result<u32> {
        let tx = self.conn.transaction()?;
        let mut added = 0u32;
        for (lemma, example) in examples {
            let raw: Option<String> = tx
                .query_row(
                    "SELECT examples FROM dict.words WHERE lemma = ?1",
                    [lemma.trim()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(raw) = raw else { continue };
            let mut list: Vec<Example> = serde_json::from_str(&raw).unwrap_or_default();
            if list.iter().any(|e| e.en == example.en) {
                continue;
            }
            list.push(example.clone());
            if list.len() > 8 {
                list.remove(0);
            }
            tx.execute(
                "UPDATE dict.words SET examples = ?1 WHERE lemma = ?2",
                params![serde_json::to_string(&list)?, lemma.trim()],
            )?;
            added += 1;
        }
        tx.commit()?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_core::model::RootKind;
    use qe_core::srs;

    fn temp_store() -> (SqliteStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_at(dir.path().join("test.db")).unwrap();
        (store, dir)
    }

    fn sample_plan() -> ImportPlan {
        let mut plan = ImportPlan::new("unit", "单元测试词书", "测试用");
        let mut word = Word::new("inspect", "v. 检查");
        word.phonetic = Some("/ɪnˈspekt/".into());
        word.roots = vec![
            qe_core::model::WordRoot::new("in-", "向内", RootKind::Prefix),
            qe_core::model::WordRoot::new("spect", "看", RootKind::Root),
        ];
        word.examples = vec![Example::new("Inspect the goods.", Some("检查货物。".into()))];
        plan.words.push(word);
        plan
    }

    #[test]
    fn import_then_study_flow() {
        let (mut store, _dir) = temp_store();
        let report = store.import_plan(&sample_plan()).unwrap();
        assert_eq!(report.inserted, 1);

        let book = store.current_book().unwrap().unwrap();
        assert_eq!(book.slug, "unit");

        let new_words = store.new_cards(book.id, 10).unwrap();
        assert_eq!(new_words.len(), 1);

        let word = &new_words[0];
        let card = srs::review(None, Rating::Good, Utc::now());
        store
            .record_review(&word.lemma, book.id, None, &card, Rating::Good, Utc::now(), true)
            .unwrap();

        assert!(store.new_cards(book.id, 10).unwrap().is_empty());
        let daily = store.daily(time::today()).unwrap();
        assert_eq!(daily.new_learned, 1);
        assert_eq!(daily.reviews, 1);
        assert_eq!(store.streak().unwrap(), 1);

        let loaded = store.word("INSPECT").unwrap().unwrap();
        assert_eq!(loaded.roots.len(), 2);
        assert_eq!(loaded.examples.len(), 1);
    }

    #[test]
    fn search_finds_root_and_lemma() {
        let (mut store, _dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        assert_eq!(store.search("spect", 10).unwrap().len(), 1);
        assert_eq!(store.search("insp", 10).unwrap().len(), 1);
        assert_eq!(store.search("nothing", 10).unwrap().len(), 0);
    }

    #[test]
    fn examples_are_merged() {
        let (mut store, _dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        let added = store
            .add_examples(&[("inspect".into(), Example::new("A new sentence.", None))])
            .unwrap();
        assert_eq!(added, 1);
        let word = store.word("inspect").unwrap().unwrap();
        assert_eq!(word.examples.len(), 2);
    }

    fn graded_store() -> (SqliteStore, tempfile::TempDir, i64) {
        let (mut store, dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        let book = store.current_book().unwrap().unwrap();
        let card = srs::review(None, Rating::Good, Utc::now());
        store
            .record_review("inspect", book.id, None, &card, Rating::Good, Utc::now(), true)
            .unwrap();
        (store, dir, book.id)
    }

    #[test]
    fn drop_last_review_and_restore_card_undo_a_grade() {
        let (mut store, _dir, book_id) = graded_store();
        assert_eq!(store.daily(time::today()).unwrap().reviews, 1);
        assert_eq!(store.progress(book_id).unwrap().learned, 1);

        store.drop_last_review("inspect").unwrap();
        store.restore_card("inspect", None).unwrap();

        assert_eq!(store.daily(time::today()).unwrap().reviews, 0);
        assert_eq!(store.progress(book_id).unwrap().learned, 0);
        // The word is new again.
        assert_eq!(store.new_cards(book_id, 10).unwrap().len(), 1);
    }

    #[test]
    fn restore_card_reinstates_the_previous_state() {
        let (mut store, _dir, book_id) = graded_store();
        let previous = SrsCard::new(Utc::now());
        store.restore_card("inspect", Some(&previous)).unwrap();
        let cards = store.due_cards(book_id, Utc::now(), 10).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].card.as_ref().unwrap().phase, Phase::New);
    }

    #[test]
    fn postpone_hides_a_card_from_today() {
        let (mut store, _dir, book_id) = graded_store();
        let tomorrow = time::day_start(time::today() + ChronoDuration::days(1));
        store.postpone("inspect", tomorrow).unwrap();
        assert!(store.due_cards(book_id, Utc::now(), 10).unwrap().is_empty());
        // But it is due tomorrow.
        let later = tomorrow + ChronoDuration::hours(1);
        assert_eq!(store.due_cards(book_id, later, 10).unwrap().len(), 1);
    }

    #[test]
    fn add_to_book_links_existing_and_new_words() {
        let (mut store, _dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        let book = store.current_book().unwrap().unwrap();
        let before = store.progress(book.id).unwrap().total;

        // Already in the deck.
        let word = store.word("inspect").unwrap().unwrap();
        assert!(!store.add_to_book(book.id, &word).unwrap());

        // Brand new word, created on the fly.
        let fresh = Word::new("serendipity", "n. 意外发现");
        assert!(store.add_to_book(book.id, &fresh).unwrap());
        assert_eq!(store.progress(book.id).unwrap().total, before + 1);
        assert!(store.word("serendipity").unwrap().is_some());
    }

    #[test]
    fn import_phrases_attaches_to_existing_words_only() {
        let (mut store, _dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();

        let dir = std::env::temp_dir().join("qe_phrases_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("phrases.tsv");
        std::fs::write(
            &path,
            "lemma\tphrase\tgloss\ninspect\tinspect for\t检查是否存在\nmissing\tnot a word\t忽略\n",
        )
        .unwrap();

        let (words, added) = store.import_phrases(&path).unwrap();
        assert_eq!((words, added), (1, 1), "unknown words are skipped");

        let word = store.word("inspect").unwrap().unwrap();
        assert_eq!(word.collocations.len(), 1);
        assert_eq!(word.collocations[0].phrase, "inspect for");
        assert_eq!(word.collocations[0].gloss, "检查是否存在");

        // Importing the same file again must not duplicate the phrase.
        let (_, added) = store.import_phrases(&path).unwrap();
        assert_eq!(added, 0);
        assert_eq!(store.word("inspect").unwrap().unwrap().collocations.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn add_collocation_appends_once() {
        let (mut store, _dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();

        assert!(store.add_collocation("inspect", "inspect for", "检查").unwrap());
        assert!(
            !store.add_collocation("inspect", "inspect for", "重复").unwrap(),
            "duplicates are rejected"
        );
        assert!(
            !store.add_collocation("nope", "nope out", "").unwrap(),
            "unknown words are rejected"
        );

        let word = store.word("inspect").unwrap().unwrap();
        assert_eq!(word.collocations.len(), 1);
        assert_eq!(word.collocations[0].gloss, "检查");
    }

    #[test]
    fn migration_adds_the_collocations_column() {
        // A v1 database must open and gain the column rather than error out.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let store = SqliteStore::open_at(&path).unwrap();
            drop(store);
        }
        let store = SqliteStore::open_at(&path).unwrap();
        let word = store.word("anything").unwrap();
        assert!(word.is_none());
    }

    #[test]
    fn words_live_in_their_own_file() {
        let (mut store, dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();

        // 学习库里没有 words 表，词典库里才有。
        let core_has: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM main.sqlite_master WHERE type='table' AND name='words'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(core_has, 0, "the study db must not hold the dictionary");

        let words_path = store.words_path().to_path_buf();
        assert!(words_path.exists(), "the dictionary file is created on open");
        assert_eq!(
            words_path.file_name().unwrap().to_string_lossy(),
            "test-words.db",
            "named after the core file"
        );
        assert!(dir.path().join("test-words.db").exists());
        assert_eq!(store.word("inspect").unwrap().unwrap().lemma, "inspect");
    }

    #[test]
    fn an_old_single_file_database_is_split_on_open() {
        // 造一个 v4 布局：words 还在学习库里，子表带指向它的外键。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE words (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    lemma TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    phonetic TEXT, translation TEXT NOT NULL DEFAULT '',
                    definition TEXT, roots TEXT NOT NULL DEFAULT '[]',
                    examples TEXT NOT NULL DEFAULT '[]',
                    tags TEXT NOT NULL DEFAULT '[]',
                    source TEXT NOT NULL DEFAULT '',
                    collocations TEXT NOT NULL DEFAULT '[]',
                    glosses TEXT NOT NULL DEFAULT '[]');
                 CREATE TABLE books (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE,
                    name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                    builtin INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE book_words (
                    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
                    word_id INTEGER NOT NULL REFERENCES words(id) ON DELETE CASCADE,
                    rank INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (book_id, word_id));
                 CREATE TABLE cards (
                    word_id INTEGER PRIMARY KEY REFERENCES words(id) ON DELETE CASCADE,
                    book_id INTEGER, phase TEXT NOT NULL DEFAULT 'new',
                    step INTEGER NOT NULL DEFAULT 0, ease REAL NOT NULL DEFAULT 2.5,
                    interval_days INTEGER NOT NULL DEFAULT 0,
                    reps INTEGER NOT NULL DEFAULT 0, lapses INTEGER NOT NULL DEFAULT 0,
                    due_at TEXT, due_ts INTEGER, last_review_at TEXT);
                 INSERT INTO words(lemma, translation) VALUES ('inspect', 'v. 检查');
                 INSERT INTO books(slug, name) VALUES ('legacy', 'Legacy Deck');
                 INSERT INTO book_words(book_id, word_id, rank) VALUES (1, 1, 0);
                 INSERT INTO cards(word_id, book_id, phase) VALUES (1, 1, 'review');
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }

        let store = SqliteStore::open_at(&path).unwrap();
        // 词条搬进单独的库，引用关系还在。
        assert_eq!(store.word("inspect").unwrap().unwrap().translation, "v. 检查");
        let book = store.book_by_slug("legacy").unwrap().unwrap();
        assert_eq!(store.new_cards(book.id, 10).unwrap().len(), 0, "already studied");
        let cards = store.due_cards(book.id, chrono::Utc::now(), 10).unwrap();
        assert!(cards.is_empty(), "nothing due, but the card survived");
        assert!(dir.path().join("legacy-words.db").exists());

        let core_has: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM main.sqlite_master WHERE type='table' AND name='words'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(core_has, 0, "the legacy table is dropped after the move");
    }

    #[test]
    fn v5_word_ids_are_rekeyed_to_lemmas_on_open() {
        // v5 的布局：学习表还按 `words.id` 引用，词典已经在独立文件里。
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("v5.db");
        let words = dir.path().join("v5-words.db");
        {
            let conn = Connection::open(&words).unwrap();
            conn.execute_batch(
                "CREATE TABLE words (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    lemma TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    phonetic TEXT, translation TEXT NOT NULL DEFAULT '',
                    definition TEXT, roots TEXT NOT NULL DEFAULT '[]',
                    examples TEXT NOT NULL DEFAULT '[]',
                    tags TEXT NOT NULL DEFAULT '[]',
                    source TEXT NOT NULL DEFAULT '',
                    collocations TEXT NOT NULL DEFAULT '[]',
                    glosses TEXT NOT NULL DEFAULT '[]');
                 INSERT INTO words(id, lemma, translation) VALUES (7, 'inspect', 'v. 检查');",
            )
            .unwrap();
        }
        {
            let conn = Connection::open(&core).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V4).unwrap();
            conn.execute_batch(
                "INSERT INTO books(slug, name) VALUES ('v5', 'V5 Deck');
                 INSERT INTO book_words(book_id, word_id, rank) VALUES (1, 7, 0);
                 INSERT INTO cards(word_id, book_id, phase, due_at, due_ts)
                    VALUES (7, 1, 'review', '2026-01-01T00:00:00Z', 0);
                 INSERT INTO reviews(word_id, at, day, rating, phase_after)
                    VALUES (7, '2026-01-01T00:00:00Z', '2026-01-01', 'good', 'review');
                 PRAGMA user_version = 5;",
            )
            .unwrap();
        }

        let store = SqliteStore::open_at(&core).unwrap();
        let columns: Vec<String> = store
            .conn
            .prepare("PRAGMA main.table_info(book_words)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        assert!(
            columns.iter().any(|col| col == "lemma") && !columns.iter().any(|col| col == "word_id"),
            "book_words must be keyed by lemma: {columns:?}"
        );

        let book = store.book_by_slug("v5").unwrap().unwrap();
        assert_eq!(store.progress(book.id).unwrap().total, 1);
        assert_eq!(store.progress(book.id).unwrap().learned, 1);
        assert!(store.new_cards(book.id, 10).unwrap().is_empty());
        assert_eq!(store.deck_lemmas().unwrap(), vec!["inspect".to_string()]);
        let reviews: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM reviews WHERE lemma = 'inspect'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(reviews, 1, "review history follows the word");
    }

    /// 同样的词、同样的词书，但词典重排了 id：词书和进度必须还指向原来的词。
    #[test]
    fn decks_and_cards_survive_a_dictionary_swap() {
        let (mut store, dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        let book = store.current_book().unwrap().unwrap();
        let card = srs::review(None, Rating::Good, Utc::now());
        store
            .record_review("inspect", book.id, None, &card, Rating::Good, Utc::now(), true)
            .unwrap();

        // 换一份词典：先插一个别的词，`inspect` 的 id 从 1 变成 2。
        let words_path = dir.path().join("test-words.db");
        store.conn.execute_batch("DETACH DATABASE dict").unwrap();
        fs::remove_file(&words_path).unwrap();
        store
            .conn
            .execute("ATTACH DATABASE ?1 AS dict", [words_path.to_str().unwrap()])
            .unwrap();
        store.conn.execute_batch(SCHEMA_WORDS).unwrap();
        store
            .conn
            .execute_batch(
                "ALTER TABLE dict.words ADD COLUMN collocations TEXT NOT NULL DEFAULT '[]';
                 ALTER TABLE dict.words ADD COLUMN glosses TEXT NOT NULL DEFAULT '[]';
                 INSERT INTO dict.words(lemma, translation) VALUES ('zzz', 'x'), ('inspect', 'v. 检查');",
            )
            .unwrap();
        assert_eq!(store.word("inspect").unwrap().unwrap().id, 2, "ids shifted");

        // lemma 是跨库键，所以词书、卡片、进度都不受影响。
        assert_eq!(store.progress(book.id).unwrap().learned, 1);
        assert!(store.new_cards(book.id, 10).unwrap().is_empty());
        let later = Utc::now() + ChronoDuration::days(2);
        let due = store.due_cards(book.id, later, 10).unwrap();
        assert_eq!(due.len(), 1, "the card still rides on `inspect`");
        assert_eq!(due[0].word.lemma, "inspect");
        assert_eq!(store.deck_lemmas().unwrap(), vec!["inspect".to_string()]);
        assert!(store.doctor(false).unwrap().is_clean());
    }

    #[test]
    fn doctor_reports_and_prunes_dangling_lemma_refs() {
        let (mut store, _dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        let book = store.current_book().unwrap().unwrap();

        // 词典里没有的词混进词书、卡片和流水。
        store
            .conn
            .execute(
                "INSERT INTO book_words(book_id, lemma, rank) VALUES (?1, 'ghost', 99)",
                [book.id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO cards(lemma, book_id, phase) VALUES ('ghost', ?1, 'new')",
                [book.id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO reviews(lemma, at, day, rating, phase_after)
                 VALUES ('ghost', '2026-01-01T00:00:00Z', '2026-01-01', 'good', 'review')",
                [],
            )
            .unwrap();

        let report = store.doctor(false).unwrap();
        assert!(!report.is_clean());
        assert_eq!(
            report.missing_in_decks,
            vec![("unit".to_string(), "ghost".to_string())]
        );
        assert_eq!(report.missing_cards, vec!["ghost".to_string()]);
        assert_eq!(report.missing_reviews, 1);
        assert_eq!(report.orphans(), 3);
        assert_eq!(report.fixed, 0);
        assert_eq!(store.progress(book.id).unwrap().total, 2, "no --fix, no delete");

        let report = store.doctor(true).unwrap();
        assert_eq!(report.fixed, 3);
        let after = store.doctor(false).unwrap();
        assert!(after.is_clean());
        assert_eq!(after.decks, vec![("unit".to_string(), 1)]);
        assert_eq!(after.words, 1);
    }

    #[test]
    fn doctor_never_prunes_against_an_empty_dictionary() {
        // 词典库丢了的时候 `--fix` 会把整个词书删光，这比留下悬空引用更糟。
        let (mut store, _dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        let book = store.current_book().unwrap().unwrap();
        store
            .conn
            .execute_batch("DELETE FROM dict.words")
            .unwrap();

        let report = store.doctor(true).unwrap();
        assert_eq!(report.fixed, 0);
        assert_eq!(store.progress(book.id).unwrap().total, 1, "deck untouched");
    }
}
