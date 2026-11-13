//! 新华网英文版（english.news.cn）：英文原文，**没有译文**。
//!
//! 直连就能打开，每天更新几十条，六级水平读起来正好。没有中文对照，所以这本书
//! 在抓取结果里标成 `en only`——读的时候靠词条查，不靠整段译文。

use anyhow::Result;

use crate::html;
use crate::news::{Article, Source};

const HOME: &str = "https://english.news.cn/";
/// 文章 URL 形如 `https://english.news.cn/20261113/<32 位十六进制>/c.html`。
const SUFFIX: &str = "/c.html";
const ID_LEN: usize = 32;
const DATE_LEN: usize = 8;

pub const SOURCE: Source = Source {
    name: "Xinhua English",
    book: "Xinhua English",
    translated: false,
    limit: 3,
    listing,
    article,
};

fn listing() -> Result<Vec<String>> {
    let page = crate::fetch::get(HOME)?;
    let links = article_links(&page);
    anyhow::ensure!(!links.is_empty(), "首页没有解析出文章链接");
    Ok(links)
}

fn article(url: &str) -> Result<Option<Article>> {
    let page = crate::fetch::get(url)?;
    let Some(title) = html::element_inner(&page, "h1")
        .map(|inner| html::clean_text(&inner))
        .filter(|title| !title.is_empty())
    else {
        return Ok(None);
    };
    // 正文在 `id="detail"` 里；脚本和评论之前就结束。
    let region = html::region_after(&page, "id=\"detail\"", &["<script", "class=\"comment"]);
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

/// 署名行不是正文。
fn is_noise(text: &str) -> bool {
    const PREFIXES: [&str; 4] = ["Editor:", "责任编辑", "编辑:", "Source:"];
    PREFIXES.iter().any(|prefix| text.starts_with(prefix))
}

/// 首页里所有 `/YYYYMMDD/<32 hex>/c.html` 形式的文章链接，保持顺序并去重。
pub fn article_links(page: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut from = 0usize;
    while let Some(found) = page[from..].find(SUFFIX) {
        let end = from + found + SUFFIX.len();
        if let Some(url) = link_ending_at(page, end) {
            if !out.contains(&url) {
                out.push(url);
            }
        }
        from = end;
    }
    out
}

/// `/c.html` 结束处往回认 `日期/id/c.html`，认不出就返回 `None`。
fn link_ending_at(page: &str, end: usize) -> Option<String> {
    let id_end = end.checked_sub(SUFFIX.len())?;
    let id_start = id_end.checked_sub(ID_LEN)?;
    let id = page.get(id_start..id_end)?;
    if !id.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let slash = id_start.checked_sub(1)?;
    if page.get(slash..id_start)? != "/" {
        return None;
    }
    let date_start = slash.checked_sub(DATE_LEN)?;
    let date = page.get(date_start..slash)?;
    if !date.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(format!("https://english.news.cn/{date}/{id}/c.html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<a href="https://english.news.cn/20261113/8f76cba7542b499fa9870905053f9a16/c.html">x</a>
        <a href="/20261112/4b1ffb86270047c98bafd079e54d9303/c.html">y</a>
        <a href="https://english.news.cn/20261113/8f76cba7542b499fa9870905053f9a16/c.html">again</a>
        <a href="/photos/index.htm">no</a>"#;

    #[test]
    fn article_links_are_absolute_unique_and_ordered() {
        assert_eq!(
            article_links(PAGE),
            vec![
                "https://english.news.cn/20261113/8f76cba7542b499fa9870905053f9a16/c.html".to_string(),
                "https://english.news.cn/20261112/4b1ffb86270047c98bafd079e54d9303/c.html".to_string(),
            ]
        );
    }

    #[test]
    fn something_that_is_not_a_dated_id_is_ignored() {
        assert!(article_links("/c.html").is_empty());
        assert!(article_links("/2026/abc/c.html").is_empty());
    }

    #[test]
    fn the_body_drops_the_credit_line() {
        let page = r#"<h1>China injects impetus</h1>
            <div id="detail">
              <p>BEIJING, Nov. 13 (Xinhua) -- First paragraph.</p>
              <p>Second paragraph.</p>
              <p>Editor: Zhang San</p>
            </div>
            <script>var x = 1;</script>"#;
        let region = html::region_after(page, "id=\"detail\"", &["<script", "class=\"comment"]);
        let paragraphs: Vec<String> = html::paragraphs(region)
            .into_iter()
            .filter(|paragraph| !is_noise(paragraph))
            .collect();
        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs[0].ends_with("First paragraph."));
    }
}
