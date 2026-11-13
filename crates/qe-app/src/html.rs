//! 把网页变成「一段一段的正文」。
//!
//! 几个抓取源（TED、BBC、新华网英文）都要同一件事：从 HTML 里取出正文段落，
//! 去掉标签、解实体、压空白，再判断一段是中文还是英文。放在这里共用。

/// 去掉标签、解实体、压空白。
///
/// 解两遍：网页（BBC 尤其）常把实体再转义一次，`&amp;#039;` 第一遍解出来是
/// `&#039;`，第二遍才是 `'`。普通文本解两遍没有副作用。
pub fn clean_text(raw: &str) -> String {
    let stripped = strip_tags(raw).replace(['\u{3000}', '\u{a0}', '\u{200b}'], " ");
    let decoded = crate::epub::decode_entities(&crate::epub::decode_entities(&stripped));
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉所有标签，`<br>` 压成一个空格（标题里的折行不该粘在一起）。
pub fn strip_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            return out;
        };
        if rest[start + 1..start + end].to_ascii_lowercase().starts_with("br") {
            out.push(' ');
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// 一段 HTML 里的 `<p>` 段落（`<br>` 视为段落分隔），清理后返回。
pub fn paragraphs(region: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = region;
    while let Some(start) = find_ci(rest, "<p") {
        rest = &rest[start..];
        let Some(open_end) = rest.find('>') else { break };
        rest = &rest[open_end + 1..];
        let Some(close) = find_ci(rest, "</p>") else { break };
        let block = &rest[..close];
        rest = &rest[close + 4..];
        for chunk in split_br(block) {
            let text = clean_text(chunk);
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

/// 从 `marker` 处切到文件末尾，再在 `stops` 里最早出现的标记前截断。
///
/// 用来把「正文区」从导航和脚本里切出来：`region_after(html, "id=\"detail\"", &["<script"])`。
pub fn region_after<'a>(html: &'a str, marker: &str, stops: &[&str]) -> &'a str {
    let Some(start) = find_ci(html, marker) else {
        return "";
    };
    let region = &html[start..];
    let cut = stops
        .iter()
        .filter_map(|stop| find_ci(region, stop))
        .min()
        .unwrap_or(region.len());
    &region[..cut]
}

fn split_br(block: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(start) = find_ci(rest, "<br") {
        out.push(&rest[..start]);
        let after = &rest[start..];
        match after.find('>') {
            Some(end) => rest = &after[end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push(rest);
    out
}

/// 这一段是不是中文为主。
pub fn is_chinese(text: &str) -> bool {
    let total = text.chars().count();
    if total == 0 {
        return false;
    }
    let cjk = text
        .chars()
        .filter(|ch| ('\u{4e00}'..='\u{9fff}').contains(ch))
        .count();
    cjk as f32 / total as f32 > 0.3
}

/// 第一个 `<tag…>…</tag>` 的内部 HTML。
pub fn element_inner(html: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = find_ci(html, &open)?;
    let after_open = html[start..].find('>')? + start + 1;
    let close = find_ci(&html[after_open..], &format!("</{tag}>"))?;
    Some(html[after_open..after_open + close].to_string())
}

/// 不区分大小写地找位置。只把两边都转小写来比，字节偏移仍对应原文
/// （ASCII 大小写转换不改长度，标签名都是 ASCII）。
pub fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_are_decoded_twice_because_pages_escape_them_twice() {
        // BBC 的标题里常见的 `&amp;#039;`：解一遍只得到 `&#039;`，得再解一遍。
        assert_eq!(
            clean_text("World&amp;#039;s oldest shipwreck"),
            "World's oldest shipwreck"
        );
        assert_eq!(clean_text("&pound;10m &middot; 12%"), "£10m · 12%");
    }

    #[test]
    fn br_splits_paragraphs_inside_one_p_tag() {
        assert_eq!(paragraphs("<p>One<br/>Two<br>Three</p>"), vec!["One", "Two", "Three"]);
    }

    #[test]
    fn tags_are_stripped_and_whitespace_collapsed() {
        assert_eq!(
            clean_text("<a href=\"#\">Hello\n\t  world</a> &nbsp;again"),
            "Hello world again"
        );
    }

    #[test]
    fn a_region_stops_before_the_first_stop_marker() {
        let html = "<div id=\"detail\"><p>Body</p><script>var x = 1;</script></div>";
        let region = region_after(html, "id=\"detail\"", &["<script"]);
        assert!(region.contains("Body"));
        assert!(!region.contains("var x"));
    }

    #[test]
    fn chinese_is_detected_by_character_ratio() {
        assert!(is_chinese("这艘古代沉船位于凯考瓦岛附近海域。"));
        assert!(!is_chinese("The ancient wreck was found off the coast."));
        assert!(!is_chinese(""));
    }
}
