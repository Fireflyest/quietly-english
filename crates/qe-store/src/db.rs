//! SQLite 适配器。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use qe_core::model::{
    Book, BookProgress, DailyStats, Example, ImportPlan, ImportReport, Phase, Rating, SessionLog,
    SrsCard, StudyCard, Word,
};
use qe_core::store::Store;
use qe_core::time;

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS words (
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

CREATE TABLE IF NOT EXISTS books (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    slug        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    builtin     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS book_words (
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    word_id INTEGER NOT NULL REFERENCES words(id) ON DELETE CASCADE,
    rank    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (book_id, word_id)
);

CREATE TABLE IF NOT EXISTS cards (
    word_id        INTEGER PRIMARY KEY REFERENCES words(id) ON DELETE CASCADE,
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

const WORD_COLS: &str = "w.id, w.lemma, w.phonetic, w.translation, w.definition, w.roots, w.examples, w.tags";

pub struct SqliteStore {
    conn: Connection,
    path: PathBuf,
}

impl SqliteStore {
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

        let mut store = Self { conn, path };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
            self.conn.execute_batch("PRAGMA user_version = 1")?;
        }
        Ok(())
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

            let (id, inserted) = upsert_word_tx(
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
                        "INSERT OR IGNORE INTO book_words(book_id, word_id, rank) VALUES (?1, ?2, ?3)",
                        params![book_id, id, *rank],
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
        .query_row("SELECT id FROM words WHERE lemma = ?1", [lemma], |row| row.get(0))
        .optional()?;
    let id: i64 = tx.query_row(
        "INSERT INTO words(lemma, phonetic, translation, definition, roots, examples, tags, source)
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

fn decode_word(row: &Row<'_>) -> rusqlite::Result<Word> {
    let roots_raw: String = row.get(5)?;
    let examples_raw: String = row.get(6)?;
    let tags_raw: String = row.get(7)?;
    Ok(Word {
        id: row.get(0)?,
        lemma: row.get(1)?,
        phonetic: row.get(2)?,
        translation: row.get(3)?,
        definition: row.get(4)?,
        roots: serde_json::from_str(&roots_raw).unwrap_or_default(),
        examples: serde_json::from_str(&examples_raw).unwrap_or_default(),
        tags: serde_json::from_str(&tags_raw).unwrap_or_default(),
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
            "SELECT COUNT(*) FROM book_words bw JOIN cards c ON c.word_id = bw.word_id
             WHERE bw.book_id = ?1",
            [book_id],
            |row| row.get(0),
        )?;
        let due: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM book_words bw JOIN cards c ON c.word_id = bw.word_id
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
             JOIN words w ON w.id = c.word_id
             JOIN book_words bw ON bw.word_id = w.id AND bw.book_id = ?1
             WHERE c.due_ts IS NOT NULL AND c.due_ts <= ?2
             ORDER BY c.due_ts ASC
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![book_id, now.timestamp(), limit as i64], |row| {
            let word = decode_word(row)?;
            let card = decode_card(row, 8)?;
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
             JOIN words w ON w.id = bw.word_id
             LEFT JOIN cards c ON c.word_id = w.id
             WHERE bw.book_id = ?1 AND c.word_id IS NULL
             ORDER BY bw.rank ASC, w.id ASC
             LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![book_id, limit as i64], decode_word)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn record_review(
        &mut self,
        word_id: i64,
        book_id: i64,
        _before: Option<&SrsCard>,
        after: &SrsCard,
        rating: Rating,
        now: DateTime<Utc>,
        is_new: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO cards(word_id, book_id, phase, step, ease, interval_days, reps, lapses, due_at, due_ts, last_review_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(word_id) DO UPDATE SET
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
                word_id,
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
            "INSERT INTO reviews(word_id, at, day, rating, phase_after, interval_after, is_new)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                word_id,
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
        word_id: i64,
        book_id: i64,
        after: &SrsCard,
        rating: Rating,
        now: DateTime<Utc>,
        is_new: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO cards(word_id, book_id, phase, step, ease, interval_days, reps, lapses, due_at, due_ts, last_review_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(word_id) DO UPDATE SET
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
                word_id,
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
                "SELECT id FROM reviews WHERE word_id = ?1 ORDER BY id DESC LIMIT 1",
                [word_id],
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
                    "INSERT INTO reviews(word_id, at, day, rating, phase_after, interval_after, is_new)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        word_id,
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
        let sql = format!("SELECT {WORD_COLS} FROM words w WHERE w.lemma = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let word = stmt.query_row([lemma.trim()], decode_word).optional()?;
        Ok(word)
    }

    fn postpone(&mut self, word_id: i64, until: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE cards SET due_at = ?1, due_ts = ?2 WHERE word_id = ?3",
            params![until.to_rfc3339(), until.timestamp(), word_id],
        )?;
        Ok(())
    }

    fn restore_card(&mut self, word_id: i64, card: Option<&SrsCard>) -> Result<()> {
        match card {
            Some(card) => {
                self.conn.execute(
                    "INSERT INTO cards(word_id, book_id, phase, step, ease, interval_days, reps, lapses, due_at, due_ts, last_review_at)
                     VALUES (?1, (SELECT book_id FROM cards WHERE word_id = ?1), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(word_id) DO UPDATE SET
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
                        word_id,
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
                    .execute("DELETE FROM cards WHERE word_id = ?1", [word_id])?;
            }
        }
        Ok(())
    }

    fn drop_last_review(&mut self, word_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM reviews WHERE id = (
                SELECT id FROM reviews WHERE word_id = ?1 ORDER BY id DESC LIMIT 1
             )",
            [word_id],
        )?;
        Ok(())
    }

    fn add_to_book(&mut self, book_id: i64, word: &Word) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let roots = serde_json::to_string(&word.roots)?;
        let examples = serde_json::to_string(&word.examples)?;
        let tags = serde_json::to_string(&word.tags)?;
        let (id, _) = upsert_word_tx(
            &tx,
            word.lemma.trim(),
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
            "INSERT OR IGNORE INTO book_words(book_id, word_id, rank) VALUES (?1, ?2, ?3)",
            params![book_id, id, rank],
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
            "SELECT {WORD_COLS} FROM words w
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
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT lower(w.lemma) FROM book_words bw JOIN words w ON w.id = bw.word_id",
        )?;
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
             JOIN book_words bw ON bw.word_id = r.word_id AND bw.book_id = ?1
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
            let (id, inserted) = upsert_word_tx(
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
                "INSERT OR IGNORE INTO book_words(book_id, word_id, rank) VALUES (?1, ?2, ?3)",
                params![book_id, id, rank],
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
        progress: &mut dyn FnMut(u64),
    ) -> Result<(u32, u32)> {
        use crate::stardict::Stardict;

        let mut dict = Stardict::open(path)?;
        let tx = self.conn.transaction()?;
        let mut inserted = 0u32;
        let mut updated = 0u32;

        dict.for_each_word(progress, |word| {
            let (id, is_new) = upsert_word_tx(
                &tx,
                &word.lemma,
                word.phonetic.as_deref(),
                &word.translation,
                None,
                "[]",
                "[]",
                "[]",
                "ecdict-stardict",
            )?;
            let _ = id;
            if is_new {
                inserted += 1;
            } else {
                updated += 1;
            }
            Ok(())
        })?;

        tx.commit()?;
        Ok((inserted, updated))
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
            let mut stmt = self.conn.prepare("SELECT lemma FROM words")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for lemma in rows {
                existing.insert(lemma?.to_ascii_lowercase());
            }
        }

        let tx = self.conn.transaction()?;
        let mut words = 0u32;
        let mut morphemes = 0u32;

        if etymology::looks_like_jsonl(path) {
            // Pass one gathers the affixes' own glosses, pass two walks every word.
            let index = etymology::index_morphemes(path)?;
            etymology::for_each_analysis(path, &index, progress, |lemma, roots| {
                if !existing.contains(&lemma.to_ascii_lowercase()) {
                    return Ok(());
                }
                let payload = serde_json::to_string(&roots)?;
                tx.execute(
                    "UPDATE words SET roots = ?1 WHERE lemma = ?2",
                    params![payload, lemma],
                )?;
                words += 1;
                morphemes += roots.len() as u32;
                Ok(())
            })?;
        } else {
            for (lemma, roots) in etymology::parse_roots_tsv(path)? {
                if !existing.contains(&lemma.to_ascii_lowercase()) {
                    continue;
                }
                let payload = serde_json::to_string(&roots)?;
                tx.execute(
                    "UPDATE words SET roots = ?1 WHERE lemma = ?2",
                    params![payload, lemma],
                )?;
                words += 1;
                morphemes += roots.len() as u32;
            }
        }

        tx.commit()?;
        Ok((words, morphemes))
    }

    fn add_examples(&mut self, examples: &[(String, Example)]) -> Result<u32> {
        let tx = self.conn.transaction()?;
        let mut added = 0u32;
        for (lemma, example) in examples {
            let raw: Option<String> = tx
                .query_row(
                    "SELECT examples FROM words WHERE lemma = ?1",
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
                "UPDATE words SET examples = ?1 WHERE lemma = ?2",
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
            .record_review(word.id, book.id, None, &card, Rating::Good, Utc::now(), true)
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

    fn graded_store() -> (SqliteStore, tempfile::TempDir, i64, i64) {
        let (mut store, dir) = temp_store();
        store.import_plan(&sample_plan()).unwrap();
        let book = store.current_book().unwrap().unwrap();
        let word = store.word("inspect").unwrap().unwrap();
        let card = srs::review(None, Rating::Good, Utc::now());
        store
            .record_review(word.id, book.id, None, &card, Rating::Good, Utc::now(), true)
            .unwrap();
        (store, dir, book.id, word.id)
    }

    #[test]
    fn drop_last_review_and_restore_card_undo_a_grade() {
        let (mut store, _dir, book_id, word_id) = graded_store();
        assert_eq!(store.daily(time::today()).unwrap().reviews, 1);
        assert_eq!(store.progress(book_id).unwrap().learned, 1);

        store.drop_last_review(word_id).unwrap();
        store.restore_card(word_id, None).unwrap();

        assert_eq!(store.daily(time::today()).unwrap().reviews, 0);
        assert_eq!(store.progress(book_id).unwrap().learned, 0);
        // The word is new again.
        assert_eq!(store.new_cards(book_id, 10).unwrap().len(), 1);
    }

    #[test]
    fn restore_card_reinstates_the_previous_state() {
        let (mut store, _dir, book_id, word_id) = graded_store();
        let previous = SrsCard::new(Utc::now());
        store.restore_card(word_id, Some(&previous)).unwrap();
        let cards = store.due_cards(book_id, Utc::now(), 10).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].card.as_ref().unwrap().phase, Phase::New);
    }

    #[test]
    fn postpone_hides_a_card_from_today() {
        let (mut store, _dir, book_id, word_id) = graded_store();
        let tomorrow = time::day_start(time::today() + ChronoDuration::days(1));
        store.postpone(word_id, tomorrow).unwrap();
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
}
