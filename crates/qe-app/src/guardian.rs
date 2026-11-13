//! The Guardian「The long read」：英文长篇，一篇 4,000 词上下，**没有译文**。
//!
//! 新闻源一天几百词的短文读起来不过瘾，这里补上真正的长文：一篇长读顶得上
//! 十几条消息稿，读完再回头查标记的词，词义是在上下文里记住的。
//!
//! 几条刻意的取舍：
//!
//! * **直连可用**：不像 BBC 那样依赖代理，国内也能打开。
//! * **一次只抓一篇**：4,000 词够读好几天，抓多了只是把库塞满。
//! * **正文到 `</article>` 为止**：文末的播客/订阅推广不是正文，整段切掉。

use anyhow::Result;

use crate::html;
use crate::news::{Article, Source};

/// 系列页。一页只列两三篇，往回翻几页才凑得够候选。
const SERIES: &str = "https://www.theguardian.com/news/series/the-long-read";
const PAGES: usize = 3;
/// Guardian 的日期段一律是三个字母的月份。
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

pub const SOURCE: Source = Source {
    name: "Guardian Long Read",
    book: "Guardian Long Read",
    translated: false,
    limit: 1,
    listing,
    article,
};

fn listing() -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for page in 1..=PAGES {
        let url = if page == 1 {
            SERIES.to_string()
        } else {
            format!("{SERIES}?page={page}")
        };
        let html = match crate::fetch::get(&url) {
            Ok(html) => html,
            // 第一页打不开才算这个来源失败；往后翻页失败就用已经拿到的候选。
            Err(err) if page == 1 => return Err(err),
            Err(_) => break,
        };
        for link in article_links(&html) {
            if !out.contains(&link) {
                out.push(link);
            }
        }
    }
    anyhow::ensure!(!out.is_empty(), "系列页没有解析出文章链接");
    Ok(out)
}

fn article(url: &str) -> Result<Option<Article>> {
    let page = crate::fetch::get(url)?;
    let Some(title) = headline(&page) else {
        return Ok(None);
    };
    let region = html::region_after(&page, "article-body-commercial-selector", &["</article>"]);
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

/// 文末的推广段不是正文。
fn is_noise(text: &str) -> bool {
    const PREFIXES: [&str; 4] = [
        "Listen to our podcasts",
        "Sign up to",
        "This article was amended",
        "A longer version of this article",
    ];
    PREFIXES.iter().any(|prefix| text.starts_with(prefix))
}

/// 系列页里的文章链接，保持页面顺序（最新在前）并去重。
///
/// 只认 `/栏目/YYYY/mon/DD/slug` 这种形状：系列页里还有导航、作者页、图片页，
/// 靠形状一次筛掉，比维护黑名单稳。
pub fn article_links(page: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = page;
    while let Some(index) = rest.find("href=\"") {
        rest = &rest[index + "href=\"".len()..];
        let Some(end) = rest.find('"') else { break };
        let href = &rest[..end];
        rest = &rest[end..];
        let path = href
            .strip_prefix("https://www.theguardian.com")
            .unwrap_or(href);
        if !is_long_read_path(path) {
            continue;
        }
        let url = format!("https://www.theguardian.com{path}");
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

fn is_long_read_path(path: &str) -> bool {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    parts.len() == 5
        && !parts[0].is_empty()
        && parts[1].len() == 4
        && parts[1].chars().all(|ch| ch.is_ascii_digit())
        && MONTHS.contains(&parts[2])
        && parts[3].len() == 2
        && parts[3].chars().all(|ch| ch.is_ascii_digit())
        && !parts[4].is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<a href="/news/2026/nov/10/alien-menace-or-victim">x</a>
        <a href="https://www.theguardian.com/news/2026/oct/22/everyone-hates-oxford-street">y</a>
        <a href="/news/2026/nov/10/alien-menace-or-victim">again</a>
        <a href="/news/series/the-long-read">series</a>
        <a href="/profile/someone">author</a>"#;

    #[test]
    fn links_are_absolute_unique_and_ordered() {
        assert_eq!(
            article_links(PAGE),
            vec![
                "https://www.theguardian.com/news/2026/nov/10/alien-menace-or-victim".to_string(),
                "https://www.theguardian.com/news/2026/oct/22/everyone-hates-oxford-street".to_string(),
            ]
        );
    }

    #[test]
    fn a_path_without_a_full_date_is_not_an_article() {
        assert!(!is_long_read_path("/news/series/the-long-read"));
        assert!(!is_long_read_path("/news/2026/nov/10"));
        assert!(!is_long_read_path("/news/26/nov/10/slug"));
        assert!(!is_long_read_path("/news/2026/xxx/10/slug"));
        assert!(is_long_read_path("/news/2026/nov/10/slug"));
    }

    #[test]
    fn the_body_stops_before_the_promo_lines() {
        let page = r#"<h1>The headline</h1>
            <div class="article-body-commercial-selector article-body-viewer-selector">
              <p>First paragraph of the long read.</p>
              <p>Second paragraph of the long read.</p>
              <p>Listen to our podcasts here.</p>
            </div></article>
            <div class="after"><p>Related teaser.</p></div>"#;
        let region = html::region_after(page, "article-body-commercial-selector", &["</article>"]);
        let paragraphs: Vec<String> = html::paragraphs(region)
            .into_iter()
            .filter(|paragraph| !is_noise(paragraph))
            .collect();
        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs[0].starts_with("First paragraph"));
    }

    #[test]
    fn the_headline_is_read_from_the_h1() {
        assert_eq!(
            headline("<h1>Alien menace &amp; other stories</h1>").as_deref(),
            Some("Alien menace & other stories")
        );
        assert!(headline("<div>no heading</div>").is_none());
    }
}
