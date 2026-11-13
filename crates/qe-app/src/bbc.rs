//! BBC Learning English 中文「媒体英语」：英文一段、紧跟中文译文一段。
//!
//! 这个源在国内多半**需要代理**才能打开（`HTTPS_PROXY` 配着才会走代理，没配就
//! 自动跳过，不拖累别的来源）。
//!
//! 几条刻意的取舍：
//!
//! * **只做相邻配对**：只认「英文段紧跟中文段」这一种关系，配对不上就丢掉那段。
//!   宁可少一段，也不能让 `translation_at(i)` 指到别的段落。
//! * **练习区和导航行整段切掉**：`一、听第一遍音频` 之后全是题目和答案，收进来
//!   只会污染正文。

use anyhow::Result;

use crate::html::{clean_text, is_chinese, paragraphs};
use crate::news::{Article, Source};

/// 「媒体英语」栏目。每条 `ep-YYMMDD` 是一期，每周更新。
const FEATURE_URL: &str = "https://www.bbc.co.uk/learningenglish/chinese/features/media-english";
const LINK_NEEDLE: &str = "/learningenglish/chinese/features/media-english/ep-";

pub const SOURCE: Source = Source {
    name: "BBC Media English",
    book: "BBC Media English",
    translated: true,
    limit: 3,
    listing,
    article,
};

fn listing() -> Result<Vec<String>> {
    let page = crate::fetch::get(FEATURE_URL)?;
    let links = episode_links(&page);
    anyhow::ensure!(!links.is_empty(), "栏目页没有解析出文章链接");
    Ok(links)
}

/// 抓一期并解析；解析不出对照正文时返回 `Ok(None)`。
fn article(url: &str) -> Result<Option<Article>> {
    let raw = crate::fetch::get(url)?;
    let paired = pairs(&paragraphs(&raw));
    if paired.is_empty() {
        return Ok(None);
    }
    let (english, chinese): (Vec<String>, Vec<String>) = paired.into_iter().unzip();
    Ok(Some(Article {
        title: episode_title(&raw, url),
        body: english.join("\n\n"),
        translation: Some(chinese.join("\n\n")),
    }))
}

/// 栏目页里的每一期链接，保持页面顺序（最新在前）并去重。
pub fn episode_links(html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(index) = rest.find(LINK_NEEDLE) {
        rest = &rest[index..];
        // 链接后面直接跟 `"` 或 `'`，紧跟的那串数字就是期号。
        let digits: String = rest[LINK_NEEDLE.len()..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            let url = format!("https://www.bbc.co.uk{LINK_NEEDLE}{digits}");
            if !out.contains(&url) {
                out.push(url);
            }
        }
        rest = &rest[LINK_NEEDLE.len() + digits.len().max(1)..];
    }
    out
}

/// 正文里「英文段 + 紧跟的中文段」的配对。
///
/// 切掉导航行和练习区之后，只保留相邻的（英文, 中文）组合：某一段缺译文时，
/// 它的英文段直接丢掉，不会把后面的中文错配给它。
pub fn pairs(paragraphs: &[String]) -> Vec<(String, String)> {
    let mut cutting = false;
    let mut body: Vec<&String> = Vec::new();
    for paragraph in paragraphs {
        if is_noise(paragraph) {
            continue;
        }
        // `一、听第一遍音频，回答问题。` 之后是题目与答案，正文到此为止。
        if paragraph.contains("听第一遍音频") || paragraph.starts_with("一、") {
            cutting = true;
        }
        if cutting {
            continue;
        }
        body.push(paragraph);
    }
    body.windows(2)
        .filter(|pair| !is_chinese(pair[0]) && is_chinese(pair[1]))
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect()
}

/// 导航、脚本、音频说明这些不是正文。
fn is_noise(text: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "Accessibility links",
        "Skip to content",
        "require(",
        "本节目音频首次发布",
        "关于台词的备注",
    ];
    MARKERS.iter().any(|marker| text.contains(marker))
}

/// 标题取 `<title>` 里 `媒体英语 / ` 之后的部分：英文标题 + 中文标题，正好。
pub fn episode_title(html: &str, url: &str) -> String {
    if let Some(inner) = crate::html::element_inner(html, "title") {
        let title = clean_text(&inner);
        if let Some((_, rest)) = title.split_once("媒体英语 / ") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    url.rsplit('/').next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn episode_links_keep_page_order_without_duplicates() {
        let html = r#"
            <a href="/learningenglish/chinese/features/media-english/ep-261109">x</a>
            <a href="/learningenglish/chinese/features/media-english/ep-261102">y</a>
            <a href="/learningenglish/chinese/features/media-english/ep-261109">again</a>
            <a href="/learningenglish/chinese/features/take-away-english/ep-261109">other</a>
        "#;
        assert_eq!(
            episode_links(html),
            vec![
                "https://www.bbc.co.uk/learningenglish/chinese/features/media-english/ep-261109"
                    .to_string(),
                "https://www.bbc.co.uk/learningenglish/chinese/features/media-english/ep-261102"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn pairs_take_english_followed_by_chinese_only() {
        let body = list(&[
            "考古学家称，在土耳其沿海发现了目前已知最古老的沉船残骸。",
            "The ancient wreck was found off the coast of Kekova Island.",
            "这艘古代沉船位于凯考瓦岛附近海域。",
            "Divers discovered three large stone anchors.",
            "潜水员发现了三个大型石锚。",
        ]);
        let pairs = pairs(&body);
        assert_eq!(pairs.len(), 2, "the leading summary has no English twin");
        assert_eq!(pairs[0].0, "The ancient wreck was found off the coast of Kekova Island.");
        assert_eq!(pairs[0].1, "这艘古代沉船位于凯考瓦岛附近海域。");
        assert_eq!(pairs[1].1, "潜水员发现了三个大型石锚。");
    }

    #[test]
    fn an_english_paragraph_without_translation_is_dropped() {
        let body = list(&[
            "The ancient wreck was found off the coast of an island.",
            "Divers discovered three large stone anchors.",
            "潜水员发现了三个大型石锚。",
        ]);
        let pairs = pairs(&body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "Divers discovered three large stone anchors.");
    }

    #[test]
    fn the_quiz_section_is_cut_off() {
        let body = list(&[
            "The ancient wreck was found off the coast of an island.",
            "这艘古代沉船位于凯考瓦岛附近海域。",
            "一、听第一遍音频，回答问题。",
            "1. According to the report, what do the objects tell us?",
            "These artefacts tell us about how old the shipwreck could be.",
            "答：这些文物告诉我们沉船有多古老。",
        ]);
        let pairs = pairs(&body);
        assert_eq!(pairs.len(), 1, "quiz and answers are not part of the article");
    }

    #[test]
    fn navigation_noise_is_ignored() {
        let body = list(&[
            "Homepage Accessibility links Skip to content require([\"idcta\"]);",
            "（本节目音频首次发布于 2026 年 10 月 22 日）",
            "The ancient wreck was found off the coast of an island.",
            "这艘古代沉船位于凯考瓦岛附近海域。",
        ]);
        assert_eq!(pairs(&body).len(), 1);
    }

    #[test]
    fn the_title_drops_the_site_prefix() {
        let html = "<title>BBC Learning English - 媒体英语 / UK City of Culture 2029: Who are the contenders? 2029 年英国 “文化之城”：哪些城市参与角逐？</title>";
        assert_eq!(
            episode_title(html, "https://x/ep-261109"),
            "UK City of Culture 2029: Who are the contenders? 2029 年英国 “文化之城”：哪些城市参与角逐？"
        );
    }

    #[test]
    fn the_title_falls_back_to_the_episode_slug() {
        assert_eq!(
            episode_title("<html></html>", "https://x/features/media-english/ep-261109"),
            "ep-261109"
        );
    }
}
