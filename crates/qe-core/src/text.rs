//! ECDICT 释义文本的清洗与解析。
//!
//! ECDICT 的 StarDict 包把数据库字段一起写进了释义，直接显示会让人莫名其妙：
//!
//! ```text
//! *[ɡəʊ]                                  ← 音标，已单独存字段
//! -K5                                      ← 柯林斯星级 + 牛津标记
//! vi. 去, 走, 达到, 运转, …
//! vt. 以...打赌, 对付, 忍受
//! [时态] went, gone, going, goes            ← 词形变化，对学习者有用
//! (中高雅 35/44)                            ← 标签 + BNC/COCA 排名
//! ```
//!
//! 这里的任务只有两件：把 `-K5` 和 `(中高雅 35/44)` 这类**元数据行**认出来，
//! 以及把星级解析成可以渲染的数值。释义正文（含 `[时态]` 这种词形行）原样保留。
//!
//! 清洗放在渲染层做，不动数据库：早期导入的数据已经带上了这些行，重灌 327 万条
//! 只为了删几行不值得。

/// 词频信息，对应 ECDICT 的 `collins` / `oxford` 两列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freq {
    /// 柯林斯星级，1–5。
    pub stars: u8,
    /// 是否属于牛津核心词表。
    pub oxford: bool,
}

/// 元数据行：`-K5`、`-3`、`-K`、`-1`。字母（若有）表示牛津标记，数字是星级。
///
/// 无论字母到底代表哪一列，数字始终落在 1–5，所以据此出星标不会出错。
/// `-K` 这种没有数字的形态也出现过（牛津词但缺星级），同样要过滤。
fn parse_rating_line(line: &str) -> Option<Option<Freq>> {
    let rest = line.strip_prefix('-')?;
    if rest.is_empty() || rest.len() > 3 {
        return None;
    }
    let (oxford, digits) = match rest.chars().next() {
        Some(ch) if ch.is_ascii_alphabetic() => (true, &rest[1..]),
        _ => (false, rest),
    };
    if digits.is_empty() {
        // `-K`: known to be an Oxford word, star count omitted.
        return oxford.then_some(None);
    }
    if digits.len() > 2 {
        return None;
    }
    let stars: u8 = digits.parse().ok()?;
    if !(1..=5).contains(&stars) {
        return None;
    }
    Some(Some(Freq { stars, oxford }))
}

fn is_rating_line(line: &str) -> bool {
    parse_rating_line(line).is_some()
}

/// 排名行：`(中高雅 35/44)` —— 标签缩写 + BNC/COCA 词频排名。
fn is_rank_line(line: &str) -> bool {
    let Some(inner) = line.strip_prefix('(').and_then(|l| l.strip_suffix(')')) else {
        return false;
    };
    let Some((left, right)) = inner.rsplit_once('/') else {
        return false;
    };
    let numeric = |s: &str| !s.trim().is_empty() && s.trim().chars().all(|ch| ch.is_ascii_digit());
    numeric(left.rsplit(' ').next().unwrap_or("")) && numeric(right)
}

/// 从一段释义里取词频信息；没有星级数字时返回 `None`。
pub fn freq(raw: &str) -> Option<Freq> {
    raw.lines()
        .find_map(|line| parse_rating_line(line.trim_end()).flatten())
}

/// 去掉元数据行后的释义。`[时态]` / `[级别]` 这类词形行会保留。
pub fn clean_translation(raw: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end();
        if is_rating_line(line) || is_rank_line(line.trim()) {
            continue;
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    kept.join("\n")
}

/// 星级渲染成五格星标，方便放进灰色说明行。
pub fn stars(freq: Freq) -> String {
    let filled = freq.stars.min(5) as usize;
    format!("{}{}", "★".repeat(filled), "☆".repeat(5 - filled))
}

/// 一条英文释义，来自 Wiktionary。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gloss {
    pub text: String,
    /// 语体标签：`archaic`、`obsolete`、`slang`…
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Gloss {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tags: Vec::new(),
        }
    }

    pub fn is_secondary(&self) -> bool {
        !self.tags.is_empty()
    }
}

/// 一条词义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sense {
    /// 词性缩写，如 `vt.`；源数据未标注时为空。
    pub pos: String,
    /// 领域或语体标签，如 `医`、`古`、`口`；为空表示常用义。
    pub label: String,
    /// 释义正文。
    pub text: String,
}

impl Sense {
    /// 带标签的一律归入"生僻/专业义"，与常用义分开展示。
    pub fn is_secondary(&self) -> bool {
        !self.label.is_empty()
    }
}

/// `parse_senses` 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSenses {
    pub senses: Vec<Sense>,
    /// `[时态] went, gone, …` 这类词形变化，单独一行展示。
    pub forms: Option<String>,
}

/// ECDICT 的词性前缀与展示用缩写。
const POS_TABLE: &[(&str, &str)] = &[
    ("abbr.", "abbr"),
    ("prep.", "prep"),
    ("conj.", "conj"),
    ("pron.", "pron"),
    ("adj.", "adj"),
    ("adv.", "adv"),
    ("aux.", "aux"),
    ("art.", "art"),
    ("num.", "num"),
    ("int.", "int"),
    ("vt.", "vt"),
    ("vi.", "vi"),
    ("v.", "v"),
    ("n.", "n"),
    ("a.", "adj"),
    ("ad.", "adv"),
];

/// 把 `[医]`、`[时态]` 这类方括号前缀拆出来。
fn split_bracket(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some((&rest[..end], rest[end + 1..].trim_start()))
}

/// 拆出词性前缀，返回 `(词性, 剩余释义)`。
fn split_pos(line: &str) -> Option<(&'static str, &str)> {
    POS_TABLE
        .iter()
        .find(|(tag, _)| line.starts_with(*tag))
        .map(|(tag, _)| (*tag, line[tag.len()..].trim_start()))
}

/// 把一段 ECDICT 释义拆成结构化词义。
///
/// 规则只有一条：**保留原文的分行**。ECDICT 用一行表示"一个词性下的一组近义
/// 释义"，把它拆散会丢失这层分组，所以一行仍是一条 `Sense`。
///
/// ```text
/// -K1                                    → 丢弃（元数据）
/// n. 银行, 堤, 岸                         → 常用义，pos = n.
/// [医] 库                                → 生僻/专业义，label = 医
/// [时态] banked, banking, banks           → forms
/// (中高雅 663/406)                        → 丢弃（元数据）
/// ```
pub fn parse_senses(raw: &str) -> ParsedSenses {
    let mut parsed = ParsedSenses::default();

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty()
            || parse_rating_line(line).is_some()
            || is_rank_line(line)
        {
            continue;
        }

        if let Some((tag, rest)) = split_bracket(line) {
            match tag {
                // 词形变化不是词义。
                "时态" | "级别" | "变形" => {
                    if !rest.is_empty() {
                        parsed.forms = Some(format!("[{tag}] {rest}"));
                    }
                    continue;
                }
                // 其余方括号都是领域/语体标记。
                _ => {
                    if let Some((pos, text)) = split_pos(rest) {
                        parsed.senses.push(Sense {
                            pos: pos.to_string(),
                            label: tag.to_string(),
                            text: text.to_string(),
                        });
                    } else if !rest.is_empty() {
                        parsed.senses.push(Sense {
                            pos: String::new(),
                            label: tag.to_string(),
                            text: rest.to_string(),
                        });
                    }
                }
            }
            continue;
        }

        match split_pos(line) {
            Some((pos, text)) if !text.is_empty() => parsed.senses.push(Sense {
                pos: pos.to_string(),
                label: String::new(),
                text: text.to_string(),
            }),
            _ => parsed.senses.push(Sense {
                pos: String::new(),
                label: String::new(),
                text: line.to_string(),
            }),
        }
    }

    parsed
}

/// 从释义里取词性，如 `["adj", "n"]`。
///
/// 词性按出现顺序去重；`[时态]`、`[医]` 这类行和元数据行都不产生词性。
/// 没有可识别前缀时返回空表，调用方据此决定还剩下什么可提示。
pub fn parts_of_speech(raw: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    for sense in parse_senses(raw).senses {
        if sense.pos.is_empty() {
            continue;
        }
        let Some((_, label)) = POS_TABLE.iter().find(|(tag, _)| *tag == sense.pos) else {
            continue;
        };
        let label: &'static str = label;
        if !found.contains(&label) {
            found.push(label);
        }
    }
    found
}#[cfg(test)]
mod tests {
    use super::*;

    const GO: &str = "-K5\nvi. 去, 走, 达到\n[时态] went, gone, going, goes\n(中高雅 35/44)";
    const INSPECT: &str = "-3\nvt. 检查, 检阅\n(高研四六托 5031/4286)";

    #[test]
    fn parses_collins_and_oxford() {
        assert_eq!(
            freq(GO),
            Some(Freq {
                stars: 5,
                oxford: true
            })
        );
        assert_eq!(
            freq(INSPECT),
            Some(Freq {
                stars: 3,
                oxford: false
            })
        );
        assert_eq!(freq("vt. 检查"), None);
        // Bare `-K` marks an Oxford word but carries no star count.
        assert_eq!(freq("-K\nvt. 检查"), None);
        assert_eq!(clean_translation("-K\nvt. 检查"), "vt. 检查");
    }

    #[test]
    fn drops_metadata_but_keeps_word_forms() {
        let cleaned = clean_translation(GO);
        assert!(!cleaned.contains("-K5"), "{cleaned}");
        assert!(!cleaned.contains("35/44"), "{cleaned}");
        // Word forms are useful to a learner, so they survive.
        assert!(cleaned.contains("[时态] went, gone, going, goes"), "{cleaned}");
        assert!(cleaned.contains("vi. 去, 走, 达到"), "{cleaned}");
    }

    #[test]
    fn leaves_plain_text_untouched() {
        let plain = "n. 逆境, 不幸\nvt. 放弃";
        assert_eq!(clean_translation(plain), plain);
        assert_eq!(clean_translation(""), "");
    }

    #[test]
    fn keeps_domain_tags() {
        // `[医] 儿童` is a real meaning, not metadata.
        assert_eq!(clean_translation("[医] 儿童"), "[医] 儿童");
    }

    #[test]
    fn rank_line_needs_numbers_on_both_sides() {
        assert!(is_rank_line("(中高雅 35/44)"));
        assert!(!is_rank_line("(见 also)"));
        assert!(!is_rank_line("35/44"));
    }

    #[test]
    fn stars_render_five_cells() {
        assert_eq!(stars(Freq { stars: 5, oxford: true }), "★★★★★");
        assert_eq!(
            stars(Freq {
                stars: 2,
                oxford: false
            }),
            "★★☆☆☆"
        );
    }

    #[test]
    fn reads_parts_of_speech_from_line_heads() {
        assert_eq!(parts_of_speech("a. 无希望的, 绝望的"), vec!["adj"]);
        // Source order is preserved: dictionaries list the dominant sense first.
        assert_eq!(parts_of_speech("n. 逆境\nvt. 放弃"), vec!["n", "vt"]);
        // `v.` must not be read as a prefix of `vt.` / `vi.`.
        assert_eq!(parts_of_speech("vt. 检查\nvi. 检查"), vec!["vt", "vi"]);
        // `adj.` must not be read as `a.`.
        assert_eq!(parts_of_speech("adj. 大的"), vec!["adj"]);
    }

    #[test]
    fn ignores_lines_that_are_not_pos_tagged() {
        // Word-form and domain lines carry no part of speech.
        assert!(parts_of_speech("[时态] went, gone").is_empty());
        assert!(parts_of_speech("[医] 儿童").is_empty());
        assert!(parts_of_speech("").is_empty());
        // Metadata rows are skipped too, since they do not start with a tag.
        assert!(parts_of_speech("-K5\n(中高雅 35/44)").is_empty());
    }

    #[test]
    fn parts_of_speech_are_deduplicated_and_ordered() {
        let kinds = parts_of_speech("vt. 做\nn. 事\nvt. 又做");
        assert_eq!(kinds, vec!["vt", "n"], "no duplicates, source order");
    }

    #[test]
    fn senses_split_pos_tags_from_text() {
        let parsed = parse_senses("-K1\nn. 银行, 堤, 岸\nvt. 存入银行\n(中高雅 663/406)");
        assert_eq!(parsed.senses.len(), 2);
        assert_eq!(parsed.senses[0].pos, "n.");
        assert_eq!(parsed.senses[0].text, "银行, 堤, 岸");
        assert!(!parsed.senses[0].is_secondary());
        assert_eq!(parsed.senses[1].pos, "vt.");
        assert!(parsed.forms.is_none());
    }

    #[test]
    fn domain_tagged_senses_are_secondary() {
        let parsed = parse_senses("n. 银行\n[医] 库\n[网络] 银行网站");
        assert_eq!(parsed.senses.len(), 3);
        assert_eq!(parsed.senses[1].label, "医");
        assert_eq!(parsed.senses[1].text, "库");
        assert!(parsed.senses[1].is_secondary());
        // A tag may also carry its own part of speech.
        let parsed = parse_senses("[口] vt. 糊弄");
        assert_eq!(parsed.senses[0].label, "口");
        assert_eq!(parsed.senses[0].pos, "vt.");
        assert_eq!(parsed.senses[0].text, "糊弄");
    }

    #[test]
    fn word_forms_are_not_senses() {
        let parsed = parse_senses("n. 银行\n[时态] banked, banking, banks");
        assert_eq!(parsed.senses.len(), 1);
        assert_eq!(
            parsed.forms.as_deref(),
            Some("[时态] banked, banking, banks")
        );
    }

    #[test]
    fn untagged_lines_survive_as_bare_senses() {
        let parsed = parse_senses("run的过去式和过去分词");
        assert_eq!(parsed.senses.len(), 1);
        assert_eq!(parsed.senses[0].pos, "");
        assert_eq!(parsed.senses[0].text, "run的过去式和过去分词");
    }

    #[test]
    fn empty_translation_has_no_senses() {
        assert!(parse_senses("").senses.is_empty());
        assert!(parse_senses("-K5\n(中高雅 35/44)").senses.is_empty());
    }
}
