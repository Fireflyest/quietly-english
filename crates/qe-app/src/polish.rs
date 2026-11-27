//! 总结时的「薄弱词加工」：把今天忘得多、且缺词根/例句的词交给模型补齐。
//!
//! 几条边界：
//!
//! * **只补缺失**——已有词根/例句（多带真实出处）绝不覆盖（落库侧还有
//!   `set_word_roots` 的 `roots = '[]'` 守卫和 `add_examples` 的去重）；
//! * **来源必须标注**——词根写 `WordRoot.source`、例句写 `Example.source`，
//!   统一为 `AI · <模型短名>`；卡片上词根的 `CodeBlock.source` 与例句下的
//!   `· AI · …` 小字都会把它显示出来；
//! * 模型偶尔不守规矩：输出只认**第一个 `[` 到最后一个 `]`** 的 JSON 数组，
//!   单词缺字段、kind 写错都尽量容错，两个字段全空的条目直接丢弃。

use anyhow::{Context, Result};
use serde::Deserialize;

use qe_core::model::{Example, RootKind, WordRoot};

use crate::translator;

/// 交给模型加工的一个词。
pub struct PolishWord {
    pub lemma: String,
    /// 中文释义，给模型当上下文。
    pub translation: String,
    /// 今天忘的次数（提示词里按它排序强调）。
    pub again: u32,
    pub need_roots: bool,
    pub need_examples: bool,
}

/// 一次加工的成果：线程里生成，主线程落库并出报告。
pub struct Polished {
    /// 实际出活的模型短名（`deepseek` / `mimo` / `gemini`），进来源标注。
    pub model: String,
    pub upgrades: Vec<Upgrade>,
}

/// 一个词补到的内容（roots/examples 至少一个非空）。
pub struct Upgrade {
    pub lemma: String,
    pub roots: Vec<WordRoot>,
    pub examples: Vec<Example>,
}

/// 模型要输出的 JSON 形状（原样贴进提示词，避免 format! 的花括号转义地狱）。
const SHAPE: &str = r#"[{"lemma":"inspect","roots":[{"part":"in-","kind":"prefix","gloss":"向内"},{"part":"spect","kind":"root","gloss":"看"}],"examples":[{"en":"The inspector came to inspect the site.","zh":"检查员来检查了现场。"}]}]"#;

/// 加工提示词：词表 + 严格 JSON 形状 + 规则。
pub fn prompt(words: &[PolishWord]) -> String {
    let mut list = String::new();
    for word in words {
        list.push_str(&format!(
            "- {}（{}）今天忘 {} 次；{}；{}\n",
            word.lemma,
            word.translation,
            word.again,
            if word.need_roots {
                "需要词根拆解"
            } else {
                "词根已有"
            },
            if word.need_examples {
                "需要例句"
            } else {
                "例句已有"
            },
        ));
    }
    format!(
        "你是英语词源专家和例句作者。为下面每个词生成内容。\
只输出一个 JSON 数组，不要代码围栏、不要任何解释文字，形状如下：\n{SHAPE}\n\
规则：\n\
1. kind 只能是 prefix、root、suffix；每部分给一句简短中文说明；\
标了「需要词根拆解」的才给 roots，否则给 []。\n\
2. 标了「需要例句」的才给 1-2 个常用义例句（英文简短自然 + 通顺中文），否则给 []。\n\
3. lemma 原样输出；每词都要出现。\n\n词表：\n{list}"
    )
}

/// 把模型输出解析成加工结果；`model` 是来源标注里的短名。
pub fn parse(raw: &str, model: &str) -> Result<Vec<Upgrade>> {
    let start = raw.find('[').context("批改结果里没有 JSON 数组")?;
    let end = raw
        .rfind(']')
        .filter(|end| *end > start)
        .context("JSON 数组没有闭合")?;
    let items: Vec<RawUpgrade> =
        serde_json::from_str(&raw[start..=end]).context("不是合法 JSON")?;

    let source = format!("AI · {model}");
    let mut upgrades = Vec::new();
    for item in items {
        let lemma = item.lemma.trim().to_string();
        if lemma.is_empty() {
            continue;
        }
        let roots: Vec<WordRoot> = item
            .roots
            .into_iter()
            .filter(|root| !root.part.trim().is_empty())
            .take(6)
            .map(|root| {
                WordRoot::new(root.part, root.gloss, RootKind::parse(&root.kind))
                    .with_source(source.clone())
            })
            .collect();
        let examples: Vec<Example> = item
            .examples
            .into_iter()
            .filter(|example| !example.en.trim().is_empty())
            .take(2)
            .map(|example| Example {
                source: Some(source.clone()),
                ..Example::new(example.en, example.zh)
            })
            .collect();
        if roots.is_empty() && examples.is_empty() {
            continue;
        }
        upgrades.push(Upgrade {
            lemma,
            roots,
            examples,
        });
    }
    Ok(upgrades)
}

/// 一次生成 → 加工结果（线程里调用）。
pub fn polish_from_generated(generated: translator::Generated) -> Result<Polished> {
    let model = generated.id.clone();
    let upgrades = parse(&generated.text, &model)?;
    Ok(Polished { model, upgrades })
}

#[derive(Deserialize)]
struct RawUpgrade {
    lemma: String,
    #[serde(default)]
    roots: Vec<RawRoot>,
    #[serde(default)]
    examples: Vec<RawExample>,
}

#[derive(Deserialize)]
struct RawRoot {
    part: String,
    #[serde(default)]
    gloss: String,
    #[serde(default)]
    kind: String,
}

#[derive(Deserialize)]
struct RawExample {
    en: String,
    #[serde(default)]
    zh: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(lemma: &str, need_roots: bool, need_examples: bool) -> PolishWord {
        PolishWord {
            lemma: lemma.to_string(),
            translation: "v. 检查".to_string(),
            again: 3,
            need_roots,
            need_examples,
        }
    }

    #[test]
    fn the_prompt_lists_each_word_and_its_missing_parts() {
        let prompt = prompt(&[word("inspect", true, false), word("reject", false, true)]);
        assert!(prompt.contains("- inspect（v. 检查）今天忘 3 次；需要词根拆解；例句已有"));
        assert!(prompt.contains("- reject（v. 检查）今天忘 3 次；词根已有；需要例句"));
        assert!(prompt.contains(r#""kind":"prefix""#), "JSON 形状要给出");
        assert!(prompt.contains("只输出一个 JSON 数组"));
    }

    #[test]
    fn a_clean_array_parses_and_attaches_the_source() {
        let raw = r#"[{"lemma":"inspect","roots":[{"part":"in-","kind":"prefix","gloss":"向内"}],"examples":[{"en":"They inspected it.","zh":"他们检查了它。"}]}]"#;
        let upgrades = parse(raw, "deepseek").unwrap();
        assert_eq!(upgrades.len(), 1);
        let upgrade = &upgrades[0];
        assert_eq!(upgrade.lemma, "inspect");
        assert_eq!(upgrade.roots.len(), 1);
        assert_eq!(
            upgrade.roots[0].source.as_deref(),
            Some("AI · deepseek"),
            "词根带来源"
        );
        assert_eq!(upgrade.roots[0].kind, RootKind::Prefix);
        assert_eq!(
            upgrade.examples[0].source.as_deref(),
            Some("AI · deepseek"),
            "例句带来源"
        );
        assert_eq!(upgrade.examples[0].zh.as_deref(), Some("他们检查了它。"));
    }

    #[test]
    fn fenced_and_sloppy_output_is_tolerated() {
        // 围栏、解释文字、kind 拼错、缺 gloss：都尽量容错。
        let raw = "好的：\n```json\n[{\"lemma\":\"reject\",\"roots\":[{\"part\":\"re-\",\"kind\":\"乱写\",\"gloss\":\"向后\"}],\"examples\":[]}]\n```\n以上。";
        let upgrades = parse(raw, "mimo").unwrap();
        assert_eq!(upgrades.len(), 1);
        assert_eq!(upgrades[0].roots[0].kind, RootKind::Root, "未知 kind 归为词根");
        assert_eq!(upgrades[0].roots[0].source.as_deref(), Some("AI · mimo"));
    }

    #[test]
    fn entries_with_nothing_to_add_are_dropped_and_missing_fields_default() {
        // 两个字段都空 / 缺 lemma：丢弃；缺 roots 字段默认空数组。
        let raw = r#"[{"lemma":"a","roots":[],"examples":[]},{"lemma":"","roots":[{"part":"x","kind":"root","gloss":"x"}]},{"lemma":"b","examples":[{"en":"B goes here."}]}]"#;
        let upgrades = parse(raw, "gemini").unwrap();
        assert_eq!(upgrades.len(), 1, "空条目和缺 lemma 的都被丢掉");
        assert_eq!(upgrades[0].lemma, "b");
        assert!(upgrades[0].roots.is_empty(), "缺 roots 字段默认空");
        assert_eq!(upgrades[0].examples.len(), 1);
    }

    #[test]
    fn no_array_at_all_is_an_error() {
        assert!(parse("模型今天不想输出数组", "deepseek").is_err());
        assert!(parse("", "deepseek").is_err());
    }
}
