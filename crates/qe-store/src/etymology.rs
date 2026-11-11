//! 词源（词根）导入。
//!
//! 词根**不靠算法猜**，只从有出处的语料读。这里支持两种：
//!
//! 1. **kaikki.org / Wiktionary 抽取的 JSONL**（推荐）
//!    <https://kaikki.org/dictionary/English/> 每个词一条 JSON，其中
//!    `etymology_templates` 记录了 Wiktionary 编辑者填写的构词模板，
//!    例如 `{{prefix|en|un|happy}}`、`{{suffix|en|read|able}}`、
//!    `{{compound|en|sun|flower}}`、`{{confix|en|tele|graph}}`，
//!    以及简写 `{{af}}` / `{{pre}}` / `{{suf}}` / `{{com}}`。
//!    这些模板由人工依据词源词典录入，是可核查的学术来源（CC BY-SA）。
//!
//! 2. **自备 TSV**：`lemma<TAB>part<TAB>gloss<TAB>kind<TAB>source`，
//!    方便把教材、论文或人工整理的词根表直接导入。
//!
//! 导入只更新已经存在的词条，不会因为词源文件而新建单词。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use qe_core::model::{RootKind, WordRoot};

/// 导入结果。
#[derive(Debug, Clone, Default)]
pub struct EtymologyReport {
    pub words: u32,
    pub morphemes: u32,
    pub skipped: u32,
}

impl EtymologyReport {
    pub fn merge(&mut self, other: EtymologyReport) {
        self.words += other.words;
        self.morphemes += other.morphemes;
        self.skipped += other.skipped;
    }
}

// ---------------------------------------------------------------- kaikki JSONL

#[derive(Debug, Deserialize)]
struct KaikkiEntry {
    #[serde(default)]
    word: Option<String>,
    #[serde(default)]
    etymology_templates: Option<Vec<KaikkiTemplate>>,
    #[serde(default)]
    senses: Option<Vec<KaikkiSense>>,
}

#[derive(Debug, Deserialize)]
struct KaikkiTemplate {
    name: String,
    #[serde(default)]
    args: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct KaikkiSense {
    #[serde(default)]
    glosses: Option<Vec<String>>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// Present on "Alternative spelling of …" / "plural of …" senses.
    #[serde(default)]
    alt_of: Option<Vec<KaikkiFormOf>>,
    #[serde(default)]
    form_of: Option<Vec<KaikkiFormOf>>,
}

#[derive(Debug, Deserialize)]
struct KaikkiFormOf {
    word: String,
}

/// One English gloss taken from Wiktionary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gloss {
    pub text: String,
    /// Register / usage tags: `archaic`, `obsolete`, `slang`, …
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Everything one lemma yielded in the scan.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub roots: Vec<WordRoot>,
    pub glosses: Vec<Gloss>,
}

impl Analysis {
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty() && self.glosses.is_empty()
    }
}

/// 词素→释义索引，一次导入内复用。
/// One affix entry's glosses, in dictionary order.
#[derive(Debug, Clone)]
pub struct AffixGlosses {
    kind: RootKind,
    glosses: Vec<String>,
}

/// 词素 → 释义索引，一次导入内复用。
pub type MorphemeIndex = HashMap<String, AffixGlosses>;

fn is_affix(word: &str) -> bool {
    (word.starts_with('-') || word.ends_with('-')) && word.trim_matches('-').len() >= 2
}

/// Index key keeps the hyphen side, so `un-` and `-un` cannot overwrite each other.
fn affix_key(word: &str) -> String {
    word.trim().to_ascii_lowercase()
}

/// Look a morpheme's gloss up in the affix index.
///
/// A morpheme can be spelled three ways in templates (`spect`, `spect-`, `-spect`)
/// and the dictionary entry may use a different one than the template, so every
/// spelling is tried. Entries whose recorded kind matches the requested kind win;
/// otherwise any entry is accepted, because a bound root like `spect` legitimately
/// only has an affix entry.
fn gloss_for(index: &MorphemeIndex, part: &str, kind: RootKind) -> Option<String> {
    let base = part.trim_matches('-').to_ascii_lowercase();
    if base.is_empty() {
        return None;
    }
    let candidates = match kind {
        RootKind::Prefix => [format!("{base}-"), format!("-{base}"), base.clone()],
        RootKind::Suffix => [format!("-{base}"), format!("{base}-"), base.clone()],
        RootKind::Root => [base.clone(), format!("-{base}"), format!("{base}-")],
    };

    let hits: Vec<&AffixGlosses> = candidates
        .iter()
        .filter_map(|key| index.get(key))
        .collect();

    let chosen = hits
        .iter()
        .find(|entry| entry.kind == kind)
        .or_else(|| hits.first())?;

    // First gloss that is an actual meaning rather than a "Used to form …"
    // boilerplate sentence describing the suffix itself.
    chosen
        .glosses
        .iter()
        .find(|gloss| !is_boilerplate(gloss))
        .or_else(|| chosen.glosses.first())
        .cloned()
}

/// Wiktionary affix senses often open with a sentence describing the grammar
/// rather than the meaning. Those make poor card glosses when a real one exists.
fn is_boilerplate(gloss: &str) -> bool {
    const OPENERS: [&str; 9] = [
        "Used to form",
        "Used in the sense of",
        "Used to ",
        "Used after",
        "Used before",
        "Forming ",
        "Appended",
        "Denoting ",
        "Indicating ",
    ];
    OPENERS.iter().any(|opener| gloss.starts_with(opener))
}

fn affix_kind(word: &str) -> RootKind {
    if word.ends_with('-') {
        RootKind::Prefix
    } else {
        RootKind::Suffix
    }
}

/// 第一遍：收集词缀条目自身的释义（`un-`、`-ness` 这类）。
///
/// 只解析 `pos` 为前缀/后缀/环缀的行，避免对整份 150 万行的转储做全量 JSON 解析。
pub fn index_morphemes(path: &Path) -> Result<MorphemeIndex> {
    let file = File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut index: MorphemeIndex = HashMap::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() || !mentions_affix_pos(&line) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<KaikkiEntry>(&line) else {
            continue;
        };
        let Some(word) = entry.word.as_deref() else {
            continue;
        };
        if !is_affix(word) {
            continue;
        }

        // Keep every sense in dictionary order; which one reads best is decided at
        // lookup time, since that depends on what kind of morpheme asked for it.
        let glosses: Vec<String> = entry
            .senses
            .as_ref()
            .map(|senses| {
                senses
                    .iter()
                    .filter_map(|sense| sense.glosses.as_ref())
                    .flatten()
                    .map(|gloss| clean_gloss(gloss))
                    .filter(|gloss| gloss.chars().count() >= 3)
                    .collect()
            })
            .unwrap_or_default();
        if glosses.is_empty() {
            continue;
        }

        index.entry(affix_key(word)).or_insert(AffixGlosses {
            kind: affix_kind(word),
            glosses,
        });
    }

    Ok(index)
}

/// Clean up an affix gloss without shortening it.
///
/// Wiktionary writes these as full sentences, and the whole sentence is often more
/// useful than a fragment, so only register parentheticals are dropped.
fn clean_gloss(gloss: &str) -> String {
    let mut text = gloss.trim();
    // Drop leading parenthetical register notes: "(no longer productive) From."
    while text.starts_with('(') {
        match text.find(')') {
            Some(end) => text = text[end + 1..].trim_start(),
            None => break,
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cheap check for `"pos": "prefix"` / `"pos": "suffix"` without parsing.
fn mentions_affix_pos(line: &str) -> bool {
    const MARKERS: [&str; 10] = [
        "\"pos\": \"prefix\"",
        "\"pos\":\"prefix\"",
        "\"pos\": \"suffix\"",
        "\"pos\":\"suffix\"",
        "\"pos\": \"circumfix\"",
        "\"pos\":\"circumfix\"",
        "\"pos\": \"interfix\"",
        "\"pos\":\"interfix\"",
        "\"pos\": \"infix\"",
        "\"pos\":\"infix\"",
    ];
    MARKERS.iter().any(|marker| line.contains(marker))
}

/// Skip lines that cannot carry a usable formation template.
fn mentions_build_template(line: &str) -> bool {
    const NAMES: [&str; 9] = [
        "prefix", "suffix", "compound", "confix", "circumfix", "af", "pre", "suf", "com",
    ];
    if NAMES.iter().any(|name| {
        let spaced = format!("\"name\": \"{name}\"");
        let tight = format!("\"name\":\"{name}\"");
        line.contains(&spaced) || line.contains(&tight)
    }) {
        return true;
    }
    // `{{etymon|en|:af|non-|sense}}` or `{{ety|en|:af|re-|write}}` wraps the type.
    const ETYMON_TYPES: [&str; 5] = [":af\"", ":prefix\"", ":suffix\"", ":com\"", ":con\""];
    (line.contains("\"etymon\"") || line.contains("\"ety\""))
        && ETYMON_TYPES.iter().any(|marker| line.contains(marker))
}

/// Result of a streaming pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    /// Lines read from the file.
    pub scanned: u64,
    /// Lines that produced an analysis.
    pub produced: u32,
}

/// 第二遍：流式扫描每一行，收集构词与英文释义。
///
/// 同一个词可能有多条词源（不同词性、不同 `etymology_number`）。构词只取第一条，
/// 释义则合并所有词性，因为 `sake` 的「清酒」和「目的」分属不同词条。
/// `progress` 每 10 万行回调一次已扫描行数。
pub fn for_each_analysis(
    path: &Path,
    index: &MorphemeIndex,
    progress: &mut dyn FnMut(u64),
    mut on_word: impl FnMut(&str, Analysis) -> Result<()>,
) -> Result<ScanStats> {
    let file = File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut stats = ScanStats::default();
    // Roots are kept from the first entry that describes a formation; glosses
    // accumulate across entries.
    let mut roots_taken: HashSet<String> = HashSet::new();
    let mut pending: HashMap<String, Analysis> = HashMap::new();
    // `sake` → `saké`: an alternative spelling, whose meanings live on the target.
    let mut alt_targets: HashMap<String, String> = HashMap::new();

    for line in reader.lines() {
        let line = line?;
        stats.scanned += 1;
        if stats.scanned % 100_000 == 0 {
            progress(stats.scanned);
        }
        // Cheap pre-filter: a line with neither templates nor glosses is useless.
        if line.trim().is_empty()
            || (!line.contains("\"etymology_templates\"") && !line.contains("\"glosses\""))
        {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<KaikkiEntry>(&line) else {
            continue;
        };
        let Some(word) = entry.word.as_deref() else {
            continue;
        };
        // Affix entries describe the affix itself, and phrases are not single words.
        if is_affix(word) || word.contains(' ') || !word.chars().any(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        let key = word.to_ascii_lowercase();

        let mut analysis = Analysis::default();

        if !roots_taken.contains(&key) {
            if let Some(templates) = entry.etymology_templates.as_ref() {
                if mentions_build_template(&line) {
                    let roots = roots_from_templates(templates, index);
                    if !roots.is_empty() {
                        analysis.roots = roots;
                        roots_taken.insert(key.clone());
                    }
                }
            }
        }

        if let Some(senses) = entry.senses.as_ref() {
            analysis.glosses = collect_glosses(senses);
            // "Alternative spelling of saké." carries no meaning of its own, but it
            // tells us which entry does: record the target and inherit its senses
            // once the whole file has been read.
            if let Some(target) = alt_target(senses) {
                alt_targets.insert(key.clone(), target);
            }
        }

        if analysis.is_empty() {
            continue;
        }

        let slot = pending.entry(key).or_default();
        if slot.roots.is_empty() {
            slot.roots = analysis.roots;
        }
        for gloss in analysis.glosses {
            if slot.glosses.len() >= MAX_GLOSSES {
                break;
            }
            if slot.glosses.iter().any(|g| g.text == gloss.text) {
                continue;
            }
            slot.glosses.push(gloss);
        }
    }

    // Emission happens at the end: a lemma's entries are not adjacent in the dump,
    // and the caller wants one row per word.
    inherit_alt_of(&mut pending, &alt_targets);
    let mut words: Vec<(String, Analysis)> = pending.into_iter().collect();
    words.sort_by(|a, b| a.0.cmp(&b.0));
    for (lemma, analysis) in words {
        stats.produced += 1;
        on_word(&lemma, analysis)?;
    }

    Ok(stats)
}

/// Keep at most this many English glosses per word.
const MAX_GLOSSES: usize = 6;

/// The word an "alternative spelling / form of" sense points at.
fn alt_target(senses: &[KaikkiSense]) -> Option<String> {
    for sense in senses {
        for list in [sense.alt_of.as_ref(), sense.form_of.as_ref()].into_iter().flatten() {
            if let Some(entry) = list.first() {
                let target = entry.word.trim().to_ascii_lowercase();
                if !target.is_empty() {
                    return Some(target);
                }
            }
        }
    }
    None
}

/// Move meanings from an alias target onto the alias itself.
///
/// `sake` is only an "alternative spelling of saké", so its card would show nothing
/// useful. Copying the target's glosses over makes `sake` list "Japanese rice wine"
/// — which is exactly the rare sense a learner is looking for.
fn inherit_alt_of(pending: &mut HashMap<String, Analysis>, alt_targets: &HashMap<String, String>) {
    // Copy first, then apply, so chains are resolved one level at a time without
    // borrowing the map while iterating it.
    let inherited: Vec<(String, Vec<Gloss>)> = alt_targets
        .iter()
        .filter_map(|(alias, target)| {
            let source = pending.get(target)?;
            if source.glosses.is_empty() {
                return None;
            }
            Some((alias.clone(), source.glosses.clone()))
        })
        .collect();

    for (alias, glosses) in inherited {
        let Some(entry) = pending.get_mut(&alias) else {
            continue;
        };
        // Drop the "Alternative spelling of …" placeholder; the real meanings follow.
        entry
            .glosses
            .retain(|gloss| !is_alt_of_gloss(&gloss.text));
        for gloss in glosses {
            if entry.glosses.len() >= MAX_GLOSSES {
                break;
            }
            if entry.glosses.iter().any(|g| g.text == gloss.text) {
                continue;
            }
            entry.glosses.push(gloss);
        }
    }
}

fn is_alt_of_gloss(text: &str) -> bool {
    text.starts_with("Alternative spelling of")
        || text.starts_with("Alternative form of")
        || text.starts_with("Alternative letter-case form of")
}

/// Turn Wiktionary senses into glosses, keeping register tags but dropping the
/// "no-gloss" senses wiktionary uses for form-of entries.
fn collect_glosses(senses: &[KaikkiSense]) -> Vec<Gloss> {
    let mut out = Vec::new();
    for sense in senses {
        let Some(glosses) = sense.glosses.as_ref() else {
            continue;
        };
        let Some(text) = glosses.first() else { continue };
        let text = text.trim();
        // Form-of senses read "present participle of run"; they are not meanings.
        if text.is_empty() || text.starts_with("present participle of") {
            continue;
        }
        let tags: Vec<String> = sense
            .tags
            .as_ref()
            .map(|tags| {
                tags.iter()
                    .filter(|tag| {
                        matches!(
                            tag.as_str(),
                            "archaic" | "obsolete" | "slang" | "informal" | "vulgar" | "dated"
                                | "rare" | "poetic" | "humorous" | "figurative" | "literally"
                        )
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.push(Gloss {
            text: text.to_string(),
            tags,
        });
        if out.len() >= MAX_GLOSSES {
            break;
        }
    }
    out
}

/// Convert one entry's templates into an ordered morpheme list.
///
/// Only templates that state the parts themselves are used. `inh` (inherited),
/// `der` (derived), `bor` (borrowed), `cog` (cognate), `root` (a Proto-Indo-European
/// root note) and friends are skipped: they describe history, not the word's own
/// construction, and turning them into a morpheme split would be invention.
fn roots_from_templates(templates: &[KaikkiTemplate], index: &MorphemeIndex) -> Vec<WordRoot> {
    for template in templates {
        let Some((kind, parts, label)) = formation(template) else {
            continue;
        };
        let roots = build(parts, kind, index, &label);
        if let Some(roots) = roots {
            return roots;
        }
    }
    Vec::new()
}

/// Normalise a template into `(build kind, morphemes, label)`.
///
/// Handles both the direct templates (`{{prefix|en|un|happy}}`) and the
/// `{{etymon|en|:af|un-|happy}}` wrapper, whose type sits in argument 2.
fn formation(template: &KaikkiTemplate) -> Option<(BuildKind, Vec<String>, String)> {
    // `{{etymon}}` and `{{ety}}` wrap the formation type in argument 2.
    if template.name == "etymon" || template.name == "ety" {
        let raw = template.args.get("2")?.trim().trim_start_matches(':');
        let kind = match raw {
            "af" => BuildKind::Hyphenated,
            "prefix" | "pre" => BuildKind::Prefix,
            "suffix" | "suf" => BuildKind::Suffix,
            "com" | "compound" => BuildKind::Root,
            "con" | "confix" | "circumfix" => BuildKind::Confix,
            // `:inh`, `:der`, `:bor`, `:root`, ... describe history, not structure.
            _ => return None,
        };
        let parts = collect_parts(template, 3);
        return (!parts.is_empty()).then(|| (kind, parts, format!("{raw}")));
    }

    let kind = match template.name.as_str() {
        "prefix" | "pre" => BuildKind::Prefix,
        "suffix" | "suf" => BuildKind::Suffix,
        "confix" | "circumfix" => BuildKind::Confix,
        "compound" | "com" => BuildKind::Root,
        // `{{af}}` is Wiktionary's omnibus formation template. Affixes are marked
        // with hyphens by the editor (`un-`, `-able`, `-s-`), so the type comes
        // from their notation rather than from guessing.
        "af" => BuildKind::Hyphenated,
        _ => return None,
    };
    let parts = collect_parts(template, 2);
    (!parts.is_empty()).then(|| (kind, parts, template.name.clone()))
}

/// Wikitext parameters are numbered from the given start; the language code in
/// argument 1 was already skipped.
fn collect_parts(template: &KaikkiTemplate, start: u32) -> Vec<String> {
    let mut parts = Vec::new();
    let mut n = start;
    loop {
        let Some(value) = template.args.get(&n.to_string()) else {
            break;
        };
        if let Some(part) = clean_part(value) {
            parts.push(part);
        }
        n += 1;
    }
    parts
}

/// Drop the inline annotations kaikki keeps in parameter values, e.g.
/// `non-<id:not>` → `non-`, and ignore non-morpheme parameters.
fn clean_part(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for ch in raw.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    let out = out.trim().trim_matches('*').trim().to_string();
    if out.is_empty() || out.contains('=') || out == "-" || out == "?" {
        return None;
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq)]
enum BuildKind {
    Prefix,
    Suffix,
    Confix,
    Root,
    /// Classify each part from its hyphen notation (`{{af|en|un-|happy}}`).
    Hyphenated,
}

/// Kind encoded by an editor's hyphen notation, or `None` when plain.
fn hyphenated_kind(part: &str) -> Option<RootKind> {
    let starts = part.starts_with('-');
    let ends = part.ends_with('-');
    match (starts, ends) {
        (false, true) => Some(RootKind::Prefix),
        (true, false) => Some(RootKind::Suffix),
        // `-s-` is an interfix: a linking element, not a prefix or suffix.
        (true, true) => None,
        (false, false) => None,
    }
}

/// Add a hyphen on one side unless the part already carries one.
fn with_hyphen(part: &str, trailing: bool) -> String {
    let trimmed = part.trim_end_matches('-');
    let trimmed = trimmed.trim_start_matches('-');
    if trailing {
        format!("{trimmed}-")
    } else {
        format!("-{trimmed}")
    }
}

fn build(
    parts: Vec<String>,
    kind: BuildKind,
    index: &MorphemeIndex,
    template: &str,
) -> Option<Vec<WordRoot>> {
    if parts.is_empty() {
        return None;
    }
    let last = parts.len() - 1;
    let source = format!("Wiktionary {{{{{template}}}}}");
    let mut roots = Vec::new();

    for (i, part) in parts.iter().enumerate() {
        let (kind, display) = match kind {
            BuildKind::Prefix => (
                if i == 0 {
                    RootKind::Prefix
                } else {
                    RootKind::Root
                },
                if i == 0 {
                    with_hyphen(part, true)
                } else {
                    part.clone()
                },
            ),
            BuildKind::Suffix => (
                if i == last {
                    RootKind::Suffix
                } else {
                    RootKind::Root
                },
                if i == last {
                    with_hyphen(part, false)
                } else {
                    part.clone()
                },
            ),
            BuildKind::Confix => (
                // A two-part confix is a prefix plus a root: `in-` + `spect`.
                match (i, last) {
                    (_, 1) if i == 0 => RootKind::Prefix,
                    (_, 1) => RootKind::Root,
                    (0, _) => RootKind::Prefix,
                    (n, l) if n == l => RootKind::Suffix,
                    _ => RootKind::Root,
                },
                match (i, last) {
                    (_, 1) if i == 0 => with_hyphen(part, true),
                    (_, 1) => part.clone(),
                    (0, _) => with_hyphen(part, true),
                    (n, l) if n == l => with_hyphen(part, false),
                    _ => part.clone(),
                },
            ),
            BuildKind::Root => (RootKind::Root, part.clone()),
            BuildKind::Hyphenated => {
                let kind = hyphenated_kind(part).unwrap_or(RootKind::Root);
                (kind, part.clone())
            }
        };

        let gloss = gloss_for(index, part, kind).unwrap_or_default();

        roots.push(WordRoot::new(display, gloss, kind).with_source(source.clone()));
    }

    Some(roots)
}

// ---------------------------------------------------------------- TSV

/// `lemma<TAB>part<TAB>gloss<TAB>kind<TAB>source`
///
/// `kind` 可为 `prefix` / `root` / `suffix`；`source` 可留空。
pub fn parse_roots_tsv(path: &Path) -> Result<Vec<(String, Vec<WordRoot>)>> {
    let file = File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out: Vec<(String, Vec<WordRoot>)> = Vec::new();

    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        let line = line.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 {
            anyhow::bail!(
                "{} 第 {} 行应为 4~5 列（lemma/part/gloss/kind/[source]），实际 {}",
                path.display(),
                idx + 1,
                fields.len()
            );
        }
        let lemma = fields[0].trim();
        if lemma.is_empty() {
            continue;
        }
        let mut root = WordRoot::new(
            fields[1].trim(),
            fields[2].trim(),
            RootKind::parse(fields[3]),
        );
        if let Some(source) = fields.get(4).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            root = root.with_source(source);
        }

        match out.iter_mut().find(|(word, _)| word.eq_ignore_ascii_case(lemma)) {
            Some((_, roots)) => roots.push(root),
            None => out.push((lemma.to_string(), vec![root])),
        }
    }

    Ok(out)
}

/// 判断文件是否像 kaikki JSONL（首行是 JSON 对象）。
pub fn looks_like_jsonl(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    let trimmed = line.trim_start();
    trimmed.starts_with('{') && trimmed.contains("\"word\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(name: &str, args: &[(&str, &str)]) -> KaikkiTemplate {
        KaikkiTemplate {
            name: name.to_string(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn entry(kind: RootKind, glosses: &[&str]) -> AffixGlosses {
        AffixGlosses {
            kind,
            glosses: glosses.iter().map(|g| g.to_string()).collect(),
        }
    }

    fn index() -> MorphemeIndex {
        let mut index = HashMap::new();
        // Keys keep the hyphen side, matching what the real index does.
        index.insert("un-".to_string(), entry(RootKind::Prefix, &["not"]));
        index.insert("happy".to_string(), entry(RootKind::Root, &["content"]));
        index.insert("read".to_string(), entry(RootKind::Root, &["read"]));
        index.insert("-able".to_string(), entry(RootKind::Suffix, &["capable of"]));
        index
    }

    #[test]
    fn prefix_template_becomes_prefix_plus_root() {
        // {{prefix|en|un|happy}}
        let roots = roots_from_templates(
            &[template("prefix", &[("1", "en"), ("2", "un"), ("3", "happy")])],
            &index(),
        );
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].part, "un-");
        assert_eq!(roots[0].kind, RootKind::Prefix);
        assert_eq!(roots[0].gloss, "not");
        assert_eq!(roots[1].part, "happy");
        assert_eq!(roots[1].kind, RootKind::Root);
        assert_eq!(roots[0].source.as_deref(), Some("Wiktionary {{prefix}}"));
    }

    #[test]
    fn suffix_template_marks_the_last_part() {
        // {{suffix|en|read|able}}
        let roots = roots_from_templates(
            &[template("suffix", &[("1", "en"), ("2", "read"), ("3", "able")])],
            &index(),
        );
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].part, "read");
        assert_eq!(roots[1].part, "-able");
        assert_eq!(roots[1].kind, RootKind::Suffix);
    }

    #[test]
    fn unknown_templates_are_ignored() {
        // {{inh|en|la|spectare}} is an inheritance note, not a breakdown.
        let roots = roots_from_templates(
            &[template("inh", &[("1", "en"), ("2", "la"), ("3", "spectare")])],
            &index(),
        );
        assert!(roots.is_empty(), "must not invent an analysis");
    }

    #[test]
    fn language_code_is_not_treated_as_a_morpheme() {
        let parts = collect_parts(
            &template("prefix", &[("1", "en"), ("2", "un"), ("3", "happy")]),
            2,
        );
        assert_eq!(parts, vec!["un".to_string(), "happy".to_string()]);
    }

    #[test]
    fn inline_id_annotations_are_stripped() {
        // kaikki keeps wikitext markup like `non-<id:not>` in the parameter value.
        assert_eq!(clean_part("non-<id:not>"), Some("non-".to_string()));
        assert_eq!(clean_part("*-s-"), Some("-s-".to_string()));
        assert_eq!(clean_part("-"), None);
        assert_eq!(clean_part(""), None);
    }

    #[test]
    fn etymon_wrapper_uses_its_type_argument() {
        // {{etymon|en|:af|non-|sense}} → prefix + root
        let roots = roots_from_templates(
            &[template(
                "etymon",
                &[("1", "en"), ("2", ":af"), ("3", "non-<id:not>"), ("4", "sense")],
            )],
            &index(),
        );
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].part, "non-");
        assert_eq!(roots[0].kind, RootKind::Prefix);
        assert_eq!(roots[1].part, "sense");
        assert_eq!(roots[1].kind, RootKind::Root);
        assert_eq!(roots[0].source.as_deref(), Some("Wiktionary {{af}}"));
    }

    #[test]
    fn etymon_inheritance_is_skipped() {
        let roots = roots_from_templates(
            &[template(
                "etymon",
                &[("1", "en"), ("2", ":inh"), ("3", "enm:fre")],
            )],
            &index(),
        );
        assert!(roots.is_empty());
    }

    #[test]
    fn af_uses_hyphens_to_type_each_part() {
        // {{af|en|un-|happy}} → prefix + root
        let roots = roots_from_templates(
            &[template("af", &[("1", "en"), ("2", "un-"), ("3", "happy")])],
            &index(),
        );
        assert_eq!(roots[0].kind, RootKind::Prefix);
        assert_eq!(roots[1].kind, RootKind::Root);

        // {{af|en|Moon|day}} → both plain, a compound
        let compound = roots_from_templates(
            &[template("af", &[("1", "en"), ("2", "Moon"), ("3", "day")])],
            &index(),
        );
        assert!(compound.iter().all(|r| r.kind == RootKind::Root));
    }

    #[test]
    fn confix_marks_both_ends_when_there_are_three_parts() {
        // {{confix|en|tele|graph|ic}} → prefix + root + suffix
        let roots = roots_from_templates(
            &[template("confix", &[("1", "en"), ("2", "tele"), ("3", "graph"), ("4", "ic")])],
            &index(),
        );
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0].kind, RootKind::Prefix);
        assert_eq!(roots[1].kind, RootKind::Root);
        assert_eq!(roots[2].kind, RootKind::Suffix);
    }

    #[test]
    fn two_part_confix_is_prefix_plus_root() {
        // {{confix|en|in|spect}}: treating the second half as a suffix would be
        // wrong — `spect` is a bound root, not a suffix.
        let roots = roots_from_templates(
            &[template("confix", &[("1", "en"), ("2", "in"), ("3", "spect")])],
            &index(),
        );
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].part, "in-");
        assert_eq!(roots[0].kind, RootKind::Prefix);
        assert_eq!(roots[1].part, "spect");
        assert_eq!(roots[1].kind, RootKind::Root);
    }

    #[test]
    fn hyphens_are_not_doubled() {
        // A template may already spell the affix with its hyphen.
        let roots = roots_from_templates(
            &[template("suffix", &[("1", "en"), ("2", "kind"), ("3", "-ness")])],
            &index(),
        );
        assert_eq!(roots[1].part, "-ness");
        assert!(!roots[1].part.starts_with("--"));
    }

    #[test]
    fn gloss_lookup_prefers_the_matching_side() {
        let mut index = MorphemeIndex::new();
        index.insert("-ness".to_string(), entry(RootKind::Suffix, &["state of"]));
        index.insert("kind-".to_string(), entry(RootKind::Prefix, &["group of"]));
        assert_eq!(
            gloss_for(&index, "ness", RootKind::Suffix).as_deref(),
            Some("state of")
        );
        assert_eq!(
            gloss_for(&index, "kind", RootKind::Prefix).as_deref(),
            Some("group of")
        );
        // A bound root falls back to whatever spelling exists: `kind` has no plain
        // entry, so it borrows the prefix one rather than showing nothing.
        assert_eq!(
            gloss_for(&index, "kind", RootKind::Root).as_deref(),
            Some("group of")
        );
    }

    #[test]
    fn bound_root_finds_its_gloss_through_the_hyphenated_entry() {
        // Regression: `inspect` renders `spect -> To look at …`. The template spells
        // it `spect`, but the dictionary entry is `-spect`, so a plain lookup misses.
        let mut index = MorphemeIndex::new();
        index.insert(
            "-spect".to_string(),
            entry(RootKind::Suffix, &["To look at, perceive, or observe"]),
        );
        assert_eq!(
            gloss_for(&index, "spect", RootKind::Root).as_deref(),
            Some("To look at, perceive, or observe")
        );
    }

    #[test]
    fn boilerplate_glosses_lose_to_real_meanings() {
        let mut index = MorphemeIndex::new();
        index.insert(
            "in-".to_string(),
            entry(
                RootKind::Prefix,
                &[
                    "Used to form verbs from nouns",
                    "in, into, towards, within",
                ],
            ),
        );
        // The first sense only describes the grammar; the second is the meaning.
        assert_eq!(
            gloss_for(&index, "in", RootKind::Prefix).as_deref(),
            Some("in, into, towards, within")
        );
    }

    #[test]
    fn boilerplate_is_used_when_nothing_else_exists() {
        let mut index = MorphemeIndex::new();
        index.insert(
            "-ish".to_string(),
            entry(RootKind::Suffix, &["Used to form adjectives from nouns"]),
        );
        assert_eq!(
            gloss_for(&index, "ish", RootKind::Suffix).as_deref(),
            Some("Used to form adjectives from nouns")
        );
    }

    #[test]
    fn tsv_rows_group_by_lemma() {
        let dir = std::env::temp_dir().join("qe_roots_tsv");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roots.tsv");
        std::fs::write(
            &path,
            "# lemma\tpart\tgloss\tkind\tsource\ninspect\tin-\tinward\tprefix\tOED\ninspect\tspect\tlook\troot\tOED\n",
        )
        .unwrap();
        let rows = parse_roots_tsv(&path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.len(), 2);
        assert_eq!(rows[0].1[1].gloss, "look");
        assert_eq!(rows[0].1[0].source.as_deref(), Some("OED"));
        let _ = std::fs::remove_file(&path);
    }

    fn sense(gloss: &str, tags: &[&str]) -> KaikkiSense {
        KaikkiSense {
            glosses: Some(vec![gloss.to_string()]),
            tags: Some(tags.iter().map(|t| t.to_string()).collect()),
            alt_of: None,
            form_of: None,
        }
    }

    #[test]
    fn glosses_keep_register_tags_and_drop_form_of_noise() {
        let senses = vec![
            sense("cause, interest or account", &[]),
            sense("Japanese rice wine", &["countable"]),
            sense("present participle of run", &[]),
            sense("the aforementioned reason", &["archaic"]),
        ];
        let glosses = collect_glosses(&senses);
        // `countable` is grammar, not register, so it is dropped.
        assert_eq!(glosses.len(), 3);
        assert_eq!(glosses[1].text, "Japanese rice wine");
        assert!(glosses[1].tags.is_empty());
        assert_eq!(glosses[2].tags, vec!["archaic"]);
        assert!(
            !glosses.iter().any(|g| g.text.contains("present participle")),
            "form-of senses are not meanings"
        );
    }

    #[test]
    fn glosses_are_capped_and_deduplicated() {
        let senses: Vec<KaikkiSense> = (0..12)
            .map(|i| sense(&format!("meaning {i}"), &[]))
            .collect();
        let glosses = collect_glosses(&senses);
        assert_eq!(glosses.len(), MAX_GLOSSES);

        let repeated = vec![sense("one", &[]), sense("one", &[])];
        assert_eq!(collect_glosses(&repeated).len(), 2, "caller dedupes");
    }

    fn alt_of(word: &str) -> KaikkiSense {
        KaikkiSense {
            glosses: Some(vec![format!("Alternative spelling of {word}.")]),
            tags: Some(vec!["alt-of".to_string()]),
            alt_of: Some(vec![KaikkiFormOf {
                word: word.to_string(),
            }]),
            form_of: None,
        }
    }

    #[test]
    fn alternative_spellings_inherit_the_target_meanings() {
        // `sake` is only "Alternative spelling of saké"; the meanings live on `saké`.
        let mut pending: HashMap<String, Analysis> = HashMap::new();
        pending.insert(
            "sake".to_string(),
            Analysis {
                roots: Vec::new(),
                glosses: collect_glosses(&[alt_of("saké")]),
            },
        );
        pending.insert(
            "saké".to_string(),
            Analysis {
                roots: Vec::new(),
                glosses: collect_glosses(&[sense(
                    "Rice wine; a drink made from filtered fermented rice",
                    &[],
                )]),
            },
        );

        let mut targets = HashMap::new();
        targets.insert("sake".to_string(), "saké".to_string());
        inherit_alt_of(&mut pending, &targets);

        let glosses = &pending["sake"].glosses;
        assert_eq!(glosses.len(), 1, "placeholder is replaced");
        assert!(glosses[0].text.contains("Rice wine"));
    }

    #[test]
    fn alt_target_handles_form_of_too() {
        let sense = KaikkiSense {
            glosses: Some(vec!["plural of mouse".to_string()]),
            tags: None,
            alt_of: None,
            form_of: Some(vec![KaikkiFormOf {
                word: "Mouse".to_string(),
            }]),
        };
        assert_eq!(alt_target(&[sense]).as_deref(), Some("mouse"));
    }

    #[test]
    fn missing_target_leaves_the_alias_alone() {
        let mut pending: HashMap<String, Analysis> = HashMap::new();
        pending.insert(
            "thing".to_string(),
            Analysis {
                roots: Vec::new(),
                glosses: collect_glosses(&[alt_of("nonexistent")]),
            },
        );
        let mut targets = HashMap::new();
        targets.insert("thing".to_string(), "nonexistent".to_string());
        inherit_alt_of(&mut pending, &targets);
        // Nothing to inherit, so the original gloss stays rather than vanishing.
        assert_eq!(pending["thing"].glosses.len(), 1);
    }
}
