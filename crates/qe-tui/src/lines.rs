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
use qe_core::view::{Accent, Body, CodeBlock, Message, Role, TableView, ToolLine, ToolView};
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
        Body::Card(state) => tool_lines(&ToolView::word_card(state), width),
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
        Accent::Neutral => theme::faint(),
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

    // `│ name  badge` — name and badge share one grey, so the header reads as a
    // single label rather than two different weights of dim.
    let mut head = vec![Span::styled(tool.name.clone(), theme::label())];
    if let Some(badge) = &tool.badge {
        head.push(Span::styled(format!("  {badge}"), theme::label()));
    }
    out.push(rail_line(rail, 0, head));

    if let Some(result) = &tool.result {
        out.extend(rail_text(result, width, rail, theme::bold()));
    }

    // Two columns when the terminal allows it: details on the left, examples on the
    // right. Narrow terminals stack the aside underneath, because side-by-side text
    // below ~96 columns wraps into unreadable ribbons.
    let aside_width = if tool.aside.is_empty() || width < 96 {
        0
    } else {
        // Roughly 40% for examples, but never so wide that the glosses get squeezed.
        ((width - RAIL_WIDTH) * 42 / 100).clamp(30, 54)
    };

    let left_width = if aside_width > 0 {
        width.saturating_sub(aside_width + 2)
    } else {
        width
    };

    let mut left = detail_rows(&tool.lines, left_width);

    if aside_width > 0 {
        let right = detail_rows(&tool.aside, aside_width);
        out.extend(join_columns(rail, &mut left, right, left_width));
    } else {
        for row in left {
            out.push(rail_line(rail, 0, row));
        }
        if !tool.aside.is_empty() {
            out.push(rail_line(rail, 0, Vec::new()));
            for row in detail_rows(&tool.aside, width) {
                out.push(rail_line(rail, 0, row));
            }
        }
    }

    out
}

/// Styles for the transcript. Kept as a small struct so a future tinted variant
/// can swap the whole palette without touching every `match` arm.
#[derive(Clone, Copy)]
struct Palette {
    text: Style,
    muted: Style,
    accent: Style,
}

impl Palette {
    fn plain() -> Self {
        Self {
            text: theme::text(),
            muted: theme::muted(),
            accent: theme::value(),
        }
    }
}

/// Render `ToolLine`s into rows of spans, without the leading rail.
fn detail_rows(lines: &[ToolLine], width: usize) -> Vec<Vec<Span<'static>>> {
    rows_in(lines, width, Palette::plain())
}

fn rows_in(lines: &[ToolLine], width: usize, palette: Palette) -> Vec<Vec<Span<'static>>> {
    let inner = width.saturating_sub(RAIL_WIDTH).max(8);
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();

    for line in lines {
        match line {
            ToolLine::Text(text) => {
                for piece in wrap(text, inner) {
                    rows.push(vec![Span::styled(piece, palette.text)]);
                }
            }
            ToolLine::Muted(text) => {
                for piece in wrap(text, inner) {
                    rows.push(vec![Span::styled(piece, palette.muted)]);
                }
            }
            ToolLine::Accent(text) => {
                for piece in wrap(text, inner) {
                    rows.push(vec![Span::styled(piece, palette.accent)]);
                }
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
                    rows.push(vec![
                        Span::styled(lead, palette.muted),
                        Span::styled("  ", Style::default()),
                        Span::styled(piece.clone(), palette.text),
                    ]);
                }
            }
            ToolLine::Code(block) => {
                rows.extend(code_rows(block, inner));
            }
            ToolLine::Bar { label, value, max } => {
                const BAR_WIDTH: usize = 10;
                let filled = if *max == 0 || *value == 0 {
                    0
                } else {
                    (((*value as f64 / *max as f64) * BAR_WIDTH as f64).round() as usize)
                        .clamp(1, BAR_WIDTH)
                };
                rows.push(vec![
                    Span::styled(format!("{label}  "), palette.muted),
                    Span::styled("█".repeat(filled), theme::accent_plain()),
                    Span::styled("·".repeat(BAR_WIDTH.saturating_sub(filled)), theme::faint()),
                    Span::styled(format!("  {value}"), palette.muted),
                ]);
            }
            ToolLine::Example {
                index,
                en,
                zh,
                highlight,
            } => {
                rows.extend(example_rows(
                    *index,
                    en,
                    zh.as_deref(),
                    highlight.as_deref(),
                    inner,
                ));
            }
            ToolLine::Sense { index, label, text } => {
                rows.extend(sense_rows(*index, label, text, inner));
            }
            ToolLine::Phrase { phrase, gloss } => {
                rows.extend(phrase_rows(phrase, gloss, inner));
            }
            ToolLine::Gloss { text, tags } => {
                rows.extend(gloss_rows(text, tags, inner));
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
                        palette.muted
                    };
                    spans.push(Span::styled(label, style));
                }
                rows.push(spans);
            }
            ToolLine::Spacer => rows.push(Vec::new()),
        }
    }

    rows
}

/// Render morphology as a code block:
///
/// ```text
/// hopeless(hope : root, -less : suffix) {
///     hope  -> to hope
///     -less -> lacking (something)
/// }  // Wiktionary {{af}}
/// ```
///
/// The panel tint is applied to every row here, so the block reads as one solid
/// rectangle rather than a run of coloured text.
fn code_rows(block: &CodeBlock, inner: usize) -> Vec<Vec<Span<'static>>> {
    // One column of padding on each side keeps the tint off the glyphs.
    let content = inner.saturating_sub(2).max(8);
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();

    // Signature: `name (part : kind, part : kind) {`
    // A space before the paren keeps the name from touching the bracket.
    let mut signature: Vec<Span<'static>> = vec![
        Span::styled(block.name.clone(), theme::code_fn()),
        Span::styled(" ", theme::code_punct()),
        Span::styled("(", theme::code_punct()),
    ];
    for (index, (part, kind)) in block.parts.iter().enumerate() {
        if index > 0 {
            signature.push(Span::styled(", ", theme::code_punct()));
        }
        signature.push(Span::styled(part.clone(), theme::code_param()));
        signature.push(Span::styled(" : ", theme::code_punct()));
        signature.push(Span::styled(kind.clone(), theme::code_type()));
    }
    signature.push(Span::styled(") {", theme::code_punct()));
    rows.extend(wrap_spans(&signature, content));

    // Blank line inside the opening brace, so the body does not crowd it.
    rows.push(Vec::new());

    // Body: indented `part -> gloss`, gloss wrapping under itself.
    let key_width = block
        .body
        .iter()
        .map(|(part, _)| display_width(part))
        .max()
        .unwrap_or(0)
        .clamp(4, 16);
    for (part, gloss) in &block.body {
        let lead = format!("    {} ", pad_right(part, key_width));
        let lead_width = display_width(&lead);
        let avail = content.saturating_sub(lead_width + 4).max(8);
        for (index, piece) in wrap(gloss, avail).into_iter().enumerate() {
            let head = if index == 0 {
                Span::styled(lead.clone(), theme::code_param())
            } else {
                Span::styled(" ".repeat(lead_width), theme::code_param())
            };
            let arrow = if index == 0 { "-> " } else { "   " };
            rows.push(vec![
                head,
                Span::styled(arrow, theme::code_punct()),
                Span::styled(piece, theme::code_string()),
            ]);
        }
    }

    // Blank line before the closing brace, mirroring the opening one.
    rows.push(Vec::new());

    // Closing brace plus attribution as a trailing comment.
    let mut close = vec![Span::styled("}", theme::code_punct())];
    if let Some(source) = &block.source {
        close.push(Span::styled("  //  ", theme::code_comment()));
        close.push(Span::styled(source.clone(), theme::code_comment()));
    }
    rows.extend(wrap_spans(&close, content));

    rows.into_iter().map(|row| pad_panel(row, inner)).collect()
}

/// Paint the panel tint behind a row and pad it out to the full width.
fn pad_panel(row: Vec<Span<'static>>, inner: usize) -> Vec<Span<'static>> {
    let used: usize = row.iter().map(|span| display_width(&span.content)).sum();
    let mut spans = vec![Span::styled(" ", theme::panel_fill())];
    for span in row {
        // Keep each token's foreground, add the panel background.
        spans.push(Span::styled(
            span.content.to_string(),
            span.style.patch(theme::panel_fill()),
        ));
    }
    let pad = inner.saturating_sub(used + 2);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), theme::panel_fill()));
    }
    spans.push(Span::styled(" ", theme::panel_fill()));
    spans
}

/// One example: the English sentence (with the headword marked), then the
/// translation underneath in grey.
fn example_rows(
    index: usize,
    en: &str,
    zh: Option<&str>,
    highlight: Option<&str>,
    width: usize,
) -> Vec<Vec<Span<'static>>> {    let mut rows = Vec::new();
    // `1 ` gutter, then a space: continuation lines align under the sentence, not
    // under the number.
    let gutter = format!("{index} ");
    let indent = display_width(&gutter);
    let body = width.saturating_sub(indent).max(8);
    let pad = Span::styled(" ".repeat(indent), Style::default());

    // Wrap first, then colour: colouring before wrapping would split a highlighted
    // word across lines and lose the span boundary.
    let segments = highlighted_segments(en, highlight);
    for (line_no, row) in wrap_spans(&segments, body).into_iter().enumerate() {
        let lead = if line_no == 0 {
            Span::styled(gutter.clone(), theme::muted())
        } else {
            pad.clone()
        };
        let mut spans = vec![lead];
        spans.extend(row);
        rows.push(spans);
    }

    if let Some(zh) = zh {
        for piece in wrap(zh, body) {
            rows.push(vec![pad.clone(), Span::styled(piece, theme::muted())]);
        }
    }

    rows
}

/// One dictionary sense: `1  n.   银行, 堤, 岸`, wrapping under the gloss column.
///
/// `index == 0` renders an unnumbered secondary sense, whose label is the bracketed
/// domain tag instead of a part of speech.
fn sense_rows(index: usize, label: &str, text: &str, width: usize) -> Vec<Vec<Span<'static>>> {
    let gutter = if index > 0 {
        format!("{index} ")
    } else {
        "  ".to_string()
    };
    // Wide enough for `[网络]`, narrow enough to leave room for the gloss.
    let label_width = if index > 0 { 5 } else { 7 };
    let lead = format!("{gutter}{}", pad_right(label, label_width));
    let lead_width = display_width(&lead);
    let body = width.saturating_sub(lead_width + 2).max(8);

    let mut rows = Vec::new();
    for (line_no, piece) in wrap(text, body).into_iter().enumerate() {
        let head = if line_no == 0 {
            Span::styled(lead.clone(), theme::muted())
        } else {
            Span::styled(" ".repeat(lead_width), Style::default())
        };
        rows.push(vec![head, Span::styled(piece, theme::text())]);
    }
    rows
}

/// A fixed expression: `for the sake of  →  为了`.
fn phrase_rows(phrase: &str, gloss: &str, width: usize) -> Vec<Vec<Span<'static>>> {
    const INDENT: usize = 2;
    // Phrases are short but can be long; cap the column so the gloss still fits.
    let phrase_width = display_width(phrase).clamp(8, 26);
    let lead_width = INDENT + phrase_width;
    let body = width.saturating_sub(lead_width + 4).max(8);

    let mut rows = Vec::new();
    if gloss.trim().is_empty() {
        // 只有搭配、没有释义时，整行留给搭配本身（词典里常见，比如朗道的「相关词组」）。
        return wrap(phrase, width.saturating_sub(INDENT).max(8))
            .into_iter()
            .map(|piece| {
                vec![
                    Span::styled(" ".repeat(INDENT), Style::default()),
                    Span::styled(piece, theme::value()),
                ]
            })
            .collect();
    }

    for (line_no, piece) in wrap(gloss, body).into_iter().enumerate() {
        let head = if line_no == 0 {
            Span::styled(
                format!("{}{}", " ".repeat(INDENT), pad_right(phrase, phrase_width)),
                theme::value(),
            )
        } else {
            Span::styled(" ".repeat(lead_width), Style::default())
        };
        let arrow = if line_no == 0 { " -> " } else { "    " };
        rows.push(vec![
            head,
            Span::styled(arrow, theme::faint()),
            Span::styled(piece, theme::text()),
        ]);
    }
    rows
}

/// One English gloss: `• Japanese rice wine   archaic`.
fn gloss_rows(text: &str, tags: &[String], width: usize) -> Vec<Vec<Span<'static>>> {
    const BULLET: &str = "  • ";
    let suffix = if tags.is_empty() {
        String::new()
    } else {
        format!("  [{}]", tags.join(", "))
    };
    let suffix_width = display_width(&suffix);
    let body = width
        .saturating_sub(display_width(BULLET) + suffix_width)
        .max(8);

    let mut rows = Vec::new();
    for (line_no, piece) in wrap(text, body).into_iter().enumerate() {
        let lead = if line_no == 0 {
            Span::styled(BULLET.to_string(), theme::faint())
        } else {
            Span::styled(" ".repeat(display_width(BULLET)), Style::default())
        };
        let mut row = vec![lead, Span::styled(piece, theme::text())];
        // Tags ride on the last line so they stay next to the text they describe.
        if line_no + 1 == wrap(text, body).len() && !suffix.is_empty() {
            row.push(Span::styled(suffix.clone(), theme::muted()));
        }
        rows.push(row);
    }
    rows
}

/// Split a sentence into styled pieces, marking the headword and its inflections.
fn highlighted_segments(text: &str, highlight: Option<&str>) -> Vec<Span<'static>> {
    let Some(lemma) = highlight.filter(|l| !l.is_empty()) else {
        return vec![Span::styled(text.to_string(), theme::text())];
    };

    let mut spans = Vec::new();
    for (is_word, run) in text_runs(text) {
        if is_word && matches_headword(&run, lemma) {
            spans.push(Span::styled(run, theme::mark()));
        } else {
            spans.push(Span::styled(run, theme::text()));
        }
    }
    spans
}

/// Group the text into alternating word and non-word runs so styles can be applied
/// per word without breaking the surrounding punctuation.
fn text_runs(text: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for ch in text.chars() {
        let is_word = ch.is_alphanumeric() || ch == '\'' || ch == '-';
        match out.last_mut() {
            Some((kind, run)) if *kind == is_word => run.push(ch),
            _ => out.push((is_word, ch.to_string())),
        }
    }
    out
}

/// Does `token` refer to `lemma`? Accepts the common English inflections.
fn matches_headword(token: &str, lemma: &str) -> bool {
    let token = token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let lemma = lemma.trim().to_lowercase();
    if token.is_empty() || lemma.is_empty() {
        return false;
    }
    if token == lemma {
        return true;
    }
    // Only inflect when the token actually starts with the lemma.
    let Some(rest) = token.strip_prefix(lemma.as_str()) else {
        return false;
    };
    matches!(rest, "s" | "es" | "ed" | "d" | "ing" | "er" | "est" | "ly" | "ies")
        // `happy` → `happier`: the stem drops a `y`.
        || (lemma.ends_with('y') && token.starts_with(&lemma[..lemma.len() - 1]))
        // `run` → `running`: the final consonant doubles.
        || {
            lemma.chars().last().map(|c| c.is_alphabetic()).unwrap_or(false)
                && rest.starts_with('s') == false
                && token
                    .strip_prefix(&format!("{}{}", lemma, lemma.chars().last().unwrap()))
                    .map(|tail| matches!(tail, "ed" | "ing" | "er" | "est"))
                    .unwrap_or(false)
        }
}

/// Wrap already-styled spans to `max` columns, keeping each span's style.
/// Wrap styled text on word boundaries.
///
/// `wrap` cannot be used for highlighted sentences, because splitting a string
/// would lose the span styles. This flattens the spans to styled characters, runs
/// the same greedy word-wrap as `wrap`, then re-groups runs that share a style.
fn wrap_spans(spans: &[Span<'static>], max: usize) -> Vec<Vec<Span<'static>>> {
    let max = max.max(8);
    let flat: Vec<(char, Style)> = spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |ch| (ch, span.style)))
        .collect();

    if flat.is_empty() {
        return vec![Vec::new()];
    }

    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut line: Vec<(char, Style)> = Vec::new();
    let mut width = 0usize;
    let mut last_space: Option<usize> = None;

    for (ch, style) in flat {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max && width > 0 {
            match last_space {
                // Break at the last space, dropping it; the remainder starts the
                // next line.
                Some(index) if index > 0 => {
                    let rest = line.split_off(index + 1);
                    line.truncate(index);
                    rows.push(coalesce(&line));
                    line = rest;
                }
                // A single word longer than the line: hard break, unavoidable.
                _ => {
                    rows.push(coalesce(&line));
                    line.clear();
                }
            }
            width = line
                .iter()
                .map(|(c, _)| UnicodeWidthChar::width(*c).unwrap_or(0))
                .sum();
            last_space = line.iter().rposition(|(c, _)| *c == ' ');
        }

        line.push((ch, style));
        width += cw;
        if ch == ' ' {
            last_space = Some(line.len() - 1);
        }
    }

    if !line.is_empty() {
        rows.push(coalesce(&line));
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

/// Merge consecutive characters that share a style back into spans.
fn coalesce(chars: &[(char, Style)]) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut buffer = String::new();
    let mut current: Option<Style> = None;

    for (ch, style) in chars {
        match current {
            Some(active) if active == *style => buffer.push(*ch),
            Some(active) => {
                out.push(Span::styled(std::mem::take(&mut buffer), active));
                buffer.push(*ch);
                current = Some(*style);
            }
            None => {
                buffer.push(*ch);
                current = Some(*style);
            }
        }
    }

    if let Some(active) = current {
        if !buffer.is_empty() {
            out.push(Span::styled(buffer, active));
        }
    }
    out
}

fn row_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

/// Lay the left and right columns out side by side, padding the left one out to a
/// fixed width so the right column stays aligned.
fn join_columns(
    rail: Style,
    left: &mut Vec<Vec<Span<'static>>>,
    right: Vec<Vec<Span<'static>>>,
    left_width: usize,
) -> Vec<Line<'static>> {
    let rows = left.len().max(right.len());
    let mut out = Vec::with_capacity(rows);

    for index in 0..rows {
        let mut spans = Vec::new();
        if let Some(row) = left.get_mut(index) {
            let used = row_width(row);
            spans.append(row);
            if used < left_width {
                spans.push(Span::styled(" ".repeat(left_width - used), Style::default()));
            }
        } else {
            spans.push(Span::styled(" ".repeat(left_width), Style::default()));
        }
        spans.push(Span::styled("│ ", theme::faint()));
        if let Some(row) = right.get(index) {
            spans.extend(row.clone());
        }
        out.push(rail_line(rail, 0, spans));
    }

    out
}


fn table_lines(table: &TableView, width: usize) -> Vec<Line<'static>> {
    let rail = theme::faint();
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
        out.push(render_row(&table.headers, theme::muted()));
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
    fn a_phrase_without_a_gloss_still_renders_one_entry() {
        // 词典抽出来的搭配只有词组、没有释义，不能渲染出空箭头。
        let rows = phrase_rows("for God's sake", "", 60);
        let text: Vec<String> = rows
            .into_iter()
            .map(|spans| spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text.len(), 1);
        assert!(text[0].contains("for God's sake"));
        assert!(!text[0].contains("->"));
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

    #[test]
    fn a_card_message_renders_through_the_rail() {
        // Cards are stored as state and painted on demand, which is what lets
        // `m` fold or unfold every one of them at once.
        let mut word = qe_core::model::Word::new("sake", "n. 目的");
        word.glosses = vec![qe_core::text::Gloss::new("Rice wine.")];
        let state = qe_core::view::CardState::new(word, "1/9").revealed();

        let text = flat(&message_lines(&Message::card(state.clone()), 80));
        assert!(text[0].starts_with('│'));
        assert!(
            text.iter().any(|line| line.contains("more meanings · 1 · m")),
            "folded by default: {text:?}"
        );

        let mut open = state;
        open.expand_glosses = true;
        let text = flat(&message_lines(&Message::card(open), 80));
        assert!(text.iter().any(|line| line.contains("Rice wine.")));
    }
}
