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
/// How many completions to show at once. 命令有近三十条，5 行太少，但也不能
/// 把整个终端吃掉；超出的条数以 `+N more` 提示。
const MAX_SUGGESTIONS: usize = 12;
/// Caret blink half-period. Typing resets the phase so the caret is always visible
/// right after a keystroke.
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// How many ratings can be undone in one session.
const MAX_UNDO: usize = 50;

/// One reversible grading action.
struct UndoEntry {
    /// 卡片按 lemma 归属（`cards` 表的主键），不是词典里的 `words.id`。
    lemma: String,
    card: StudyCard,
    before: Option<SrsCard>,
    /// Message index of the card block, so undo can jump back to it.
    message: usize,
    was_new: bool,
    /// Rating was `again`, so a copy was pushed back into the session queue.
    requeued: bool,
}

/// 一个待补全的参数值。
struct Candidate {
    /// 按 tab 之后填进输入行的文本。
    value: String,
    /// 面板右侧的说明：词书名称、`dir` 之类。
    help: String,
}

pub struct App {
    pub(crate) store: Box<dyn Store>,
    book: Option<Book>,
    pub(crate) messages: Vec<Message>,
    editor: Editor,
    pub(crate) session: Option<StudySession>,
    status: String,
    pub(crate) scroll_back: u16,
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
    /// Request from the key handler to flip mouse capture; applied by the loop,
    /// which is the only place that can touch the terminal.
    toggle_mouse: bool,
    /// Import queued by `/import`; it runs after the next frame so the user sees
    /// the "importing" line instead of a frozen window.
    pending_import: Option<String>,
    /// 当前阅读会话。与 `session`（学习）互斥。
    pub(crate) reading: Option<qe_core::reading::ReadingSession>,
    /// 待作答的测验题。
    pub(crate) quizzes: Vec<qe_core::model::Quiz>,
    pub(crate) quiz_index: usize,
    /// `Some(答对与否)`：选择题已判定，再按一次翻下一题。
    pub(crate) quiz_answered: Option<bool>,
    /// 填空题已揭示答案（不判对错，也不写库），再按一次翻下一题。
    pub(crate) quiz_revealed: bool,
    pub(crate) quiz_correct: u32,
    /// 阅读时是否显示译文（`/translate` 与 `tab` 切换，两种模式都生效）。
    pub(crate) show_translation: bool,
    /// 下一帧把当前段滚进视野（全篇模式下翻页、选词后使用）。
    pub(crate) reveal_anchor: bool,
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
            toggle_mouse: false,
            pending_import: None,
            reading: None,
            quizzes: Vec::new(),
            quiz_index: 0,
            quiz_answered: None,
            quiz_revealed: false,
            quiz_correct: 0,
            // 译文默认隐藏：先读原文，需要对照时再按 tab。
            show_translation: false,
            reveal_anchor: false,
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
            let ghost = command::ghost(&input).or_else(|| self.arg_ghost(&input));
            let (suggestions, suggestions_more) = self.suggestions_for(&input);
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
                suggestions_more,
                scroll_back: self.scroll_back,
                cursor_on: self.cursor_on,
                unread: self.unread,
            };

            let mut info = DrawInfo::default();
            terminal.terminal.draw(|frame| {
                info = draw(frame, &view);
            })?;
            self.draw_info = info;

            // 全篇模式下整篇文章一起渲染，当前段可能落在视野外（长文尤甚）。
            // 翻页或选词后请求一次「把当前段带回来」，只在这一帧真滚动——
            // 读者自己滚到别处再触发重画时，不会因为已经在视野内而被拽走。
            if std::mem::take(&mut self.reveal_anchor) {
                self.scroll_to_anchor();
            }

            // Imports read a whole dictionary file; run them only once the frame is
            // on screen so the "importing" line is visible while it works.
            if let Some(path) = self.pending_import.take() {
                self.run_pending_import(&path)?;
                continue;
            }

            if self.toggle_mouse {
                self.toggle_mouse = false;
                let on = !terminal.mouse_enabled();
                terminal.set_mouse(on)?;
                if on {
                    self.note("mouse capture on · scroll and click work, text selection is off");
                } else {
                    self.note("mouse capture off · select and copy with the mouse · F2 to restore");
                }
            }

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
        // 阅读中显示「第几段 · 选中哪个词」，否则状态栏对选词毫无反馈。
        if let Some(session) = self.reading.as_ref() {
            let mut line = format!("{}/{}", session.paragraph + 1, session.total());
            if let Some(lemma) = session.focused_lemma() {
                line.push_str(&format!(" · {lemma}"));
            }
            return Some(line);
        }
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

        let suggestion_count = self.suggestions_for(&self.editor.text()).0.len();

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
            // Selection needs the terminal's own mouse handling, which we disable
            // while capturing. F2 hands the mouse back and returns it again.
            KeyCode::F(2) => {
                self.toggle_mouse = true;
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

        // 测验快捷键：有题目待答时，数字选选项，回车提交/翻页。
        //
        // 学习会话优先：`/study` 与阅读、测验可能同时存在（例如先 `/read` 再
        // `/study`），此时屏幕上画的是学习卡，数字键必须去评分，不能被下面几块
        // 截走——否则按 1-4 会变成「打开第 n 个词」的查词卡片。
        if self.session.is_none() && !self.quizzes.is_empty() && self.editor.is_empty() {
            match key.code {
                KeyCode::Char(ch @ '1'..='9') => {
                    let index = ch.to_digit(10).unwrap_or(1) as usize;
                    self.answer_quiz(Some(index))?;
                    return Ok(());
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    self.answer_quiz(None)?;
                    return Ok(());
                }
                KeyCode::Esc => {
                    self.quizzes.clear();
                    self.quiz_answered = None;
                    self.quiz_revealed = false;
                    self.note("quiz closed");
                    return Ok(());
                }
                _ => {}
            }
        }

        // 阅读快捷键：同样只在输入行为空时生效，字母键留给查词与 `/命令`。
        // 只用非字母键，所以想输入 `n` 开头的单词不会被吞掉。
        if self.session.is_none() && self.reading.is_some() && self.editor.is_empty() {
            match key.code {
                KeyCode::Char(ch @ '1'..='9') => {
                    let index = ch.to_digit(10).unwrap_or(1) as usize;
                    self.open_word_at(index)?;
                    return Ok(());
                }
                // `,` `.` 按词移动选中位置：不占字母键，也不常用在英文输入里。
                KeyCode::Char(',') => {
                    self.move_focus(-1)?;
                    return Ok(());
                }
                KeyCode::Char('.') => {
                    self.move_focus(1)?;
                    return Ok(());
                }
                KeyCode::Enter => {
                    // 选中了词就打开它的完整卡片，否则翻到下一段。
                    if self.focused_lemma().is_some() {
                        return self.open_focused();
                    }
                    self.reading_next()?;
                    return Ok(());
                }
                KeyCode::Char(' ') | KeyCode::Right => {
                    self.reading_next()?;
                    return Ok(());
                }
                KeyCode::Left => {
                    self.reading_prev()?;
                    return Ok(());
                }
                KeyCode::Tab => {
                    self.toggle_translation()?;
                    return Ok(());
                }
                KeyCode::Esc => {
                    // 先取消选中，再按才是结束阅读。
                    if self.focused_lemma().is_some() {
                        self.clear_focus()?;
                        return Ok(());
                    }
                    self.finish_reading()?;
                    return Ok(());
                }
                _ => {}
            }
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
                KeyCode::Char('m') => {
                    self.toggle_glosses();
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
                    self.open_row_at(mouse.row, mouse.column);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                self.scrub_to(mouse.row);
            }
            MouseEventKind::Up(_) => self.dragging = false,
            _ => {}
        }
    }

    /// 点击阅读段落里的词就选中它；表格行仍然打开整张词卡。
    fn open_row_at(&mut self, row: u16, column: u16) {
        // 阅读段落优先：命中区间精确到列，点空白处不选中任何词。
        if let Some(hit) = self
            .draw_info
            .word_hits
            .iter()
            .find(|hit| hit.row == row && column >= hit.start && column < hit.end)
            .copied()
        {
            let on_card = self
                .reading
                .as_ref()
                .and_then(|session| session.message)
                .map(|message| message == hit.message)
                .unwrap_or(false);
            if self.reading.is_some() && on_card {
                // 词序号只在段内有效：点到别的段时先跳到那段，否则会选中当前段
                // 里同序号的另一个词。
                let _ = self.select_word_in(hit.paragraph, hit.token);
                return;
            }
        }
        self.open_table_row(row);
    }

    /// 点击结果表格里的某一行，打开该词。
    fn open_table_row(&mut self, row: u16) {
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
            self.push(Message::card(
                CardState::new(word, "lookup")
                    .revealed()
                    .with_glosses()
                    .without_ratings(),
            ));
        }
    }

    /// 把当前段锚点滚到视野顶部附近；只在这一帧的几何信息里算，不需要知道宽度。
    ///
    /// 已经在视野内的锚点不动：读者自己滚到别处再触发重画（切译文、标记）时，
    /// 不该被强行拽回。
    fn scroll_to_anchor(&mut self) {
        let info = &self.draw_info;
        let Some(line) = info.anchor_line else {
            return;
        };
        if info.max_back == 0 {
            return;
        }
        let offset = info.max_back.saturating_sub(self.scroll_back) as i32;
        let screen_row = line as i32 - offset + i32::from(info.transcript.y);
        let top = i32::from(info.transcript.y);
        let bottom = top + i32::from(info.transcript.height);
        if screen_row >= top && screen_row < bottom {
            return;
        }
        // 目标：锚点落在视野第二行，上面留一行上下文。
        let wanted = (line as i32 - 1).clamp(0, i32::from(info.max_back));
        self.scroll_back = info.max_back - wanted as u16;
        self.unread = 0;
    }

    fn scroll_by(&mut self, delta: i32) {        let next = self.scroll_back as i32 + delta;
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

        // Completing an argument: deck slugs for /book, titles and paths for
        // /read and /quiz, files for /import.
        if let Some((name, partial)) = command::arg_completion_target(&text) {
            self.complete_argument(name, partial);
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

    /// Tab inside a command argument.
    ///
    /// Fills the highlighted candidate when the user moved the highlight, otherwise
    /// the shared prefix, otherwise the first match — repeated tab therefore cycles
    /// through `/book` decks. Commands with no candidate list only get a usage hint.
    fn complete_argument(&mut self, name: &'static str, partial: &str) {
        let candidates = self.arg_candidates(name, partial);
        if candidates.is_empty() {
            if let Some(usage) = command::usage(name) {
                self.note(format!("usage: {usage}"));
            }
            return;
        }

        let values: Vec<String> = candidates.iter().map(|c| c.value.clone()).collect();
        let hits = command::arg_hits(&values, partial);
        if hits.is_empty() {
            self.note(format!(
                "no match for \"{}\" · {} options",
                partial.trim(),
                values.len()
            ));
            return;
        }

        let picked = if hits.len() == 1 {
            hits[0].clone()
        } else if self.suggestion_index > 0 {
            hits[self.suggestion_index.min(hits.len() - 1)].clone()
        } else {
            match command::complete_arg(&values, partial) {
                Some(prefix) if prefix.chars().count() > partial.chars().count() => {
                    self.editor.set_text(&format!("{name} {prefix}"));
                    return;
                }
                _ => hits[0].clone(),
            }
        };

        self.editor.set_text(&format!("{name} {picked}"));
        self.suggestion_index = (self.suggestion_index + 1) % hits.len().min(MAX_SUGGESTIONS);
    }

    /// 参数补全的候选。
    ///
    /// 每类参数只列它自己的东西：`/book` 是词书，`/import` 是当前目录的路径，
    /// `/read` 是已导入的标题加路径。以前这里把所有命令都混成一个「词书 + 文件」
    /// 的列表，所以在 `/search` 后面按 tab 会冒出文件名的提示。
    fn arg_candidates(&self, command: &str, partial: &str) -> Vec<Candidate> {
        match command::arg_kind(command) {
            command::ArgKind::Deck => self.deck_candidates(),
            command::ArgKind::Path => path_candidates(partial),
            command::ArgKind::Text => text_candidates(self),
            command::ArgKind::TextOrPath => {
                let mut out = text_candidates(self);
                out.extend(path_candidates(partial));
                out
            }
            command::ArgKind::Choice(values) => values
                .iter()
                .map(|value| Candidate {
                    value: (*value).to_string(),
                    help: String::new(),
                })
                .collect(),
            command::ArgKind::Number | command::ArgKind::Free => Vec::new(),
        }
    }

    fn deck_candidates(&self) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = self
            .store
            .list_books()
            .unwrap_or_default()
            .into_iter()
            .map(|book| Candidate {
                value: book.slug,
                help: book.name,
            })
            .collect();
        out.sort_by(|a, b| a.value.cmp(&b.value));
        out
    }

    /// 提示行和补全面板：命令名，或命令的参数。
    fn suggestions_for(&self, input: &str) -> (Vec<Suggestion>, usize) {
        if let Some((name, partial)) = command::arg_completion_target(input) {
            let all = self.arg_candidates(name, partial);
            let values: Vec<String> = all.iter().map(|c| c.value.clone()).collect();
            let hits = command::arg_hits(&values, partial);
            let rows = hits
                .iter()
                .take(MAX_SUGGESTIONS)
                .enumerate()
                .map(|(index, value)| Suggestion {
                    name: value.clone(),
                    args: String::new(),
                    help: all
                        .iter()
                        .find(|candidate| &candidate.value == value)
                        .map(|candidate| candidate.help.clone())
                        .unwrap_or_default(),
                    selected: index == self.suggestion_index,
                })
                .collect();
            return (rows, hits.len().saturating_sub(MAX_SUGGESTIONS));
        }
        (
            build_suggestions(input, self.suggestion_index),
            hidden_suggestions(input),
        )
    }

    /// 灰字提示：参数只剩一种可能时，把没打出来的部分画出来。
    fn arg_ghost(&self, input: &str) -> Option<String> {
        let (name, partial) = command::arg_completion_target(input)?;
        let candidates = self.arg_candidates(name, partial);
        let values: Vec<String> = candidates.iter().map(|c| c.value.clone()).collect();
        let hits = command::arg_hits(&values, partial);
        if hits.len() != 1 {
            return None;
        }
        let hit = &hits[0];
        let typed = partial.chars().count();
        (hit.chars().count() > typed).then(|| hit.chars().skip(typed).collect())
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
        self.push(Message::card(
            CardState::new(word, "lookup")
                .revealed()
                .with_glosses()
                .without_ratings(),
        ));
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
            Command::Db => self.show_db(),
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
            Command::Phrase(spec) => self.add_phrase(&spec),
            Command::Read(query) => self.start_reading(&query),
            Command::Texts => self.list_texts(),
            Command::Library => self.list_library(),
            Command::Mode(arg) => self.set_reading_mode(&arg),
            Command::Mark(word) => self.mark_word(&word),
            Command::Words => self.list_reading_words(),
            Command::Next => {
                if self.reading.is_some() {
                    self.reading_next()
                } else if self.session.is_some() {
                    self.advance()
                } else {
                    self.note("nothing to advance");
                    Ok(())
                }
            }
            Command::Prev => self.reading_prev(),
            Command::Translate => self.toggle_translation(),
            Command::Quiz(arg) => {
                if arg.trim().is_empty() {
                    self.start_quiz()
                } else {
                    self.import_quiz(&arg)
                }
            }
            Command::Goto(arg) => match arg.trim().parse::<usize>() {
                Ok(n) => self.reading_goto(n.saturating_sub(1)),
                Err(_) => {
                    self.note("usage: /goto <paragraph number>");
                    Ok(())
                }
            },
            Command::Take(arg) => {
                if arg.trim().is_empty() {
                    // 不给序号时取选中的词，配合 `,` `.` / 点击使用。
                    self.add_focused()
                } else {
                    match arg.trim().parse::<usize>() {
                        Ok(n) => self.add_reading_word(n),
                        Err(_) => {
                            self.note("usage: /take <word number> · /take takes the selected word");
                            Ok(())
                        }
                    }
                }
            }
            Command::Drop(title) => self.drop_text(&title),
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
                "  {:<26} {}\n",
                format!("{}{}", spec.label(), spec.args),
                spec.help
            ));
        }
        body.push_str(
            "\ntype an English word to look it up.\n\
             in a session: 1-4 grade · enter next · tab hint · esc end\n\
             reading: , . select a word (or click one) · enter full card · /take keeps it\n\
             \t        space/→ next · ← prev · 1-9 open a word · tab translation · esc ends\n\
             \n\
             F2 releases the mouse so you can select and copy text; press it again \
             to get scroll and click back. Most terminals also accept shift+drag \
             while capture is on.",
        );
        self.push(Message::agent(body));
        Ok(())
    }

    /// `/db`：两个数据文件在哪、多大。
    fn show_db(&mut self) -> Result<()> {
        let files = self.store.db_files();
        let mut view = ToolView::new("db")
            .accent(Accent::Note)
            .result(format!("{} files", files.len()));
        for (label, path, bytes) in &files {
            let size = if *bytes >= 1_048_576 {
                format!("{:.1} MB", *bytes as f64 / 1_048_576.0)
            } else {
                format!("{:.0} KB", *bytes as f64 / 1024.0)
            };
            view = view.kv(label.clone(), format!("{size}  {path}"));
        }
        self.push(Message::tool(view));
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
    pub(crate) fn add_word(&mut self, term: &str) -> Result<()> {
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

    /// `/phrase <word> <phrase> = <gloss>` records a fixed expression.
    ///
    /// The `=` is optional: without it the phrase is stored unglossed, which still
    /// shows up on the card.
    fn add_phrase(&mut self, spec: &str) -> Result<()> {
        let spec = spec.trim();
        if spec.is_empty() {
            self.note("usage: /phrase sake for the sake of = 为了");
            return Ok(());
        }
        let (head, gloss) = match spec.split_once('=') {
            Some((head, gloss)) => (head.trim(), gloss.trim()),
            None => (spec, ""),
        };
        let Some((lemma, phrase)) = head.split_once(char::is_whitespace) else {
            self.note("usage: /phrase <word> <phrase> = <gloss>");
            return Ok(());
        };
        let phrase = phrase.trim();
        if phrase.is_empty() {
            self.note("usage: /phrase <word> <phrase> = <gloss>");
            return Ok(());
        }

        if self.store.word(lemma)?.is_none() {
            self.fail(format!("\"{lemma}\" is not in the dictionary"));
            return Ok(());
        }
        if self.store.add_collocation(lemma, phrase, gloss)? {
            self.push(Message::tool(
                ToolView::new("phrase")
                    .accent(Accent::Success)
                    .badge(lemma.to_string())
                    .result(if gloss.is_empty() {
                        phrase.to_string()
                    } else {
                        format!("{phrase}  ->  {gloss}")
                    }),
            ));
        } else {
            self.note(format!("\"{phrase}\" is already listed under {lemma}"));
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
        self.store.postpone(&card.word.lemma, until)?;
        // Take it out of the session the same way a skip does, so the `2/12` badge
        // still lands on `12/12` at the end. A new word has no `cards` row yet, so
        // the UPDATE above changed nothing — dropping it from the queue is the only
        // thing that keeps it out of the rest of this session.
        self.drop_current_ungraded();
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
        self.store.drop_last_review(&entry.lemma)?;
        self.store.restore_card(&entry.lemma, entry.before.as_ref())?;

        if let Some(session) = self.session.as_mut() {
            if entry.was_new {
                session.new_done = session.new_done.saturating_sub(1);
            } else {
                session.review_done = session.review_done.saturating_sub(1);
            }

            // Take back the copy `again` pushed into the queue, if it is still there.
            if entry.requeued {
                let position = session
                    .queue
                    .iter()
                    .position(|card| card.word.lemma.eq_ignore_ascii_case(&entry.lemma));
                if let Some(position) = position {
                    session.queue.remove(position);
                    session.review_total = session.review_total.saturating_sub(1);
                }
            }

            // The user may have already moved on to another card. Put it back so the
            // undone card does not silently swallow it.
            let current_is_other = session
                .current
                .as_ref()
                .map(|card| !card.word.lemma.eq_ignore_ascii_case(&entry.lemma))
                .unwrap_or(false);
            if current_is_other {
                if let Some(current) = session.current.take() {
                    session.queue.push_front(current);
                }
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

    pub(crate) fn lookup(&mut self, text: &str) -> Result<()> {
        if !text.contains(char::is_whitespace) {
            if let Some(word) = self.store.word(text)? {
                self.push(Message::card(
                    CardState::new(word, "lookup")
                        .revealed()
                        .with_glosses()
                        .without_ratings(),
                ));
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
                self.push(Message::card(
                    CardState::new(hits[0].clone(), "lookup")
                        .revealed()
                        .with_glosses()
                        .without_ratings(),
                ));
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
        self.push(Message::tool(
            ToolView::new("import")
                .accent(Accent::Info)
                .result(path.to_string())
                .muted("reading the file · the window may pause for a large dictionary"),
        ));
        self.pending_import = Some(path.to_string());
        Ok(())
    }

    /// The blocking half of `/import`, run right after a frame is painted.
    fn run_pending_import(&mut self, path: &str) -> Result<()> {
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

        // 阅读与学习互斥：`start_reading` 已经会收尾学习会话，反方向也要收尾，
        // 否则阅读队列会和学习队列抢同一批数字键（按 1-4 变成查词）。
        if self.reading.is_some() {
            self.finish_reading()?;
            self.note("reading closed before study");
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

        let message = Message::card(state);
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

    /// Fold or unfold the `more meanings` block on every card at once.
    ///
    /// The Wiktionary glosses are English and often five or six lines; they push
    /// the Chinese senses off a short terminal, so they start folded.
    fn toggle_glosses(&mut self) {
        // Only the card the reader is looking at. A study card is tracked by
        // `card_msg`; outside a session the newest card is the one on screen.
        let target = self
            .session
            .as_ref()
            .and_then(|session| session.card_msg)
            .or_else(|| {
                self.messages
                    .iter()
                    .rposition(|message| message.card_state().is_some())
            });
        let Some(index) = target else {
            return;
        };
        let Some(state) = self.messages[index].card_state_mut() else {
            return;
        };
        state.expand_glosses = !state.expand_glosses;
        self.status = if state.expand_glosses {
            "more meanings: shown".to_string()
        } else {
            "more meanings: hidden".to_string()
        };
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
        // Peeked but never graded: skip without recording, but keep the totals honest
        // so the progress badge does not stall one card short of the end.
        self.drop_current_ungraded();
        self.next_card()
    }

    /// Remove the current card from the session without writing a review.
    fn drop_current_ungraded(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(card) = session.current.take() else {
            return;
        };
        if card.is_new {
            session.new_total = session.new_total.saturating_sub(1);
        } else {
            session.review_total = session.review_total.saturating_sub(1);
        }
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
                &card.word.lemma,
                book_id,
                card.card.as_ref(),
                &after,
                rating,
                now,
                card.is_new,
            )?;
        } else {
            self.store.amend_review(
                &card.word.lemma,
                book_id,
                &after,
                rating,
                now,
                card.is_new,
            )?;
        }

        if first_time {
            self.undo.push(UndoEntry {
                lemma: card.word.lemma.clone(),
                card: card.clone(),
                before: card.card.clone(),
                message: card_message,
                was_new,
                requeued: rating == Rating::Again,
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

            // A forgotten card comes back later in the same session, like Anki.
            // The queue must follow the *current* rating, not just the first one:
            // changing 1 to 3 has to take the queued copy back out, otherwise the
            // same word gets graded twice and the second pass runs off a stale
            // snapshot of the card.
            let has_copy = session
                .queue
                .iter()
                .any(|queued| queued.word.id == card.word.id);
            let wants_copy = rating == Rating::Again;
            if wants_copy && !has_copy {
                let requeued = StudyCard::review(card.word.clone(), after);
                let at = 4.min(session.queue.len());
                session.queue.insert(at, requeued);
                session.review_total = session.review_total.saturating_add(1);
            } else if !wants_copy && has_copy {
                if let Some(position) = session
                    .queue
                    .iter()
                    .position(|queued| queued.word.id == card.word.id)
                {
                    session.queue.remove(position);
                    session.review_total = session.review_total.saturating_sub(1);
                }
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

    pub(crate) fn finish_session(&mut self) -> Result<()> {
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

    pub(crate) fn push(&mut self, message: Message) {
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

    pub(crate) fn note(&mut self, text: impl Into<String>) {
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

    pub(crate) fn refresh_status(&mut self) {
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

/// 当前目录（或 `partial` 指向的目录）里的条目，按路径前缀补全。
fn path_candidates(partial: &str) -> Vec<Candidate> {
    let (dir, _) = match partial.rfind(['/', '\\']) {
        Some(index) => partial.split_at(index + 1),
        None => ("", partial),
    };
    let base = if dir.is_empty() { "." } else { dir };
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut out: Vec<Candidate> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let is_dir = entry.path().is_dir();
            Some(Candidate {
                value: format!("{dir}{name}{}", if is_dir { "/" } else { "" }),
                help: if is_dir { "dir".into() } else { String::new() },
            })
        })
        .collect();
    out.sort_by(|a, b| a.value.cmp(&b.value));
    out
}

/// 已导入文本的标题。只查标题，不读正文。
fn text_candidates(app: &App) -> Vec<Candidate> {
    app.store
        .text_titles()
        .unwrap_or_default()
        .into_iter()
        .map(|title| Candidate {
            value: title,
            help: "text".into(),
        })
        .collect()
}

/// 命中但没显示出来的条数。
fn hidden_suggestions(input: &str) -> usize {
    command::matches(input)
        .len()
        .saturating_sub(MAX_SUGGESTIONS)
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
        // 没有内置词书了，测试自己准备一个小词库。
        store.import_plan(&fixture_plan()).unwrap();
        let app = App::new(Box::new(store)).unwrap();
        Harness { app, _dir: dir }
    }

    /// 三词的小词库，够覆盖新词、复习、评分与撤销。
    fn fixture_plan() -> qe_core::model::ImportPlan {
        use qe_core::model::{Example, ImportPlan, Word};

        let mut plan = ImportPlan::new("test", "Test Deck", "fixture");
        for (lemma, translation) in [
            ("inspect", "v. 检查"),
            ("consistent", "a. 一致的"),
            ("reject", "v. 拒绝"),
        ] {
            let mut word = Word::new(lemma, translation);
            word.phonetic = Some(format!("/{lemma}/"));
            word.examples.push(Example::new(
                format!("They {lemma} it."),
                Some(format!("他们{translation}。")),
            ));
            plan.words.push(word);
        }
        plan
    }

    fn card_count(app: &App) -> usize {
        app.messages.iter().filter(|m| m.card_state().is_some()).count()
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
    fn m_unfolds_only_the_card_in_front() {
        let Harness { mut app, _dir } = harness();
        app.lookup("inspect").unwrap();
        let lookup_card = app
            .messages
            .iter()
            .rposition(|message| message.card_state().is_some())
            .expect("the lookup pushed a card");
        app.execute(Command::Study(Some(1))).unwrap();
        app.advance().unwrap();

        let expanded = |app: &App, index: usize| {
            app.messages[index]
                .card_state()
                .expect("a card message")
                .expand_glosses
        };
        let study_card = app.session.as_ref().unwrap().card_msg.unwrap();
        assert!(expanded(&app, lookup_card), "a lookup shows everything");
        assert!(!expanded(&app, study_card), "a study card starts folded");

        app.toggle_glosses();
        assert_eq!(app.session.as_ref().unwrap().card_msg, Some(study_card));
        assert!(expanded(&app, study_card), "m opens the current card");

        // The second press closes only that card; the older lookup keeps its
        // own expanded state, so the key never rewrites history.
        app.toggle_glosses();
        assert!(!expanded(&app, study_card));
        assert!(expanded(&app, lookup_card));
    }

    #[test]
    fn translate_toggle_actually_changes_the_reading_card() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("bilingual.txt");
        std::fs::write(
            &path,
            "From an early age I knew I should be a writer.\n\n---\n\n从很小的时候，我就知道我要当作家。\n",
        )
        .unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        app.execute(Command::Mode("intensive".into())).unwrap();

        let card = |app: &App| {
            app.messages
                .iter()
                .rev()
                .find_map(|message| match &message.body {
                    Body::Tool(view) if view.name == "read" => Some(view.clone()),
                    _ => None,
                })
                .expect("reading card")
        };
        let has_translation = |view: &ToolView| {
            view.lines.iter().any(|line| {
                matches!(line, qe_core::view::ToolLine::Muted(text) if text.contains("从很小的时候"))
            })
        };

        assert!(!has_translation(&card(&app)), "hidden by default");
        app.execute(Command::Translate).unwrap();
        assert!(has_translation(&card(&app)), "/translate must show it");
        app.execute(Command::Translate).unwrap();
        assert!(!has_translation(&card(&app)), "/translate hides it again");
    }

    #[test]
    fn mark_all_covers_every_word_not_just_the_nine_picks() {
        let Harness { mut app, _dir } = harness();
        let words = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima",
        ];
        let path = _dir.path().join("long.txt");
        std::fs::write(&path, format!("{}\n", words.join(" "))).unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();

        // 候选列表被数字键限制在 9 条，但 `/mark all` 不能跟着截断。
        assert_eq!(app.reading_candidates().len(), 9);
        app.execute(Command::Mark("all".into())).unwrap();
        let marked = app.reading.as_ref().unwrap().marked.len();
        assert!(marked >= words.len(), "all {words:?} words marked, got {marked}");
    }

    fn reading_with_quiz(app: &mut App, dir: &tempfile::TempDir) -> String {
        let path = dir.path().join("article.txt");
        std::fs::write(&path, "First paragraph here.\n\nSecond paragraph here.").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        let text = app.store.text_by_title("article").unwrap().unwrap();

        let mut choice = qe_core::model::Quiz::new("Pick one", "yes")
            .at_paragraph(0)
            .with_source("fixture");
        choice.choices = vec!["yes".into(), "no".into()];
        choice.text_id = text.id;
        app.store.add_quiz(&choice).unwrap();

        let mut blank = qe_core::model::Quiz::new("Fill this", "written")
            .at_paragraph(0)
            .with_source("fixture");
        blank.text_id = text.id;
        app.store.add_quiz(&blank).unwrap();
        text.title
    }

    #[test]
    fn a_fill_in_question_is_revealed_without_recording_a_wrong_answer() {
        let Harness { mut app, _dir } = harness();
        reading_with_quiz(&mut app, &_dir);
        app.start_quiz().unwrap();
        // 先答选择题，再翻到填空题。
        app.answer_quiz(Some(1)).unwrap();
        app.answer_quiz(None).unwrap(); // 翻到下一题（填空题）
        assert_eq!(app.quiz_answered, None);
        assert!(!app.quiz_revealed, "not revealed until asked");

        app.answer_quiz(None).unwrap(); // 揭示答案
        assert_eq!(app.quiz_answered, None);
        assert!(app.quiz_revealed, "fill-in reveals its answer");

        // 只有选择题写了流水；填空题不该留下一条「错误」记录。
        let stats = app.store.daily_reading(qe_core::time::today()).unwrap();
        assert_eq!(stats.quizzes, 1, "exactly one attempt was recorded");
        assert_eq!(stats.correct, 1);

        app.answer_quiz(None).unwrap();
        assert!(app.quizzes.is_empty(), "the round ends after the last question");
    }

    #[test]
    fn a_second_quiz_round_starts_the_score_over() {
        let Harness { mut app, _dir } = harness();
        reading_with_quiz(&mut app, &_dir);

        app.start_quiz().unwrap();
        app.answer_quiz(Some(1)).unwrap();
        app.answer_quiz(None).unwrap(); // 翻到下一题
        app.answer_quiz(None).unwrap(); // 揭示答案
        app.answer_quiz(None).unwrap(); // 结束
        assert!(app.quizzes.is_empty());
        assert_eq!(app.quiz_correct, 1);

        app.start_quiz().unwrap();
        assert_eq!(app.quiz_correct, 0, "a new round must not inherit the score");
    }

    #[test]
    fn drop_deletes_a_text_with_its_progress_and_marks() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("doomed.txt");
        std::fs::write(&path, "Only paragraph here.").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        let id = app.store.texts().unwrap()[0].id;
        app.store.add_reading_mark(id, "paragraph", 0).unwrap();

        app.execute(Command::Drop("doomed".into())).unwrap();
        assert!(app.store.texts().unwrap().is_empty());
        assert!(app.reading.is_none(), "the reading session is closed");
        assert!(
            app.store.setting("last_text").unwrap().unwrap_or_default().is_empty(),
            "stale last_text would break a bare /read"
        );

        // 不存在的标题只提示，不报错。
        app.execute(Command::Drop("nope".into())).unwrap();
    }

    fn reading_fixture(app: &mut App, dir: &tempfile::TempDir) -> String {
        // 用 fixture 词书里的词，这样「选中词有词条」和「选中词查不到」两条路径
        // 都能在同一段里测到（`the` 不在词书里）。
        let path = dir.path().join("flow.txt");
        std::fs::write(
            &path,
            "Inspect the consistent reject here.\n\nSecond paragraph here.",
        )
        .unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        "flow".to_string()
    }

    fn reading_card_view(app: &App) -> ToolView {
        app.messages
            .iter()
            .rev()
            .find_map(|message| match &message.body {
                Body::Tool(view) if view.name == "read" => Some((**view).clone()),
                _ => None,
            })
            .expect("reading card")
    }

    #[test]
    fn comma_and_period_move_the_selection_and_show_it_inline() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);

        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focused_lemma().as_deref(), Some("inspect"));

        // 选中后，说明块就在同一张阅读卡里，不是新推一条消息。
        let before = app.messages.len();
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.messages.len(), before, "the card is replaced in place");
        assert_eq!(app.focused_lemma().as_deref(), Some("the"));

        let view = reading_card_view(&app);
        assert!(
            view.lines
                .iter()
                .any(|line| matches!(line, qe_core::view::ToolLine::Accent(text) if text.contains("the"))),
            "the selected word's headword is drawn inline"
        );
        // `the` 不在测试词书里：说明块仍要出现，并说明词典没有这个词。
        assert!(
            view.lines.iter().any(|line| matches!(
                line,
                qe_core::view::ToolLine::Muted(text) if text.contains("no definition")
            )),
            "an unknown word still gets a panel"
        );
    }

    #[test]
    fn escaping_clears_the_selection_before_ending_the_reading() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.reading.is_some(), "first esc only drops the selection");
        assert!(app.focused_lemma().is_none());

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.reading.is_none(), "second esc ends the reading");
    }

    #[test]
    fn enter_opens_the_full_card_for_the_selected_word() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        let before = card_count(&app);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(card_count(&app), before + 1, "a full word card is appended");
        assert_eq!(app.reading.as_ref().unwrap().opened, 1);
    }

    /// 用渲染层给的命中区间模拟一次点击：这就是鼠标事件走的那条路径。
    fn click_word(app: &mut App, pick: impl Fn(&qe_tui::lines::WordBox) -> bool) -> String {
        let message_index = app
            .reading
            .as_ref()
            .and_then(|session| session.message)
            .expect("card message index");
        let (_, hits) = qe_tui::lines::message_lines_hits(&app.messages[message_index], 80);
        let hit = *hits
            .words
            .iter()
            .find(|hit| pick(hit))
            .expect("a matching word box");

        app.draw_info.word_hits = hits
            .words
            .into_iter()
            .map(|hit| qe_tui::WordHit {
                row: hit.row as u16 + 1,
                start: hit.start,
                end: hit.end,
                token: hit.token,
                paragraph: hit.paragraph,
                message: message_index,
            })
            .collect();
        app.open_row_at(hit.row as u16 + 1, hit.start + 1);
        app.focused_lemma().unwrap_or_default()
    }

    #[test]
    fn clicking_a_word_in_the_paragraph_selects_it() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);

        let clicked = click_word(&mut app, |hit| hit.token == 2);
        assert_eq!(clicked, "consistent", "clicked word");
    }

    #[test]
    fn clicking_a_word_below_the_current_paragraph_jumps_there_first() {
        // 词序号只在段内有效：点第二段的第 3 个词，不能拿这个序号去第一段选词。
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();
        assert_eq!(app.reading.as_ref().unwrap().paragraph, 0);

        // 第二段是 "Second paragraph here."：token 2 = here。
        let clicked = click_word(&mut app, |hit| hit.paragraph == Some(1) && hit.token == 2);
        assert_eq!(clicked, "here", "the clicked word, not the first paragraph's");
        assert_eq!(
            app.reading.as_ref().unwrap().paragraph,
            1,
            "the reading position follows the click"
        );
        assert_eq!(
            app.store
                .reading_progress(app.store.text_by_title("flow").unwrap().unwrap().id)
                .unwrap()
                .map(|(paragraph, _)| paragraph),
            Some(1),
            "progress is persisted"
        );
    }

    #[test]
    fn closing_reading_keeps_the_paragraph_reached_by_a_click() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();
        click_word(&mut app, |hit| hit.paragraph == Some(1) && hit.token == 0);
        app.finish_reading().unwrap();

        let id = app.store.texts().unwrap()[0].id;
        assert_eq!(
            app.store.reading_progress(id).unwrap().map(|(p, _)| p),
            Some(1),
            "resuming lands on the paragraph the reader clicked"
        );
    }

    #[test]
    fn slash_mark_with_no_argument_marks_the_selected_word() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        let focused = app.focused_lemma().expect("a selection");

        app.execute(Command::Mark(String::new())).unwrap();
        assert!(
            app.reading.as_ref().unwrap().is_marked(&focused),
            "the selected word is the one that gets marked"
        );
    }

    #[test]
    fn scroll_to_anchor_brings_an_offscreen_paragraph_into_view() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();

        // 模拟一帧：40 行内容、10 行视口、锚点在第 25 行，现在停在顶部。
        app.draw_info.transcript = ratatui::layout::Rect::new(0, 0, 80, 10);
        app.draw_info.max_back = 30;
        app.draw_info.anchor_line = Some(25);
        app.scroll_back = 30; // offset = 0 → 锚点在第 25 行，视野外
        app.scroll_to_anchor();
        assert_eq!(app.scroll_back, 6, "anchor moves to the second visible line");

        // 已经在视野内就不动，免得读者刚滚到别处就被拽回来。
        app.draw_info.anchor_line = Some(30); // offset 24 → 屏幕第 6 行
        let before = app.scroll_back;
        app.scroll_to_anchor();
        assert_eq!(app.scroll_back, before, "a visible anchor is left alone");

        // 锚点跑到视口上方（往回翻）时同样要滚回去。
        app.draw_info.anchor_line = Some(10);
        app.scroll_to_anchor();
        assert_eq!(app.scroll_back, 21, "an anchor above the viewport scrolls up");
    }

    #[test]
    fn selecting_a_word_keeps_the_reader_in_place_in_full_mode() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();
        // 读者已经滚到文章中间。
        app.scroll_back = 12;
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        assert!(
            app.reveal_anchor,
            "the next frame should check whether the paragraph is on screen"
        );
        assert_eq!(app.scroll_back, 12, "selection must not jump to the end");
    }

    #[test]
    fn selecting_a_word_with_phrases_shows_them_next_to_the_definition() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        // fixture 段落是 "Inspect the consistent reject here."：
        // 只有真的出现在这段里的词组才会被提示。
        app.store
            .add_collocation("inspect", "inspect the", "检查那些")
            .unwrap();
        app.store
            .add_collocation("inspect", "inspect for", "检查是否存在")
            .unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focused_lemma().as_deref(), Some("inspect"));

        let view = reading_card_view(&app);
        let split = view
            .lines
            .iter()
            .find_map(|line| match line {
                qe_core::view::ToolLine::Split { right, .. } => Some(right.clone()),
                _ => None,
            })
            .expect("a phrase column for the matched phrase");
        assert!(
            !split
                .iter()
                .any(|line| matches!(line, qe_core::view::ToolLine::Muted(_))),
            "no header row: the phrases start on the first line so they line up \
             with the headword"
        );
        let phrases: Vec<(String, String)> = split
            .iter()
            .filter_map(|line| match line {
                qe_core::view::ToolLine::PhraseInline { phrase, gloss } => {
                    Some((phrase.clone(), gloss.clone()))
                }
                qe_core::view::ToolLine::Phrase { phrase, gloss } => {
                    Some((phrase.clone(), gloss.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            phrases,
            vec![("inspect the".to_string(), "检查那些".to_string())],
            "only the phrase the paragraph actually uses"
        );
    }

    #[test]
    fn a_phrase_absent_from_the_paragraph_is_not_shown() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        // `inspect for` 不在本段原文里，选中 inspect 时不该出现。
        app.store
            .add_collocation("inspect", "inspect for", "检查是否存在")
            .unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        let view = reading_card_view(&app);
        assert!(
            !view.lines.iter().any(|line| matches!(
                line,
                qe_core::view::ToolLine::Split { .. } | qe_core::view::ToolLine::Phrase { .. }
            )),
            "nothing to show when the paragraph does not use the phrase"
        );
    }

    /// 导入一篇带 `# book:` 的文本，返回标题。
    fn import_chapter(app: &mut App, dir: &tempfile::TempDir, file: &str, title: &str, body: &str) {
        let path = dir.path().join(file);
        std::fs::write(
            &path,
            format!("# title: {title}\n# book: Animal Farm\n# source: Project Gutenberg\n\n{body}\n"),
        )
        .unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
    }

    #[test]
    fn chapters_from_the_same_file_header_land_in_one_book() {
        let Harness { mut app, _dir } = harness();
        import_chapter(&mut app, &_dir, "ch1.txt", "Chapter One", "First.\n\nSecond.");
        import_chapter(&mut app, &_dir, "ch2.txt", "Chapter Two", "Third.\n\nFourth.");

        let books = app.store.text_books().unwrap();
        assert_eq!(books.len(), 1, "same `# book:` header → one book: {books:?}");
        assert_eq!(books[0].slug, "animal-farm");

        app.execute(Command::Library).unwrap();
        assert!(
            app.messages
                .iter()
                .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "library")),
            "the library block lists books"
        );
    }

    #[test]
    fn reading_a_book_opens_its_next_unfinished_chapter() {
        let Harness { mut app, _dir } = harness();
        import_chapter(&mut app, &_dir, "ch1.txt", "Chapter One", "First.\n\nSecond.");
        import_chapter(&mut app, &_dir, "ch2.txt", "Chapter Two", "Third.\n\nFourth.");

        app.execute(Command::Read("animal-farm".into())).unwrap();
        assert_eq!(
            app.reading.as_ref().map(|s| s.text.title.as_str()),
            Some("Chapter One"),
            "a fresh book starts at the first chapter"
        );

        // 读完第一篇（两段）。
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        assert_eq!(
            app.reading.as_ref().map(|s| s.text.title.as_str()),
            Some("Chapter Two"),
            "finishing a chapter continues with the next one"
        );
    }

    #[test]
    fn a_finished_chapter_is_marked_done_in_the_book() {
        let Harness { mut app, _dir } = harness();
        import_chapter(&mut app, &_dir, "ch1.txt", "Chapter One", "First.\n\nSecond.");

        app.execute(Command::Read("animal-farm".into())).unwrap();
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        if app.reading.is_some() {
            app.finish_reading().unwrap();
        }
        let progress = app.store.text_book_progress("animal-farm").unwrap();
        assert_eq!((progress.finished, progress.total), (1, 1));
        assert!(progress.is_done());
    }

    #[test]
    fn a_text_without_a_book_does_not_chain_anywhere() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("alone.txt");
        std::fs::write(&path, "One.\n\nTwo.").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        assert!(
            app.reading.is_some(),
            "a loose text stays open instead of jumping elsewhere"
        );
        assert_eq!(app.reading.as_ref().unwrap().paragraph, 1);
    }

    #[test]
    fn starting_to_study_closes_the_reading_session() {
        // 先 `/read` 再 `/study`：两个队列同时活着时，数字键会被阅读的「打开第 n
        // 个词」抢走，显示成查词卡片，评分也不会写进库。
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("article.txt");
        std::fs::write(&path, "First paragraph here.\n\nSecond paragraph here.").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        assert!(app.reading.is_some(), "reading started");

        app.execute(Command::Study(Some(2))).unwrap();
        assert!(app.reading.is_none(), "study takes over the number keys");
        assert!(app.session.is_some());

        app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.store.daily(qe_core::time::today()).unwrap().reviews,
            1,
            "1-4 grades instead of opening a word"
        );
        assert!(
            app.messages.iter().all(|message| message
                .card_state()
                .map(|state| state.badge != "lookup")
                .unwrap_or(true)),
            "no lookup card appears while studying"
        );
    }

    #[test]
    fn study_keys_win_over_reading_even_if_both_survive() {
        // 防御性：即便两个会话同时存在（旧状态、或将来有新入口），学习卡在屏幕上
        // 时数字键也只能用于评分。
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("article.txt");
        std::fs::write(&path, "First paragraph here.").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        app.execute(Command::Study(Some(1))).unwrap();
        // 把阅读会话塞回去，模拟互斥失效的状态。
        let reading = qe_core::reading::ReadingSession::new(
            app.store
                .text_by_title("article")
                .unwrap()
                .expect("the imported text"),
            qe_core::reading::ReadMode::Full,
            0,
        );
        app.reading = Some(reading);

        app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.store.daily(qe_core::time::today()).unwrap().reviews,
            1,
            "the rating was recorded, not a lookup"
        );
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
    fn regrading_from_again_to_good_does_not_leave_a_ghost_card() {
        // Pressing 1 requeues the word so it comes back later in the session.
        // Changing the answer to 3 afterwards must take that copy back out,
        // otherwise the same word is graded twice.
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(1))).unwrap();
        app.grade(Rating::Again).unwrap();
        let requeued = app.session.as_ref().unwrap().queue.len();
        assert_eq!(requeued, 1, "again requeues the word");

        app.grade(Rating::Good).unwrap();
        assert_eq!(
            app.session.as_ref().unwrap().queue.len(),
            0,
            "the requeued copy must go away when the rating changes"
        );
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
        assert!(
            app.messages.iter().any(|m| m.card_state().is_some()),
            "lookup renders a word card"
        );

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
        // 第二个词书用来验证切换；内置示例词书已经去掉了。
        let mut second = qe_core::model::ImportPlan::new("other", "Other Deck", "fixture");
        second
            .words
            .push(qe_core::model::Word::new("serendipity", "n. 意外发现"));
        app.store.import_plan(&second).unwrap();

        app.switch_book("other").unwrap();
        assert_eq!(app.book.as_ref().unwrap().slug, "other");
        assert_eq!(app.store.current_book().unwrap().unwrap().slug, "other");
    }

    #[test]
    fn study_respects_daily_limit() {
        let Harness { mut app, _dir } = harness();
        app.daily_new_limit = 2;
        app.execute(Command::Study(None)).unwrap();
        assert_eq!(app.session.as_ref().unwrap().new_total, 2);
    }

    #[test]
    fn phrase_command_records_a_collocation() {
        let Harness { mut app, _dir } = harness();
        // Syntax is `/phrase <lemma> <phrase> = <gloss>`; the lemma comes first, so
        // `sake`-style entries read `/phrase sake for the sake of = 为了`.
        app.execute(Command::Phrase("inspect for inspection = 供检查".into()))
            .unwrap();

        let word = app.store.word("inspect").unwrap().unwrap();
        assert_eq!(word.collocations.len(), 1);
        assert_eq!(word.collocations[0].phrase, "for inspection");
        assert_eq!(word.collocations[0].gloss, "供检查");
    }

    #[test]
    fn phrase_command_reports_duplicates_and_missing_words() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Phrase("inspect for inspection".into()))
            .unwrap();
        app.execute(Command::Phrase("inspect for inspection".into()))
            .unwrap();

        assert_eq!(
            app.store.word("inspect").unwrap().unwrap().collocations.len(),
            1,
            "second add is a no-op"
        );
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Text(t) if t.contains("already listed"))));

        app.execute(Command::Phrase("nosuchword some phrase".into()))
            .unwrap();
        assert!(app
            .messages
            .iter()
            .any(|m| matches!(&m.body, Body::Tool(t) if t.name == "error")));
    }

    #[test]
    fn tab_completes_a_deck_slug_and_cycles_the_rest() {
        let Harness { mut app, _dir } = harness();
        let mut second = qe_core::model::ImportPlan::new("cet6", "CET-6 核心词", "fixture");
        second
            .words
            .push(qe_core::model::Word::new("serendipity", "n. 意外发现"));
        app.store.import_plan(&second).unwrap();

        // 唯一命中：`/book te` 补全成 `test`。
        app.editor.set_text("/book te");
        app.tab_complete();
        assert_eq!(app.editor.text(), "/book test");

        // 有歧义时先补公共前缀，补不动了就开始循环候选。
        app.editor.set_text("/book c");
        app.tab_complete();
        assert_eq!(app.editor.text(), "/book cet6");
        app.tab_complete();
        assert_eq!(app.editor.text(), "/book cet6", "only one deck starts with `cet6`");
    }

    #[test]
    fn tab_lists_the_candidates_for_an_ambiguous_argument() {
        let Harness { mut app, _dir } = harness();
        for slug in ["cet4", "cet6"] {
            let mut plan = qe_core::model::ImportPlan::new(slug, slug.to_uppercase(), "fixture");
            plan.words.push(qe_core::model::Word::new("alpha", "n. 甲"));
            app.store.import_plan(&plan).unwrap();
        }

        let (rows, _) = app.suggestions_for("/book cet");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["cet4", "cet6"]);
        assert_eq!(rows[0].help, "CET4", "the panel names the deck, not just the slug");

        // 高亮移动之后 tab 填的是高亮那条，而不是第一个。
        app.editor.set_text("/book cet");
        app.suggestion_index = 1;
        app.tab_complete();
        assert_eq!(app.editor.text(), "/book cet6");
    }

    #[test]
    fn the_argument_ghost_shows_the_unique_remainder() {
        let Harness { app, _dir } = harness();
        assert_eq!(app.arg_ghost("/book te").as_deref(), Some("st"));
        assert_eq!(app.arg_ghost("/book zz"), None);
        // 命令名本身由 command::ghost 负责，参数提示不抢。
        assert_eq!(app.arg_ghost("/bo"), None);
    }

    #[test]
    fn free_text_arguments_offer_no_candidates_but_a_usage_hint() {
        let Harness { mut app, _dir } = harness();
        // 以前这里会把词书和当前目录的文件名混进 `/search` 的提示里。
        assert!(app.arg_candidates("/search", "ins").is_empty());
        assert!(app.arg_candidates("/add", "").is_empty());

        app.editor.set_text("/search ");
        app.tab_complete();
        assert!(
            app.messages
                .iter()
                .any(|m| matches!(&m.body, Body::Text(t) if t.contains("usage: /search <query>"))),
            "tab still explains the argument"
        );
    }

    #[test]
    fn import_completes_paths_in_subdirectories() {
        let Harness { app, _dir } = harness();
        let packs = _dir.path().join("packs").join("decks");
        std::fs::create_dir_all(&packs).unwrap();
        std::fs::write(packs.join("CET6_T.json"), "[]").unwrap();

        // 补全按「最后一个分隔符之前」找目录，所以绝对路径也能用。
        let root = _dir.path().to_string_lossy().replace('\\', "/");
        let partial = format!("{root}/pack");
        let candidates = app.arg_candidates("/import", &partial);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.value.ends_with("packs/")),
            "打字到 `pack` 应该能把 `packs/` 补出来: {:?}",
            candidates.iter().map(|c| &c.value).collect::<Vec<_>>()
        );
        assert!(candidates
            .iter()
            .any(|candidate| candidate.help == "dir"));

        let deeper = format!("{root}/packs/de");
        let decks = app.arg_candidates("/import", &deeper);
        assert!(
            decks.iter().any(|candidate| candidate.value.ends_with("decks/")),
            "二级目录也要往下走: {:?}",
            decks.iter().map(|c| &c.value).collect::<Vec<_>>()
        );
        let file = app.arg_candidates("/import", &format!("{root}/packs/decks/CET"));
        assert!(file.iter().any(|candidate| candidate.value.ends_with("CET6_T.json")));
    }

    #[test]
    fn suggestions_and_ghost_track_the_input() {
        // `/st` 命中 /study 和 /summary（别名 stats）两条，规范名那条排前面。
        let ghost = command::ghost("/st");
        assert_eq!(ghost.as_deref(), Some("udy"));
        let suggestions = build_suggestions("/st", 0);
        assert_eq!(suggestions[0].name, "/study");
        assert!(suggestions[0].selected);
        assert!(build_suggestions("inspect", 0).is_empty());

        // 唯一命中时 tab 补成规范名。
        assert_eq!(command::complete("/stu"), Some("/study ".to_string()));
    }

    #[test]
    fn aliases_are_completable() {
        // 短写也要出现在提示里，否则用户只能靠记。
        assert_eq!(
            command::ghost("/n").as_deref(),
            Some("ext"),
            "alias n -> /next"
        );
        assert_eq!(command::complete("/n"), Some("/next ".to_string()));
        assert_eq!(command::complete("/go"), Some("/goto ".to_string()));
        assert!(command::matches("/db").iter().any(|s| s.name == "/db"));
        assert!(command::matches("/where").iter().any(|s| s.name == "/db"));
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
    fn undo_after_again_takes_the_requeued_copy_back_out() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        let before_len = app.session.as_ref().unwrap().queue.len();
        let before_total = app.session.as_ref().unwrap().review_total;

        app.grade(Rating::Again).unwrap();
        assert_eq!(app.session.as_ref().unwrap().queue.len(), before_len + 1);

        app.undo_last().unwrap();

        let session = app.session.as_ref().unwrap();
        assert_eq!(
            session.queue.len(),
            before_len,
            "the requeued copy must be removed"
        );
        assert_eq!(session.review_total, before_total, "total shrinks back");
    }

    #[test]
    fn regrading_again_does_not_queue_a_second_copy() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        let before = app.session.as_ref().unwrap().queue.len();

        app.grade(Rating::Again).unwrap();
        // Re-pick on the same card: amend, not a second review.
        app.grade(Rating::Again).unwrap();

        let session = app.session.as_ref().unwrap();
        assert_eq!(
            session.queue.len(),
            before + 1,
            "only the first `again` may requeue"
        );
        assert_eq!(app.store.daily(time::today()).unwrap().reviews, 1);
    }

    #[test]
    fn undo_after_advancing_keeps_the_next_card() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(4))).unwrap();
        let graded = app.session.as_ref().unwrap().current.clone().unwrap();

        app.grade(Rating::Good).unwrap();
        app.advance().unwrap(); // moves on to the next card
        let next = app.session.as_ref().unwrap().current.clone().unwrap();
        let queue_len = app.session.as_ref().unwrap().queue.len();

        app.undo_last().unwrap();

        let session = app.session.as_ref().unwrap();
        assert_eq!(
            session.current.as_ref().map(|card| card.word.id),
            Some(graded.word.id),
            "the undone card is current again"
        );
        // The card we had moved on to is back in the queue, not lost.
        assert!(
            session.queue.iter().any(|card| card.word.id == next.word.id),
            "the next card must be requeued"
        );
        assert_eq!(session.queue.len(), queue_len + 1);
    }

    #[test]
    fn skipping_an_ungraded_card_keeps_totals_in_step() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        let before = app.session.as_ref().unwrap().total();

        app.reveal().unwrap(); // peek without grading
        app.advance().unwrap(); // skip it

        let session = app.session.as_ref().unwrap();
        assert_eq!(
            session.total(),
            before - 1,
            "a skipped card must leave the denominator"
        );
        assert_eq!(app.store.daily(time::today()).unwrap().reviews, 0);
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
    fn burying_a_new_word_shrinks_the_session_instead_of_leaving_it_short() {
        // A new word has no `cards` row, so postponing it writes nothing. It still
        // has to leave the session's totals, or the badge stops at 2/3 forever.
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        app.bury_current().unwrap();

        let session = app.session.as_ref().unwrap();
        assert_eq!(session.new_total, 2, "the buried word leaves the count");
        assert_eq!(session.total(), session.done() as usize + 2, "badge stays reachable");

        // Grade the rest and the session must consider itself complete.
        for _ in 0..2 {
            app.grade(Rating::Good).unwrap();
            app.advance().unwrap();
        }
        assert!(
            app.session.is_none(),
            "grading every remaining card ends the session"
        );
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
