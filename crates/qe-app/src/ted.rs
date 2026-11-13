//! TED Talks：英文原文 + 官方中文译文，逐句对齐。
//!
//! 用法上它更像「库」而不是「新闻」：TED 的 `/talks` 列表固定返回最新 24 条，
//! 而新演讲往往还没有字幕；主题页（`/topics/<主题>`）里的演讲带译文的概率高得多。
//! 所以每天按日期轮换几个主题，把没读过的抓进来——读得久了自然铺满整个库。
//!
//! 对齐方式：中英文各取一次 transcript，**逐句（cue）对齐**。两边的 cue 数必须
//! 一致，段落边界只按英文侧算一次，再用同一组边界切中文，段落数因此必然相等。

use anyhow::{Context, Result};
use chrono::Datelike;
use serde_json::Value;

use crate::html;
use crate::news::{Article, Source};

pub const SOURCE: Source = Source {
    name: "TED Talks",
    book: "TED Talks",
    translated: true,
    limit: 3,
    listing,
    article,
};

/// 轮换的主题。都是演讲多、译文全的常青话题。
const TOPICS: &[&str] = &[
    "climate",
    "science",
    "psychology",
    "business",
    "education",
    "health",
    "technology",
    "culture",
    "design",
    "environment",
    "creativity",
    "language",
    "innovation",
];
/// 每次看几个主题页：一页 16 条，几页就够凑出当天要的几篇。
const TOPICS_PER_RUN: usize = 4;
/// 一段至少这么长才断开：TED 的 cue 只有一句，不合并会碎成十几行。
const MIN_PARAGRAPH_CHARS: usize = 220;

fn listing() -> Result<Vec<String>> {
    let offset = qe_core::time::today().ordinal() as usize;
    let mut out: Vec<String> = Vec::new();
    for step in 0..TOPICS_PER_RUN {
        let topic = TOPICS[(offset + step) % TOPICS.len()];
        // 一个主题页打不开不该让整个来源失败：跳过它继续下一个。
        if let Ok(urls) = topic_talks(topic) {
            for url in urls {
                if !out.contains(&url) {
                    out.push(url);
                }
            }
        }
    }
    anyhow::ensure!(!out.is_empty(), "TED 主题页都没能打开");
    Ok(out)
}

fn topic_talks(topic: &str) -> Result<Vec<String>> {
    let page = crate::fetch::get(&format!("https://www.ted.com/topics/{topic}"))?;
    let data = next_data(&page).context("主题页里没有 __NEXT_DATA__")?;
    let talks = data
        .pointer("/props/pageProps/talks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(talks
        .iter()
        .filter_map(|talk| talk.get("slug").and_then(Value::as_str))
        .map(|slug| format!("https://www.ted.com/talks/{slug}"))
        .collect())
}

fn article(url: &str) -> Result<Option<Article>> {
    // 先问中文：没有译文就省掉英文那一趟请求。
    let Some(chinese) = transcript(url, "zh-cn")? else {
        return Ok(None);
    };
    if chinese.language != "zh-cn" || chinese.cues.is_empty() {
        return Ok(None);
    }
    let Some(english) = transcript(url, "en")? else {
        return Ok(None);
    };
    // cue 数不一致就没法逐句对齐：宁可跳过，也不能让译文串段。
    if english.cues.len() != chinese.cues.len() {
        return Ok(None);
    }
    let bounds = boundaries(&english.cues);
    let title = english.title;
    let body_lines = join_at(&english.cues, &bounds, false);
    let translation_lines = join_at(&chinese.cues, &bounds, true);
    if body_lines.is_empty() || body_lines.len() != translation_lines.len() {
        return Ok(None);
    }
    Ok(Some(Article {
        title,
        body: body_lines.join("\n\n"),
        translation: Some(translation_lines.join("\n\n")),
    }))
}

/// 一次 transcript：语言、标题、逐句文本。
struct Transcript {
    language: String,
    title: String,
    cues: Vec<String>,
}

fn transcript(url: &str, language: &str) -> Result<Option<Transcript>> {
    let page = crate::fetch::get(&format!("{url}?language={language}"))?;
    let Some(data) = next_data(&page) else {
        return Ok(None);
    };
    let Some(translation) = data.pointer("/props/pageProps/transcriptData/translation") else {
        // 新演讲常常还没上字幕，`transcriptData` 是 `null`。
        return Ok(None);
    };
    let language_code = translation
        .pointer("/language/internalLanguageCode")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cues: Vec<String> = translation
        .pointer("/paragraphs")
        .and_then(Value::as_array)
        .map(|paragraphs| {
            paragraphs
                .iter()
                .filter_map(|paragraph| paragraph.get("cues").and_then(Value::as_array))
                .flatten()
                .filter_map(|cue| cue.get("text").and_then(Value::as_str))
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if cues.is_empty() {
        return Ok(None);
    }
    Ok(Some(Transcript {
        language: language_code,
        title: talk_title(&data),
        cues,
    }))
}

/// 标题取英文侧的 `videoData.title`（中文侧取到的是译名）。
fn talk_title(data: &Value) -> String {
    let video = data.pointer("/props/pageProps/videoData");
    let title = video
        .and_then(|video| video.get("title"))
        .and_then(Value::as_str)
        .map(html::clean_text)
        .unwrap_or_default();
    // 年份让同名的演讲不至于撞上 `texts.title` 的唯一约束，也方便按年份翻。
    let year = video
        .and_then(|video| video.get("recordedOn"))
        .and_then(Value::as_str)
        .and_then(|date| date.get(..4))
        .filter(|year| year.chars().all(|ch| ch.is_ascii_digit()));
    match (title.is_empty(), year) {
        (true, _) => "TED talk".to_string(),
        (false, Some(year)) => format!("{title} ({year})"),
        (false, None) => title,
    }
}

/// 段落起点：累计到一句话结束且长度够了就断开。
fn boundaries(cues: &[String]) -> Vec<usize> {
    let mut out = vec![0];
    let mut length = 0usize;
    for (index, cue) in cues.iter().enumerate() {
        length += cue.chars().count() + 1;
        let ends = cue
            .trim_end()
            .ends_with(['.', '!', '?', ':', '。', '！', '？', '：']);
        if ends && length >= MIN_PARAGRAPH_CHARS && index + 1 < cues.len() {
            out.push(index + 1);
            length = 0;
        }
    }
    out
}

fn join_at(cues: &[String], bounds: &[usize], chinese: bool) -> Vec<String> {
    // 中文逐句拼接时不加空格，否则会读成「一句 一句」。
    let separator = if chinese { "" } else { " " };
    let mut lines = Vec::new();
    for (index, start) in bounds.iter().enumerate() {
        let end = bounds.get(index + 1).copied().unwrap_or(cues.len());
        let text = cues[*start..end].join(separator);
        let text = text.trim().to_string();
        if !text.is_empty() {
            lines.push(text);
        }
    }
    lines
}

/// 页面里的 `__NEXT_DATA__` JSON。
fn next_data(page: &str) -> Option<Value> {
    let start = page.find("id=\"__NEXT_DATA__\"")?;
    let open = page[start..].find('>')? + start + 1;
    let end = page[open..].find("</script>")? + open;
    serde_json::from_str(&page[open..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_break_at_sentence_ends_once_long_enough() {
        let cues: Vec<String> = vec![
            "Short one.".to_string(),
            "Another short sentence.".to_string(),
            "And a third one here.".to_string(),
            "Fourth sentence arrives.".to_string(),
        ];
        // 前两句加起来还没到 220 字，所以整段只有一处边界。
        assert_eq!(boundaries(&cues), vec![0]);
    }

    #[test]
    fn long_runs_are_split_into_several_paragraphs() {
        let sentence = "x".repeat(MIN_PARAGRAPH_CHARS) + ".";
        let cues: Vec<String> = vec![sentence.clone(), sentence, "tail".to_string()];
        assert_eq!(boundaries(&cues), vec![0, 1, 2], "one paragraph per long sentence");
    }

    #[test]
    fn the_same_boundaries_split_both_languages() {
        let cues: Vec<String> = (0..4).map(|i| format!("Sentence number {i}.")).collect();
        let bounds = vec![0, 2];
        let english = join_at(&cues, &bounds, false);
        let chinese = join_at(&cues, &bounds, true);
        assert_eq!(english.len(), chinese.len());
        assert_eq!(english[1], "Sentence number 2. Sentence number 3.");
    }

    #[test]
    fn chinese_cues_are_joined_without_spaces() {
        let cues: Vec<String> = vec!["第一句。".to_string(), "第二句。".to_string()];
        assert_eq!(join_at(&cues, &[0], true)[0], "第一句。第二句。");
    }

    #[test]
    fn the_title_prefers_the_english_side_and_keeps_the_year() {
        let data: Value = serde_json::from_str(
            r#"{"props":{"pageProps":{"videoData":{"title":"A talk","recordedOn":"2015-03-18"}}}}"#,
        )
        .unwrap();
        assert_eq!(talk_title(&data), "A talk (2015)");
    }

    #[test]
    fn a_talk_without_a_recording_date_keeps_its_plain_title() {
        let data: Value =
            serde_json::from_str(r#"{"props":{"pageProps":{"videoData":{"title":"A talk"}}}}"#)
                .unwrap();
        assert_eq!(talk_title(&data), "A talk");
    }
}
