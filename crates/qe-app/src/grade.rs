//! 翻译练习的 AI 批改：打分 + 中文反馈，结果写回提交记录。
//!
//! **模型无关**：这里只走 [`translator::ai_settings`] / [`translator::ai_generate`]
//! 这两个通用入口，不点名任何一家模型。选哪家由数据目录的 `models.json`
//! 决定——**按它的条目顺序取第一个配了 key 的**（默认 Gemini > 小米 MiMo >
//! DeepSeek），批改逻辑、命令行和库里存的 `provider` 字段都不用改。
//!
//! key 走 `/apikey set`（`keys.json`）或环境变量，和阅读机翻共用一处。
//! 结果落库后 `history` 直接能看到；不满意可以重新批改（`grade` 覆盖旧结果）。

use anyhow::{Context, Result};

use crate::translator;

/// 一次批改结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grade {
    /// 0..=100。
    pub score: u32,
    /// 中文短评：先说可取之处，再说主要问题和改法。
    pub feedback: String,
    /// 写进 `translation_attempts.provider` 的服务短名（`gemini` / `mimo`…）。
    pub provider: String,
}

/// 批改一次提交。
///
/// `source` 是题目原文，`reference` 是参考译文，`answer` 是学员的答案；
/// `direction` 是出题方向（`zh-en` / `en-zh`），决定提示词里说清考的是哪种表达。
pub fn grade(source: &str, reference: &str, direction: &str, answer: &str) -> Result<Grade> {
    anyhow::ensure!(!answer.trim().is_empty(), "翻译答案不能为空");
    let model = translator::ai_settings()?;
    let raw = translator::ai_generate(&model, &prompt(source, reference, direction, answer))?;
    let (score, feedback) = parse(&raw)?;
    // provider 跟着实际出分数的模型走：换模型后库里自然能看出是谁批的。
    Ok(Grade {
        score,
        feedback,
        provider: model.id,
    })
}

/// 出题方向的英文标签（进提示词）；没写就按默认的中译英。
fn direction_label(direction: &str) -> &'static str {
    if direction.eq_ignore_ascii_case("en-zh") {
        "English-to-Chinese"
    } else {
        "Chinese-to-English"
    }
}

/// 批改提示词。要点：
///
/// * **只回一个 JSON 对象**——评分是程序在解析，围栏和废话都会被剥掉，
///   但要求写死在最前面能大幅减少解析失败；
/// * **分值构成写明**（准确 50 / 通顺 30 / 语法完整 20），不同次批改才可比；
/// * 反馈用中文——这是给中国学习者看的，英文反馈没有意义。
fn prompt(source: &str, reference: &str, direction: &str, answer: &str) -> String {
    let pair = direction_label(direction);
    format!(
        "You grade one {pair} translation exercise done by an English learner in China. \
Reply with ONLY one JSON object — no markdown fences, no prose, exactly this shape:
{{\"score\": <integer 0-100>, \"feedback\": \"<1-3 sentences in Simplified Chinese: \
what is done well, then the main problems and how to fix them>\"}}

Scoring: accuracy of meaning 50 + fluency 30 + grammar and completeness 20.
Be strict but fair; the reference is one valid answer, not the only one.

SOURCE:
{source}

REFERENCE:
{reference}

ANSWER:
{answer}"
    )
}

/// 从模型输出里抠出批改结果，返回 `(分数, 反馈)`。
///
/// 模型偶尔会包一层 ```json 围栏或前后加一句话，所以只认**第一对花括号**
/// 起止的 JSON；分数容忍字符串和小数（四舍五入、夹到 0..=100），
/// 缺 `score` 或空 `feedback` 直接算失败——宁可报错让用户重跑，
/// 也不落一个没有依据的分数进库。
fn parse(raw: &str) -> Result<(u32, String)> {
    let start = raw.find('{').context("批改结果里没有 JSON 对象")?;
    let end = raw
        .rfind('}')
        .filter(|end| *end > start)
        .context("批改结果里的 JSON 没有闭合")?;
    let value: serde_json::Value =
        serde_json::from_str(&raw[start..=end]).context("批改结果不是合法 JSON")?;
    let score = value
        .get("score")
        .and_then(|score| score.as_f64().or_else(|| {
            // 模型有时给 `"score": "87"` 这种字符串。
            score.as_str().and_then(|text| text.parse::<f64>().ok())
        }))
        .context("批改结果缺少 score")?;
    let feedback = value
        .get("feedback")
        .and_then(|feedback| feedback.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    anyhow::ensure!(!feedback.is_empty(), "批改结果缺少 feedback");
    Ok((score.round().clamp(0.0, 100.0) as u32, feedback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_json_object_is_enough() {
        let (score, feedback) = parse(r#"{"score": 87, "feedback": "意思准确，时态更自然。"}"#).unwrap();
        assert_eq!(score, 87);
        assert_eq!(feedback, "意思准确，时态更自然。");
    }

    #[test]
    fn fenced_and_prose_wrapped_output_is_stripped() {
        let raw = "好的，批改如下：\n```json\n{\"score\": 90, \"feedback\": \"译文流畅。\"}\n```\n以上。";
        let (score, feedback) = parse(raw).unwrap();
        assert_eq!(score, 90);
        assert_eq!(feedback, "译文流畅。");
    }

    #[test]
    fn score_is_clamped_and_may_come_as_a_string() {
        assert_eq!(parse(r#"{"score": 150, "feedback": "a"}"#).unwrap().0, 100);
        assert_eq!(parse(r#"{"score": -3, "feedback": "a"}"#).unwrap().0, 0);
        assert_eq!(parse(r#"{"score": "87.6", "feedback": "a"}"#).unwrap().0, 88);
        assert_eq!(parse(r#"{"score": 87.4, "feedback": "a"}"#).unwrap().0, 87);
    }

    #[test]
    fn missing_score_or_feedback_is_an_error_not_a_guess() {
        // 没有依据的分数不能落库，宁可报错让用户重跑。
        assert!(parse(r#"{"feedback": "意思对"}"#).is_err());
        assert!(parse(r#"{"score": 50}"#).is_err());
        assert!(parse(r#"{"score": 50, "feedback": "  "}"#).is_err());
        assert!(parse("模型今天不想说话").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn the_prompt_carries_the_texts_and_the_direction() {
        let text = prompt("尽管下雨。", "Despite the rain.", "zh-en", "Even it rained.");
        assert!(text.contains("Chinese-to-English"));
        assert!(text.contains("尽管下雨。"));
        assert!(text.contains("Despite the rain."));
        assert!(text.contains("Even it rained."));
        // JSON 形状和分值构成必须原样传给模型。
        assert!(text.contains(r#"{"score":"#));
        assert!(text.contains("accuracy of meaning 50"));

        let reverse = prompt("s", "r", "en-zh", "a");
        assert!(reverse.contains("English-to-Chinese"));
    }

    #[test]
    fn an_empty_answer_is_refused_before_any_network_call() {
        let err = grade("s", "r", "zh-en", "   ").unwrap_err();
        assert!(format!("{err:#}").contains("翻译答案不能为空"));
    }
}
