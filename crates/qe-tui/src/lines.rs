//! 文本测量、换行，以及“消息 → 行”的渲染。
//!
//! 每个 agent 块用一条左侧竖线包住，竖线颜色表示状态（绿 new / 蓝 review /
//! 黄 hint），不画边框：
//!
//! ```text
//! › inspect
//!
//! │ word · lookup
//! │ inspect  /ɪnˈspekt/
//! │
//! │ v. 检查；视察
//! │ in- (prefix, inward)  +  spect (root, look)
//! │ → inward + look
//! │
//! │   Engineers inspect the bridge every year.
//! ```

use qe_core::model::Rating;
use qe_core::view::{Accent, Body, Message, Role, TableView, ToolLine, ToolView};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme;

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// 按显示宽度换行，优先在空格处断开，CJK 按双宽处理。
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(8);
    let mut out = Vec::new();
    for source in text.split('\n') {
        if source.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut width = 0usize;
        let mut last_space: Option<usize> = None;

        for ch in source.chars() {
            let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + char_width > max && width > 0 {
                if let Some(idx) = last_space {
                    let rest = current[idx + 1..].to_string();
                    current.truncate(idx);
                    out.push(std::mem::take(&mut current));
                    current = rest;
                    width = display_width(&current);
                    last_space = current.rfind(' ');
                } else {
                    out.push(std::mem::take(&mut current));
                    width = 0;
                    last_space = None;
                }
            }
            current.push(ch);
            width += char_width;
            if ch == ' ' {
                last_space = Some(current.len() - 1);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 截断到指定显示宽度，超出部分用 `…` 表示。
pub fn truncate(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    let budget = max.saturating_sub(1);
    for ch in text.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > budget {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out.push('…');
    out
}

/// 取字符串末尾的若干显示列，用于输入框横向滚动。
pub fn tail(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut width = 0usize;
    let mut start = chars.len();
    for (idx, ch) in chars.iter().enumerate().rev() {
        let cw = UnicodeWidthChar::width(*ch).unwrap_or(0);
        if width + cw > max {
            break;
        }
        width += cw;
        start = idx;
    }
    chars[start..].iter().collect()
}

pub fn pad_right(text: &str, width: usize) -> String {
    let w = display_width(text);
    if w >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - w))
    }
}

pub fn pad_left(text: &str, width: usize) -> String {
    let w = display_width(text);
    if w >= width {
        text.to_string()
    } else {
        format!("{}{text}", " ".repeat(width - w))
    }
}

/// 消息 → 若干渲染行。
pub fn message_lines(message: &Message, width: usize) -> Vec<Line<'static>> {
    match &message.body {
        Body::Text(text) => match message.role {
            Role::User => body("› ", text, theme::text(), theme::user(), width),
            Role::Agent => rail_text(text, width, theme::accent_plain(), theme::text()),
            Role::System => body("  ", text, theme::muted(), theme::dim(), width),
        },
        Body::Tool(tool) => tool_lines(tool, width),
        Body::Table(table) => table_lines(table, width),
    }
}

/// First line carries `prefix` in `lead_style`; continuation lines are indented to
/// match it and the text itself uses `style`. Keeping the two styles apart is what
/// makes a line read as prose with a small marker instead of a coloured block.
fn body(
    prefix: &str,
    text: &str,
    style: Style,
    lead_style: Style,
    width: usize,
) -> Vec<Line<'static>> {
    let indent = display_width(prefix);
    let inner = width.saturating_sub(indent).max(8);
    let mut out = Vec::new();
    let mut first = true;
    for paragraph in text.split('\n') {
        let wrapped = if paragraph.trim().is_empty() {
            vec![String::new()]
        } else {
            wrap(paragraph, inner)
        };
        for piece in wrapped {
            let lead = if first {
                prefix.to_string()
            } else {
                " ".repeat(indent)
            };
            out.push(Line::from(vec![
                Span::styled(lead, lead_style),
                Span::styled(piece, style),
            ]));
            first = false;
        }
    }
    out
}

/// Width of the rail prefix (`│ `) in display columns.
const RAIL_WIDTH: usize = 2;

/// One rail segment, optionally followed by `indent` spaces of padding.
fn rail_span(style: Style, indent: usize) -> Span<'static> {
    if indent == 0 {
        Span::styled("│ ".to_string(), style)
    } else {
        Span::styled(format!("│ {}", " ".repeat(indent)), style)
    }
}

fn rail_line(rail: Style, indent: usize, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut all = Vec::with_capacity(spans.len() + 1);
    all.push(rail_span(rail, indent));
    all.extend(spans);
    Line::from(all)
}

/// Wrapped text hung off the rail.
fn rail_text(text: &str, width: usize, rail: Style, style: Style) -> Vec<Line<'static>> {
    rail_text_indent(text, width, rail, style, 0)
}

fn rail_text_indent(
    text: &str,
    width: usize,
    rail: Style,
    style: Style,
    indent: usize,
) -> Vec<Line<'static>> {
    let inner = width
        .saturating_sub(RAIL_WIDTH + indent)
        .max(8);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let wrapped = if paragraph.trim().is_empty() {
            vec![String::new()]
        } else {
            wrap(paragraph, inner)
        };
        for piece in wrapped {
            out.push(rail_line(
                rail,
                indent,
                vec![Span::styled(piece, style)],
            ));
        }
    }
    out
}

fn accent_style(accent: Accent) -> Style {
    match accent {
        Accent::Neutral => theme::dim(),
        Accent::Info => theme::info(),
        Accent::Success => theme::accent_plain(),
        Accent::Warn => theme::warn(),
        Accent::Danger => theme::danger(),
        Accent::Note => theme::note(),
    }
}

fn tool_lines(tool: &ToolView, width: usize) -> Vec<Line<'static>> {
    let rail = accent_style(tool.accent);
    let mut out = Vec::new();

    // `│ name  badge` — the rail carries the state colour, the title stays plain.
    let mut head = vec![Span::styled(tool.name.clone(), theme::bold())];
    if let Some(badge) = &tool.badge {
        head.push(Span::styled(format!("  {badge}"), theme::dim()));
    }
    out.push(rail_line(rail, 0, head));

    if let Some(result) = &tool.result {
        out.extend(rail_text(result, width, rail, theme::text()));
    }

    let inner = width.saturating_sub(RAIL_WIDTH).max(8);
    for line in &tool.lines {
        match line {
            ToolLine::Text(text) => {
                out.extend(rail_text(text, width, rail, theme::text()));
            }
            ToolLine::Muted(text) => {
                out.extend(rail_text(text, width, rail, theme::muted()));
            }
            ToolLine::Accent(text) => {
                out.extend(rail_text(text, width, rail, theme::value()));
            }
            ToolLine::Kv { key, value } => {
                // Values can be full sentences (Wiktionary glosses), so they wrap
                // under the key column instead of running off the card.
                let key_width = 14.min(inner.saturating_sub(4));
                let key_text = truncate(key, key_width);
                let wrapped = wrap(value, inner.saturating_sub(key_width + 2).max(8));
                for (index, piece) in wrapped.iter().enumerate() {
                    let lead = if index == 0 {
                        pad_right(&key_text, key_width)
                    } else {
                        " ".repeat(key_width)
                    };
                    out.push(rail_line(
                        rail,
                        0,
                        vec![
                            Span::styled(lead, theme::muted()),
                            Span::styled("  ", Style::default()),
                            Span::styled(piece.clone(), theme::text()),
                        ],
                    ));
                }
            }
            ToolLine::Bar { label, value, max } => {
                const BAR_WIDTH: usize = 10;
                let filled = if *max == 0 || *value == 0 {
                    0
                } else {
                    (((*value as f64 / *max as f64) * BAR_WIDTH as f64).round() as usize)
                        .clamp(1, BAR_WIDTH)
                };
                out.push(rail_line(
                    rail,
                    0,
                    vec![
                        Span::styled(format!("{label}  "), theme::dim()),
                        Span::styled("█".repeat(filled), theme::accent_plain()),
                        Span::styled("·".repeat(BAR_WIDTH.saturating_sub(filled)), theme::dim()),
                        Span::styled(format!("  {value}"), theme::dim()),
                    ],
                ));
            }
            ToolLine::Example { en, zh } => {
                out.extend(rail_text_indent(en, width, rail, theme::text(), 2));
                if let Some(zh) = zh {
                    out.extend(rail_text_indent(zh, width, rail, theme::muted(), 2));
                }
            }
            ToolLine::Rating { selected } => {
                let mut spans = Vec::new();
                for (index, rating) in Rating::all().iter().enumerate() {
                    if index > 0 {
                        spans.push(Span::styled("   ", Style::default()));
                    }
                    let chosen = *selected == Some(*rating);
                    let label = if chosen {
                        format!("[{} {}]", rating.key(), rating.label().to_lowercase())
                    } else {
                        format!(" {} {}", rating.key(), rating.label().to_lowercase())
                    };
                    let style = if chosen {
                        theme::accent()
                    } else {
                        theme::dim()
                    };
                    spans.push(Span::styled(label, style));
                }
                out.push(rail_line(rail, 0, spans));
            }
            ToolLine::Spacer => out.push(rail_line(rail, 0, Vec::new())),
        }
    }

    out
}

fn table_lines(table: &TableView, width: usize) -> Vec<Line<'static>> {
    let rail = theme::dim();
    let mut out = Vec::new();
    if let Some(caption) = &table.caption {
        out.extend(rail_text(caption, width, rail, theme::muted()));
    }

    let cols = table
        .headers
        .len()
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if cols == 0 {
        if let Some(footer) = &table.footer {
            out.extend(rail_text(footer, width, rail, theme::muted()));
        }
        return out;
    }

    let mut widths: Vec<usize> = (0..cols)
        .map(|i| {
            let header = table.headers.get(i).map(String::as_str).unwrap_or("");
            let mut w = display_width(header);
            for row in &table.rows {
                if let Some(cell) = row.get(i) {
                    w = w.max(display_width(cell));
                }
            }
            w.min(32)
        })
        .collect();

    let spacing = 2 * cols.saturating_sub(1);
    let budget = width.saturating_sub(RAIL_WIDTH).max(16);
    while widths.iter().sum::<usize>() + spacing > budget {
        if let Some((idx, _)) = widths.iter().enumerate().max_by_key(|(_, w)| **w) {
            if widths[idx] <= 5 {
                break;
            }
            widths[idx] -= 1;
        } else {
            break;
        }
    }

    let render_row = |cells: &[String], style: Style| -> Line<'static> {
        let mut spans = Vec::new();
        for i in 0..cols {
            if i > 0 {
                spans.push(Span::styled("  ", Style::default()));
            }
            let cell = cells
                .get(i)
                .map(|c| truncate(c, widths[i]))
                .unwrap_or_default();
            let padded = if table.right_align.get(i).copied().unwrap_or(false) {
                pad_left(&cell, widths[i])
            } else {
                pad_right(&cell, widths[i])
            };
            spans.push(Span::styled(padded, style));
        }
        rail_line(rail, 0, spans)
    };

    if !table.headers.is_empty() {
        out.push(render_row(&table.headers, theme::dim()));
    }
    for row in &table.rows {
        out.push(render_row(row, theme::text()));
    }
    if let Some(footer) = &table.footer {
        out.push(rail_line(rail, 0, Vec::new()));
        out.extend(rail_text(footer, width, rail, theme::muted()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn wraps_cjk_by_display_width() {
        let lines = wrap("这是一个用于测试换行的中文字符串", 8);
        assert!(lines.len() > 1);
        for line in lines {
            assert!(display_width(&line) <= 8);
        }
    }

    #[test]
    fn wraps_english_at_spaces() {
        let lines = wrap("the quick brown fox jumps over the lazy dog", 12);
        assert!(lines.iter().all(|line| display_width(line) <= 12));
        assert!(lines[0].ends_with("quick"));
    }

    #[test]
    fn truncate_respects_width() {
        let cut = truncate("abcdefghij", 5);
        assert_eq!(display_width(&cut), 5);
    }

    #[test]
    fn tool_block_uses_agent_layout() {
        let mut word = qe_core::model::Word::new("inspect", "v. check");
        word.phonetic = Some("/ɪnˈspekt/".into());
        let state = qe_core::view::CardState::new(word, "new · 1/10");
        let text = flat(&message_lines(&Message::tool(ToolView::word_card(&state)), 80));
        assert_eq!(text[0], "│ word  new · 1/10");
        assert!(text[1].starts_with("│ inspect"), "got {:?}", text[1]);
        assert!(text.iter().all(|line| line.starts_with('│')), "every line rides the rail");
    }

    #[test]
    fn rating_row_marks_selection() {
        let mut word = qe_core::model::Word::new("inspect", "v. check");
        word.phonetic = Some("/ɪnˈspekt/".into());
        let state = qe_core::view::CardState::new(word, "1/10")
            .revealed()
            .graded(Rating::Good);
        let text = flat(&message_lines(&Message::tool(ToolView::word_card(&state)), 90));
        let row = text
            .iter()
            .find(|line| line.contains("again") && line.contains("easy"))
            .expect("rating row");
        assert!(row.contains("[3 good]"), "selection marker missing: {row}");
        assert!(!row.contains("[1 again]"), "only the chosen one is boxed: {row}");
    }

    #[test]
    fn user_and_agent_prefixes() {
        let user = flat(&message_lines(&Message::user("hello"), 80));
        assert_eq!(user[0], "› hello");
        let agent = flat(&message_lines(&Message::agent("ok"), 80));
        assert_eq!(agent[0], "│ ok");
    }
}
