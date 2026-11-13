//! Aeon 长文随笔：英文 3,000–6,000 词，**没有译文**。
//!
//! 和 Guardian 长读互补：那边偏纪实，这边偏思想、科学、哲学，句子更绕，六级
//! 往上一点的读者正合适。免费、直连，不需要代理。
//!
//! 几条刻意的取舍：
//!
//! * **正文到附录卡片之前**：Aeon 的文章后面紧跟着标签、作者卡、相关文章，都用
//!   `<p>` 写着，不切干净就会被当成正文段落读进来。
//! * **一次只抓一篇**：一篇就够读好几天。
//! * **跳过列表页**：`/essays/popular` 这类栏目页形状和文章一样，靠黑名单挡掉。

use anyhow::Result;

use crate::html;
use crate::news::{Article, Source};

const LIST: &str = "https://aeon.co/essays";
const PREFIX: &str = "href=\"/essays/";
/// 这些不是文章，是栏目页。
const NOT_ARTICLES: [&str; 4] = ["popular", "latest", "all", "feed"];

pub const SOURCE: Source = Source {
    name: "Aeon Essays",
    book: "Aeon Essays",
    translated: false,
    limit: 1,
    listing,
    article,
};

fn listing() -> Result<Vec<String>> {
    let page = crate::fetch::get(LIST)?;
    let links = essay_links(&page);
    anyhow::ensure!(!links.is_empty(), "essay 列表页没有解析出文章链接");
    Ok(links)
}

fn article(url: &str) -> Result<Option<Article>> {
    let page = crate::fetch::get(url)?;
    let Some(title) = headline(&page) else {
        return Ok(None);
    };
    // 正文区从 `article-content` 开始，到附录卡片（标签 / 作者 / 相关文章）之前结束。
    let region = html::region_after(&page, "class=\"article-content", &["grid-area:appendix"]);
    let paragraphs: Vec<String> = html::paragraphs(region)
        .into_iter()
        .filter(|paragraph| !is_noise(paragraph))
        .collect();
    if paragraphs.is_empty() {
        return Ok(None);
    }
    Ok(Some(Article {
        title,
        body: paragraphs.join("\n\n"),
        translation: None,
    }))
}

fn headline(page: &str) -> Option<String> {
    html::element_inner(page, "h1")
        .map(|inner| html::clean_text(&inner))
        .filter(|title| !title.is_empty())
}

/// 「22 minute listen」这种时长标签、以及开头的音频提示都不是正文。
fn is_noise(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.ends_with("minute listen")
        || lower.ends_with("minute read")
        || lower.starts_with("listen to this")
}

/// 列表页里的文章链接，保持页面顺序（最新在前）并去重。
pub fn essay_links(page: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = page;
    while let Some(index) = rest.find(PREFIX) {
        rest = &rest[index + PREFIX.len()..];
        let Some(end) = rest.find('"') else { break };
        let slug = &rest[..end];
        rest = &rest[end..];
        // 带斜杠的是子页面（`/essays/xxx/yyy`），栏目名在黑名单里，都不是文章。
        if slug.is_empty() || slug.contains('/') || NOT_ARTICLES.contains(&slug) {
            continue;
        }
        let url = format!("https://aeon.co/essays/{slug}");
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<a href="/essays/popular">popular</a>
        <a href="/essays/a-new-answer-to-why-there-is-something">x</a>
        <a href="/essays/a-new-answer-to-why-there-is-something">again</a>
        <a href="/essays/philosophy/metaphysics">topic</a>"#;

    #[test]
    fn column_pages_and_topics_are_not_essays() {
        assert_eq!(
            essay_links(PAGE),
            vec!["https://aeon.co/essays/a-new-answer-to-why-there-is-something".to_string()]
        );
    }

    #[test]
    fn the_body_stops_before_the_appendix() {
        let page = r#"<h1>Why anything exists</h1>
            <div class="article-content"><p>22 minute listen</p>
              <p>Suppose there is something rather than nothing.</p>
              <p>That needs explaining.</p>
            </div>
            <div class="flex [grid-area:appendix]"><p>Andrew T Brenner</p>
              <p>13 November 2026</p></div>"#;
        let region = html::region_after(page, "class=\"article-content", &["grid-area:appendix"]);
        let paragraphs: Vec<String> = html::paragraphs(region)
            .into_iter()
            .filter(|paragraph| !is_noise(paragraph))
            .collect();
        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs[1].starts_with("That needs explaining"));
    }

    #[test]
    fn duration_chips_are_noise() {
        assert!(is_noise("22 minute listen"));
        assert!(is_noise("Listen to this essay"));
        assert!(!is_noise("A paragraph of the actual essay."));
    }
}
