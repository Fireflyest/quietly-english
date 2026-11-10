//! 内置种子词书。
//!
//! 首次启动时写入一批带词根、例句的示例词，保证离线也能立刻体验完整流程。
//! 数据格式（`data/seed_words.tsv`，用 `|` 分隔）：
//!
//! ```text
//! book|lemma|phonetic|translation|roots|example_en|example_zh|tags
//! ```
//!
//! `roots` 用 `;` 分隔，每项是 `part:gloss:kind`，kind ∈ prefix/root/suffix。

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use qe_core::model::{Example, ImportPlan, ImportReport};
use qe_core::Store;

use crate::db::SqliteStore;
use crate::import::parse_roots;

const SEED: &str = include_str!("../data/seed_words.tsv");

fn book_meta(slug: &str) -> (&'static str, &'static str) {
    match slug {
        "cet4" => ("CET-4 Core", "College English Test Band 4 · built-in sample"),
        "cet6" => ("CET-6 Core", "College English Test Band 6 · built-in sample"),
        "toefl" => ("TOEFL Core", "Academic vocabulary · built-in sample"),
        "ielts" => ("IELTS Core", "IELTS vocabulary · built-in sample"),
        "ky" => ("Postgraduate Core", "Postgraduate exam vocabulary · built-in sample"),
        "gk" => ("Senior High Core", "Senior high vocabulary · built-in sample"),
        "zk" => ("Junior High Core", "Junior high vocabulary · built-in sample"),
        "gre" => ("GRE Core", "GRE vocabulary · built-in sample"),
        _ => ("Built-in Book", "Sample book shipped with the app"),
    }
}

/// 只在数据库还没有任何词书时写入。
pub fn seed_if_empty(store: &mut SqliteStore) -> Result<bool> {
    if !store.list_books()?.is_empty() {
        return Ok(false);
    }
    seed(store)?;
    Ok(true)
}

pub fn seed(store: &mut SqliteStore) -> Result<ImportReport> {
    let mut plans = parse_seed()?;
    let mut report = ImportReport::default();
    for plan in plans.values_mut() {
        report.merge(store.import_plan(plan)?);
    }
    Ok(report)
}

/// 解析内置词书，按 slug 聚合。
pub fn parse_seed() -> Result<BTreeMap<String, ImportPlan>> {
    let mut plans: BTreeMap<String, ImportPlan> = BTreeMap::new();

    for (idx, line) in SEED.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() != 8 {
            bail!(
                "seed_words.tsv 第 {} 行应有 8 个字段，实际 {}",
                idx + 1,
                fields.len()
            );
        }
        let slug = fields[0].trim();
        let (name, desc) = book_meta(slug);
        let plan = plans
            .entry(slug.to_string())
            .or_insert_with(|| ImportPlan::new(slug, name, desc));

        let mut word = qe_core::model::Word::new(fields[1].trim(), fields[3].trim());
        word.phonetic = non_empty(fields[2]);
        word.roots = parse_roots(fields[4]);
        let en = fields[5].trim();
        let zh = non_empty(fields[6]);
        if !en.is_empty() {
            word.examples.push(Example::new(en, zh));
        }
        word.tags = fields[7]
            .split([',', ' '])
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect();
        if word.lemma.is_empty() {
            continue;
        }
        plan.words.push(word);
    }

    Ok(plans)
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_parses_and_has_three_books() {
        let plans = parse_seed().unwrap();
        assert!(plans.len() >= 3);
        assert!(plans.get("cet4").is_some());
        let cet4 = plans.get("cet4").unwrap();
        assert!(cet4.words.len() >= 10);
        let inspect = cet4.words.iter().find(|w| w.lemma == "inspect").unwrap();
        assert_eq!(inspect.roots.len(), 2);
        assert_eq!(inspect.examples.len(), 1);
    }
}
