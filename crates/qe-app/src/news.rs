//! 每天首次启动时，从几个固定来源各抓一批阅读材料。
//!
//! 每个来源只负责两件事：**列出候选 URL**、**把一篇 URL 变成 `Article`**。去重、
//! 限量、写库都在这里统一做，所以加一个来源只需要写一个很薄的模块。
//!
//! 几条刻意的取舍：
//!
//! * **一个来源挂了不影响别人**：TED 打不开也得把新华网抓回来，出错的来源只记一行提示。
//! * **只在本地留数据**：抓到的东西写进用户自己的库，不写仓库；来源与授权写在文件头。
//! * **尽量不依赖代理**：默认直连。`HTTPS_PROXY` 配着才走代理（BBC 在国内多半需要）。
//! * **对不齐就跳过**：宁可少一篇，也不能让 `translation_at(i)` 指到别的段落。

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use qe_core::Store;
use qe_store::SqliteStore;

/// 所有来源，按抓取顺序。
pub const SOURCES: &[Source] = &[
    crate::ted::SOURCE,
    crate::bbc::SOURCE,
    crate::xinhua::SOURCE,
    crate::guardian::SOURCE,
    crate::aeon::SOURCE,
];

/// 一篇抓回来的文章。
pub struct Article {
    pub title: String,
    pub body: String,
    /// 英文原文没有译文时为 `None`，此时 `/translate` 什么也不显示。
    pub translation: Option<String>,
}

/// 一个抓取源。
pub struct Source {
    /// 显示名，用在抓取结果里。
    pub name: &'static str,
    /// 归到哪本阅读书；同时也是书名的 slug 来源。
    pub book: &'static str,
    /// 这个源是否自带译文（决定结果里写 `en + 中文` 还是 `en only`）。
    pub translated: bool,
    /// 每次最多抓几篇新的。
    pub limit: usize,
    /// 列出候选 URL，最新的在前。
    pub listing: fn() -> Result<Vec<String>>,
    /// 抓一篇；解析不出正文时返回 `Ok(None)`。
    pub article: fn(&str) -> Result<Option<Article>>,
}

/// 一个来源这一次的结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SourceReport {
    pub name: String,
    pub book: String,
    pub translated: bool,
    pub saved: u32,
    pub skipped: u32,
    pub failed: u32,
    /// 这个源整体失败时的原因（列表页都打不开）。
    pub error: Option<String>,
}

impl SourceReport {
    /// 结果里的一行说明，例如 `3 · en + 中文` 或 `skipped 4 · 1 failed`。
    pub fn detail(&self) -> String {
        if let Some(error) = &self.error {
            return format!("unavailable · {error}");
        }
        let mut parts = vec![format!("{} new", self.saved)];
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        let kind = if self.translated { "en + 中文" } else { "en only" };
        format!("{} · {kind}", parts.join(" · "))
    }
}

/// 一次抓取的结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FetchReport {
    pub sources: Vec<SourceReport>,
}

impl FetchReport {
    pub fn saved(&self) -> u32 {
        self.sources.iter().map(|source| source.saved).sum()
    }

    pub fn failed(&self) -> u32 {
        self.sources.iter().map(|source| source.failed).sum()
    }

    /// 一行总览：`7 new · 3 skipped`。
    pub fn summary(&self) -> String {
        let skipped: u32 = self.sources.iter().map(|source| source.skipped).sum();
        let mut out = format!("{} new", self.saved());
        if skipped > 0 {
            out.push_str(&format!(" · {skipped} skipped"));
        }
        let failed = self.failed();
        if failed > 0 {
            out.push_str(&format!(" · {failed} failed"));
        }
        out
    }

    /// 有内容（或值得说一句）的来源，供结果块逐行显示。
    pub fn lines(&self) -> Vec<(String, String)> {
        self.sources
            .iter()
            .filter(|source| source.saved > 0 || source.failed > 0 || source.error.is_some())
            .map(|source| (source.name.clone(), source.detail()))
            .collect()
    }
}

/// 从所有来源抓一遍。单个来源出错不中断其它来源。
pub fn fetch_all(db: &Path) -> Result<FetchReport> {
    let mut store =
        SqliteStore::open_at(db).with_context(|| format!("无法打开 {}", db.display()))?;
    // 已经导过的就不再抓：`source` 存的是原文链接，正好当去重键。
    let known: HashSet<String> = store
        .texts()?
        .into_iter()
        .filter_map(|text| text.source)
        .collect();

    let mut report = FetchReport::default();
    for source in SOURCES {
        report
            .sources
            .push(fetch_one_source(&mut store, source, &known));
    }
    Ok(report)
}

fn fetch_one_source(
    store: &mut SqliteStore,
    source: &Source,
    known: &HashSet<String>,
) -> SourceReport {
    let mut out = SourceReport {
        name: source.name.to_string(),
        book: source.book.to_string(),
        translated: source.translated,
        ..SourceReport::default()
    };
    let listing = match (source.listing)() {
        Ok(listing) => listing,
        Err(err) => {
            out.error = Some(first_line(&format!("{err:#}")));
            return out;
        }
    };
    for url in listing {
        if out.saved as usize >= source.limit {
            break;
        }
        if known.contains(&url) {
            out.skipped += 1;
            continue;
        }
        match (source.article)(&url) {
            Ok(Some(article)) => {
                let mut text = qe_core::reading::Text::new(article.title, article.body);
                text.translation = article.translation.unwrap_or_default();
                text.book = Some(source.book.to_string());
                text.source = Some(url.clone());
                text.license = Some(LICENSE.to_string());
                match store.save_text(&text) {
                    Ok(_) => out.saved += 1,
                    // 标题是 UNIQUE：同名的另一篇已经在库里时这里会失败。
                    Err(_) => out.failed += 1,
                }
            }
            Ok(None) => out.skipped += 1,
            Err(_) => out.failed += 1,
        }
        // 对站点客气一点，也避免启动时把网络打满。
        std::thread::sleep(Duration::from_millis(250));
    }
    out
}

const LICENSE: &str = "personal study only · do not redistribute";

/// 错误信息常常好几行，界面上只留第一行。
fn first_line(message: &str) -> String {
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(message)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(sources: Vec<SourceReport>) -> FetchReport {
        FetchReport { sources }
    }

    fn source(name: &str, saved: u32, skipped: u32, failed: u32) -> SourceReport {
        SourceReport {
            name: name.to_string(),
            book: name.to_string(),
            translated: true,
            saved,
            skipped,
            failed,
            error: None,
        }
    }

    #[test]
    fn the_summary_adds_up_across_sources() {
        let report = report(vec![source("TED", 3, 1, 0), source("Xinhua", 2, 0, 1)]);
        assert_eq!(report.saved(), 5);
        assert_eq!(report.summary(), "5 new · 1 skipped · 1 failed");
    }

    #[test]
    fn a_source_with_nothing_to_say_is_left_out() {
        let report = report(vec![source("TED", 0, 4, 0), source("Xinhua", 2, 0, 0)]);
        let lines = report.lines();
        assert_eq!(lines.len(), 1, "only the source that saved something is shown");
        assert_eq!(lines[0].0, "Xinhua");
    }

    #[test]
    fn an_unavailable_source_is_reported_not_hidden() {
        let mut bbc = source("BBC", 0, 0, 0);
        bbc.error = Some("抓取失败：timeout".to_string());
        let lines = report(vec![bbc]).lines();
        assert_eq!(lines.len(), 1, "an unavailable source still says so");
        assert!(lines[0].1.starts_with("unavailable"));
    }

    #[test]
    fn the_detail_says_whether_there_is_a_translation() {
        let mut translated = source("TED", 2, 0, 0);
        translated.translated = true;
        assert!(translated.detail().contains("en + 中文"));

        let mut english_only = source("Xinhua", 2, 0, 0);
        english_only.translated = false;
        assert!(english_only.detail().contains("en only"));
    }

    #[test]
    fn only_the_first_line_of_an_error_survives() {
        assert_eq!(first_line("boom\n caused by: x"), "boom");
        assert_eq!(first_line("\n\nboom"), "boom");
    }
}
