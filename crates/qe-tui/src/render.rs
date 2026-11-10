//! Single-frame rendering.
//!
//! Deliberately restrained, like a CLI agent: a one-line header, an unbordered
//! conversation stream, an optional suggestion list, one rule, one input line and
//! one dim footer. A scrollbar appears only when the transcript overflows.

use qe_core::view::Message;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::Frame;

use crate::lines::{display_width, tail, truncate};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Input,
    Study,
}

/// One inline completion hint: `(prefix, description)`.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub name: String,
    pub args: String,
    pub help: String,
    pub selected: bool,
}

pub struct View<'a> {
    pub messages: &'a [Message],
    pub input: &'a str,
    pub input_cursor: usize,
    pub mode: ViewMode,
    pub title: &'a str,
    pub context: Option<&'a str>,
    pub meta: Option<&'a str>,
    pub status: &'a str,
    pub placeholder: &'a str,
    /// Dimmed completion of the word currently being typed.
    pub ghost: Option<&'a str>,
    pub suggestions: &'a [Suggestion],
    pub scroll_back: u16,
    /// Blink phase of the input caret.
    pub cursor_on: bool,
    /// Messages arrived while scrolled back.
    pub unread: u16,
}

/// Geometry captured during a draw so the app can map mouse events back to it.
#[derive(Debug, Clone, Default)]
pub struct DrawInfo {
    pub transcript: Rect,
    pub scrollbar_x: Option<u16>,
    pub max_back: u16,
    /// Screen row of the first transcript line (after padding/scroll).
    pub first_row: u16,
    /// For each visible transcript row, the message index it belongs to.
    /// `u16::MAX` marks padding above the transcript.
    pub row_message: Vec<usize>,
    /// Line offset of each message inside its own block, parallel to `row_message`.
    pub row_offset: Vec<usize>,
}

pub fn draw(frame: &mut Frame, view: &View) -> DrawInfo {
    let area = frame.area();
    // Reset every cell before painting. Without this, rows that a shorter layout no
    // longer reaches keep whatever the previous frame drew there.
    frame.render_widget(ratatui::widgets::Clear, area);
    if area.width < 30 || area.height < 8 {
        frame.render_widget(
            Paragraph::new("terminal too small · resize to at least 30x8").style(theme::warn()),
            area,
        );
        return DrawInfo::default();
    }

    // One row per suggestion, capped so the transcript keeps at least 3 rows.
    let suggestion_rows = view
        .suggestions
        .len()
        .min(5)
        .min((area.height as usize).saturating_sub(8)) as u16;

    let rows = Layout::vertical([
        Constraint::Length(1),               // header
        Constraint::Min(3),                  // transcript
        Constraint::Length(suggestion_rows), // completions
        Constraint::Length(1),               // separator above the input
        Constraint::Length(1),               // input
        Constraint::Length(1),               // footer
    ])
    .split(area);

    render_header(frame, rows[0], view);
    let info = render_transcript(frame, rows[1], view);
    if suggestion_rows > 0 {
        render_suggestions(frame, rows[2], view);
    }
    render_rule(frame, rows[3]);
    render_input(frame, rows[4], view);
    render_footer(frame, rows[5], view);
    info
}

fn render_header(frame: &mut Frame, area: Rect, view: &View) {
    let mut spans: Vec<Span> = Vec::new();
    if !view.title.is_empty() {
        spans.push(Span::styled(view.title.to_string(), theme::bold()));
    }
    if let Some(context) = view.context {
        if !spans.is_empty() {
            spans.push(Span::styled("  ", theme::dim()));
        }
        spans.push(Span::styled(context.to_string(), theme::dim()));
    }
    let left_width: usize = spans.iter().map(|s| display_width(&s.content)).sum();
    let meta = view.meta.unwrap_or("");
    let pad = (area.width as usize)
        .saturating_sub(left_width)
        .saturating_sub(display_width(meta) + 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(format!("{meta} "), theme::dim()));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_transcript(frame: &mut Frame, area: Rect, view: &View) -> DrawInfo {
    let width = area.width as usize;
    let height = area.height as usize;
    let mut rendered: Vec<Line<'static>> = Vec::new();
    // Parallel array: which message each rendered line came from, and the line's
    // offset inside that message's block. Used for click-to-select.
    let mut owners: Vec<(usize, usize)> = Vec::new();

    let mut previous_was_text = false;
    for (index, message) in view.messages.iter().enumerate() {
        let is_text = matches!(message.body, qe_core::view::Body::Text(_));
        // A blank line before every block makes successive cards read as separate
        // units instead of one long run of text.
        if index > 0 && !(is_text && previous_was_text) {
            rendered.push(Line::from(""));
            owners.push((index, usize::MAX));
        }
        let block = crate::lines::message_lines(message, width);
        for (offset, line) in block.into_iter().enumerate() {
            rendered.push(line);
            owners.push((index, offset));
        }
        previous_was_text = is_text;
    }

    if rendered.len() < height {
        let pad = height - rendered.len();
        let mut padded: Vec<Line<'static>> = vec![Line::from(""); pad];
        let mut pad_owners: Vec<(usize, usize)> = vec![(usize::MAX, usize::MAX); pad];
        padded.append(&mut rendered);
        pad_owners.append(&mut owners);
        rendered = padded;
        owners = pad_owners;
    }

    let total = rendered.len();
    let max_back = total.saturating_sub(height) as u16;
    let back = view.scroll_back.min(max_back);
    let offset = max_back - back;

    frame.render_widget(Paragraph::new(rendered).scroll((offset, 0)), area);

    let visible: Vec<(usize, usize)> = owners
        .into_iter()
        .skip(offset as usize)
        .take(height)
        .collect();
    let row_message = visible.iter().map(|(msg, _)| *msg).collect();
    let row_offset = visible.iter().map(|(_, off)| *off).collect();

    let mut scrollbar_x = None;
    if max_back > 0 {
        let x = area.x + area.width.saturating_sub(1);
        scrollbar_x = Some(x);
        let mut state = ScrollbarState::new(total)
            .viewport_content_length(height)
            .position(offset as usize);
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .style(theme::muted());
        frame.render_stateful_widget(bar, area, &mut state);
    }

    DrawInfo {
        transcript: area,
        scrollbar_x,
        max_back,
        first_row: area.y,
        row_message,
        row_offset,
    }
}

fn render_suggestions(frame: &mut Frame, area: Rect, view: &View) {
    let name_width = view
        .suggestions
        .iter()
        .map(|s| display_width(&s.name) + 1 + display_width(&s.args))
        .max()
        .unwrap_or(0)
        .min(24);

    let mut lines = Vec::with_capacity(view.suggestions.len());
    for suggestion in view.suggestions {
        let marker = if suggestion.selected { "› " } else { "  " };
        let name = format!("{}{}", suggestion.name, suggestion.args);
        let style = if suggestion.selected {
            theme::bold()
        } else {
            theme::muted()
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), theme::accent_plain()),
            Span::styled(format!("{:<name_width$}", name), style),
            Span::styled(format!("  {}", suggestion.help), theme::dim()),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Prompt shown before the editable text.
const PROMPT: &str = "› ";

/// Hairline that separates the transcript from the input. Kept a single dim row so
/// the input reads as its own region without turning into a framed box.
fn render_rule(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "─".repeat(area.width as usize),
        theme::dim(),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

fn render_input(frame: &mut Frame, area: Rect, view: &View) {
    let budget = (area.width as usize)
        .saturating_sub(display_width(PROMPT))
        .max(1);

    // Empty input: the caret comes first and the placeholder follows it, so it is
    // obvious where typing will appear.
    if view.input.is_empty() {
        let caret = if view.cursor_on {
            Span::styled(" ", theme::cursor())
        } else {
            Span::raw(" ")
        };
        let line = Line::from(vec![
            Span::styled(PROMPT, theme::user()),
            caret,
            Span::styled(view.placeholder.to_string(), theme::dim()),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let chars: Vec<char> = view.input.chars().collect();
    let cursor = view.input_cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor..].iter().collect();
    let at_end = cursor == chars.len();
    let ghost = if at_end { view.ghost.unwrap_or("") } else { "" };

    // Scroll horizontally only as far as needed to keep the caret on screen.
    let total = display_width(&before) + display_width(&after) + display_width(ghost);
    let (before, scrolled) = if total <= budget {
        (before, false)
    } else {
        let keep = budget.saturating_sub(1); // leave room for the caret cell
        let shown = tail(&before, keep);
        let scrolled = shown != before;
        (shown, scrolled)
    };

    let mut spans = vec![Span::styled(PROMPT, theme::user())];
    if scrolled {
        spans.push(Span::styled("…", theme::dim()));
    }
    spans.push(Span::styled(before, theme::text()));

    // The caret is drawn *on* the character it edits rather than inserted between
    // characters, so the column always matches the insertion point.
    match chars.get(cursor) {
        Some(ch) => spans.push(Span::styled(
            ch.to_string(),
            if view.cursor_on {
                theme::cursor()
            } else {
                theme::text()
            },
        )),
        None if view.cursor_on => spans.push(Span::styled(" ", theme::cursor())),
        None => {}
    }

    let rest_start = (cursor + 1).min(chars.len());
    spans.push(Span::styled(
        chars[rest_start..].iter().collect::<String>(),
        theme::text(),
    ));
    if !ghost.is_empty() {
        spans.push(Span::styled(ghost.to_string(), theme::dim()));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_footer(frame: &mut Frame, area: Rect, view: &View) {
    let hint = match view.mode {
        ViewMode::Study => "1-4 grade · enter/s next · tab hint · esc end",
        ViewMode::Input => "tab complete · ↑↓ history · ctrl+c quit",
    };
    let status = if view.unread > 0 {
        format!("↓ {} new · end to jump", view.unread)
    } else {
        view.status.to_string()
    };
    let left = truncate(&status, area.width as usize);
    let left_width = display_width(&left);
    let hint_width = display_width(hint);
    let pad = (area.width as usize)
        .saturating_sub(left_width)
        .saturating_sub(hint_width + 1);
    let line = Line::from(vec![
        Span::styled(format!(" {left}"), theme::muted()),
        Span::raw(" ".repeat(pad)),
        Span::styled(format!("{hint} "), theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_core::model::{RootKind, Word, WordRoot};
    use qe_core::view::{CardState, TableView, ToolView};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_to_string(view: &View, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| {
            draw(frame, view);
        })
        .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn sample_word() -> Word {
        let mut word = Word::new("inspect", "v. 检查；视察");
        word.phonetic = Some("/ɪnˈspekt/".into());
        word.roots = vec![
            WordRoot::new("in-", "inward", RootKind::Prefix),
            WordRoot::new("spect", "look", RootKind::Root),
        ];
        word
    }

    #[test]
    fn renders_dialog_without_panic() {
        let messages = vec![
            Message::tool(ToolView::new("ready").badge("CET-4 Core").result("40 words")),
            Message::user("inspect"),
            Message::tool(ToolView::word_card(
                &CardState::new(sample_word(), "lookup").revealed(),
            )),
            Message::table(
                TableView::new(
                    vec!["word".into(), "meaning".into()],
                    vec![vec!["inspect".into(), "检查".into()]],
                )
                .caption("1 match"),
            ),
            Message::hint("1-4 to grade"),
        ];
        let input = "/study".to_string();
        let suggestions = vec![Suggestion {
            name: "/study".into(),
            args: " [n]".into(),
            help: "start a session".into(),
            selected: true,
        }];
        let view = View {
            messages: &messages,
            input: &input,
            input_cursor: 3,
            mode: ViewMode::Input,
            title: "quietly",
            context: Some("CET-4 Core"),
            meta: Some("due 8 · new 4/20 · streak 3"),
            status: "ready",
            placeholder: "ask anything · /help",
            ghost: Some("dy"),
            suggestions: &suggestions,
            scroll_back: 0,
            cursor_on: true,
            unread: 0,
        };
        let text = render_to_string(&view, 100, 24);
        assert!(text.contains("CET-4"));
        assert!(text.contains('│'), "blocks should ride a rail:\n{text}");
        assert!(text.contains("spect"), "roots missing:\n{text}");
        assert!(text.contains("inspect"), "lemma missing:\n{text}");
        assert!(text.contains("start a session"), "hint missing:\n{text}");

        let narrow = render_to_string(&view, 44, 16);
        assert!(narrow.contains('│'));

        let tiny = render_to_string(&view, 20, 5);
        assert!(tiny.contains("too small"));
    }

    #[test]
    fn scrollbar_appears_when_overflowing() {
        let messages: Vec<Message> = (0..40)
            .map(|i| Message::tool(ToolView::new("word").badge(format!("{i}/40")).result("inspect")))
            .collect();
        let view = View {
            messages: &messages,
            input: "",
            input_cursor: 0,
            mode: ViewMode::Study,
            title: "quietly",
            context: None,
            meta: None,
            status: "",
            placeholder: "",
            ghost: None,
            suggestions: &[],
            scroll_back: 0,
            cursor_on: true,
            unread: 0,
        };
        let text = render_to_string(&view, 60, 12);
        assert!(text.contains('█'), "thumb should be visible:\n{text}");
    }

    #[test]
    fn cards_are_separated_by_blank_lines() {
        let messages = vec![
            Message::tool(ToolView::new("word").badge("1/2").result("alpha")),
            Message::tool(ToolView::new("word").badge("2/2").result("beta")),
        ];
        let view = View {
            messages: &messages,
            input: "",
            input_cursor: 0,
            mode: ViewMode::Study,
            title: "quietly",
            context: None,
            meta: None,
            status: "",
            placeholder: "",
            ghost: None,
            suggestions: &[],
            scroll_back: 0,
            cursor_on: true,
            unread: 0,
        };
        let text = render_to_string(&view, 40, 10);
        let alpha = text.lines().position(|l| l.contains("alpha")).unwrap();
        let beta = text.lines().position(|l| l.contains("beta")).unwrap();
        assert!(beta >= alpha + 3, "blocks need breathing room:\n{text}");
    }

    fn input_view<'a>(
        input: &'a str,
        cursor: usize,
        placeholder: &'a str,
        cursor_on: bool,
    ) -> View<'a> {
        View {
            messages: &[],
            input,
            input_cursor: cursor,
            mode: ViewMode::Input,
            title: "",
            context: None,
            meta: None,
            status: "",
            placeholder,
            ghost: None,
            suggestions: &[],
            scroll_back: 0,
            cursor_on,
            unread: 0,
        }
    }

    #[test]
    fn caret_does_not_shift_text() {
        // The caret is drawn over the character it edits, so the line must read the
        // same whether the blink phase is on or off.
        for cursor_on in [true, false] {
            let view = input_view("abc", 1, "", cursor_on);
            let text = render_to_string(&view, 40, 10);
            let line = text.lines().find(|l| l.contains("abc")).expect("input line");
            assert!(line.contains("› abc"), "caret shifted text: {line:?}");
        }
    }

    #[test]
    fn caret_at_end_adds_a_single_cell() {
        // At end of line the caret needs its own cell; with the blink off nothing is
        // drawn, so the cell keeps the default background.
        let on = buffer_of(&input_view("abc", 3, "", true), 40, 10);
        let off = buffer_of(&input_view("abc", 3, "", false), 40, 10);
        let row = 10 - 2; // footer is the last row
        assert_eq!(on[(5, row)].bg, theme::ACCENT, "caret cell missing");
        assert_ne!(off[(5, row)].bg, theme::ACCENT, "caret should be hidden");
    }

    fn buffer_of(view: &View, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, view);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn empty_input_shows_caret_before_placeholder() {
        let text = render_to_string(&input_view("", 0, "type a word", true), 40, 10);
        let line = text.lines().find(|l| l.contains("type a word")).unwrap();
        let prompt = line.find('›').unwrap();
        let caret = line.find("type a word").unwrap();
        assert!(caret > prompt, "caret must sit between prompt and hint: {line:?}");
    }
}
