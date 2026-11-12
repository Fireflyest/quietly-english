//! 读取 EPUB，按章拆成 quietly 的文本。
//!
//! EPUB 就是个 zip：`META-INF/container.xml` 指向 OPF，OPF 的 spine 给出阅读顺序。
//! 顺着 spine 把每一章取出来、剥掉标签，就得到和 `.txt` 一样的分段正文。这样
//! `quietly read book.epub` 一步就能把整本书按 `# book:` 归好类，不必先手工转换。
//!
//! 只做「读」需要的事：不解析 CSS、不下载图片、不重建目录层级。斜体、脚注编号
//! 这类内联标记会被丢掉，正文与段落边界保留。

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use qe_core::reading::Text;

/// 一章正文少于这个字符数就不收：用于兜底跳过空壳条目。
///
/// 扉页、版权页、colophon 主要由 `epub:type` 过滤，不靠长度判断，所以这里的
/// 门槛只用来挡「只剩一个标题」的坏条目。
const MIN_CHAPTER_CHARS: usize = 40;

/// 一个 EPUB 拆出来的结果。
pub struct Epub {
    /// 书名（`dc:title`），同时用作 `# book:`。
    pub title: String,
    /// 各章，已带上书名与来源。
    pub texts: Vec<Text>,
}

/// 按扩展名判断是否是 EPUB。
pub fn is_epub(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("epub"))
        .unwrap_or(false)
}

/// 读一个 EPUB，按 spine 顺序拆章。
pub fn load(path: &Path) -> Result<Epub> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("无法打开 {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("{} 不是有效的 EPUB（zip 读取失败）", path.display()))?;

    let opf_path = find_opf(&mut archive, path)?;
    let opf = read_entry(&mut archive, &opf_path)?;
    let title = element_text(&opf, "dc:title")
        .or_else(|| element_text(&opf, "title"))
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".to_string())
        });
    let author = element_text(&opf, "dc:creator").or_else(|| element_text(&opf, "creator"));
    let book = title.clone();

    // OPF 里的 href 相对 OPF 所在目录；spine 的 idref 指向 manifest 的 id。
    let base = opf_path.rsplit_once('/').map(|(dir, _)| format!("{dir}/")).unwrap_or_default();
    let manifest = parse_manifest(&opf);
    let mut chapters: Vec<Text> = Vec::new();

    for idref in spine_ids(&opf) {
        let Some(href) = manifest.iter().find(|(id, _)| *id == idref).map(|(_, href)| href) else {
            continue;
        };
        let entry = format!("{base}{href}");
        let Ok(raw) = read_entry(&mut archive, &entry) else {
            continue;
        };
        // Standard Ebooks 把扉页/版权页/colophon 也放进 spine，用 `epub:type` 区分：
        // 只有 bodymatter 是正文。没有声明类型的老书照旧按长度过滤。
        if let Some(kinds) = body_types(&raw) {
            if !kinds.contains("bodymatter") {
                continue;
            }
        }
        let paragraphs = body_paragraphs(&raw);
        let body = paragraphs.join("\n\n");
        if body.chars().count() < MIN_CHAPTER_CHARS {
            continue;
        }
        let number = chapters.len() + 1;
        let mut text = Text::new(format!("{book} — {number}"), body);
        text.book = Some(book.clone());
        text.source = author.clone();
        text.license = Some("public domain".to_string());
        chapters.push(text);
    }

    anyhow::ensure!(
        !chapters.is_empty(),
        "{} 里没有可读的章节（spine 为空或全是扉页/版权页）",
        path.display()
    );

    Ok(Epub {
        title,
        texts: chapters,
    })
}

/// `META-INF/container.xml` → OPF 的 zip 内路径。
fn find_opf(archive: &mut zip::ZipArchive<std::fs::File>, path: &Path) -> Result<String> {
    let container = read_entry(archive, "META-INF/container.xml")
        .with_context(|| format!("{} 缺少 META-INF/container.xml", path.display()))?;
    extract_attr(&container, "full-path")
        .with_context(|| format!("{} 的 container.xml 里没有 rootfile", path.display()))
}

fn read_entry(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("EPUB 里没有 {name}"))?;
    let mut raw = Vec::new();
    entry.read_to_end(&mut raw)?;
    Ok(String::from_utf8_lossy(&raw).to_string())
}

/// manifest：`(id, href)`。
fn parse_manifest(opf: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = opf;
    while let Some(start) = rest.find("<item") {
        rest = &rest[start..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        rest = &rest[end..];
        if tag.contains("media-type=\"application/xhtml+xml\"") || tag.contains(".xhtml") || tag.contains(".html") {
            if let (Some(id), Some(href)) = (extract_attr(tag, "id"), extract_attr(tag, "href")) {
                out.push((id, href));
            }
        }
    }
    out
}

/// spine 的阅读顺序：`<itemref idref="..."/>`。
fn spine_ids(opf: &str) -> Vec<String> {
    let Some(spine_start) = opf.find("<spine") else {
        return Vec::new();
    };
    let spine = &opf[spine_start..];
    let spine = match spine.find("</spine>") {
        Some(end) => &spine[..end],
        None => spine,
    };
    let mut out = Vec::new();
    let mut rest = spine;
    while let Some(start) = rest.find("<itemref") {
        rest = &rest[start..];
        let Some(end) = rest.find('>') else { break };
        if let Some(idref) = extract_attr(&rest[..end], "idref") {
            out.push(idref);
        }
        rest = &rest[end..];
    }
    out
}

/// 取标签里的某个属性值，属性可能带单引号或双引号。
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let mut rest = tag;
    loop {
        let index = rest.find(&needle)?;
        // 必须是独立属性名，不能是 `xid=` 里的 `id=`。
        let boundary_ok = index == 0
            || !rest[..index]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_');
        rest = &rest[index + needle.len()..];
        if !boundary_ok {
            continue;
        }
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let value = &rest[quote.len_utf8()..];
        let end = value.find(quote)?;
        return Some(value[..end].to_string());
    }
}

/// `<dc:title>X</dc:title>` 的文本内容。
fn element_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = xml.find(&open)?;
    let after_open = xml[start..].find('>')? + start + 1;
    let end = xml[after_open..].find(&close)? + after_open;
    let value = decode_entities(xml[after_open..end].trim());
    (!value.is_empty()).then_some(value)
}

/// `<body epub:type="bodymatter z3998:fiction">` 里的类型列表。
fn body_types(raw: &str) -> Option<String> {
    let start = raw.find("<body")?;
    let end = raw[start..].find('>')? + start;
    extract_attr(&raw[start..end], "epub:type")
}

/// 取出 `<body>` 里的段落。
fn body_paragraphs(raw: &str) -> Vec<String> {
    let body = match raw.find("<body") {
        Some(start) => {
            let start = raw[start..].find('>').map(|i| start + i + 1).unwrap_or(start);
            let end = raw.rfind("</body>").unwrap_or(raw.len());
            &raw[start..end.max(start)]
        }
        None => raw,
    };
    paragraphs_from_html(body)
}

/// 把一段 HTML 变成若干段落。
///
/// 块级标签结束时断开段落，`<br>` 断行；其余标签（`<em>`、`<abbr>`、脚注标记）
/// 直接丢掉，标签之间的文字保留。
pub fn paragraphs_from_html(body: &str) -> Vec<String> {
    let mut text = String::with_capacity(body.len());
    let mut rest = body;

    while let Some(lt) = rest.find('<') {
        text.push_str(&rest[..lt]);
        let after = &rest[lt..];
        // 注释整段跳过。
        if after.starts_with("<!--") {
            match after.find("-->") {
                Some(end) => rest = &after[end + 3..],
                None => return finish_paragraphs(&text),
            }
            continue;
        }
        let Some(gt) = after.find('>') else {
            // 半个标签：原文到此为止。
            rest = &after[..0];
            break;
        };
        let tag = after[1..gt].trim_start_matches('/').to_ascii_lowercase();
        let name: String = tag
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .collect();
        let closing = after[1..].chars().next() == Some('/');
        if closing {
            if is_block(&name) {
                text.push_str("\n\n");
            }
        } else if name == "br" {
            text.push('\n');
        }
        rest = &after[gt + 1..];
    }
    text.push_str(rest);
    finish_paragraphs(&text)
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "tr"
            | "td"
            | "dd"
            | "dt"
            | "pre"
            | "figcaption"
    )
}

fn finish_paragraphs(text: &str) -> Vec<String> {
    let decoded = decode_entities(text)
        .replace('\u{a0}', " ")
        .replace('\u{2009}', " ")
        .replace('\u{202f}', " ")
        .replace('\u{200b}', "");
    decoded
        .split("\n\n")
        .map(|paragraph| {
            // 段内保留单个换行（`<br>`，诗歌分行），但把行首尾与多余空格收紧。
            paragraph
                .lines()
                .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        })
        .filter(|paragraph| !paragraph.is_empty())
        .collect()
}

/// 常见的 HTML 实体。EPUB 里的排版断字（`&nbsp;`、`&mdash;`）比正式文本多，
/// 这里只处理出现频率高的那些；数字实体一并支持。
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "lsquo" => Some('‘'),
            "rsquo" => Some('’'),
            "ldquo" => Some('“'),
            "rdquo" => Some('”'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|num| num.parse::<u32>().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_break_on_block_tags_and_keep_inline_text() {
        let html = r#"<h2>I</h2><p>It is a truth <em>universally</em> acknowledged.</p>
            <p>However little known <abbr>Mr.</abbr> Bennet was.</p>"#;
        let paragraphs = paragraphs_from_html(html);
        assert_eq!(
            paragraphs,
            vec![
                "I".to_string(),
                "It is a truth universally acknowledged.".to_string(),
                "However little known Mr. Bennet was.".to_string(),
            ]
        );
    }

    #[test]
    fn br_keeps_a_line_break_inside_the_paragraph_and_comments_vanish() {
        // 诗歌用 `<br>` 分行：段内保留换行，但仍是同一段。
        let html = "One<br/>Two<!-- note -->Three";
        assert_eq!(paragraphs_from_html(html), vec!["One\nTwoThree"]);
    }

    #[test]
    fn entities_and_nbsp_are_decoded() {
        let html = "<p>Mr.&nbsp;Bennet &amp; Mrs.&#8212;Bennet</p>";
        assert_eq!(paragraphs_from_html(html), vec!["Mr. Bennet & Mrs.—Bennet"]);
    }

    #[test]
    fn attributes_are_read_with_either_quote() {
        assert_eq!(extract_attr("<item id='x' href=\"a.xhtml\">", "id").as_deref(), Some("x"));
        assert_eq!(
            extract_attr("<item id='x' href=\"a.xhtml\">", "href").as_deref(),
            Some("a.xhtml")
        );
        // `xid=` 不能被当成 `id=`。
        assert_eq!(extract_attr("<item xid='y'>", "id"), None);
    }

    #[test]
    fn spine_keeps_the_reading_order() {
        let opf = r#"<spine><itemref idref="a"/><itemref idref="b" linear="no"/></spine>"#;
        assert_eq!(spine_ids(opf), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn manifest_only_lists_content_documents() {
        let opf = r#"<manifest>
            <item id="a" href="text/a.xhtml" media-type="application/xhtml+xml"/>
            <item id="css" href="css/core.css" media-type="text/css"/>
        </manifest>"#;
        let entries = parse_manifest(opf);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "a");
    }
}
