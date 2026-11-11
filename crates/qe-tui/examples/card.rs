//! 渲染一张真实单词卡，检查两栏布局与高亮。
//!
//! ```bash
//! cargo run -p qe-tui --example card [-- 100 24]
//! ```

use qe_core::model::{Example, RootKind, Word, WordRoot};
use qe_core::view::{CardState, Message, ToolView};
use qe_tui::{draw, View, ViewMode};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn main() {
    let mut args = std::env::args().skip(1);
    let width: u16 = args.next().and_then(|v| v.parse().ok()).unwrap_or(100);
    let height: u16 = args.next().and_then(|v| v.parse().ok()).unwrap_or(26);

    let mut word = Word::new("inspect", "vt. 检查，检验；视察");
    word.phonetic = Some("/ɪn'spekt/".into());
    word.roots = vec![
        WordRoot::new("in-", "in, into, towards, within", RootKind::Prefix)
            .with_source("Wiktionary {{confix}}"),
        WordRoot::new("spect", "To look at, perceive, or observe", RootKind::Root)
            .with_source("Wiktionary {{confix}}"),
    ];
    word.examples = vec![
        Example::new(
            "Engineers inspect the bridge every year.",
            Some("工程师每年检查这座桥。".into()),
        ),
        Example::new(
            "She inspected the room before leaving.",
            Some("她离开前检查了房间。".into()),
        ),
        Example::new(
            "The officer will inspect our passports.",
            Some("官员将检查我们的护照。".into()),
        ),
    ];

    let state = CardState::new(word, "new · 3/12").revealed();
    let messages = vec![Message::tool(ToolView::word_card(&state))];

    let view = View {
        messages: &messages,
        input: "",
        input_cursor: 0,
        mode: ViewMode::Study,
        title: "",
        context: Some("CET-6 核心词"),
        meta: Some("due 5 · new 3/20 · streak 4"),
        status: "inspect · 3/12 · 1-4 grade",
        placeholder: "",
        ghost: None,
        suggestions: &[],
        suggestions_more: 0,
        scroll_back: 0,
        cursor_on: false,
        unread: 0,
    };

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            draw(frame, &view);
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        println!("{}", line.trim_end());
    }
}
