//! Tatoeba 例句导入。
//!
//! Tatoeba 的导出是三个文件：
//!
//! * `cmn_sentences.tsv` — `id<TAB>lang<TAB>句子`
//! * `eng_sentences.tsv` — 同上，英文
//! * `links.csv` — `sentence_id<TAB>translation_id`，没有表头
//!
//! 要得到中英例句对，需要把三者 join 起来。全量 join 会把上百万条英文句子留在内存里，
//! 所以分三遍流式处理：
//!
//! 1. 读中文句子（约 9 万条，可以直接放内存）
//! 2. 扫一遍 links，记下"被中文句子引用到的英文 id"
//! 3. 读英文句子，只保留第 2 步要的那些
//! 4. 再扫一遍 links，对每条配对找例句里出现的"词书里的词"
//!
//! 只为词书里已有的词收例句，避免给 300 万条词条都塞语料。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};

use qe_core::model::Example;

/// 每个词最多保留几条例句。
pub const MAX_PER_WORD: usize = 3;

/// 一次导入的统计。
#[derive(Debug, Clone, Copy, Default)]
pub struct PairStats {
    pub pairs: u32,
    pub attached: u32,
    pub words: u32,
}

/// 从三个导出文件生成 `(词, 例句)`。
///
/// `vocabulary` 是当前词书里的词，只有它们会拿到例句。
pub fn collect_examples(
    cmn_path: &Path,
    eng_path: &Path,
    links_path: &Path,
    vocabulary: &HashSet<String>,
    mut progress: impl FnMut(&str, u64),
) -> Result<(Vec<(String, Example)>, PairStats)> {
    // 1. Chinese side: small enough to hold in memory.
    let mut chinese: HashMap<u64, String> = HashMap::new();
    for (index, line) in BufReader::new(open(cmn_path)?).lines().enumerate() {
        let line = line?;
        if let Some((id, text)) = parse_sentence(&line) {
            chinese.insert(id, text);
        }
        if index % 20_000 == 0 {
            progress("chinese", index as u64);
        }
    }

    // 2. Which English sentences are actually paired with Chinese ones?
    let mut wanted: HashSet<u64> = HashSet::new();
    let mut links = 0u64;
    for line in BufReader::new(open(links_path)?).lines() {
        let line = line?;
        links += 1;
        if links % 1_000_000 == 0 {
            progress("links", links);
        }
        let Some((a, b)) = parse_link(&line) else {
            continue;
        };
        match (chinese.contains_key(&a), chinese.contains_key(&b)) {
            (true, false) => {
                wanted.insert(b);
            }
            (false, true) => {
                wanted.insert(a);
            }
            _ => {}
        }
    }

    // 3. English side, filtered down to the ids found above.
    let mut english: HashMap<u64, String> = HashMap::new();
    for (index, line) in BufReader::new(open(eng_path)?).lines().enumerate() {
        let line = line?;
        if let Some((id, text)) = parse_sentence(&line) {
            if wanted.contains(&id) {
                english.insert(id, text);
            }
        }
        if index % 200_000 == 0 {
            progress("english", index as u64);
        }
    }

    // 4. Walk the links again and mint examples.
    let mut per_word: HashMap<String, u8> = HashMap::new();
    let mut out: Vec<(String, Example)> = Vec::new();
    let mut stats = PairStats::default();
    let mut seen_links = 0u64;

    for line in BufReader::new(open(links_path)?).lines() {
        let line = line?;
        seen_links += 1;
        if seen_links % 1_000_000 == 0 {
            progress("pairs", seen_links);
        }
        let Some((a, b)) = parse_link(&line) else {
            continue;
        };
        let pair = match (chinese.get(&a), english.get(&b)) {
            (Some(zh), Some(en)) => Some((en, zh)),
            _ => match (chinese.get(&b), english.get(&a)) {
                (Some(zh), Some(en)) => Some((en, zh)),
                _ => None,
            },
        };
        let Some((en, zh)) = pair else { continue };
        stats.pairs += 1;

        // Only words from the deck, and only a few sentences each.
        let mut used_any = false;
        for token in tokens(en) {
            if !vocabulary.contains(&token) {
                continue;
            }
            let slot = per_word.entry(token.clone()).or_insert(0);
            if *slot as usize >= MAX_PER_WORD {
                continue;
            }
            *slot += 1;
            out.push((
                token,
                Example {
                    en: en.to_string(),
                    zh: Some(zh.to_string()),
                },
            ));
            stats.attached += 1;
            used_any = true;
        }
        if used_any {
            stats.words = per_word.len() as u32;
        }
    }

    Ok((out, stats))
}

/// 把句子切成候选单词：小写、只留字母与连字符/撇号、长度 ≥ 3。
fn tokens(sentence: &str) -> Vec<String> {
    sentence
        .split(|ch: char| !(ch.is_alphabetic() || ch == '\'' || ch == '-'))
        .map(|word| word.trim_matches(|ch| ch == '\'' || ch == '-'))
        .filter(|word| word.chars().count() >= 3 && word.chars().all(|ch| ch.is_ascii_alphabetic()))
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

fn parse_sentence(line: &str) -> Option<(u64, String)> {
    let mut parts = line.splitn(3, '\t');
    let id = parts.next()?.trim().parse().ok()?;
    let _lang = parts.next()?;
    let text = parts.next()?.trim();
    if text.is_empty() {
        return None;
    }
    Some((id, text.replace('\\', "")))
}

fn parse_link(line: &str) -> Option<(u64, u64)> {
    let mut parts = line.split(['\t', ',']);
    let a = parts.next()?.trim().parse().ok()?;
    let b = parts.next()?.trim().parse().ok()?;
    Some((a, b))
}

fn open(path: &Path) -> Result<File> {
    File::open(path).with_context(|| format!("无法打开 {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_keep_words_and_drop_noise() {
        let words = tokens("Engineers inspect the bridge, every year — don't they?");
        assert!(words.contains(&"engineers".to_string()));
        assert!(words.contains(&"inspect".to_string()));
        assert!(words.contains(&"bridge".to_string()));
        // Punctuation is stripped and everything is lowercased.
        assert!(!words.iter().any(|w| w.contains(',') || w.contains('—')));
        // Apostrophes split the clitic, so `don't` never reaches the deck lookup as
        // a single token.
        assert!(!words.iter().any(|w| w.contains('\'')), "{words:?}");
        // Two-letter words and shorter are dropped, not the deck filter's job.
        assert!(!words.iter().any(|w| w.chars().count() < 3), "{words:?}");
    }

    #[test]
    fn parses_sentence_rows() {
        let (id, text) = parse_sentence("1276\teng\tLet's try something.").unwrap();
        assert_eq!(id, 1276);
        assert_eq!(text, "Let's try something.");
        assert!(parse_sentence("bad row").is_none());
    }

    #[test]
    fn parses_link_rows_in_both_dialects() {
        assert_eq!(parse_link("1\t2481"), Some((1, 2481)));
        assert_eq!(parse_link("1,2481"), Some((1, 2481)));
        assert_eq!(parse_link("nonsense"), None);
    }
}
