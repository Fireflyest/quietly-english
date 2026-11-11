//! 词库文件解析。
//!
//! 支持四种来源：
//! * **ECDICT** CSV（开源英汉词典，自动按 tag 生成 cet4/cet6/toefl... 词书）
//! * **通用 JSON**（扇贝导出、自建词表，推荐）
//! * **通用 CSV/TSV**（`word,translation,phonetic,roots,example,example_zh`）
//! * **例句合并 CSV**（`word,sentence,translation`，可配合 Tatoeba）
//! * **纯词表 TXT**（一行一词，用已有词典补全释义）

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use qe_core::model::{Collocation, Example, ImportPlan, RootKind, Word, WordRoot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Ecdict,
    Json,
    WordsCsv,
    ExamplesCsv,
    Plain,
    /// Sourced etymology data: kaikki JSONL or a `roots.tsv` table.
    Etymology,
}

/// 只读取文件头，判断应该用哪种导入器。
pub fn inspect(path: &Path) -> Result<FileKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "json" => return Ok(FileKind::Json),
        // kaikki.org ships one JSON object per line.
        "jsonl" | "ndjson" => return Ok(FileKind::Etymology),
        "txt" | "md" | "list" => return Ok(FileKind::Plain),
        "csv" | "tsv" => {
            // A roots table (lemma/part/gloss/kind) is etymology, not a deck.
            let header = first_line(path)?;
            let lower = header.to_ascii_lowercase();
            let cells: Vec<&str> = lower
                .split(['\t', ','])
                .map(|cell| cell.trim().trim_matches('"'))
                .collect();
            if cells.contains(&"part") && cells.contains(&"gloss") && cells.contains(&"lemma") {
                return Ok(FileKind::Etymology);
            }
        }
        other => bail!("不支持的文件类型 .{other}，请使用 csv / tsv / json / jsonl / txt"),
    }

    let header = first_line(path)?;
    let lower = header.to_ascii_lowercase();
    let has = |name: &str| {
        lower
            .split(['\t', ','])
            .any(|cell| cell.trim().trim_matches('"').eq_ignore_ascii_case(name))
    };

    if has("tag") && has("word") && (has("collins") || has("bnc") || has("frq")) {
        return Ok(FileKind::Ecdict);
    }
    if has("word") && (has("sentence") || has("example")) && !has("phonetic") {
        // 例句表通常没有音标列；带 example 列的词表仍按词表导入。
        if !has("roots") && !has("meaning") && !has("definition") {
            return Ok(FileKind::ExamplesCsv);
        }
    }
    Ok(FileKind::WordsCsv)
}

fn first_line(path: &Path) -> Result<String> {
    let file = fs::File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim_start_matches('\u{feff}').trim().to_string())
}

fn delimiter(path: &Path, header: &str) -> u8 {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "tsv" || (header.matches('\t').count() > header.matches(',').count()) {
        b'\t'
    } else {
        b','
    }
}

fn csv_reader(path: &Path) -> Result<csv::Reader<fs::File>> {
    let header = first_line(path)?;
    let file = fs::File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    Ok(csv::ReaderBuilder::new()
        .flexible(true)
        .delimiter(delimiter(path, &header))
        .from_reader(file))
}

fn header_map(headers: &csv::StringRecord) -> HashMap<String, usize> {
    headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            (
                h.trim().trim_start_matches('\u{feff}').to_ascii_lowercase(),
                i,
            )
        })
        .collect()
}

fn pick<'a>(record: &'a csv::StringRecord, map: &HashMap<String, usize>, names: &[&str]) -> &'a str {
    for name in names {
        if let Some(idx) = map.get(*name) {
            if let Some(value) = record.get(*idx) {
                return value.trim();
            }
        }
    }
    ""
}

fn optional(record: &csv::StringRecord, map: &HashMap<String, usize>, names: &[&str]) -> Option<String> {
    let value = pick(record, map, names);
    (!value.is_empty()).then(|| value.to_string())
}

/// 解析 `roots` 列：支持 `a-:远离:prefix;band:束缚:root` 或 JSON 数组。
pub fn parse_roots(raw: &str) -> Vec<WordRoot> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "[]" {
        return Vec::new();
    }
    if raw.starts_with('[') {
        if let Ok(roots) = serde_json::from_str::<Vec<WordRoot>>(raw) {
            return roots;
        }
    }
    raw.split(';')
        .filter_map(|chunk| {
            let chunk = chunk.trim();
            if chunk.is_empty() {
                return None;
            }
            let mut parts = chunk.splitn(3, ':');
            let part = parts.next()?.trim();
            let gloss = parts.next().unwrap_or("").trim();
            let kind = RootKind::parse(parts.next().unwrap_or("root"));
            Some(WordRoot::new(part, gloss, kind))
        })
        .collect()
}

pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "imported".to_string()
    } else {
        out
    }
}

struct PlanBuilder {
    plans: Vec<ImportPlan>,
    index: HashMap<String, usize>,
}

impl PlanBuilder {
    fn new() -> Self {
        Self {
            plans: Vec::new(),
            index: HashMap::new(),
        }
    }

    fn plan(&mut self, slug: &str, name: &str, description: &str) -> &mut ImportPlan {
        if let Some(idx) = self.index.get(slug) {
            return &mut self.plans[*idx];
        }
        self.plans
            .push(ImportPlan::new(slug, name, description));
        let idx = self.plans.len() - 1;
        self.index.insert(slug.to_string(), idx);
        &mut self.plans[idx]
    }

    fn push_word(plan: &mut ImportPlan, word: Word) {
        if let Some(existing) = plan.words.iter_mut().find(|w| w.lemma.eq_ignore_ascii_case(&word.lemma)) {
            if existing.translation.is_empty() {
                existing.translation = word.translation;
            }
            if existing.phonetic.is_none() {
                existing.phonetic = word.phonetic;
            }
            if existing.roots.is_empty() {
                existing.roots = word.roots;
            }
            for example in word.examples {
                if !existing.examples.iter().any(|e| e.en == example.en) {
                    existing.examples.push(example);
                }
            }
        } else {
            plan.words.push(word);
        }
    }
}

/// 通用 CSV/TSV 词表。
pub fn parse_words_csv(path: &Path) -> Result<Vec<ImportPlan>> {
    let mut reader = csv_reader(path)?;
    let headers = reader.headers()?.clone();
    let map = header_map(&headers);
    if !map.contains_key("word") && !map.contains_key("lemma") {
        bail!("词表缺少 word 列（可用列：word, translation, phonetic, definition, roots, example, example_zh, tags, book）");
    }

    let default_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("导入词书")
        .to_string();
    let mut builder = PlanBuilder::new();

    for record in reader.records() {
        let record = record?;
        let lemma = pick(&record, &map, &["word", "lemma"]);
        if lemma.is_empty() {
            continue;
        }
        let book_name = optional(&record, &map, &["book", "book_name", "词书"])
            .unwrap_or_else(|| default_name.clone());
        let slug = slugify(&book_name);
        let plan = builder.plan(&slug, &book_name, "从 CSV/TSV 导入");

        let mut word = Word::new(lemma, pick(&record, &map, &["translation", "meaning", "释义", "中文"]));
        word.phonetic = optional(&record, &map, &["phonetic", "音标"]);
        word.definition = optional(&record, &map, &["definition", "english", "英文释义"]);
        word.roots = parse_roots(pick(&record, &map, &["roots", "root", "词根"]));
        word.tags = pick(&record, &map, &["tags", "tag"])
            .split([',', ' ', ';'])
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect();

        let en = pick(&record, &map, &["example", "sentence", "例句"]);
        if !en.is_empty() {
            let zh = optional(&record, &map, &["example_zh", "zh", "sentence_zh", "例句翻译"]);
            word.examples.push(Example::new(en, zh));
        }
        PlanBuilder::push_word(plan, word);
    }

    Ok(builder.plans)
}

#[derive(Debug, Deserialize)]
struct JsonWord {
    #[serde(alias = "word", alias = "name")]
    lemma: String,
    #[serde(default)]
    translation: String,
    #[serde(default)]
    phonetic: Option<String>,
    #[serde(default)]
    definition: Option<String>,
    #[serde(default, deserialize_with = "de_roots")]
    roots: Vec<WordRoot>,
    #[serde(default)]
    examples: Vec<Example>,
    #[serde(default)]
    tags: Vec<String>,
    /// 兼容扇贝/自建词表的单例句写法。
    #[serde(default)]
    example: Option<String>,
    #[serde(default)]
    example_zh: Option<String>,
    /// 兼容 `"root": "spect:看:root"` 的简写。
    #[serde(default)]
    root: Option<String>,
    /// qwerty-learner 用 `trans: ["释义", ...]` 存释义。
    #[serde(default)]
    trans: Vec<String>,
    #[serde(default)]
    ukphone: Option<String>,
    #[serde(default)]
    usphone: Option<String>,
}

impl JsonWord {
    /// 最终释义：优先 `translation`，退回 `trans` 数组。
    fn meaning(&self) -> String {
        if !self.translation.trim().is_empty() {
            return self.translation.trim().to_string();
        }
        self.trans
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 音标：显式 `phonetic` 优先，其次英式，再次美式。
    fn sound(&self) -> Option<String> {
        if self.phonetic.is_some() {
            return self.phonetic.clone();
        }
        let pick = self.ukphone.as_ref().or(self.usphone.as_ref())?;
        let pick = pick.trim();
        if pick.is_empty() {
            None
        } else if pick.starts_with('/') {
            Some(pick.to_string())
        } else {
            Some(format!("/{pick}/"))
        }
    }
}

fn de_roots<'de, D>(de: D) -> Result<Vec<WordRoot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(de)?;
    Ok(match value {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::String(s)) => parse_roots(&s),
        Some(other) => serde_json::from_value(other).unwrap_or_default(),
    })
}

/// Map a filename stem to the deck it belongs to, so importing
/// `CET6_T.json` from qwerty-learner lands in the same `cet6` deck that
/// ECDICT and the built-in seed data use instead of creating `cet6-t`.
pub fn book_from_stem(stem: &str) -> Option<(&'static str, &'static str)> {
    // Normalise `CET6_T`, `cet-6`, `IELTS_4_T` to `CET6-T`, `CET-6`, `IELTS-4-T`.
    let key: String = stem
        .chars()
        .map(|ch| if ch == '_' || ch == ' ' { '-' } else { ch })
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect::<String>()
        .to_ascii_uppercase();

    const TABLE: &[(&[&str], &str, &str)] = &[
        (&["CET4", "CET-4"], "cet4", "CET-4 核心词"),
        (&["CET6", "CET-6"], "cet6", "CET-6 核心词"),
        (&["TOEFL"], "toefl", "TOEFL 核心词"),
        (&["IELTS"], "ielts", "IELTS 核心词"),
        (&["GRE"], "gre", "GRE 核心词"),
        (&["KY", "KAOYAN"], "ky", "考研核心词"),
        (&["GK", "GAOKAO"], "gk", "高考核心词"),
        (&["ZK", "ZHONGKAO"], "zk", "中考核心词"),
    ];

    for (aliases, slug, name) in TABLE {
        for alias in *aliases {
            let prefixed = format!("{alias}-");
            if key == *alias || key.starts_with(&prefixed) {
                return Some((slug, name));
            }
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct JsonBook {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, alias = "words")]
    words: Vec<JsonWord>,
}

/// 通用 JSON 词表。支持 `{name, slug, words: [...]}` 或直接 `[...]`。
pub fn parse_json(path: &Path) -> Result<Vec<ImportPlan>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("无法读取 {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("{} 不是合法 JSON", path.display()))?;

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("导入词书");
    // Recognise exam dictionaries by filename so they merge into the same deck the
    // built-in seed data and ECDICT use.
    let (default_slug, default_name) = match book_from_stem(stem) {
        Some((slug, name)) => (Some(slug.to_string()), name.to_string()),
        None => (None, stem.to_string()),
    };

    let books: Vec<JsonBook> = if value.is_array() {
        vec![JsonBook {
            name: Some(default_name.clone()),
            slug: default_slug.clone(),
            description: Some("从 JSON 导入".into()),
            words: serde_json::from_value(value).unwrap_or_default(),
        }]
    } else if value.is_object() && value.get("words").is_some() {
        vec![serde_json::from_value(value).unwrap_or(JsonBook {
            name: Some(default_name.clone()),
            slug: default_slug.clone(),
            description: Some("从 JSON 导入".into()),
            words: Vec::new(),
        })]
    } else if value.is_object() {
        // 多词书：{ "books": [ ... ] } 或 { "书名": [ ... ] }
        if let Some(books) = value.get("books").and_then(|b| b.as_array()) {
            let mut out = Vec::new();
            for entry in books {
                if let Ok(book) = serde_json::from_value::<JsonBook>(entry.clone()) {
                    out.push(book);
                }
            }
            out
        } else {
            let mut out = Vec::new();
            for (name, words) in value.as_object().into_iter().flatten() {
                if let Ok(list) = serde_json::from_value::<Vec<JsonWord>>(words.clone()) {
                    out.push(JsonBook {
                        name: Some(name.clone()),
                        slug: None,
                        description: Some("从 JSON 导入".into()),
                        words: list,
                    });
                }
            }
            out
        }
    } else {
        bail!("JSON 结构无法识别：应为数组、{{words: [...]}} 或 {{books: [...]}}");
    };

    let mut plans = Vec::new();
    for book in books {
        let name = book.name.unwrap_or_else(|| default_name.clone());
        let slug = book.slug.unwrap_or_else(|| slugify(&name));
        let mut plan = ImportPlan::new(
            slug,
            name,
            book.description.unwrap_or_else(|| "从 JSON 导入".into()),
        );
        for entry in book.words {
            let mut word = Word::new(entry.lemma.trim(), entry.meaning());
            word.phonetic = entry.sound();
            word.definition = entry.definition;
            word.roots = entry.roots;
            if word.roots.is_empty() {
                if let Some(root) = entry.root {
                    word.roots = parse_roots(&root);
                }
            }
            word.examples = entry.examples;
            if let Some(en) = entry.example {
                if !word.examples.iter().any(|e| e.en == en) {
                    word.examples.push(Example::new(en, entry.example_zh));
                }
            }
            word.tags = entry.tags;
            if word.lemma.is_empty() {
                continue;
            }
            PlanBuilder::push_word(&mut plan, word);
        }
        plans.push(plan);
    }
    Ok(plans)
}

/// 例句合并文件：`word,sentence,translation`。
pub fn parse_examples_csv(path: &Path) -> Result<Vec<(String, Example)>> {
    let mut reader = csv_reader(path)?;
    let headers = reader.headers()?.clone();
    let map = header_map(&headers);
    let mut out = Vec::new();
    for record in reader.records() {
        let record = record?;
        let lemma = pick(&record, &map, &["word", "lemma"]);
        let en = pick(&record, &map, &["sentence", "example", "en", "例句"]);
        if lemma.is_empty() || en.is_empty() {
            continue;
        }
        let zh = optional(&record, &map, &["translation", "zh", "example_zh"]);
        out.push((lemma.to_string(), Example::new(en, zh)));
    }
    Ok(out)
}

/// 常用词组 / 固定搭配表：`lemma<TAB>phrase<TAB>gloss`。
///
/// 首行可以带表头（含 `lemma` / `phrase` 字样），也可以直接是数据。
/// 逗号分隔同样可以，因为分隔符是探测出来的。
pub fn parse_phrases(path: &Path) -> Result<Vec<(String, Collocation)>> {
    let mut reader = csv_reader(path)?;
    let headers = reader.headers()?.clone();
    let first = headers.iter().next().unwrap_or("").to_ascii_lowercase();
    let has_header = first.contains("lemma") || first.contains("word") || first.contains("phrase");
    let map = if has_header {
        header_map(&headers)
    } else {
        // 无表头时按位置解释，让 `demo.tsv` 这种手工文件也能直接导入。
        HashMap::from([
            ("lemma".to_string(), 0usize),
            ("phrase".to_string(), 1),
            ("gloss".to_string(), 2),
        ])
    };

    let mut out = Vec::new();
    let mut records = reader.records();
    if !has_header {
        // `headers()` already consumed the first record; replay it as data.
        if let Some((lemma, phrase, gloss)) = phrase_row(&headers, &map) {
            out.push((lemma, Collocation::new(phrase, gloss)));
        }
    }
    for record in records.by_ref() {
        let record = record?;
        if let Some((lemma, phrase, gloss)) = phrase_row(&record, &map) {
            out.push((lemma, Collocation::new(phrase, gloss)));
        }
    }
    Ok(out)
}

fn phrase_row(record: &csv::StringRecord, map: &HashMap<String, usize>) -> Option<(String, String, String)> {
    let lemma = pick(record, map, &["lemma", "word"]);
    let phrase = pick(record, map, &["phrase", "collocation", "搭配", "词组"]);
    if lemma.is_empty() || phrase.is_empty() {
        return None;
    }
    let gloss = pick(record, map, &["gloss", "meaning", "translation", "释义"]);
    Some((lemma.to_string(), phrase.to_string(), gloss.to_string()))
}

/// 纯词表：一行一个单词，忽略空行与 `#` 注释。
pub fn parse_plain(path: &Path) -> Result<Vec<String>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("无法读取 {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split(['\t', ',', ' '])
                .next()
                .unwrap_or(line)
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty() && line.chars().all(|c| c.is_ascii_alphabetic() || c == '-' || c == '\''))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_parse_colon_format() {
        let roots = parse_roots("in-:向内:prefix;spect:看:root");
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].kind, RootKind::Prefix);
        assert_eq!(roots[1].gloss, "看");
    }

    #[test]
    fn roots_parse_json_format() {
        let roots = parse_roots(r#"[{"part":"vis","gloss":"看","kind":"root"}]"#);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].part, "vis");
    }

    #[test]
    fn slugify_handles_chinese_names() {
        assert_eq!(slugify("CET-4 核心词"), "cet-4");
        assert_eq!(slugify("我的词书"), "imported");
        assert_eq!(slugify("My Book!"), "my-book");
    }

    #[test]
    fn exam_filenames_map_to_canonical_decks() {
        assert_eq!(book_from_stem("CET6_T"), Some(("cet6", "CET-6 核心词")));
        assert_eq!(book_from_stem("CET-6"), Some(("cet6", "CET-6 核心词")));
        assert_eq!(book_from_stem("CET4_T"), Some(("cet4", "CET-4 核心词")));
        assert_eq!(book_from_stem("KaoYan_3_T"), Some(("ky", "考研核心词")));
        assert_eq!(book_from_stem("IELTS_4_T"), Some(("ielts", "IELTS 核心词")));
        assert_eq!(book_from_stem("TOEFL_T"), Some(("toefl", "TOEFL 核心词")));
        assert_eq!(book_from_stem("my-own-list"), None);
        assert_eq!(book_from_stem("考研核心词"), None);
    }

    #[test]
    fn qwerty_learner_shape_is_accepted() {
        let raw = r#"[
            {"usphone":"'kænsl","ukphone":"'kænsl","name":"cancel","trans":["取消， 撤销 (vt.)"]},
            {"name":"numerous","trans":["众多的 (adj.)","许多的"]}
        ]"#;
        let dir = std::env::temp_dir().join("qe_qwerty_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("CET6_T.json");
        std::fs::write(&path, raw).unwrap();

        let plans = parse_json(&path).unwrap();
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        assert_eq!(plan.book_slug, "cet6", "must merge into the CET-6 deck");
        assert_eq!(plan.book_name, "CET-6 核心词");
        assert_eq!(plan.words.len(), 2);

        let cancel = &plan.words[0];
        assert_eq!(cancel.lemma, "cancel");
        assert_eq!(cancel.translation, "取消， 撤销 (vt.)");
        assert_eq!(cancel.phonetic.as_deref(), Some("/'kænsl/"));

        let numerous = &plan.words[1];
        assert_eq!(numerous.translation, "众多的 (adj.); 许多的");
        assert!(numerous.phonetic.is_none());
        let _ = std::fs::remove_file(&path);
    }
}
