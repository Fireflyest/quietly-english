//! Application state machine: turns input into `Message`s and drives study sessions.
//! Nothing here paints the screen; it only produces view models.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use qe_core::model::{Book, Rating, SessionLog, SrsCard, StudyCard, Word};
use qe_core::view::{summary_tool, Accent, CardState, Message, SummaryStatus, TableView, ToolView};
use qe_core::{srs, time, Store};
use qe_tui::{draw, DrawInfo, Suggestion, TerminalSession, View, ViewMode};

use crate::command::{self, Command};
use crate::editor::Editor;
use crate::session::StudySession;

const DUE_BATCH: usize = 300;
const MAX_SUGGESTIONS: usize = 5;
/// Caret blink half-period. Typing resets the phase so the caret is always visible
/// right after a keystroke.
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// How many ratings can be undone in one session.
const MAX_UNDO: usize = 50;

/// One reversible grading action.
struct UndoEntry {
    word_id: i64,
    card: StudyCard,
    before: Option<SrsCard>,
    /// Message index of the card block, so undo can jump back to it.
    message: usize,
    was_new: bool,
}

pub struct App {
    store: Box<dyn Store>,
    book: Option<Book>,
    messages: Vec<Message>,
    editor: Editor,
    session: Option<StudySession>,
    status: String,
    scroll_back: u16,
    quit: bool,
    daily_new_limit: u32,
    /// Discreet mode: hide the deck name so the window looks like a plain agent.
    discreet: bool,
    draw_info: DrawInfo,
    dragging: bool,
    cursor_on: bool,
    last_blink: Instant,
    /// Highlighted row in the completion list.
    suggestion_index: usize,
    /// Newest messages that arrived while the view was scrolled back.
    unread: u16,
    /// `/exit` needs a second esc when a card is mid-flight.
    confirm_exit: bool,
    /// Words behind each table message, for click-to-open and numeric picking.
    table_words: HashMap<usize, Vec<Word>>,
    undo: Vec<UndoEntry>,
}

impl App {
    pub fn new(store: Box<dyn Store>) -> Result<Self> {
        let book = store.current_book()?;
        let daily_new_limit = store
            .setting("daily_new_limit")?
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(20)
            .clamp(1, 500);
        let discreet = store
            .setting("discreet")?
            .map(|value| value != "0")
            .unwrap_or(true);

        let mut app = Self {
            store,
            book,
            messages: Vec::new(),
            editor: Editor::new(),
            session: None,
            status: "ready".to_string(),
            scroll_back: 0,
            quit: false,
            daily_new_limit,
            discreet,
            draw_info: DrawInfo::default(),
            dragging: false,
            cursor_on: true,
            last_blink: Instant::now(),
            suggestion_index: 0,
            unread: 0,
            confirm_exit: false,
            table_words: HashMap::new(),
            undo: Vec::new(),
        };
        app.load_history();
        app.greet()?;
        Ok(app)
    }

    /// Main loop. Polls at 120ms so resizes and mouse drags feel responsive.
    pub fn run(mut self, terminal: &mut TerminalSession) -> Result<()> {
        while !self.quit {
            self.tick_blink();
            let input = self.editor.text();
            let context = if self.discreet {
                None
            } else {
                self.book.as_ref().map(|book| book.name.clone())
            };
            let meta = self.meta_line();
            let ghost = command::ghost(&input);
            let suggestions = build_suggestions(&input, self.suggestion_index);
            let mode = if self.session.is_some() {
                ViewMode::Study
            } else {
                ViewMode::Input
            };
            let placeholder = if self.session.is_some() {
                "1-4 grade · tab hint · esc end"
            } else {
                "type a word, or / for commands"
            };

            let view = View {
                messages: &self.messages,
                input: &input,
                input_cursor: self.editor.cursor(),
                mode,
                title: "",
                context: context.as_deref(),
                meta: meta.as_deref(),
                status: &self.status,
                placeholder,
                ghost: ghost.as_deref(),
                suggestions: &suggestions,
                scroll_back: self.scroll_back,
                cursor_on: self.cursor_on,
                unread: self.unread,
            };

            let mut info = DrawInfo::default();
            terminal.terminal.draw(|frame| {
                info = draw(frame, &view);
            })?;
            self.draw_info = info;

            if event::poll(Duration::from_millis(120))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        self.handle_key(key)?;
                    }
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    Event::Resize(_, _) => {
                        // The back buffer is stale after a resize; repaint from scratch.
                        terminal.clear()?;
                    }
                    Event::Paste(text) => {
                        // The editor is single-line: fold newlines and tabs into spaces
                        // instead of corrupting the cursor position.
                        for ch in text.chars() {
                            if ch == '\r' {
                                continue;
                            }
                            if ch == '\n' || ch == '\t' {
                                self.editor.insert_char(' ');
                            } else {
                                self.editor.insert_char(ch);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.save_history();
        // Quitting mid-session must not lose the time already spent.
        if self.session.is_some() {
            self.finish_session()?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- render state

    /// Flip the caret phase once per blink interval. Polling stays at 120ms so
    /// resizes and mouse drags remain responsive.
    fn tick_blink(&mut self) {
        if self.last_blink.elapsed() >= BLINK_INTERVAL {
            self.cursor_on = !self.cursor_on;
            self.last_blink = Instant::now();
        }
    }

    /// Any edit or navigation restarts the blink so the caret is immediately visible.
    fn wake_cursor(&mut self) {
        self.cursor_on = true;
        self.last_blink = Instant::now();
    }

    fn meta_line(&self) -> Option<String> {
        let book = self.book.as_ref()?;
        let progress = self.store.progress(book.id).ok()?;
        let streak = self.store.streak().unwrap_or(0);
        let today = self.store.learned_today(book.id).unwrap_or(0);
        Some(format!(
            "due {} · new {}/{} · streak {}",
            progress.due, today, self.daily_new_limit, streak
        ))
    }

    // ---------------------------------------------------------------- input

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.wake_cursor();
        // Any keypress cancels a pending "press esc again".
        let was_confirming = self.confirm_exit;

        let suggestion_count = command::matches(&self.editor.text()).len().min(MAX_SUGGESTIONS);

        // Global keys come first.
        match key.code {
            KeyCode::Char('c') if ctrl => {
                return self.request_quit();
            }
            KeyCode::Char('d') if ctrl && self.editor.is_empty() => {
                return self.request_quit();
            }
            KeyCode::Char('l') if ctrl => {
                self.messages.clear();
                self.table_words.clear();
                self.undo.clear();
                self.greet()?;
                return Ok(());
            }
            KeyCode::PageUp => {
                self.scroll_by(10);
                return Ok(());
            }
            KeyCode::PageDown => {
                self.scroll_by(-10);
                return Ok(());
            }
            KeyCode::End if self.scroll_back > 0 => {
                self.scroll_back = 0;
                self.unread = 0;
                return Ok(());
            }
            KeyCode::Up if suggestion_count > 0 => {
                self.suggestion_index =
                    (self.suggestion_index + suggestion_count - 1) % suggestion_count;
                return Ok(());
            }
            KeyCode::Down if suggestion_count > 0 => {
                self.suggestion_index = (self.suggestion_index + 1) % suggestion_count;
                return Ok(());
            }
            _ => {}
        }

        // Study shortcuts, only while the input line is empty so commands still work.
        if self.session.is_some() && self.editor.is_empty() {
            match key.code {
                KeyCode::Char(ch @ '1'..='4') => {
                    self.grade(rating_from_char(ch))?;
                    return Ok(());
                }
                KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Right => {
                    self.advance()?;
                    return Ok(());
                }
                KeyCode::Tab => {
                    self.toggle_hint()?;
                    return Ok(());
                }
                KeyCode::Esc => {
                    return self.request_end_session();
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                if !self.editor.is_empty() {
                    self.editor.clear();
                    self.suggestion_index = 0;
                }
            }
            KeyCode::Enter => self.submit()?,
            KeyCode::Tab => self.tab_complete(),
            KeyCode::Backspace if ctrl => self.editor.delete_word_back(),
            KeyCode::Delete if ctrl => self.editor.delete_word_forward(),
            KeyCode::Left if ctrl => self.editor.word_left(),
            KeyCode::Right if ctrl => self.editor.word_right(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            KeyCode::Up => self.editor.history_prev(),
            KeyCode::Down => self.editor.history_next(),
            KeyCode::Char('u') if ctrl => self.editor.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.editor.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.editor.kill_word(),
            KeyCode::Char('a') if ctrl => self.editor.home(),
            KeyCode::Char('e') if ctrl => self.editor.end(),
            KeyCode::Char(ch) if !ctrl => {
                self.editor.insert_char(ch);
                self.suggestion_index = 0;
            }
            _ => {}
        }

        if self.editor.text().is_empty() {
            self.suggestion_index = 0;
        }
        if was_confirming {
            self.confirm_exit = false;
        }
        Ok(())
    }

    /// Quit, but only after confirming when a session is in progress.
    fn request_quit(&mut self) -> Result<()> {
        if self.session.is_some() && !self.confirm_exit {
            self.confirm_exit = true;
            self.note("a session is running · press ctrl+c again to quit, esc to keep going");
            return Ok(());
        }
        self.quit = true;
        Ok(())
    }

    /// Esc in a session: confirm once, so a stray keypress never ends a run.
    fn request_end_session(&mut self) -> Result<()> {
        let unreviewed = self
            .session
            .as_ref()
            .map(|session| session.current.is_some() && session.graded.is_none())
            .unwrap_or(false);
        if unreviewed && !self.confirm_exit {
            self.confirm_exit = true;
            self.note("press esc again to end the session · enter to keep going");
            return Ok(());
        }
        self.confirm_exit = false;
        self.end_session()
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_by(3),
            MouseEventKind::ScrollDown => self.scroll_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                if Some(mouse.column) == self.draw_info.scrollbar_x {
                    self.dragging = true;
                    self.scrub_to(mouse.row);
                } else {
                    self.open_row_at(mouse.row);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                self.scrub_to(mouse.row);
            }
            MouseEventKind::Up(_) => self.dragging = false,
            _ => {}
        }
    }

    /// Clicking a row inside a result table opens that word.
    fn open_row_at(&mut self, row: u16) {
        let info = &self.draw_info;
        let track = info.transcript;
        if row < track.y || row >= track.y + track.height {
            return;
        }
        let idx = (row - track.y) as usize;
        let message = match info.row_message.get(idx) {
            Some(&index) if index != usize::MAX => index,
            _ => return,
        };
        let offset = info.row_offset.get(idx).copied().unwrap_or(0);
        let words = match self.table_words.get(&message) {
            Some(words) if !words.is_empty() => words.clone(),
            _ => return,
        };
        // Table block: optional caption line, then header, then rows.
        let has_caption = match self.messages.get(message) {
            Some(Message {
                body: qe_core::view::Body::Table(table),
                ..
            }) => table.caption.is_some(),
            _ => false,
        };
        let header = (has_caption as usize) + 1;
        let Some(row_index) = offset.checked_sub(header) else {
            return;
        };
        if let Some(word) = words.get(row_index) {
            let word = word.clone();
            self.push(Message::tool(ToolView::word_card(
                &CardState::new(word, "lookup").revealed(),
            )));
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let next = self.scroll_back as i32 + delta;
        self.scroll_back = next.clamp(0, self.draw_info.max_back as i32) as u16;
        if self.scroll_back == 0 {
            self.unread = 0;
        }
    }

    /// Map a click/drag on the scrollbar column to a scroll position.
    fn scrub_to(&mut self, row: u16) {
        let track = self.draw_info.transcript;
        let max_back = self.draw_info.max_back;
        if track.height == 0 || max_back == 0 {
            return;
        }
        let rel = row.saturating_sub(track.y).min(track.height.saturating_sub(1));
        let ratio = rel as f64 / (track.height.max(1) - 1).max(1) as f64;
        let offset = (ratio * max_back as f64).round() as u16;
        self.scroll_back = max_back.saturating_sub(offset);
    }

    fn tab_complete(&mut self) {
        let text = self.editor.text();

        // Completing an argument: deck slugs for /book, paths for /import.
        if let Some((name, partial)) = command::arg_completion_target(&text) {
            let candidates = self.arg_candidates(name);
            if let Some(done) = command::complete_arg(&candidates, partial) {
                self.editor.set_text(&format!("{name} {done}"));
                return;
            }
            if !candidates.is_empty() {
                let rows = candidates
                    .iter()
                    .take(12)
                    .map(|candidate| vec![candidate.clone()])
                    .collect();
                self.push(Message::table(
                    TableView::new(vec!["candidates".into()], rows)
                        .caption(format!("{name} · {} options", candidates.len())),
                ));
            }
            return;
        }

        if text.starts_with('/') {
            // No row picked yet: extend to the shared prefix. Once the user has moved
            // the highlight, tab fills that exact command.
            if self.suggestion_index == 0 {
                if let Some(done) = command::complete(&text) {
                    self.editor.set_text(&done);
                    return;
                }
            }
            let hits = command::matches(&text);
            if !hits.is_empty() {
                let index = self.suggestion_index.min(hits.len() - 1);
                let picked = hits[index].name;
                let filled = if text.trim_end() == picked {
                    picked.to_string()
                } else {
                    format!("{picked} ")
                };
                self.editor.set_text(&filled);
                self.suggestion_index = 0;
            }
            return;
        }

        if text.is_empty() && self.session.is_none() {
            self.editor.insert_char('/');
            return;
        }

        // Inside a session, tab is the hint key even with a half-typed word.
        if self.session.is_some() {
            let _ = self.toggle_hint();
        }
    }

    /// Candidate values for a command argument.
    fn arg_candidates(&self, _command: &str) -> Vec<String> {
        // Deck slugs, then files in the working directory for imports.
        let mut out: Vec<String> = self
            .store
            .list_books()
            .unwrap_or_default()
            .into_iter()
            .map(|book| book.slug)
            .collect();
        if let Ok(entries) = std::fs::read_dir(".") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let path = entry.path();
                let display = if path.is_dir() {
                    format!("{name}/")
                } else {
                    name
                };
                out.push(display);
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn submit(&mut self) -> Result<()> {
        let text = self.editor.take();
        self.suggestion_index = 0;
        // A user action always snaps the view back to the newest output.
        self.scroll_back = 0;
        self.unread = 0;
        if text.is_empty() {
            // Enter on an empty line during study advances the card.
            if self.session.is_some() {
                self.advance()?;
            }
            return Ok(());
        }
        self.push(Message::user(text.clone()));

        if let Some(command) = Command::parse(&text) {
            return self.execute(command);
        }
        if self.session.is_some() {
            return self.study_input(&text);
        }
        // A bare number picks that row of the most recent result table.
        if let Ok(pick) = text.parse::<usize>() {
            if self.open_table_pick(pick) {
                return Ok(());
            }
        }
        self.lookup(&text)
    }

    /// Open the n-th word of the newest table message (1-based).
    fn open_table_pick(&mut self, pick: usize) -> bool {
        let Some((&index, words)) = self.table_words.iter().max_by_key(|(index, _)| **index) else {
            return false;
        };
        let Some(word) = words.get(pick.saturating_sub(1)).cloned() else {
            return false;
        };
        let _ = index;
        self.push(Message::tool(ToolView::word_card(
            &CardState::new(word, "lookup").revealed(),
        )));
        true
    }

    fn study_input(&mut self, text: &str) -> Result<()> {
        let lower = text.to_ascii_lowercase();
        if let Some(rating) = Rating::parse(&lower) {
            return self.grade(rating);
        }
        match lower.as_str() {
            "skip" | "next" => self.advance(),
            "space" | "reveal" | "show" => self.reveal(),
            "exit" | "stop" | "end" | "quit" => self.end_session(),
            _ => {
                self.note("in a session: 1-4 grade · enter next · tab hint · esc end");
                Ok(())
            }
        }
    }

    // ---------------------------------------------------------------- commands

    fn execute(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Help => self.help(),
            Command::Study(limit) => self.start_study(limit),
            Command::Books => self.show_books(),
            Command::Book(target) => self.switch_book(&target),
            Command::Search(query) => self.search(&query),
            Command::Root(term) => self.show_root(&term),
            Command::Summary => self.push_summary(),
            Command::Import(path) => self.import(&path),
            Command::Quiet => self.toggle_discreet(),
            Command::Limit(value) => self.set_limit(value),
            Command::Add(word) => self.add_word(&word),
            Command::Bury => self.bury_current(),
            Command::Undo => self.undo_last(),
            Command::Skip => {
                if self.session.is_some() {
                    self.advance()
                } else {
                    self.note("no active session · /study to start");
                    Ok(())
                }
            }
            Command::Exit => self.end_session(),
            Command::Quit => {
                self.quit = true;
                Ok(())
            }
            Command::Unknown(name) => {
                self.note(format!("unknown command /{name} · /help"));
                Ok(())
            }
        }
    }

    fn help(&mut self) -> Result<()> {
        let mut body = String::from("commands\n");
        for spec in command::SPECS {
            body.push_str(&format!(
                "  {:<14} {}\n",
                format!("{}{}", spec.name, spec.args),
                spec.help
            ));
        }
        body.push_str("\ntype an English word to look it up.\nin a session: 1-4 grade · enter next · tab hint · esc end");
        self.push(Message::agent(body));
        Ok(())
    }

    fn toggle_discreet(&mut self) -> Result<()> {
        self.discreet = !self.discreet;
        self.store
            .set_setting("discreet", if self.discreet { "1" } else { "0" })?;
        let label = if self.discreet { "on" } else { "off" };
        self.note(format!("discreet mode {label} · deck name {label}"));
        Ok(())
    }

    /// `/limit` shows the quota; `/limit 30` changes and persists it.
    fn set_limit(&mut self, value: Option<u32>) -> Result<()> {
        match value {
            Some(value) => {
                let value = value.clamp(1, 500);
                self.daily_new_limit = value;
                self.store
                    .set_setting("daily_new_limit", &value.to_string())?;
                self.note(format!("daily new words set to {value}"));
            }
            None => {
                let learned = match &self.book {
                    Some(book) => self.store.learned_today(book.id).unwrap_or(0),
                    None => 0,
                };
                self.note(format!(
                    "daily new words {}/{} · /limit <n> to change",
                    learned, self.daily_new_limit
                ));
            }
        }
        Ok(())
    }

    /// `/add <word>` puts a word into the active deck.
    fn add_word(&mut self, term: &str) -> Result<()> {
        let term = term.trim();
        if term.is_empty() {
            self.note("usage: /add <word>");
            return Ok(());
        }
        let Some(book) = self.book.clone() else {
            self.note("no deck yet · run quietly import <file>");
            return Ok(());
        };
        let word = match self.store.word(term)? {
            Some(word) => word,
            None => {
                let hits = self.store.search(term, 1)?;
                match hits.into_iter().next() {
                    Some(word) => word,
                    None => {
                        self.fail(format!("\"{term}\" is not in the dictionary"));
                        return Ok(());
                    }
                }
            }
        };
        let added = self.store.add_to_book(book.id, &word)?;
        if added {
            self.push(Message::tool(
                ToolView::new("add")
                    .accent(Accent::Success)
                    .badge(book.name.clone())
                    .result(word.lemma.clone())
                    .muted("added to the deck · it will show up in the next session"),
            ));
        } else {
            self.note(format!("{} is already in {}", word.lemma, book.name));
        }
        Ok(())
    }

    /// `/bury` postpones the current card to tomorrow.
    fn bury_current(&mut self) -> Result<()> {
        let Some(session) = self.session.as_mut() else {
            self.note("no active session · /study to start");
            return Ok(());
        };
        let Some(card) = session.current.clone() else {
            return Ok(());
        };
        let until = time::day_start(time::today()) + chrono::Duration::days(1);
        self.store.postpone(card.word.id, until)?;
        if let Some(session) = self.session.as_mut() {
            session.current = None;
        }
        self.push(Message::tool(
            ToolView::new("bury")
                .accent(Accent::Warn)
                .result(card.word.lemma.clone())
                .muted("back tomorrow"),
        ));
        self.next_card()
    }

    /// `/undo` rolls back the most recent rating and returns to that card.
    fn undo_last(&mut self) -> Result<()> {
        let Some(entry) = self.undo.pop() else {
            self.note("nothing to undo");
            return Ok(());
        };
        self.store.drop_last_review(entry.word_id)?;
        self.store.restore_card(entry.word_id, entry.before.as_ref())?;

        if let Some(session) = self.session.as_mut() {
            if entry.was_new {
                session.new_done = session.new_done.saturating_sub(1);
            } else {
                session.review_done = session.review_done.saturating_sub(1);
            }
            session.revealed = true;
            session.hint = false;
            session.graded = None;
            session.last_after = None;
            session.current = Some(entry.card.clone());
            session.card_msg = Some(entry.message);
        }

        // Drop the trailing "rated" note the undo makes false.
        self.truncate_after(entry.message);
        self.render_card()?;
        self.push(Message::tool(
            ToolView::new("undo")
                .accent(Accent::Info)
                .result(entry.card.word.lemma.clone())
                .muted("rating rolled back · pick a new one with 1-4"),
        ));
        self.refresh_status();
        Ok(())
    }

    /// Remove messages after `keep` (exclusive), e.g. the rated confirmation.
    fn truncate_after(&mut self, keep: usize) {
        let limit = (keep + 1).min(self.messages.len());
        if self.messages.len() > limit {
            self.messages.truncate(limit);
        }
        self.table_words.retain(|index, _| *index < self.messages.len());
    }

    // ---------------------------------------------------------------- history

    fn load_history(&mut self) {
        if let Ok(Some(raw)) = self.store.setting("input_history") {
            let entries: Vec<String> = raw
                .split('\u{1f}')
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            self.editor.set_history(entries);
        }
    }

    fn save_history(&mut self) {
        let entries = self.editor.history();
        if entries.is_empty() {
            return;
        }
        let _ = self.store.set_setting("input_history", &entries.join("\u{1f}"));
    }

    // ---------------------------------------------------------------- lookup

    fn lookup(&mut self, text: &str) -> Result<()> {
        if !text.contains(char::is_whitespace) {
            if let Some(word) = self.store.word(text)? {
                self.push(Message::tool(ToolView::word_card(
                    &CardState::new(word, "lookup").revealed(),
                )));
                return Ok(());
            }
        }
        let hits = self.store.search(text, 8)?;
        match hits.len() {
            0 => {
                self.fail(format!(
                    "no match for \"{text}\" · /import a dictionary or /search"
                ));
                Ok(())
            }
            1 => {
                self.push(Message::tool(ToolView::word_card(
                    &CardState::new(hits[0].clone(), "lookup").revealed(),
                )));
                Ok(())
            }
            _ => {
                self.push_table(hits_table(&hits, text), hits.clone());
                Ok(())
            }
        }
    }

    fn search(&mut self, query: &str) -> Result<()> {
        let query = query.trim();
        if query.is_empty() {
            self.note("usage: /search <word | meaning | root>");
            return Ok(());
        }
        self.lookup(query)
    }

    fn show_root(&mut self, term: &str) -> Result<()> {
        let term = term.trim();
        if term.is_empty() {
            self.note("usage: /root <affix>, e.g. /root spect, /root in-, /root -ology");
            return Ok(());
        }

        if let Some(hit) = qe_core::affix::lookup(term) {
            let mut tool = ToolView::new("root")
                .badge(hit.kind.label())
                .result(format!("{}  {}", hit.part, hit.gloss));
            let hits = self.store.search(hit.part, 6)?;
            if !hits.is_empty() {
                tool = tool.spacer().muted("words using it");
                for word in hits.iter().take(6) {
                    tool = tool.kv(word.lemma.clone(), truncate_text(&word.translation, 30));
                }
            }
            self.push(Message::tool(tool));
            return Ok(());
        }

        let related = qe_core::affix::containing(term);
        if related.is_empty() {
            self.note(format!("no affix \"{term}\" on file"));
            return Ok(());
        }
        let rows = related
            .iter()
            .map(|affix| {
                vec![
                    affix.part.to_string(),
                    affix.kind.label().to_string(),
                    affix.gloss.to_string(),
                ]
            })
            .collect();
        self.push(Message::table(
            TableView::new(
                vec!["affix".into(), "type".into(), "gloss".into()],
                rows,
            )
            .caption(format!("{} matches · {term}", related.len()))
            .footer("/root <affix> for details"),
        ));
        Ok(())
    }

    // ---------------------------------------------------------------- decks

    fn show_books(&mut self) -> Result<()> {
        let books = self.store.list_books()?;
        if books.is_empty() {
            self.note("no decks yet · run quietly import <file>");
            return Ok(());
        }
        let current = self.book.as_ref().map(|book| book.id);
        let rows = books
            .iter()
            .map(|book| {
                let progress = self.store.progress(book.id).unwrap_or_default();
                vec![
                    (if Some(book.id) == current { "▸" } else { " " }).to_string(),
                    book.slug.clone(),
                    book.name.clone(),
                    format!("{}/{}", progress.learned, progress.total),
                    progress.due.to_string(),
                ]
            })
            .collect();
        self.push(Message::table(
            TableView::new(
                vec![
                    String::new(),
                    "slug".into(),
                    "deck".into(),
                    "learned".into(),
                    "due".into(),
                ],
                rows,
            )
            .caption("decks")
            .right_align(3)
            .right_align(4)
            .footer("/book <slug> to switch"),
        ));
        Ok(())
    }

    fn switch_book(&mut self, target: &str) -> Result<()> {
        let target = target.trim();
        if target.is_empty() {
            self.note("usage: /book <slug> · /books lists them");
            return Ok(());
        }
        let needle = target.to_ascii_lowercase();
        let found = self.store.list_books()?.into_iter().find(|book| {
            book.slug.eq_ignore_ascii_case(&needle)
                || book.name.to_ascii_lowercase().contains(needle.as_str())
        });
        match found {
            Some(book) => {
                self.store.set_current_book(&book.slug)?;
                let progress = self.store.progress(book.id)?;
                self.push(Message::tool(
                    ToolView::new("deck")
                        .badge(book.name.clone())
                        .result(format!(
                            "switched · {}/{} learned · {} due",
                            progress.learned, progress.total, progress.due
                        )),
                ));
                self.book = Some(book);
            }
            None => self.note(format!("no deck matching \"{target}\" · /books")),
        }
        Ok(())
    }

    fn import(&mut self, path: &str) -> Result<()> {
        let path = path.trim();
        if path.is_empty() {
            self.note("usage: /import <path> (csv / tsv / json / txt)");
            return Ok(());
        }
        if !Path::new(path).exists() {
            self.fail(format!("file not found: {path}"));
            return Ok(());
        }
        match self.store.import_file(Path::new(path)) {
            Ok(report) => {
                self.push(Message::table(crate::cli::report_table(&report)));
                self.book = self.store.current_book()?;
            }
            Err(err) => self.fail(format!("import failed: {err:#}")),
        }
        Ok(())
    }

    // ---------------------------------------------------------------- study

    fn start_study(&mut self, limit: Option<u32>) -> Result<()> {
        if self.session.is_some() {
            self.note("a session is already running · esc to end it first");
            return Ok(());
        }
        let Some(book) = self.book.clone() else {
            self.note("no deck yet · run quietly import <file>");
            return Ok(());
        };

        let now = Utc::now();
        let daily_limit = limit.unwrap_or(self.daily_new_limit).max(1);
        let learned_today = self.store.learned_today(book.id)?;
        let remaining = daily_limit.saturating_sub(learned_today);
        let due = self.store.due_cards(book.id, now, DUE_BATCH)?;
        let new_words = if remaining == 0 {
            Vec::new()
        } else {
            self.store.new_cards(book.id, remaining as usize)?
        };

        let review_total = due.len() as u32;
        let new_total = new_words.len() as u32;
        if review_total == 0 && new_total == 0 {
            if remaining == 0 {
                self.note(format!(
                    "daily new-word quota reached ({daily_limit}) and nothing is due · /study <n> to add more"
                ));
            } else {
                self.note("deck finished · nothing due for review");
            }
            return Ok(());
        }

        // Interleave reviews and new words so a run of new words never piles up.
        let mut due_iter = due.into_iter();
        let mut new_iter = new_words.into_iter().map(StudyCard::new_word);
        let mut take_due = true;
        let mut queue = VecDeque::new();
        loop {
            let next = if take_due {
                due_iter.next().or_else(|| new_iter.next())
            } else {
                new_iter.next().or_else(|| due_iter.next())
            };
            let Some(card) = next else { break };
            queue.push_back(card);
            take_due = !take_due;
        }

        self.session = Some(StudySession::new(book.clone(), queue, new_total, review_total));
        self.push(Message::tool(
            ToolView::new("study")
                .badge(book.name.clone())
                .result(format!("{new_total} new · {review_total} due"))
                .muted(format!(
                    "today's new words {learned_today}/{daily_limit} · 1-4 grade · esc end"
                )),
        ));
        self.next_card()
    }

    fn next_card(&mut self) -> Result<()> {
        let popped = match self.session.as_mut() {
            Some(session) => session.queue.pop_front(),
            None => return Ok(()),
        };
        let Some(card) = popped else {
            return self.finish_session();
        };
        if let Some(session) = self.session.as_mut() {
            session.set_current(card);
        }
        self.render_card()?;
        self.scroll_back = 0;
        self.refresh_status();
        Ok(())
    }

    /// Rebuild the current card block in place from session state.
    /// Everything the card shows (hidden, hinted, revealed, graded) goes through here,
    /// so a hint never pushes the card out of view.
    fn render_card(&mut self) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        let Some(card) = session.current.clone() else {
            return Ok(());
        };

        // Rail colour encodes card state: red when the word was forgotten, yellow
        // while a hint is showing, green for a fresh word, blue for a review.
        let accent = match (session.graded, session.hint && !session.revealed) {
            (Some(Rating::Again), _) => Accent::Warn,
            (_, true) => Accent::Warn,
            _ if session.is_new_current() => Accent::Success,
            _ => Accent::Info,
        };
        let mut state = CardState::new(card.word.clone(), session.badge())
            .hint(session.hint)
            .accent(accent);
        if session.revealed {
            state = state.revealed();
        }
        if let Some(rating) = session.graded {
            state = state.graded(rating);
        }
        if let Some(after) = &session.last_after {
            state = state.footer(format!(
                "next {duration} · interval {}d · ease {:.2}",
                after.interval_days, after.ease,
                duration = human_interval(after, Utc::now())
            ));
        }

        let message = Message::tool(ToolView::word_card(&state));
        match session.card_msg {
            Some(index) if index < self.messages.len() => self.messages[index] = message,
            _ => {
                self.messages.push(message);
                let index = self.messages.len() - 1;
                if let Some(session) = self.session.as_mut() {
                    session.card_msg = Some(index);
                }
            }
        }
        Ok(())
    }

    fn reveal(&mut self) -> Result<()> {
        if self.session.is_none() {
            self.note("no active session · /study to start");
            return Ok(());
        }
        if let Some(session) = self.session.as_mut() {
            session.revealed = true;
        }
        self.render_card()?;
        self.scroll_back = 0;
        Ok(())
    }

    fn toggle_hint(&mut self) -> Result<()> {
        if self.session.is_none() {
            return Ok(());
        }
        if let Some(session) = self.session.as_mut() {
            if session.revealed || session.graded.is_some() {
                return Ok(());
            }
            session.hint = !session.hint;
        }
        self.render_card()?;
        Ok(())
    }

    /// Enter/space with an empty input line: peek, or move on once graded.
    fn advance(&mut self) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            self.note("no active session · /study to start");
            return Ok(());
        };
        if session.graded.is_some() {
            if let Some(session) = self.session.as_mut() {
                session.current = None;
            }
            return self.next_card();
        }
        if !session.revealed {
            return self.reveal();
        }
        // Peeked but never graded: skip without recording.
        if let Some(session) = self.session.as_mut() {
            session.current = None;
        }
        self.next_card()
    }

    /// Grade the current card. Pressing another 1-4 afterwards replaces the rating
    /// instead of creating a second review.
    fn grade(&mut self, rating: Rating) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            self.note("no active session · /study to start");
            return Ok(());
        };
        let Some(card) = session.current.clone() else {
            return Ok(());
        };
        let book_id = session.book.id;
        let first_time = session.graded.is_none();
        let card_message = session.card_msg.unwrap_or(0);
        let was_new = card.is_new;

        let now = Utc::now();
        let after = srs::review(card.card.as_ref(), rating, now);
        if first_time {
            self.store.record_review(
                card.word.id,
                book_id,
                card.card.as_ref(),
                &after,
                rating,
                now,
                card.is_new,
            )?;
        } else {
            self.store
                .amend_review(card.word.id, book_id, &after, rating, now, card.is_new)?;
        }

        if first_time {
            self.undo.push(UndoEntry {
                word_id: card.word.id,
                card: card.clone(),
                before: card.card.clone(),
                message: card_message,
                was_new,
            });
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }

        if let Some(session) = self.session.as_mut() {
            session.revealed = true;
            session.hint = false;
            session.graded = Some(rating);
            session.last_after = Some(after.clone());
            if first_time {
                if card.is_new {
                    session.new_done += 1;
                } else {
                    session.review_done += 1;
                }
            }
        }

        // A forgotten card comes back later in the same session, like Anki.
        if rating == Rating::Again {
            let requeued = StudyCard::review(card.word.clone(), after);
            if let Some(session) = self.session.as_mut() {
                let at = 4.min(session.queue.len());
                session.queue.insert(at, requeued);
                session.review_total = session.review_total.saturating_add(1);
            }
        }

        self.render_card()?;
        self.scroll_back = 0;
        self.refresh_status();
        Ok(())
    }

    fn end_session(&mut self) -> Result<()> {
        if self.session.is_none() {
            self.note("no active session · /study to start");
            return Ok(());
        }
        let done = self
            .session
            .as_ref()
            .map(|session| session.done())
            .unwrap_or(0);
        if done == 0 {
            self.session = None;
            self.note("session closed · nothing graded");
            self.refresh_status();
            return Ok(());
        }
        self.finish_session()
    }

    fn finish_session(&mut self) -> Result<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        let now = Utc::now();
        let seconds = (now - session.started_at)
            .num_seconds()
            .clamp(0, i64::from(u32::MAX)) as u32;
        self.store.record_session(&SessionLog {
            started_at: session.started_at,
            ended_at: now,
            seconds,
            new_count: session.new_done,
            review_count: session.review_done,
        })?;

        self.push_summary()?;
        self.note("session ended · /study again, or type a word");
        self.refresh_status();
        Ok(())
    }

    fn push_summary(&mut self) -> Result<()> {
        let daily = self.store.daily(time::today())?;
        let streak = self.store.streak()?;
        let upcoming = self
            .store
            .upcoming(time::today(), 7)?
            .into_iter()
            .map(|(day, count)| (time::format_short(day), count))
            .collect::<Vec<_>>();

        // Remaining budget today, across the active deck.
        let (new_left, due_left, deck_total) = match &self.book {
            Some(book) => {
                let learned_today = self.store.learned_today(book.id).unwrap_or(0);
                let progress = self.store.progress(book.id).unwrap_or_default();
                (
                    self.daily_new_limit.saturating_sub(learned_today),
                    progress.due,
                    progress.total,
                )
            }
            None => (self.daily_new_limit, 0, 0),
        };
        let status = SummaryStatus {
            new_left,
            due_left,
            deck_total,
        };

        self.push(Message::tool(summary_tool(
            &daily, streak, &upcoming, &status,
        )));
        Ok(())
    }

    // ---------------------------------------------------------------- misc

    fn greet(&mut self) -> Result<()> {
        let tool = match &self.book {
            Some(book) => {
                let progress = self.store.progress(book.id)?;
                let mut view = ToolView::new("ready")
                    .result(format!(
                        "{} words · {} learned · {} due",
                        progress.total, progress.learned, progress.due
                    ))
                    .muted("type a word to look it up · /study to begin · /help");
                if !self.discreet {
                    view = view.badge(book.name.clone());
                }
                view
            }
            None => ToolView::new("ready")
                .result("no vocabulary yet")
                .muted("run quietly import <file>, or /import here"),
        };
        self.push(Message::tool(tool));
        Ok(())
    }

    fn push(&mut self, message: Message) {
        self.messages.push(message);
        // Keep the reader's place when new output arrives while scrolled back.
        if self.scroll_back > 0 {
            self.unread = self.unread.saturating_add(1);
        }
    }

    /// Push a table and remember the words behind its rows.
    fn push_table(&mut self, view: TableView, words: Vec<Word>) {
        self.messages.push(Message::table(view));
        let index = self.messages.len() - 1;
        if !words.is_empty() {
            self.table_words.insert(index, words);
        }
        if self.scroll_back > 0 {
            self.unread = self.unread.saturating_add(1);
        }
    }

    fn note(&mut self, text: impl Into<String>) {
        self.push(Message::hint(text));
    }

    /// Error line: a red block so a failed import or lookup is hard to miss.
    fn fail(&mut self, text: impl Into<String>) {
        self.push(Message::tool(
            ToolView::new("error")
                .accent(Accent::Danger)
                .text(text.into()),
        ));
    }

    fn refresh_status(&mut self) {
        self.status = match &self.session {
            Some(session) => {
                let word = session
                    .current
                    .as_ref()
                    .map(|card| card.word.lemma.as_str())
                    .unwrap_or("");
                let state = if session.graded.is_some() {
                    "enter next"
                } else if session.revealed {
                    "1-4 grade"
                } else {
                    "tab hint · 1-4 grade"
                };
                let left_new = session.queue.iter().filter(|card| card.is_new).count();
                let left_review = session.queue.len().saturating_sub(left_new);
                format!(
                    "{word} · {}/{} · {left_new} new + {left_review} review left · {state}",
                    (session.done() + 1).min(session.total() as u32),
                    session.total()
                )
            }
            None => "ready · type a word, or /  for commands".to_string(),
        };
    }
}

// -------------------------------------------------------------------- helpers

fn build_suggestions(input: &str, selected: usize) -> Vec<Suggestion> {
    command::matches(input)
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .enumerate()
        .map(|(index, spec)| Suggestion {
            name: spec.name.to_string(),
            args: spec.args.to_string(),
            help: spec.help.to_string(),
            selected: index == selected,
        })
        .collect()
}

fn rating_from_char(ch: char) -> Rating {
    match ch {
        '1' => Rating::Again,
        '2' => Rating::Hard,
        '4' => Rating::Easy,
        _ => Rating::Good,
    }
}

fn human_interval(after: &SrsCard, now: DateTime<Utc>) -> String {
    let minutes = (after.due_at - now).num_minutes().max(1);
    if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 60 * 24 {
        format!("{}h", (minutes as f64 / 60.0).round() as i64)
    } else {
        format!("{}d", (minutes as f64 / 1440.0).round() as i64)
    }
}

fn truncate_text(text: &str, max: usize) -> String {
    let text = text.replace('\n', " ");
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

fn hits_table(words: &[Word], query: &str) -> TableView {
    let rows = words
        .iter()
        .map(|word| {
            vec![
                word.lemma.clone(),
                word.phonetic.clone().unwrap_or_default(),
                truncate_text(&word.translation, 34),
            ]
        })
        .collect();
    TableView::new(
        vec!["word".into(), "phonetic".into(), "meaning".into()],
        rows,
    )
    .caption(format!("{} matches · {query}", words.len()))
    .footer("type a word for roots and examples")
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_core::view::Body;
    use qe_store::SqliteStore;

    struct Harness {
        app: App,
        _dir: tempfile::TempDir,
    }

    fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open_at(dir.path().join("quietly.db")).unwrap();
        qe_store::seed::seed_if_empty(&mut store).unwrap();
        let app = App::new(Box::new(store)).unwrap();
        Harness { app, _dir: dir }
    }

    fn card_count(app: &App) -> usize {
        app.messages
            .iter()
            .filter(|m| matches!(&m.body, Body::Tool(t) if t.name == "word"))
            .count()
    }

    #[test]
    fn study_flow_grades_without_revealing_first() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        assert!(app.session.is_some());
        assert_eq!(card_count(&app), 1);

        // Pressing 3 alone reveals the answer and records the rating.
        app.grade(Rating::Good).unwrap();
        let session = app.session.as_ref().unwrap();
        assert!(session.revealed);
        assert_eq!(session.graded, Some(Rating::Good));
        let daily = app.store.daily(time::today()).unwrap();
        assert_eq!(daily.reviews, 1);
        assert_eq!(daily.new_learned, 1);
    }

    #[test]
    fn regrading_replaces_the_review() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(2))).unwrap();
        app.grade(Rating::Good).unwrap();
        app.grade(Rating::Easy).unwrap();

        let daily = app.store.daily(time::today()).unwrap();
        assert_eq!(daily.reviews, 1, "re-rating must not add a second review");
        assert_eq!(daily.new_learned, 1);
        assert_eq!(daily.easy, 1);
        assert_eq!(daily.good, 0);
        assert_eq!(app.session.as_ref().unwrap().graded, Some(Rating::Easy));
        assert_eq!(app.session.as_ref().unwrap().new_done, 1);
    }

    #[test]
    fn hint_keeps_the_card_in_place_and_space_still_works() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(2))).unwrap();
        let before = card_count(&app);

        app.toggle_hint().unwrap();
        assert_eq!(card_count(&app), before, "hint reuses the card block");
        assert!(app.session.as_ref().unwrap().hint);

        // space → reveal (previously the hint message pushed the card away)
        app.advance().unwrap();
        assert!(app.session.as_ref().unwrap().revealed);
        assert_eq!(card_count(&app), before);
    }

    #[test]
    fn enter_after_grading_advances() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        app.grade(Rating::Good).unwrap();
        app.advance().unwrap();
        assert_eq!(card_count(&app), 2, "next card appears");
        assert_eq!(app.session.as_ref().unwrap().graded, None);
        assert!(!app.session.as_ref().unwrap().revealed);
    }

    #[test]
    fn summary_and_session_log_written_on_exit() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(2))).unwrap();
        app.grade(Rating::Easy).unwrap();
        app.end_session().unwrap();
        assert!(app.session.is_none());
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "summary")));
    }

    #[test]
    fn lookup_and_root_commands_produce_tool_blocks() {
        let Harness { mut app, _dir } = harness();
        app.lookup("inspect").unwrap();
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "word")));

        app.show_root("spect").unwrap();
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "root")));

        app.show_books().unwrap();
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Table(t) if t.caption.as_deref() == Some("decks"))));
    }

    #[test]
    fn book_switch_changes_current_book() {
        let Harness { mut app, _dir } = harness();
        app.switch_book("toefl").unwrap();
        assert_eq!(app.book.as_ref().unwrap().slug, "toefl");
        assert_eq!(app.store.current_book().unwrap().unwrap().slug, "toefl");
    }

    #[test]
    fn study_respects_daily_limit() {
        let Harness { mut app, _dir } = harness();
        app.daily_new_limit = 2;
        app.execute(Command::Study(None)).unwrap();
        assert_eq!(app.session.as_ref().unwrap().new_total, 2);
    }

    #[test]
    fn suggestions_and_ghost_track_the_input() {
        let ghost = command::ghost("/st");
        assert_eq!(ghost.as_deref(), Some("udy"));
        let suggestions = build_suggestions("/st", 0);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "/study");
        assert!(suggestions[0].selected);
        assert!(build_suggestions("inspect", 0).is_empty());
    }

    #[test]
    fn suggestion_selection_follows_the_index() {
        let suggestions = build_suggestions("/b", 1);
        assert!(suggestions.len() >= 2, "expected /books and /book");
        assert!(!suggestions[0].selected);
        assert!(suggestions[1].selected);
    }

    #[test]
    fn forgotten_card_is_requeued_in_the_session() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        let before = app.session.as_ref().unwrap().queue.len();
        let total_before = app.session.as_ref().unwrap().review_total;

        app.grade(Rating::Again).unwrap();

        let session = app.session.as_ref().unwrap();
        // One graded card, and one copy pushed back for a later pass.
        assert_eq!(session.queue.len(), before + 1, "card should come back");
        assert_eq!(
            session.review_total,
            total_before + 1,
            "total grows by the requeued card"
        );
        // The relearned copy is queued as a review, not as new.
        assert!(session.queue.iter().any(|card| !card.is_new));
    }

    #[test]
    fn undo_restores_the_card_and_removes_the_review() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        let word = app.session.as_ref().unwrap().current.clone().unwrap();

        app.grade(Rating::Good).unwrap();
        assert_eq!(app.store.daily(time::today()).unwrap().reviews, 1);

        app.undo_last().unwrap();

        let daily = app.store.daily(time::today()).unwrap();
        assert_eq!(daily.reviews, 0, "review row should be gone");
        let session = app.session.as_ref().unwrap();
        assert_eq!(session.new_done, 0, "session counter rolls back");
        assert_eq!(
            session.current.as_ref().map(|card| card.word.lemma.as_str()),
            Some(word.word.lemma.as_str()),
            "the undone card is current again"
        );
        // A new card is untouched: no review row, no card row.
        assert!(app.store.word(&word.word.lemma).unwrap().is_some());
    }

    #[test]
    fn bury_postpones_the_card_to_tomorrow() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        let word = app.session.as_ref().unwrap().current.clone().unwrap();

        app.bury_current().unwrap();

        let book = app.book.clone().unwrap();
        let due = app.store.due_cards(book.id, Utc::now(), 10).unwrap();
        assert!(
            !due.iter().any(|card| card.word.id == word.word.id),
            "buried card must not be due today"
        );
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "bury")));
    }

    #[test]
    fn limit_persists_and_reports() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Limit(Some(5))).unwrap();
        assert_eq!(app.daily_new_limit, 5);
        assert_eq!(
            app.store.setting("daily_new_limit").unwrap().as_deref(),
            Some("5")
        );
        app.execute(Command::Limit(Some(0))).unwrap();
        assert_eq!(app.daily_new_limit, 1, "clamped to at least one");
    }

    #[test]
    fn add_puts_a_word_into_the_active_deck() {
        let Harness { mut app, _dir } = harness();
        let book = app.book.clone().unwrap();
        let before = app.store.progress(book.id).unwrap().total;

        app.add_word("inspect").unwrap();
        let after = app.store.progress(book.id).unwrap().total;
        // `inspect` already lives in cet4, so this must be a no-op for that deck.
        assert_eq!(after, before, "duplicate additions are ignored");

        app.add_word("serendipity").unwrap();
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "error")));
    }

    #[test]
    fn quitting_mid_session_records_the_log() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(2))).unwrap();
        app.grade(Rating::Good).unwrap();
        // Simulate the end-of-run path.
        app.finish_session().unwrap();
        assert!(app.session.is_none());
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "summary")));
    }
}
