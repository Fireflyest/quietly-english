//! 翻译练习的来源与合集：**网页导入**、**每日自动发现**、**合集统计**。
//!
//! 三个入口共用一条出题流水（`create_exercises`）：
//!
//! * **网页导入**（`/practice <url>` / `translate-practice from`）：
//!   抓中文文章页按 `<p>` 拆段，每段一题；合集 = 这篇文章（名字取页面标题）。
//! * **每日自动**（`translate-practice daily` / 启动时）：按
//!   `practice_sources.json` 里配的**列表页**找新文章，导过的 URL 记在
//!   `practice_articles` 里直接跳过；每个来源对应一个合集。
//! * **手动**（`translate-practice create`）：都进「手动出题」合集。
//!
//! 题目 = 网页里的一段中文正文；参考译文没有现成的，由 `models.json` 里
//! 选中的模型逐段生成（`source_type` 记成 `web:<url>`，方便追溯）。
//!
//! 几条取舍：
//!
//! * **先验 key 再抓网页**：没有模型 key 就出不了参考译文，别等抓完才发现。
//! * **按原文去重**：同一个页面重复导入不会长出重复题。
//! * **限量**：一次最多 [`LIMIT`] 题、每个每日来源每天
//!   [`ARTICLES_PER_DAY`] 篇以内，逐段都要过模型，不能一篇长文打光额度。
//! * **一段失败不拖累后面**：翻不出来的数进 `failed`，能翻的照常入库。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use qe_core::Store;

use qe_store::SqliteStore;

/// 一次导入的结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// 新建的练习。
    pub created: u32,
    /// 新建练习的 id，按创建顺序，方便接着 `history <id>` 看。
    pub ids: Vec<i64>,
    /// 已有同题跳过、或太短太像噪声的段落。
    pub skipped: u32,
    /// 参考译文生成失败或入库失败的段落。
    pub failed: u32,
    /// 逐来源/逐篇的补充说明（抓取失败、没有新文章之类）。
    pub notes: Vec<String>,
}

impl ImportReport {
    /// 一行总览：`3 created · 2 skipped`。
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} created", self.created)];
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.join(" · ")
    }

    /// 把一次单篇的结果并进总计。
    fn merge(&mut self, one: ImportReport) {
        self.created += one.created;
        self.ids.extend(one.ids);
        self.skipped += one.skipped;
        self.failed += one.failed;
        self.notes.extend(one.notes);
    }
}

/// 一次最多建几题：逐段都要过模型，长文不能一口吃成胖子。
const LIMIT: usize = 12;
/// 每个每日来源每天最多导几篇（来源没写 `limit` 时用这个）。
const ARTICLES_PER_DAY: usize = 2;
/// 正文段的字符范围：太短像页脚噪声，太长一题塞不下。
const MIN_CHARS: usize = 12;
const MAX_CHARS: usize = 200;

// ---- 网页导入 ----

/// CLI 用：直接借已经打开的库导入。
pub fn import(store: &mut SqliteStore, url: &str) -> Result<ImportReport> {
    // 先探路：模型整体不通就立刻失败——一次不通就够了，
    // 不要抓完网页再一段段挨超时。
    let candidates = crate::translator::ai_candidates()?;
    crate::translator::ai_generate(&candidates, "Reply with the single word: ok")
        .context("模型探路失败，先不抓网页")?;
    import_article(store, url, None, &candidates)
}

/// 后台线程用：自己开一条连接（和新闻抓取同款），不碰主界面的连接。
pub fn import_at(db: &Path, url: &str) -> Result<ImportReport> {
    let mut store =
        SqliteStore::open_at(db).with_context(|| format!("无法打开 {}", db.display()))?;
    import(&mut store, url)
}

/// 抓一篇文章出题。
///
/// `set` 指定合集 id；`None` 表示每篇文章各成一个合集（网页导入，
/// 合集名取页面标题，按文章 URL 复用）。
fn import_article(
    store: &mut SqliteStore,
    url: &str,
    set: Option<i64>,
    models: &[crate::translator::AiModel],
) -> Result<ImportReport> {
    let raw = crate::fetch::get(url).with_context(|| format!("抓取失败：{url}"))?;
    let paragraphs = article_paragraphs(&raw);
    anyhow::ensure!(!paragraphs.is_empty(), "页面里没解析出中文正文段落");
    let set_id = match set {
        Some(id) => id,
        None => store.ensure_practice_set(&page_title(&raw, url), "web", Some(url))?,
    };
    let translator = |text: &str| -> Result<String> {
        Ok(crate::translator::ai_generate(models, &reference_prompt(text))?.text)
    };
    let report = create_exercises(store, &paragraphs, url, Some(set_id), &translator)?;
    // 导过就记 URL：每日发现不再重复抓这篇（全失败时不记，下轮还能重试）。
    if report.created > 0 || report.skipped > 0 {
        store.record_practice_article(url)?;
    }
    Ok(report)
}

/// 合集名：页面 `<title>`（清理、截 40 字），拿不到就退回 URL 末段。
fn page_title(html: &str, url: &str) -> String {
    let title = crate::html::element_inner(html, "title")
        .map(|inner| crate::html::clean_text(&inner))
        .unwrap_or_default();
    let title: String = title.chars().take(40).collect();
    let title = title.trim();
    if title.is_empty() {
        url.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(url)
            .to_string()
    } else {
        title.to_string()
    }
}

/// 页面里的中文正文段：`<p>` 清理后按中文比例、长度、噪声三道筛子。
pub fn article_paragraphs(html: &str) -> Vec<String> {
    crate::html::paragraphs(html)
        .into_iter()
        .filter(|text| crate::html::is_chinese(text))
        .filter(|text| {
            let chars = text.chars().count();
            (MIN_CHARS..=MAX_CHARS).contains(&chars)
        })
        .filter(|text| !is_noise(text))
        .collect()
}

/// 页脚、版权、互动行这些不是正文。
fn is_noise(text: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "版权所有",
        "著作权",
        "扫码关注",
        "点击下方",
        "责任编辑",
        "来源：",
    ];
    MARKERS.iter().any(|marker| text.contains(marker))
}

/// 参考译文的提示词：中文段 → 纯英文，别夹注释或拼音。
fn reference_prompt(text: &str) -> String {
    format!(
        "Translate the following Simplified Chinese paragraph into natural English. \
         Output only the translation — no notes, no pinyin, no explanations.\n\n{text}"
    )
}

/// 把段落变成练习；`translate` 可注入——测试里给假翻译，不联网。
fn create_exercises(
    store: &mut SqliteStore,
    paragraphs: &[String],
    source_url: &str,
    set_id: Option<i64>,
    translate: &dyn Fn(&str) -> Result<String>,
) -> Result<ImportReport> {
    let source_type = format!("web:{source_url}");
    let mut report = ImportReport::default();
    let mut consecutive_failures = 0usize;
    for paragraph in paragraphs {
        if report.created as usize >= LIMIT {
            break;
        }
        // 同一段原文已经有题就跳过：重复导入不该长出重复题。
        if store.translation_exercise_by_source(paragraph)?.is_some() {
            report.skipped += 1;
            continue;
        }
        match translate(paragraph) {
            Ok(reference) => {
                consecutive_failures = 0;
                let created = store.create_translation_exercise(
                    paragraph,
                    &reference,
                    "zh-en",
                    &source_type,
                    None,
                );
                match created {
                    Ok(id) => {
                        if let Some(set) = set_id {
                            store.assign_exercise_set(id, set)?;
                        }
                        report.created += 1;
                        report.ids.push(id);
                    }
                    Err(_) => report.failed += 1,
                }
            }
            // 连续两段翻不出来：网络/模型整体有问题（冷却、断网都是这个形态），
            // 再一段段试下去只是白等，剩下的段下轮再说。
            Err(_) => {
                report.failed += 1;
                consecutive_failures += 1;
                if consecutive_failures >= 2 {
                    break;
                }
            }
        }
        // 对模型客气一点，也避免一次把免费额度打满。
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Ok(report)
}

// ---- 每日自动发现 ----

/// `practice_sources.json` 里的一项：一个**发现入口**（列表页）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DailySource {
    /// 合集名，也是 `/practice` 列表里显示的名字。
    #[serde(default)]
    pub name: String,
    /// 列表页 URL。留空的条目会被跳过——方便把示例留在文件里照着改。
    #[serde(default)]
    pub url: String,
    /// 每天最多导几篇；0 / 没写用 [`ARTICLES_PER_DAY`]，封顶 6。
    #[serde(default)]
    pub limit: usize,
}

impl DailySource {
    fn articles_per_day(&self) -> usize {
        if self.limit == 0 {
            ARTICLES_PER_DAY
        } else {
            self.limit.min(6)
        }
    }
}

/// 内置默认来源：实测国内直连、列表页是静态 HTML、正文段落适合出题。
/// 每天各导 2 篇；想增删换改 `practice_sources.json`（文件存在时**不覆盖**）。
const DEFAULT_SOURCES: &str = r#"[
  {
    "name": "人民网 · 观点",
    "url": "http://opinion.people.com.cn/",
    "limit": 2
  },
  {
    "name": "中国政府网 · 要闻",
    "url": "https://www.gov.cn/",
    "limit": 2
  }
]
"#;

/// 老版本写出的占位条目（只有示例、url 为空）：识别出来换成内置真实来源，
/// 别让老用户停在「没有可用来源」上。**用户自己改过的文件绝不动。**
fn is_old_placeholder(sources: &[DailySource]) -> bool {
    sources.len() == 1
        && sources[0].url.trim().is_empty()
        && sources[0].name.contains("示例来源")
}

/// 真正参与每日发现的来源：url 留空的条目当停用开关，跳过。
fn active_sources(sources: Vec<DailySource>) -> Vec<DailySource> {
    sources
        .into_iter()
        .filter(|source| !source.url.trim().is_empty())
        .collect()
}

/// `practice_sources.json` 的完整路径（与 `keys.json` 同目录）。
pub fn sources_path() -> PathBuf {
    qe_store::keys::default_dir().join("practice_sources.json")
}

/// 读每日来源；文件缺失先写出内置默认（真实来源）再读。
pub fn load_sources() -> Result<Vec<DailySource>> {
    load_sources_at(&qe_store::keys::default_dir())
}

fn load_sources_at(dir: &Path) -> Result<Vec<DailySource>> {
    let path = dir.join("practice_sources.json");
    if !path.exists() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("无法创建数据目录 {}", dir.display()))?;
        std::fs::write(&path, DEFAULT_SOURCES)
            .with_context(|| format!("无法写出默认来源清单 {}", path.display()))?;
    }
    let parse = || -> Result<Vec<DailySource>> {
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("无法读取 {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("{} 不是合法 JSON", path.display()))
    };
    let sources = parse()?;
    if is_old_placeholder(&sources) {
        std::fs::write(&path, DEFAULT_SOURCES)
            .with_context(|| format!("无法更新 {}", path.display()))?;
        return parse();
    }
    Ok(sources)
}

/// 每日自动导入（CLI `translate-practice daily` 也走这里）：
/// 逐个来源抓列表页 → 找没导过的新文章 → 按段出题，进该来源的合集。
pub fn daily_import(store: &mut SqliteStore) -> Result<ImportReport> {
    let sources = active_sources(load_sources()?);
    if sources.is_empty() {
        return Ok(ImportReport {
            notes: vec![format!(
                "{} 里还没有可用来源（填列表页 URL）",
                sources_path().display()
            )],
            ..ImportReport::default()
        });
    }
    // 没有模型 key 就出不了参考译文；先探路，模型整体不通就本轮直接失败。
    let candidates = crate::translator::ai_candidates()?;
    crate::translator::ai_generate(&candidates, "Reply with the single word: ok")
        .context("模型探路失败，本轮先跳过")?;
    let mut report = ImportReport::default();
    for source in &sources {
        let listing = match crate::fetch::get(&source.url) {
            Ok(page) => page,
            Err(err) => {
                report.notes.push(format!(
                    "{} · {}",
                    source.name,
                    first_line(&format!("{err:#}"))
                ));
                continue;
            }
        };
        let mut fresh = 0;
        // 尝试上限：打不开/没正文的链接别把一轮拖死（失败的不会记库，
        // 下一轮还会再试它们，所以这里只是本轮内的止损）。
        let mut attempts = 0;
        let cap = source.articles_per_day() * 4;
        for link in article_links(&listing, &source.url) {
            if fresh >= source.articles_per_day() || attempts >= cap {
                break;
            }
            if store.practice_article_imported(&link)? {
                continue;
            }
            attempts += 1;
            let name = if source.name.trim().is_empty() {
                link.as_str()
            } else {
                source.name.trim()
            };
            let set = store.ensure_practice_set(name, "daily", Some(&source.url))?;
            match import_article(store, &link, Some(set), &candidates) {
                Ok(one) => {
                    report.merge(one);
                    fresh += 1;
                }
                Err(err) => {
                    report.failed += 1;
                    report.notes.push(format!(
                        "{} · {}",
                        crate::html::clean_text(&link),
                        first_line(&format!("{err:#}"))
                    ));
                }
            }
        }
        if fresh == 0 {
            report.notes.push(format!("{} · 没有新文章", source.name));
        } else if attempts >= cap && fresh < source.articles_per_day() {
            report.notes.push(format!(
                "{} · {attempts} 个链接试完只成了 {fresh} 篇，其余下轮再试",
                source.name
            ));
        }
    }
    Ok(report)
}

/// 后台线程用的每日导入：自己开一条连接。
pub fn daily_import_at(db: &Path) -> Result<ImportReport> {
    let mut store =
        SqliteStore::open_at(db).with_context(|| format!("无法打开 {}", db.display()))?;
    daily_import(&mut store)
}

/// 列表页里的文章链接：同域、去掉导航/翻页/资源类地址，**按「像不像文章」排序**
/// （列表页 DOM 里导航通常在正文前面，按文档顺序取会先抓到一堆栏目页），
/// 同分的保持文档顺序。
pub fn article_links(html: &str, listing_url: &str) -> Vec<String> {
    let Some(base_host) = host_of(listing_url) else {
        return Vec::new();
    };
    let listing = listing_url.trim_end_matches('/');
    let mut out: Vec<String> = Vec::new();
    for href in hrefs(html) {
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
            continue;
        }
        let full = resolve(listing_url, href);
        if host_of(&full).as_deref() != Some(base_host.as_str()) {
            continue;
        }
        // 列表页自己、标签页、翻页、图片这些都不当文章。
        if full.trim_end_matches('/') == listing || junk_link(&full) {
            continue;
        }
        if !out.contains(&full) {
            out.push(full);
        }
    }
    out.sort_by_key(|link| std::cmp::Reverse(link_score(link)));
    out
}

/// 「像文章」的分数：路径深、带年份日期、是网页文档的排前面；栏目根目录排后面。
fn link_score(url: &str) -> i32 {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let segments = path.split('/').filter(|part| !part.is_empty()).count() as i32;
    let mut score = segments * 2;
    // `/2026/1120/…` 这类带年份的路径几乎都是文章页。
    if path.contains("/20") {
        score += 6;
    }
    if path.ends_with(".html") || path.ends_with(".htm") || path.ends_with(".shtml") {
        score += 4;
    }
    if path.ends_with('/') {
        score -= 6;
    }
    score
}

/// HTML 里所有 `href="…"` / `href='…'` 的目标（原文切片）。
fn hrefs(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = crate::html::find_ci(rest, "href") {
        rest = &rest[start + 4..];
        let Some(eq) = rest.find('=') else { break };
        rest = &rest[eq + 1..];
        let trimmed = rest.trim_start();
        let quote = match trimmed.chars().next() {
            Some('"') => '"',
            Some('\'') => '\'',
            _ => {
                // 无引号写法：到空白为止。
                let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
                if end == 0 {
                    break;
                }
                out.push(&trimmed[..end]);
                rest = &trimmed[end..];
                continue;
            }
        };
        let body = &trimmed[1..];
        let Some(end) = body.find(quote) else { break };
        out.push(&body[..end]);
        rest = &body[end + quote.len_utf8()..];
    }
    out
}

/// `href` 补成绝对 URL：绝对、`//host/x`、`/path`、相对路径都认。
fn resolve(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return href.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..authority_end]);
    if let Some(after) = href.strip_prefix("//") {
        return format!("{scheme}://{after}");
    }
    if let Some(path) = href.strip_prefix('/') {
        return format!("{origin}/{path}");
    }
    // 相对链接拼到当前目录下（去掉 base 自己的 query/fragment）。
    let path = rest[authority_end..].split(['?', '#']).next().unwrap_or("");
    let dir = match path.rfind('/') {
        Some(index) => &path[..=index],
        None => "/",
    };
    format!("{origin}{dir}{href}")
}

/// 列表页的主机名（小写）；没有 scheme 是相对路径，返回 `None`。
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 不该当成文章的链接：版块页、登录页、图片与脚本、翻页参数。
fn junk_link(url: &str) -> bool {
    const MARKERS: [&str; 16] = [
        "/tag",
        "/tags",
        "/category",
        "/search",
        "/login",
        "/register",
        "/about",
        "/contact",
        "/author",
        "/video",
        "/live",
        "/planet",
        "/index.",
        "/shipin",
        "/photo",
        "/slide",
    ];
    let lower = url.to_ascii_lowercase();
    const EXTENSIONS: [&str; 7] = [".jpg", ".jpeg", ".png", ".css", ".js", ".xml", ".ico"];
    if EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        return true;
    }
    if lower.contains("page=") || lower.contains('#') {
        return true;
    }
    MARKERS.iter().any(|marker| lower.contains(marker))
}

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

    #[test]
    fn only_chinese_body_paragraphs_are_taken() {
        let html = r#"
            <p>首页 栏目 关于我们</p>
            <p>这是一段够长的中文正文，讲的是一件真实发生的事情，读起来通顺自然。</p>
            <p>Short.</p>
            <p>版权所有 © 2026 某某网，未经许可不得转载。</p>
            <p>这一段也是中文，长度合适，可以拿来出一道翻译题来试试看。</p>
            <p>This is an English paragraph and should be dropped entirely.</p>
        "#;
        let paragraphs = article_paragraphs(html);
        assert_eq!(
            paragraphs.len(),
            2,
            "导航、过短、版权、英文段都要滤掉：{paragraphs:?}"
        );
        assert!(paragraphs[0].contains("真实发生的事情"));
        assert!(paragraphs[1].contains("出一道翻译题"));
    }

    #[test]
    fn importing_creates_exercises_and_skips_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open_at(dir.path().join("t.db")).unwrap();
        let set = store
            .ensure_practice_set("测试合集", "web", Some("https://example.cn/a"))
            .unwrap();
        let paragraphs = vec![
            "第一段中文正文，足够长可以出一道题。".to_string(),
            "第二段中文正文，同样足够长可以出题。".to_string(),
        ];
        let fake = |text: &str| -> Result<String> { Ok(format!("EN {text}")) };

        let first = create_exercises(
            &mut store,
            &paragraphs,
            "https://example.cn/a",
            Some(set),
            &fake,
        )
        .unwrap();
        assert_eq!(first.created, 2);
        assert_eq!(first.ids.len(), 2, "新题的 id 要报出来，方便接着 history");
        assert_eq!(first.failed, 0);

        // 再导一遍：同段原文全部跳过，不长重复题。
        let again = create_exercises(
            &mut store,
            &paragraphs,
            "https://example.cn/a",
            Some(set),
            &fake,
        )
        .unwrap();
        assert_eq!(again.created, 0);
        assert_eq!(again.skipped, 2);

        let exercise = store
            .translation_exercise_by_source(&paragraphs[0])
            .unwrap()
            .unwrap();
        assert_eq!(exercise.direction, "zh-en");
        assert!(exercise.source_type.starts_with("web:"), "记下来源页面");
        assert!(exercise.reference.starts_with("EN "));

        // 题都挂进了合集，列表统计里能看到。
        let stats = store.practice_sets_with_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].name, "测试合集");
        assert_eq!(stats[0].exercises, 2);
    }

    #[test]
    fn a_failed_reference_does_not_stop_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open_at(dir.path().join("t.db")).unwrap();
        let paragraphs = vec![
            "第一段翻不出来，会数进 failed。".to_string(),
            "第二段照常入库，不受上一段影响。".to_string(),
        ];
        let flaky = |text: &str| -> Result<String> {
            if text.contains("翻不出来") {
                anyhow::bail!("model hiccup");
            }
            Ok(format!("EN {text}"))
        };
        let report = create_exercises(
            &mut store,
            &paragraphs,
            "https://example.cn/b",
            None,
            &flaky,
        )
        .unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(report.failed, 1);
        assert!(store
            .translation_exercise_by_source(&paragraphs[1])
            .unwrap()
            .is_some());
        assert!(report.summary().contains("1 failed"));
    }

    #[test]
    fn two_consecutive_failures_stop_instead_of_retrying_every_paragraph() {
        // 网络/模型整体有问题的形态：连续两段翻不出来就停——
        // 再一段段下去只是白等超时，剩下的段下轮再说。
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open_at(dir.path().join("t.db")).unwrap();
        let paragraphs: Vec<String> = (0..5)
            .map(|i| format!("第 {i} 段正文，长度足够长可以拿来出一道题。"))
            .collect();
        let calls = std::cell::Cell::new(0usize);
        let always_down = |_: &str| -> Result<String> {
            calls.set(calls.get() + 1);
            anyhow::bail!("network down")
        };
        let report =
            create_exercises(&mut store, &paragraphs, "https://example.cn/down", None, &always_down)
                .unwrap();
        assert_eq!(calls.get(), 2, "连续两次不通就停，不把五段全试一遍");
        assert_eq!(report.failed, 2);
        assert_eq!(report.created, 0);
    }

    #[test]
    fn one_page_is_capped_so_a_long_article_cannot_burn_the_quota() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open_at(dir.path().join("t.db")).unwrap();
        let paragraphs: Vec<String> = (0..30)
            .map(|i| format!("这是第 {i} 段正文，长度足够长，完全可以拿来出一道翻译练习题。"))
            .collect();
        let fake = |text: &str| -> Result<String> { Ok(format!("EN {text}")) };
        let report =
            create_exercises(&mut store, &paragraphs, "https://example.cn/long", None, &fake)
                .unwrap();
        assert_eq!(report.created as usize, LIMIT, "一次最多建 LIMIT 题");
    }

    #[test]
    fn the_page_title_names_the_collection() {
        let html = "<html><head><title>某某深度好文 - 某某网</title></head></html>";
        assert_eq!(
            page_title(html, "https://example.cn/a/1"),
            "某某深度好文 - 某某网"
        );
        // 没有标题就用 URL 末段。
        assert_eq!(page_title("<html></html>", "https://example.cn/a/1"), "1");
        // 超长标题截断，合集名不该占满整行。
        let long = format!("<title>{}</title>", "很长".repeat(60));
        assert_eq!(page_title(&long, "https://x").chars().count(), 40);
    }

    #[test]
    fn article_links_rank_deep_dated_pages_above_navigation() {
        // 列表页里导航通常排在正文前面：按文档顺序取会先抓一堆栏目页。
        let html = r#"
            <a href="/gb/">栏目根目录</a>
            <a href="/GB/index.html">频道首页</a>
            <a href="/n1/2026/1120/c1001-1.html">正文</a>
            <a href="/n1/2026/1119/c998-1.html">另一篇正文</a>
        "#;
        let links = article_links(html, "http://opinion.people.com.cn/");
        assert_eq!(
            links,
            vec![
                "http://opinion.people.com.cn/n1/2026/1120/c1001-1.html".to_string(),
                "http://opinion.people.com.cn/n1/2026/1119/c998-1.html".to_string(),
                "http://opinion.people.com.cn/gb/".to_string(),
            ],
            "带日期的正文页排最前，栏目根目录垫底：{links:?}"
        );
        assert!(
            links[0].contains("/2026/"),
            "每天取前 limit 个时先拿到的是文章"
        );
    }

    #[test]
    fn listing_links_keep_articles_and_drop_navigation() {
    let html = r##"
            <a href="https://www.example.cn/lishi/2026/0101/article-one.html">文章一</a>
            <a href="/lishi/2026/0102/article-two.html">文章二</a>
            <a href='//www.example.cn/lishi/2026/0103/article-three.html'>文章三</a>
            <a href="https://www.example.cn/tag/历史/">标签页</a>
            <a href="/lishi/?page=2">下一页</a>
            <a href="https://other.cn/elsewhere.html">外站</a>
            <a href="/images/logo.png">图</a>
            <a href="#top">顶部</a>
            <a href="/lishi/2026/0101/article-one.html">重复的相对写法</a>
        "##;
        let links = article_links(html, "https://www.example.cn/lishi/");
        assert_eq!(
            links,
            vec![
                "https://www.example.cn/lishi/2026/0101/article-one.html".to_string(),
                "https://www.example.cn/lishi/2026/0102/article-two.html".to_string(),
                "https://www.example.cn/lishi/2026/0103/article-three.html".to_string(),
            ],
            "同域文章按页面顺序去重，标签/翻页/外站/图片/锚点都排除"
        );
        // 列表页自己不算文章。
        assert!(
            article_links("<a href=\"/lishi/\">本列表</a>", "https://www.example.cn/lishi/")
                .is_empty()
        );
    }

    #[test]
    fn the_sources_file_ships_with_real_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let sources = load_sources_at(dir.path()).unwrap();
        assert!(dir.path().join("practice_sources.json").exists(), "缺了写默认");
        assert_eq!(sources.len(), 2, "内置两个实测可用的来源");
        assert!(
            sources.iter().all(|source| !source.url.trim().is_empty()),
            "默认必须开箱即跑：{sources:?}"
        );
        assert!(sources[0].url.contains("people.com.cn"));
        assert!(sources[1].url.contains("gov.cn"));
        assert_eq!(sources[0].articles_per_day(), ARTICLES_PER_DAY);

        // 手写的 limit 生效，且封顶防止一次抓太多。
        let custom: DailySource =
            serde_json::from_str(r#"{"name":"某网","url":"https://e.cn/","limit":99}"#).unwrap();
        assert_eq!(custom.articles_per_day(), 6);

        // url 留空的条目是停用开关，运行时被跳过。
        let with_disabled = DailySource {
            name: "停用的".to_string(),
            url: "  ".to_string(),
            limit: 0,
        };
        assert!(active_sources(vec![with_disabled]).is_empty());
        assert_eq!(active_sources(sources).len(), 2);
    }

    #[test]
    #[ignore] // 联网 + 花少量模型额度，手动跑：cargo test -p quietly temp_live_daily -- --ignored --nocapture
    fn temp_live_daily_import_into_a_scratch_home() {
        let _guard = crate::translator::tests::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("QUIETLY_HOME");
        std::env::set_var("QUIETLY_HOME", dir.path());

        let outcome = daily_import_at(&dir.path().join("scratch.db"));

        match previous {
            Some(value) => std::env::set_var("QUIETLY_HOME", value),
            None => std::env::remove_var("QUIETLY_HOME"),
        }

        // 模型可用 → 真的出题；模型不通 → **快速失败**并把真实原因带出来
        // （探路一次失败就整轮退出，不再一段段挨超时）。
        match outcome {
            Ok(report) => {
                println!("summary: {}", report.summary());
                for note in &report.notes {
                    println!("note: {note}");
                }
                assert_eq!(report.ids.len() as u32, report.created);
                assert!(report.created > 0, "模型可用时内置来源应当能出题");
            }
            Err(err) => {
                let message = format!("{err:#}");
                println!("failed fast: {message}");
                assert!(
                    message.contains("探路失败"),
                    "探路失败要把真实原因带出来：{message}"
                );
            }
        }
    }

    #[test]
    fn an_old_placeholder_file_is_replaced_with_real_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("practice_sources.json");
        // 老版本写出的占位条目：识别出来换成内置真实来源。
        std::fs::write(
            &path,
            r#"[{"name":"示例来源（把 url 换成列表页）","url":"","limit":2}]"#,
        )
        .unwrap();
        let sources = load_sources_at(dir.path()).unwrap();
        assert_eq!(sources.len(), 2, "老占位文件被换成内置来源");
        assert!(sources.iter().all(|source| !source.url.is_empty()));

        // 用户自己清空过 url 的文件绝不动（名字不是示例就算用户改的）。
        std::fs::write(&path, r#"[{"name":"我的来源","url":"","limit":2}]"#).unwrap();
        let sources = load_sources_at(dir.path()).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].url, "");
    }
}
