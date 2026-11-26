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
use qe_tui::{draw, DrawInfo, Suggestion, TerminalSession, Toast, View, ViewMode};

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

/// 学习卡流式的时长（毫秒）：内容按进度从头吐出，到点给全量。
const STUDY_STREAM_MS: u64 = 600;

/// 会话在对话里保留的卡片消息上限：留最近十张可回翻，
/// 又不让背词时长推高每帧的全量重排版成本。
const KEEP_CARD_MESSAGES: usize = 10;

/// 一条右上角的悬浮提示：不写进对话历史，[`App::live_toasts`] 到点剔除。
#[derive(Debug)]
struct FloatingNote {
    text: String,
    warn: bool,
    until: Instant,
}

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
    pub(crate) status: String,
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
    /// 当前段译文请求的状态，显示在阅读卡片中。
    pub(crate) translation_status: Option<String>,
    /// 当前段译文来源，显示在全篇阅读的段落锚点后。
    pub(crate) translation_provider: Option<String>,
    /// 下一帧把当前段滚进视野（全篇模式下翻页、跳段后使用）。
    pub(crate) reveal_anchor: bool,
    /// 下一帧保持画面不动：内容在原位变长/变短（选词出说明块、切译文）时，
    /// 按锚点行的位移补回滚动量，读者看到的行不会整体滑动。
    pub(crate) pin_view: bool,
    /// 上一帧段落锚点的行号，用来算 `pin_view` 要补的位移。
    prev_anchor: Option<usize>,
    /// 上次为阅读进度条重绘的时间。
    reading_progress_tick: Instant,
    /// 学习卡流式的起点：内容按进度从头吐出，[`STUDY_STREAM_MS`] 出齐。
    study_fade_started: Option<Instant>,
    /// 阅读过渡的起点：新文章的段落按顺序分批加载。
    pub(crate) reading_fade_started: Option<Instant>,
    /// 读完一篇后，等空格接上的下一篇标题。
    pub(crate) pending_next_text: Option<String>,
    /// 后台抓取朗读材料的结果通道；`Some` 表示有任务在跑。
    news_fetch: Option<std::sync::mpsc::Receiver<Result<crate::news::FetchReport, String>>>,
    /// `/practice <url>` 后台导入翻译练习的结果通道。
    practice_fetch:
        Option<std::sync::mpsc::Receiver<Result<crate::practice::ImportReport, String>>>,
    /// 背完一轮后 AI 总结建议的后台通道。
    advice_fetch:
        Option<std::sync::mpsc::Receiver<Result<crate::translator::Generated, String>>>,
    /// 右上角的悬浮提示（后台任务、瞬时状态），几秒自动消失。
    floating: Vec<FloatingNote>,
    /// 后台机翻一段的结果通道：`(文本 id, 段落, 段落结果)`。
    pub(crate) translation_fetch: Option<(
        i64,
        usize,
        std::sync::mpsc::Receiver<(usize, Result<crate::translator::Translation, String>)>,
    )>,
    /// 数据库路径：抓取线程要自己开一条连接，所以这里存一份。
    db_path: Option<std::path::PathBuf>,
}

impl App {
    pub fn new(store: Box<dyn Store>) -> Result<Self> {        let book = store.current_book()?;
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
            // 译文逐段显示、默认隐藏：先读原文，需要对照时再按 tab。
            translation_status: None,
            translation_provider: None,
            reveal_anchor: false,
            pin_view: false,
            prev_anchor: None,
            reading_progress_tick: Instant::now(),
            study_fade_started: None,
            reading_fade_started: None,
            pending_next_text: None,
            news_fetch: None,
            practice_fetch: None,
            advice_fetch: None,
            floating: Vec::new(),
            translation_fetch: None,
            db_path: None,
        };
        app.load_history();
        app.greet()?;
        Ok(app)
    }

    /// 启用「每天首次启动抓一批双语新闻」。
    ///
    /// 记一个 `news_fetched_on = 2026-11-12` 的设置，同一天只抓一次；抓取放在
    /// 后台线程里（自己开一条 SQLite 连接），主界面不阻塞。
    pub fn enable_daily_news(&mut self, db_path: std::path::PathBuf) -> Result<()> {
        self.db_path = Some(db_path);
        let today = qe_core::time::format_day(qe_core::time::today());
        if self.store.setting("news_fetched_on")?.as_deref() == Some(today.as_str()) {
            return Ok(());
        }
        self.spawn_news_fetch()
    }

    /// 立刻抓一次（`/news`），不受「今天已抓过」限制。
    pub fn fetch_news_now(&mut self) -> Result<()> {
        if self.news_fetch.is_some() {
            self.float("already fetching news · one moment");
            return Ok(());
        }
        if self.db_path.is_none() {
            self.note("news fetching is only wired up in the TUI binary");
            return Ok(());
        }
        self.spawn_news_fetch()
    }

    /// 右上角悬浮几秒的提示：后台任务的开始/失败、瞬时状态这类**不需要留档**
    /// 的信息才进来——要能回看的仍走 [`Self::note`] 进对话。
    fn float(&mut self, text: impl Into<String>) {
        self.push_floating(text, false);
    }

    /// 警告样式的悬浮提示（红字），用于失败类消息。
    fn float_warn(&mut self, text: impl Into<String>) {
        self.push_floating(text, true);
    }

    fn push_floating(&mut self, text: impl Into<String>, warn: bool) {
        self.floating.push(FloatingNote {
            text: text.into(),
            warn,
            until: Instant::now() + Duration::from_secs(5),
        });
        // 最多叠四条：新的顶掉最老的，别糊满右上角。
        if self.floating.len() > 4 {
            self.floating.remove(0);
        }
    }

    /// 给这一帧的悬浮提示：先剔除过期的，再映射成渲染层要的 `Toast`。
    fn live_toasts(&mut self) -> Vec<Toast> {
        self.floating
            .retain(|note| note.until > Instant::now());
        self.floating
            .iter()
            .map(|note| Toast {
                text: note.text.clone(),
                warn: note.warn,
            })
            .collect()
    }

    fn spawn_news_fetch(&mut self) -> Result<()> {
        let Some(db) = self.db_path.clone() else {
            return Ok(());
        };

        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // 每个来源各抓一批；单个来源挂了不影响别的（TED 打不开也得把新华网抓回来）。
            let outcome = crate::news::fetch_all(&db).map_err(|err| format!("{err:#}"));
            let _ = sender.send(outcome);
        });
        self.news_fetch = Some(receiver);
        self.float("fetching reading material in the background …");
        Ok(())
    }

    /// 抓取线程有结果了就报到对话里；每帧调一次。
    fn poll_news_fetch(&mut self) {
        let Some(receiver) = self.news_fetch.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(report)) => {
                self.news_fetch = None;
                let _ = self.store.set_setting(
                    "news_fetched_on",
                    &qe_core::time::format_day(qe_core::time::today()),
                );
                if report.saved() > 0 {
                    let mut view = ToolView::new("news")
                        .accent(Accent::Note)
                        .result(report.summary());
                    for (name, detail) in report.lines() {
                        view = view.kv(name, detail);
                    }
                    self.push(Message::tool(
                        view.muted("/library shows the books · /read <slug> starts one"),
                    ));
                } else {
                    self.float(format!("news: nothing new · {}", report.summary()));
                }
            }
            Ok(Err(error)) => {
                self.news_fetch = None;
                // 网络不通不该打断阅读：给一行提示，下次启动再试。
                self.float_warn(format!("news fetch failed · {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.news_fetch = None;
            }
        }
    }

    /// `/practice`：不带参数（或 `list`）列合集；`daily` 立刻跑一轮每日发现；
    /// 其余当 URL，后台导入。
    fn practice_command(&mut self, arg: &str) -> Result<()> {
        let arg = arg.trim();
        if arg.is_empty() || arg.eq_ignore_ascii_case("list") {
            return self.show_practice_list();
        }
        if arg.eq_ignore_ascii_case("daily") {
            if self.practice_fetch.is_some() {
                self.float("already importing · one moment");
                return Ok(());
            }
            return self.spawn_practice_daily();
        }
        self.import_practice(arg)
    }

    /// 题目合集一览：每行一个合集，题数 / 已作答 / 已批改。
    fn show_practice_list(&mut self) -> Result<()> {
        let sets = self.store.practice_sets_with_stats()?;
        if sets.is_empty() {
            self.note(
                "no exercises yet · /practice <中文网页 url> 出题 · translate-practice list",
            );
            return Ok(());
        }
        let mut view = ToolView::new("practice")
            .accent(Accent::Note)
            .result(format!("{} collections", sets.len()));
        for stat in &sets {
            let mut line = format!("{} exercises", stat.exercises);
            if stat.submitted > 0 {
                line.push_str(&format!(" · {} submitted", stat.submitted));
            }
            if stat.graded > 0 {
                line.push_str(&format!(" · {} graded", stat.graded));
            }
            if let Some(url) = &stat.url {
                line.push_str(&format!(" · {url}"));
            }
            view = view.kv(&stat.name, &line);
        }
        self.push(Message::tool(
            view.muted("translate-practice from <url> 导新题 · history <id> 做题"),
        ));
        Ok(())
    }

    /// 启动时后台探一遍模型连通性：探不通的地址**本次运行内直接不再尝试**，
    /// 下次重启才重新启用。结果不上屏——真用到时会秒跳过并带上探测原因。
    ///
    /// 只在真实 TUI 里跑（`db_path` 由 main 设置），测试天然不触发；
    /// CLI 是一次性进程，用到时自己探，不额外起线程。
    pub fn probe_models(&self) {
        if self.db_path.is_none() {
            return;
        }
        let Ok(candidates) = crate::translator::ai_candidates() else {
            return; // 没配模型 key：没东西可探
        };
        std::thread::spawn(move || {
            let _ = crate::translator::ai_generate_quick(&candidates, "Reply with the single word: ok");
        });
    }

    /// `/model`：看模型候选顺序（谁排第一、谁被封）；`/model reset` 解封并重新探测。
    fn model_command(&mut self, arg: &str) -> Result<()> {
        match arg.trim() {
            "" | "list" => self.show_models(),
            "reset" | "retry" => {
                let cleared = crate::translator::clear_dead();
                // 只在真实 TUI 里真的重探（`db_path` 门控在 probe_models 里）。
                self.probe_models();
                self.float(format!(
                    "model bans cleared ({cleared}) · re-probing in the background"
                ));
                Ok(())
            }
            _ => {
                self.note("usage: /model [reset]");
                Ok(())
            }
        }
    }

    /// 模型状态一览：`models.json` 的顺序、当前第一、以及被封禁的地址。
    fn show_models(&mut self) -> Result<()> {
        let candidates = match crate::translator::ai_candidates() {
            Ok(candidates) => candidates,
            Err(err) => {
                self.note(format!("no model configured · {err:#}"));
                return Ok(());
            }
        };
        let mut view = ToolView::new("model")
            .accent(Accent::Note)
            .result(format!("{} candidates · models.json order", candidates.len()));
        // 「当前第一」标给**真正会被用到的**那家：全封禁的会被 ai_generate 秒跳过，
        // 文件顺序只是配置里的优先级，不等于当下可用。
        let mut marked_first = false;
        for (index, ai) in candidates.iter().enumerate() {
            let banned = ai
                .models
                .iter()
                .filter(|(url, _)| crate::translator::dead_reason(url).is_some())
                .count();
            let usable = banned < ai.models.len();
            let mut value = format!("{} · {} 型号", ai.id, ai.models.len());
            if banned > 0 {
                if usable {
                    value.push_str(&format!(" · {banned}/{} 封禁", ai.models.len()));
                } else {
                    value.push_str(" · 已封禁");
                }
            }
            if usable && !marked_first {
                value.push_str(" · 当前第一");
                marked_first = true;
            }
            view = view.kv(format!("{}. {}", index + 1, ai.name), &value);
        }
        if !marked_first {
            view = view
                .spacer()
                .muted("所有模型都封禁中 · /model reset 清除并重新探测");
        }
        let banned = crate::translator::dead_endpoints_snapshot();
        if !banned.is_empty() {
            view = view
                .spacer()
                .muted(format!("banned until restart ({})", banned.len()));
            for (url, reason) in banned.iter().take(5) {
                let host = url
                    .split("://")
                    .nth(1)
                    .and_then(|rest| rest.split('/').next())
                    .unwrap_or(url.as_str());
                view = view.kv(host, reason);
            }
        }
        self.push(Message::tool(
            view.muted("/model reset 清除封禁并重新探测"),
        ));
        Ok(())
    }

    /// 每天第一次启动时自动按来源发现新文章、导入练习（和新闻抓取同款）。
    pub fn enable_daily_practice(&mut self, db_path: std::path::PathBuf) -> Result<()> {
        self.db_path = Some(db_path);
        let today = qe_core::time::format_day(qe_core::time::today());
        if self
            .store
            .setting("practice_fetched_on")?
            .as_deref()
            == Some(today.as_str())
        {
            return Ok(());
        }
        self.spawn_practice_daily()
    }

    /// 后台跑一轮每日发现；结果并进 `/practice` 的结果通道。
    fn spawn_practice_daily(&mut self) -> Result<()> {
        let Some(db) = self.db_path.clone() else {
            self.note("daily practice is only wired up in the TUI binary");
            return Ok(());
        };
        if self.practice_fetch.is_some() {
            self.float("already importing · one moment");
            return Ok(());
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome =
                crate::practice::daily_import_at(&db).map_err(|err| format!("{err:#}"));
            if outcome.is_ok() {
                // 记住今天跑过了（同新闻的 news_fetched_on）；跑完再记，
                // 半途挂了明天还会自动重试。
                if let Ok(store) = qe_store::SqliteStore::open_at(&db) {
                    let today = qe_core::time::format_day(qe_core::time::today());
                    let _ = store.set_setting("practice_fetched_on", &today);
                }
            }
            let _ = sender.send(outcome);
        });
        self.practice_fetch = Some(receiver);
        self.float("checking practice sources in the background …");
        Ok(())
    }

    /// `/practice <url>`：后台抓中文网页、按段建题，界面不阻塞。
    fn import_practice(&mut self, url: &str) -> Result<()> {
        if self.practice_fetch.is_some() {
            self.float("already importing · one moment");
            return Ok(());
        }
        let url = url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.note("usage: /practice <中文网页 url>");
            return Ok(());
        }
        // 先验候选（key 有没有配）：没有它出不了参考译文，抓完网页也是白抓。
        if let Err(err) = crate::translator::ai_candidates() {
            self.note(format!("practice needs a model key · {err:#}"));
            return Ok(());
        }
        let Some(db) = self.db_path.clone() else {
            self.note("practice import is only wired up in the TUI binary");
            return Ok(());
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome =
                crate::practice::import_at(&db, &url).map_err(|err| format!("{err:#}"));
            let _ = sender.send(outcome);
        });
        self.practice_fetch = Some(receiver);
        self.float("importing exercises from the page in the background …");
        Ok(())
    }

    /// 导入线程有结果了就报到对话里；每帧调一次。
    fn poll_practice(&mut self) {
        let Some(receiver) = self.practice_fetch.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(report)) => {
                self.practice_fetch = None;
                if report.created > 0 {
                    let ids: Vec<String> = report.ids.iter().map(|id| format!("#{id}")).collect();
                    let mut view = ToolView::new("practice")
                        .accent(Accent::Note)
                        .result(report.summary())
                        .kv("new", ids.join(" "));
                    for note in &report.notes {
                        view = view.kv("·", note);
                    }
                    self.push(Message::tool(
                        view.muted("translate-practice history <id> 看题目 · submit 作答"),
                    ));
                } else if report.notes.is_empty() {
                    self.float(format!("practice: nothing new · {}", report.summary()));
                } else {
                    // 没有新题但有话说（来源没配、列表页抓不开……）。
                    let mut view = ToolView::new("practice")
                        .accent(Accent::Note)
                        .result(report.summary());
                    for note in &report.notes {
                        view = view.kv("·", note);
                    }
                    self.push(Message::tool(view));
                }
            }
            Ok(Err(error)) => {
                self.practice_fetch = None;
                self.float_warn(format!("practice import failed · {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.practice_fetch = None;
            }
        }
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
            // 学习模式的按键提示只在页脚出现一次；占位符留空，避免同一行提示
            // 在输入框和页脚各写一遍。
            let placeholder = if self.session.is_some() {
                ""
            } else {
                "type a word, or / for commands"
            };

            // 悬浮提示：剔除过期的，取活的给这一帧（右上角，不占对话历史）。
            let toasts = self.live_toasts();

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
                toasts: &toasts,
            };

            let mut info = DrawInfo::default();
            terminal.terminal.draw(|frame| {
                info = draw(frame, &view);
            })?;
            self.draw_info = info;

            // 全篇模式下整篇文章一起渲染，当前段可能落在视野外（长文尤甚）。
            // 翻页或跳段后请求一次「把当前段带回来」，只在这一帧真滚动——
            // 读者自己滚到别处时不会被拽走；选词、切译文请求的是「保持画面不动」。
            if std::mem::take(&mut self.reveal_anchor) {
                self.scroll_to_anchor();
                self.pin_view = false;
            } else if std::mem::take(&mut self.pin_view) {
                self.pin_view_on_screen();
            }
            self.prev_anchor = self.anchor_line();

            // Imports read a whole dictionary file; run them only once the frame is
            // on screen so the "importing" line is visible while it works.
            if let Some(path) = self.pending_import.take() {
                self.run_pending_import(&path)?;
                continue;
            }

            // 后台抓取跑在独立线程里，这里只收结果。
            self.poll_news_fetch();
            // 网页导入翻译练习也一样：建完题再报，界面不卡。
            self.poll_practice();
            // AI 总结建议也是后台线程，好了补一条。
            self.poll_summary_advice();
            // 机器翻译也一样：翻完再补进卡片，界面不卡。
            self.poll_translation();
            if let Some(started) = self.study_fade_started {
                if started.elapsed() >= Duration::from_millis(STUDY_STREAM_MS) {
                    self.study_fade_started = None;
                    let _ = self.render_card();
                } else {
                    let _ = self.render_card();
                }
            }
            // 阅读过渡同理：每帧重画推进「正文流式吐出」，吐完后收掉。
            if let Some(started) = self.reading_fade_started {
                if started.elapsed() >= Duration::from_millis(1200) {
                    self.reading_fade_started = None;
                }
                let _ = self.render_reading();
            }
            if self.reading.is_some()
                && self.reading_progress_tick.elapsed() >= Duration::from_secs(1)
            {
                self.reading_progress_tick = Instant::now();
                let _ = self.render_reading();
            }

            if self.toggle_mouse {
                self.toggle_mouse = false;
                let on = !terminal.mouse_enabled();
                terminal.set_mouse(on)?;
                if on {
                    self.float("mouse capture on · scroll and click work, text selection is off");
                } else {
                    self.float("mouse capture off · select and copy with the mouse · F2 to restore");
                }
            }

            // 流式过渡（阅读与学习卡）期间把轮询加密到 50ms：字往外蹦得才顺。
            let poll_ms = if self.reading_fade_started.is_some()
                || self.study_fade_started.is_some()
            {
                50
            } else {
                120
            };
            if event::poll(Duration::from_millis(poll_ms))? {
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

        // 读完一篇后的总结在等空格/回车：接下一篇。
        // 别的按键**不作废**这个状态——手滑按了方向键、退格，再按空格照样能过篇；
        // 作废只发生在执行命令（`execute`）或新开阅读时。输入行有实质内容时，
        // 空格照常打字，不抢；只有残留空白（先前误敲的空格）会被清掉再进去。
        if self.pending_next_text.is_some()
            && matches!(key.code, KeyCode::Char(' ') | KeyCode::Enter)
            && self.editor.text().trim().is_empty()
            && self.session.is_none()
        {
            let next = self.pending_next_text.take().expect("checked above");
            self.editor.clear();
            return self.start_reading(&next);
        }

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
                // 消息没了，卡片槽位也作废：下一张重新开槽，裁剪不删错。
                if let Some(session) = self.session.as_mut() {
                    session.card_slots.clear();
                    session.card_msg = None;
                }
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
                KeyCode::Char('m') => {
                    self.mark_word("")?;
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
                // 词序号只在段内有效：要记住点的是哪一段，否则会选中当前段里
                // 同序号的另一个词。阅读位置不动，读者不会因为点了个词就跳走。
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

    /// 当前阅读卡片里锚点行（`▸ 12/50`）的绝对行号。
    ///
    /// 对话里可能同时留着上一篇的卡片（也带锚点，`▸` 停在上一篇读到的位置），
    /// 翻页时必须只认**当前这篇**的锚点；取第一处会把视野拽到上一篇的结尾去。
    /// 还没在读、或当前卡片没有锚点（精读模式）时返回 `None`。
    fn anchor_line(&self) -> Option<usize> {
        // 没在读、或卡片还没挂上消息（`add word` 刚把 `message` 置空）时都不跟：
        // 此时对话里的锚点属于某篇旧卡片，跟它走只会把视野拽到那篇去。
        let message = self.reading.as_ref()?.message?;
        self.draw_info
            .anchors
            .iter()
            .find(|(owner, _)| *owner == message)
            .map(|(_, line)| *line)
    }

    /// 把当前段锚点滚到视野顶部附近；只在这一帧的几何信息里算，不需要知道宽度。
    ///
    /// 已经在视野内的锚点不动：读者自己滚到别处再触发重画（切译文、标记）时，
    /// 不该被强行拽回。
    fn scroll_to_anchor(&mut self) {
        let Some(line) = self.anchor_line() else {
            return;
        };
        let info = &self.draw_info;
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

    /// 内容在原位变长/变短时保持画面不动：锚点行下移了几行，就把滚动量补几行，
    /// 视野里的文字因而停在原地（选词弹出说明块不会让整篇往上跑）。
    fn pin_view_on_screen(&mut self) {
        let (Some(before), Some(after)) = (self.prev_anchor, self.anchor_line()) else {
            return;
        };
        if before == after {
            return;
        }
        let delta = after as i32 - before as i32;
        let next = self.scroll_back as i32 + delta;
        self.scroll_back = next.clamp(0, self.draw_info.max_back as i32) as u16;
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
                // 书排在前面：导入一本书会带来几十章，逐章列出来既刷屏又难选；
                // 读者真正想输入的是书名/slug，`/read <slug>` 自己会接着读下一篇。
                let mut out = self.reading_book_candidates();
                out.extend(loose_text_candidates(self));
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
            command::ArgKind::ApiKey => Self::apikey_candidates(partial),
            command::ArgKind::Number | command::ArgKind::Free => Vec::new(),
        }
    }

    /// `/apikey` 的候选。
    ///
    /// 打完子命令再 tab 才轮到 key 名，候选带上子命令前缀（`set baidu_app_id`），
    /// 这样命中、公共前缀和回填走的都是通用逻辑，不用为第二段参数另开一条路。
    fn apikey_candidates(partial: &str) -> Vec<Candidate> {
        if !partial.contains(' ') {
            // `set` 带尾随空格：tab 完直接停在等 key 名的位置。
            return ["set ", "list", "clear"]
                .iter()
                .map(|value| Candidate {
                    value: (*value).to_string(),
                    help: String::new(),
                })
                .collect();
        }
        let sub = partial.split_whitespace().next().unwrap_or("");
        if !matches!(sub, "set" | "clear") {
            // `/apikey list ` 之后没有候选，提示走 usage。
            return Vec::new();
        }
        qe_store::keys::KEYS
            .iter()
            .map(|key| Candidate {
                value: format!("{sub} {}", key.name),
                help: key.hint.to_string(),
            })
            .collect()
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

    /// `/read` 的候选：书（slug + 进度），读者输入 slug 就能接着读下一篇。
    fn reading_book_candidates(&self) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = self
            .store
            .text_books()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|book| {
                let progress = self.store.text_book_progress(&book.slug).ok()?;
                let state = if progress.is_done() {
                    format!("done · {}/{}", progress.finished, progress.total)
                } else {
                    format!(
                        "{}/{} · next {}",
                        progress.finished,
                        progress.total,
                        progress.next_title.unwrap_or_default()
                    )
                };
                Some(Candidate {
                    value: book.slug,
                    help: format!("{} · {state}", book.name),
                })
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
        // 任何命令都作废「总结等空格接下一篇」的状态。
        self.pending_next_text = None;
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
            Command::News => self.fetch_news_now(),
            Command::Practice(arg) => self.practice_command(&arg),
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
            Command::ApiKey(arg) => self.api_key(&arg),
            Command::Model(arg) => self.model_command(&arg),
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

    /// `/apikey`：翻译 API key 的增删查。
    ///
    /// 存 `keys.json`（数据目录），不进 `quietly.db`、不进仓库；
    /// 环境变量继续有效，文件里的值优先。
    fn api_key(&mut self, arg: &str) -> Result<()> {
        let mut parts = arg.splitn(3, char::is_whitespace);
        let sub = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        match sub.as_str() {
            "" | "list" | "show" => {
                let mut configured = 0;
                let mut view = ToolView::new("apikey").accent(Accent::Note);
                for status in qe_store::keys::status() {
                    let shown = match &status.value {
                        Some(value) => {
                            configured += 1;
                            // 邮箱是身份不是密钥，原样显示。
                            if status.name == "mymemory_email" {
                                value.clone()
                            } else {
                                qe_store::keys::mask(value)
                            }
                        }
                        None => "—".to_string(),
                    };
                    let origin = match status.origin {
                        "unset" => "not set".to_string(),
                        other => format!("{other} · {}", status.hint),
                    };
                    view = view.kv(status.name, format!("{shown}  ·  {origin}"));
                }
                let active = crate::translator::settings()
                    .map(|settings| settings.detail())
                    .unwrap_or_else(|| "off (QUIETLY_TRANSLATE=none)".to_string());
                view = view.result(format!("{configured}/{} set · {active}", qe_store::keys::KEYS.len()));
                view = view.muted(format!(
                    "/apikey set <name> <value> · /apikey clear [name] · {}",
                    qe_store::keys::path().display()
                ));
                self.push(Message::tool(view));
            }
            "set" => {
                let usage = "usage: /apikey set <name> <value> · names: /apikey list";
                let Some(name) = parts.next().map(str::trim).filter(|name| !name.is_empty()) else {
                    self.note(usage);
                    return Ok(());
                };
                let Some(value) = parts.next().map(str::trim).filter(|value| !value.is_empty()) else {
                    self.note(usage);
                    return Ok(());
                };
                match qe_store::keys::set(name, value) {
                    Ok(canonical) => {
                        let shown = if canonical == "mymemory_email" {
                            value.to_string()
                        } else {
                            qe_store::keys::mask(value)
                        };
                        let active = crate::translator::settings()
                            .map(|settings| settings.detail())
                            .unwrap_or_else(|| "off".to_string());
                        self.note(format!(
                            "saved {canonical} = {shown} · machine translation now: {active}"
                        ));
                    }
                    Err(err) => self.fail(format!("{err:#}")),
                }
            }
            "clear" => {
                let target = parts.next().map(str::trim).filter(|name| !name.is_empty());
                match qe_store::keys::clear(target) {
                    Ok(removed) => {
                        let detail = if removed.is_empty() {
                            match target {
                                Some(name) => format!("{name} was not in keys.json"),
                                None => "keys.json is already empty".to_string(),
                            }
                        } else {
                            format!("cleared {} from keys.json", removed.join(", "))
                        };
                        self.note(format!("{detail} · environment variables untouched"));
                    }
                    Err(err) => self.fail(format!("{err:#}")),
                }
            }
            _ => self.note("usage: /apikey list · /apikey set <name> <value> · /apikey clear [name]"),
        }
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

    fn save_study_snapshot(&self) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        let raw = serde_json::to_string(&session.snapshot())?;
        self.store.set_setting("study_snapshot", &raw)
    }

    fn clear_study_snapshot(&self) -> Result<()> {
        self.store.set_setting("study_snapshot", "")
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
            // truncate_after 砍掉了更新的卡片槽：同步丢弃，别留失效下标。
            session.card_slots.retain(|slot| *slot <= entry.message);
        }

        // Drop the trailing "rated" note the undo makes false.
        self.truncate_after(entry.message);
        self.render_card()?;
        self.save_study_snapshot()?;
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

        if self.reading.is_some() {
            self.finish_reading()?;
            self.note("reading closed before study");
        }

        if limit.is_none() {
            if let Some(raw) = self.store.setting("study_snapshot")? {
                match serde_json::from_str::<crate::session::StudySnapshot>(&raw)
                    .ok()
                    .and_then(|snapshot| StudySession::from_snapshot(book.clone(), snapshot))
                {
                    Some(session) if session.current.is_some() || !session.queue.is_empty() => {
                        let remaining = session.queue.len() + usize::from(session.current.is_some());
                        self.session = Some(session);
                        self.push(Message::tool(
                            ToolView::new("study")
                                .badge(book.name.clone())
                                .result(format!("resumed · {remaining} cards left"))
                                .muted("1-4 grade · esc end · session saved automatically"),
                        ));
                        self.render_card()?;
                        self.refresh_status();
                        return Ok(());
                    }
                    _ => {
                        self.store.set_setting("study_snapshot", "")?;
                    }
                }
            }
        }

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
        self.study_fade_started = Some(Instant::now());
        self.render_card()?;
        self.save_study_snapshot()?;
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
        // 流式：卡片内容按进度从头吐出，[`STUDY_STREAM_MS`] 出齐（和阅读同一手感）。
        let stream = self.study_fade_started.and_then(|started| {
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_millis(STUDY_STREAM_MS) {
                None
            } else {
                Some(elapsed.as_millis() as f32 / STUDY_STREAM_MS as f32)
            }
        });

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
        if let Some(progress) = stream {
            state = state.stream(progress);
        }
        if session.revealed {
            state = state.revealed();
        }
        if let Some(rating) = session.graded {
            state = state.graded(rating);
        }
        if let Some(after) = &session.last_after {
            let mut footer = format!(
                "next {duration} · interval {}d · ease {:.2}",
                after.interval_days, after.ease,
                duration = human_interval(after, Utc::now())
            );
            if session.hinted {
                footer.push_str(" · after hint");
            }
            state = state.footer(footer);
        }

        let message = Message::card(state);
        match session.card_msg {
            Some(index) if index < self.messages.len() => self.messages[index] = message,
            _ => {
                self.messages.push(message);
                let index = self.messages.len() - 1;
                if let Some(session) = self.session.as_mut() {
                    session.card_msg = Some(index);
                    session.card_slots.push(index);
                }
                // 裁到最近 KEEP_CARD_MESSAGES 条：留十张可回翻，
                // 对话不随背词时长无限膨胀。
                self.trim_old_cards();
            }
        }
        Ok(())
    }

    /// 会话只保留最近 [`KEEP_CARD_MESSAGES`] 条卡片消息：删掉最老的槽位，
    /// 并把它之后的所有下标前移（当前卡片槽、undo 记录）——下标不动就会指错消息。
    fn trim_old_cards(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if session.card_slots.len() <= KEEP_CARD_MESSAGES {
            return;
        }
        let removed = session.card_slots[0];
        // 下标已失效（对话被清空过之类）：丢掉坏槽自愈，绝不删错消息。
        if removed >= self.messages.len() {
            if let Some(session) = self.session.as_mut() {
                session.card_slots.remove(0);
            }
            return;
        }
        self.messages.remove(removed);
        if let Some(session) = self.session.as_mut() {
            session.card_slots.remove(0);
            for slot in &mut session.card_slots {
                *slot -= 1;
            }
            if let Some(current) = session.card_msg.as_mut() {
                if *current > removed {
                    *current -= 1;
                }
            }
        }
        // 指向被删槽的 undo 记录作废（那是十张以前的卡了）；靠后的整体前移。
        self.undo.retain(|entry| entry.message != removed);
        for entry in &mut self.undo {
            if entry.message > removed {
                entry.message -= 1;
            }
        }
    }

    fn reveal(&mut self) -> Result<()> {
        if self.session.is_none() {
            self.note("no active session · /study to start");
            return Ok(());
        }
        if let Some(session) = self.session.as_mut() {
            session.revealed = true;
        }
        // Revealing the answer is also a card transition: show the headword
        // first, then let the full definition settle in.
        self.study_fade_started = Some(Instant::now());
        self.render_card()?;
        self.save_study_snapshot()?;
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
            // 看过一次提示就算「用过」：评分时调度降一档（关掉提示也一样）。
            if session.hint {
                session.hinted = true;
            }
        }
        // Keep the feedback and the revealed card from appearing as one abrupt
        // replacement after a 1-4 rating.
        self.study_fade_started = Some(Instant::now());
        self.render_card()?;
        self.save_study_snapshot()?;
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
        // 空格已经揭晓过的卡：评级只是补状态，不该重放过渡。
        let was_revealed = session.revealed;
        // 这张卡是否用过提示（看过一次就算）。
        let hinted = session.hinted;
        let card_message = session.card_msg.unwrap_or(0);
        let was_new = card.is_new;

        let now = Utc::now();
        // 用过提示：调度降一档（Good→Hard、Easy→Good），不是凭空想起来的；
        // 统计里仍记用户实际按的键。
        let scheduled = if hinted {
            srs::hinted_rating(rating)
        } else {
            rating
        };
        let after = srs::review(card.card.as_ref(), scheduled, now);
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

        // 流式过渡只在「评级本身负责揭晓」时启动（没按空格直接给分）；
        // 空格已经揭晓过的，评级保持原状——过渡在跑就让它跑完，已出齐全量显示，
        // 绝不把整卡重流一遍。改分（非首次评级）同样直接给全量。
        if first_time {
            if !was_revealed {
                self.study_fade_started = Some(Instant::now());
            }
        } else {
            self.study_fade_started = None;
        }
        self.render_card()?;
        self.save_study_snapshot()?;
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
            self.clear_study_snapshot()?;
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
        self.clear_study_snapshot()?;
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
        // 统计之后补一条 AI 总结建议：后台生成，好了再上屏，不卡结束这一下。
        self.spawn_summary_advice(session.new_done, session.review_done);
        self.float("session ended · /study again, or type a word");
        self.refresh_status();
        Ok(())
    }

    /// 背完一轮的 AI 建议：后台生成，结果并进独立通道（和抓取同款轮询）。
    ///
    /// 只在真实 TUI 里跑——`db_path` 只有 main 会设，测试和 CLI 由此天然不联网；
    /// 没配模型 key 就安静跳过，统计照常显示。
    fn spawn_summary_advice(&mut self, session_new: u32, session_reviews: u32) {
        if self.advice_fetch.is_some() || self.db_path.is_none() {
            return;
        }
        let candidates = match crate::translator::ai_candidates() {
            Ok(candidates) => candidates,
            Err(_) => return,
        };
        let context = match self.advice_context(session_new, session_reviews) {
            Ok(context) => context,
            Err(_) => return,
        };
        let prompt = Self::advice_prompt(&context);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = crate::translator::ai_generate(&candidates, &prompt)
                .map_err(|err| format!("{err:#}"));
            let _ = sender.send(outcome);
        });
        self.advice_fetch = Some(receiver);
    }

    /// 给 AI 的数据切片：本轮 + 今日累计 + 反复忘的词 + **近 7 天逐日趋势**。
    fn advice_context(&self, session_new: u32, session_reviews: u32) -> Result<String> {
        let daily = self.store.daily(time::today())?;
        let streak = self.store.streak()?;
        let difficult = self
            .store
            .review_stats(time::today(), 8)?
            .difficult_words
            .iter()
            .map(|(lemma, again)| format!("{lemma}×{again}"))
            .collect::<Vec<_>>()
            .join("、");
        // 多天视角：只给今天，模型没法判断「今天算好还是坏」。
        let past = self
            .store
            .daily_history(time::today() - chrono::Duration::days(6), 7)?;
        let timeline = past
            .iter()
            .filter_map(|stats| {
                stats.day.map(|day| {
                    format!(
                        "{} 新{}/复习{}",
                        time::format_short(day),
                        stats.new_learned,
                        stats.reviews
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("，");
        let days = past.len().max(1) as u32;
        let avg_reviews = past.iter().map(|stats| stats.reviews).sum::<u32>() / days;
        let avg_new = past.iter().map(|stats| stats.new_learned).sum::<u32>() / days;
        Ok(format!(
            "本轮新学 {session_new} 词、复习 {session_reviews} 次；\
今日新学 {}、复习 {}，评分 again {} / hard {} / good {} / easy {}，\
忘记率 {}%，连续学习 {} 天，今日专注 {} 分钟。今天反复忘的词：{}。\
近7天（日期 新词/复习）：{timeline}；7天平均：复习 {avg_reviews}、新词 {avg_new}。",
            daily.new_learned,
            daily.reviews,
            daily.again,
            daily.hard,
            daily.good,
            daily.easy,
            daily.forget_rate(),
            streak,
            daily.minutes(),
            if difficult.is_empty() {
                "没有"
            } else {
                difficult.as_str()
            }
        ))
    }

    /// AI 建议线程有结果了就补进对话；每帧调一次。
    fn poll_summary_advice(&mut self) {
        let Some(receiver) = self.advice_fetch.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(generated)) => {
                self.advice_fetch = None;
                let view = ToolView::new("advice")
                    .accent(Accent::Note)
                    .badge(generated.id)
                    .result(generated.text);
                self.push(Message::tool(view));
            }
            Ok(Err(error)) => {
                self.advice_fetch = None;
                self.float_warn(format!("ai advice unavailable · {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.advice_fetch = None;
            }
        }
    }

    /// AI 总结建议的提示词：数据 → 2-4 句中文（评价 + 主要问题 + 明天怎么做）。
    fn advice_prompt(context: &str) -> String {
        format!(
            "你是一位英语学习教练，根据今天与近7天的学习数据做一次简短复盘\
（结合7天均值判断今天算好还是差）。\
只输出 2-4 句简体中文，不要列表、不要英文、不要客套话：\
第一句结合均值与趋势评价今天的表现；第二句点出最主要的问题\
（结合忘记率、反复忘的词和近几天的波动）；\
最后一句给出明天具体可执行的做法。\n\n学习数据：{context}"
        )
    }

    fn push_summary(&mut self) -> Result<()> {
        let daily = self.store.daily(time::today())?;
        let review_stats = self.store.review_stats(time::today(), 5)?;
        let streak = self.store.streak()?;
        // 近 7 天（含今天，缺的补零）：均值对比与逐日趋势都从这里来。
        let past = self
            .store
            .daily_history(time::today() - chrono::Duration::days(6), 7)?;
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
            &daily,
            streak,
            &past,
            &upcoming,
            &status,
            &review_stats.difficult_words,
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
            // 空闲时状态栏只说「ready」：输入行占位符已经写了同样的提示，
            // 两边都写会让同一句话在屏幕上出现两次。
            None => "ready".to_string(),
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

/// 不属于任何书的散篇。书里的章节不在这里重复列出——它们已经被书代表了。
fn loose_text_candidates(app: &App) -> Vec<Candidate> {
    app.store
        .texts()
        .unwrap_or_default()
        .into_iter()
        .filter(|text| text.book.is_none())
        .map(|text| Candidate {
            value: text.title,
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
    use qe_core::view::{Body, ToolLine};
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
    fn study_session_resume_restores_current_card_and_queue_order() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        let first = app
            .session
            .as_ref()
            .and_then(|session| session.current.as_ref())
            .map(|card| card.word.lemma.clone())
            .unwrap();
        let snapshot = app
            .store
            .setting("study_snapshot")
            .unwrap()
            .expect("study session is persisted");
        assert!(snapshot.contains(&first));

        app.session = None;
        app.execute(Command::Study(None)).unwrap();

        let resumed = app.session.as_ref().unwrap();
        assert_eq!(
            resumed.current.as_ref().unwrap().word.lemma,
            first,
            "resume keeps the current card"
        );
        assert_eq!(
            resumed.queue.iter().map(|card| card.word.lemma.clone()).collect::<Vec<_>>(),
            serde_json::from_str::<crate::session::StudySnapshot>(
                &app.store.setting("study_snapshot").unwrap().unwrap()
            )
            .unwrap()
            .queue
            .into_iter()
            .map(|card| card.word.lemma)
            .collect::<Vec<_>>(),
            "resume keeps the remaining queue order"
        );
    }

    #[test]
    fn a_study_card_streams_then_settles_to_full_content() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();

        let stream = app
            .messages
            .iter()
            .rev()
            .find_map(|message| message.card_state())
            .and_then(|state| state.stream);
        let progress = stream.expect("新卡走流式输出");
        assert!(progress < 1.0, "刚开始只吐一部分：{progress}");

        // 流式结束（状态被收掉）→ 重画，内容全量。
        app.study_fade_started = None;
        app.render_card().unwrap();
        let stream = app
            .messages
            .iter()
            .rev()
            .find_map(|message| message.card_state())
            .and_then(|state| state.stream);
        assert!(stream.is_none(), "出齐后全量显示");
    }

    #[test]
    fn grading_after_the_space_reveal_does_not_replay_the_stream() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();

        // 空格揭晓（过渡走起），流式跑完。
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            .unwrap();
        assert!(app.study_fade_started.is_some(), "空格揭晓走流式");
        app.study_fade_started = None;

        // 再按等级：只更新状态，不重放过渡——这就是「先空格再评分会刷新」的修复。
        app.grade(Rating::Good).unwrap();
        assert!(
            app.study_fade_started.is_none(),
            "空格揭晓过的卡，评级不重放流式"
        );

        // 没揭晓直接评级：评级本身负责揭晓，走流式。
        app.advance().unwrap();
        app.study_fade_started = None; // 先收掉新卡过渡，隔离断言
        app.grade(Rating::Easy).unwrap();
        assert!(
            app.study_fade_started.is_some(),
            "直接评级 = 揭晓，仍走流式"
        );
    }

    #[test]
    fn a_hinted_grade_is_demoted_and_marked_in_the_footer() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();

        // tab 开提示 → 这张卡记为「用过」。
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert!(app.session.as_ref().unwrap().hinted, "看过提示就算用过");

        // 关掉提示也算用过（看过一次就记住了）。
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert!(!app.session.as_ref().unwrap().hint, "提示关掉了");
        assert!(app.session.as_ref().unwrap().hinted, "但降档仍然生效");

        // 评分：调度降一档，角标注明 after hint。
        app.grade(Rating::Good).unwrap();
        let footer = app
            .messages
            .iter()
            .rev()
            .find_map(|message| message.card_state())
            .and_then(|state| state.footer.clone())
            .expect("评完有下一次间隔的角标");
        assert!(footer.contains("after hint"), "角标要说明降档：{footer}");

        // 下一张卡复位。
        app.advance().unwrap();
        assert!(!app.session.as_ref().unwrap().hinted, "换卡复位");

        // 没用过提示的直接评级：不降档、不标 after hint。
        app.grade(Rating::Good).unwrap();
        let footer = app
            .messages
            .iter()
            .rev()
            .find_map(|message| message.card_state())
            .and_then(|state| state.footer.clone())
            .expect("评完有角标");
        assert!(!footer.contains("after hint"), "直接想起不标：{footer}");
    }

    #[test]
    fn a_study_session_keeps_the_last_ten_cards() {
        // 保留最近十张卡可回翻，同时对话不随背词时长无限膨胀（越背越卡的根源）。
        // Again 会把卡重新排队，永远有下一张，可以连推验证裁剪。
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();
        for _ in 0..13 {
            app.grade(Rating::Again).unwrap();
            app.advance().unwrap();
        }

        let cards = app
            .messages
            .iter()
            .filter(|message| message.card_state().is_some())
            .count();
        assert_eq!(cards, KEEP_CARD_MESSAGES, "保留最近十张卡");

        let session = app.session.as_ref().unwrap();
        assert_eq!(
            session.card_slots.len(),
            KEEP_CARD_MESSAGES,
            "槽位表同步裁剪"
        );
        assert_eq!(
            session.card_msg,
            Some(*session.card_slots.last().unwrap()),
            "当前卡是最新槽"
        );
        assert!(
            session.card_slots.windows(2).all(|pair| pair[0] < pair[1]),
            "槽位保持递增：{:?}",
            session.card_slots
        );
        assert!(
            app.undo
                .iter()
                .all(|entry| entry.message < app.messages.len()),
            "undo 下标都指向仍存在的消息：{:?}",
            app.undo.iter().map(|entry| entry.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn regrading_an_expanded_card_does_not_replay_the_reveal() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Study(Some(3))).unwrap();

        app.grade(Rating::Good).unwrap();
        assert!(app.study_fade_started.is_some());

        app.grade(Rating::Easy).unwrap();
        assert!(
            app.study_fade_started.is_none(),
            "changing an existing rating must not replay the reveal"
        );
        assert_eq!(
            app.session.as_ref().and_then(|session| session.graded),
            Some(Rating::Easy)
        );
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
                matches!(
                    line,
                    qe_core::view::ToolLine::ReadingTranslation { text, .. }
                        if text.contains("从很小的时候")
                )
            })
        };

        assert!(!has_translation(&card(&app)), "hidden by default");
        app.execute(Command::Translate).unwrap();
        assert!(has_translation(&card(&app)), "/translate must show it");
        app.execute(Command::Translate).unwrap();
        assert!(!has_translation(&card(&app)), "/translate hides it again");
    }

    #[test]
    fn full_mode_tab_reveals_only_the_current_paragraph() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("full-bilingual.txt");
        std::fs::write(
            &path,
            "First paragraph.\n\nSecond paragraph.\n\n---\n\n第一段译文。\n\n第二段译文。\n",
        )
        .unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        app.execute(Command::Mode("full".into())).unwrap();

        // 当前卡片里显示着的译文行。
        let shown = |app: &App| -> String {
            app.messages
                .iter()
                .rev()
                .find_map(|message| match &message.body {
                    Body::Tool(view) if view.name == "read" => Some(view.clone()),
                    _ => None,
                })
                .expect("reading card")
                .lines
                .iter()
                .filter_map(|line| match line {
                    qe_core::view::ToolLine::ReadingTranslation { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        // 官方译文的双语全文：tab 只开当前段，整篇不会一次铺出来。
        app.execute(Command::Translate).unwrap();
        let visible = shown(&app);
        assert!(visible.contains("第一段译文。"), "{visible}");
        assert!(!visible.contains("第二段译文。"), "第二段不该跟着出现: {visible}");

        app.reading_next().unwrap();
        let visible = shown(&app);
        assert!(visible.contains("第一段译文。"), "上一段开着的还开着: {visible}");
        assert!(!visible.contains("第二段译文。"), "新段默认不显示: {visible}");

        app.execute(Command::Translate).unwrap();
        let visible = shown(&app);
        assert!(visible.contains("第二段译文。"), "tab 打开当前段: {visible}");
        assert!(visible.contains("第一段译文。"), "上一段不受影响: {visible}");

        // 再按一次只收起当前段。
        app.execute(Command::Translate).unwrap();
        let visible = shown(&app);
        assert!(!visible.contains("第二段译文。"), "再按收起当前段: {visible}");
        assert!(visible.contains("第一段译文。"), "别的段不动: {visible}");
    }

    #[test]
    fn a_cached_machine_translation_is_shown_without_touching_the_network() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("english-only.txt");
        std::fs::write(&path, "First paragraph only.\n\nSecond paragraph only.\n").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        app.execute(Command::Mode("intensive".into())).unwrap();

        // 之前翻过这一段：结果在库里，切译文就不该再发请求。
        let text = app.store.text_by_title("english-only").unwrap().unwrap();
        app.store
            .set_text_translation(text.id, 0, "只翻这一步。", "mymemory")
            .unwrap();

        app.execute(Command::Translate).unwrap();
        assert!(app.reading.as_ref().unwrap().current_translation_shown());
        assert!(
            app.translation_fetch.is_none(),
            "a cache hit must not spawn a request"
        );
        assert_eq!(
            app.reading.as_ref().unwrap().translation_at(0),
            Some("只翻这一步。")
        );
        assert!(
            app.translation_status.is_none(),
            "cached translations should be silent in the reading view"
        );

        // 关掉不需要通知任何人，也不该留下任务在跑。
        app.execute(Command::Translate).unwrap();
        assert!(!app.reading.as_ref().unwrap().current_translation_shown());
        assert!(app.translation_fetch.is_none());
    }

    #[test]
    fn a_new_paragraph_starts_hidden_and_tab_shows_only_it() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("two-paragraphs.txt");
        std::fs::write(&path, "First paragraph.\n\nSecond paragraph.").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        app.execute(Command::Mode("intensive".into())).unwrap();

        let text = app.store.text_by_title("two-paragraphs").unwrap().unwrap();
        app.store
            .set_text_translation(text.id, 1, "第二段。", "mymemory")
            .unwrap();
        // 第一段开着（直接标，避免为没有译文的段发请求）。
        app.reading
            .as_mut()
            .unwrap()
            .set_translation_shown(0, true);
        app.reading_next().unwrap();

        // 每段独立：翻到新段不会把上一段的状态带过来。
        assert!(
            !app.reading.as_ref().unwrap().translation_shown(1),
            "新段默认不显示译文"
        );
        assert!(
            app.reading.as_ref().unwrap().translation_shown(0),
            "上一段开着的还开着"
        );

        app.execute(Command::Translate).unwrap();
        assert!(
            app.reading.as_ref().unwrap().translation_shown(1),
            "tab 打开当前段的译文"
        );
        assert_eq!(
            app.reading.as_ref().unwrap().translation_at(1),
            Some("第二段。")
        );
        assert!(
            app.translation_fetch.is_none(),
            "缓存命中不该发请求"
        );
    }

    #[test]
    fn an_official_translation_is_preferred_over_machine_output() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("bilingual.txt");
        std::fs::write(&path, "English line.\n\n---\n\n官方译文。\n").unwrap();
        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        let text = app.store.text_by_title("bilingual").unwrap().unwrap();
        // 库里还留着一段旧的机翻，但官方译文必须优先。
        app.store
            .set_text_translation(text.id, 0, "机器译文。", "mymemory")
            .unwrap();

        app.execute(Command::Translate).unwrap();
        assert!(app.translation_fetch.is_none(), "no request when text has its own");
        assert_eq!(
            app.reading.as_ref().unwrap().translation_at(0),
            Some("官方译文。")
        );
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
    fn clicking_a_word_in_another_paragraph_advances_reading_position() {
        // 词序号只在段内有效：点第二段的第 3 个词，不能拿这个序号去第一段选词；
        // 同时把它设为当前段，让鼠标阅读自然推进。
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();
        assert_eq!(app.reading.as_ref().unwrap().paragraph, 0);

        // 第二段是 "Second paragraph here."：token 2 = here。
        let clicked = click_word(&mut app, |hit| hit.paragraph == Some(1) && hit.token == 2);
        assert_eq!(clicked, "here", "the clicked word, not the first paragraph's");
        let session = app.reading.as_ref().unwrap();
        assert_eq!(session.paragraph, 1, "clicking a later paragraph advances to it");
        assert_eq!(
            session.focus_paragraph(),
            Some(1),
            "the selection is remembered with its own paragraph"
        );
        assert_eq!(
            app.store
                .reading_progress(app.store.text_by_title("flow").unwrap().unwrap().id)
                .unwrap()
                .map(|(p, _)| p),
            Some(1),
            "clicking a paragraph persists the new position immediately"
        );
    }

    #[test]
    fn closing_reading_persists_a_paragraph_reached_by_click() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();
        click_word(&mut app, |hit| hit.paragraph == Some(1) && hit.token == 0);
        app.finish_reading().unwrap();

        let id = app.store.texts().unwrap()[0].id;
        assert_eq!(
            app.store.reading_progress(id).unwrap().map(|(p, _)| p),
            Some(1),
            "a click advances the paragraph that closing persists"
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
        let message = app.reading.as_ref().unwrap().message.unwrap();
        app.draw_info.transcript = ratatui::layout::Rect::new(0, 0, 80, 10);
        app.draw_info.max_back = 30;
        app.draw_info.anchors = vec![(message, 25)];
        app.scroll_back = 30; // offset = 0 → 锚点在第 25 行，视野外
        app.scroll_to_anchor();
        assert_eq!(app.scroll_back, 6, "anchor moves to the second visible line");

        // 已经在视野内就不动，免得读者刚滚到别处就被拽回来。
        app.draw_info.anchors = vec![(message, 30)]; // offset 24 → 屏幕第 6 行
        let before = app.scroll_back;
        app.scroll_to_anchor();
        assert_eq!(app.scroll_back, before, "a visible anchor is left alone");

        // 锚点跑到视口上方（往回翻）时同样要滚回去。
        app.draw_info.anchors = vec![(message, 10)];
        app.scroll_to_anchor();
        assert_eq!(app.scroll_back, 21, "an anchor above the viewport scrolls up");
    }

    #[test]
    fn a_previous_articles_anchor_is_not_followed() {
        // 上一篇的卡片还留在对话里，它的锚点（`▸ 50/50`）停在上一篇读到的位置。
        // 翻页必须只认当前这篇的锚点，否则视野会被拽到上一篇结尾和新文章
        // 开头的交界处——那正是「跳到奇怪位置」的来源。
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();
        let current = app.reading.as_ref().unwrap().message.unwrap();
        app.draw_info.transcript = ratatui::layout::Rect::new(0, 0, 80, 10);
        app.draw_info.max_back = 30;
        // 渲染按消息顺序报锚点：上一篇的（别的下标）排在前面，行号 25 在视野外；
        // 当前这篇的锚点在第 12 行。
        app.draw_info.anchors = vec![(current + 7, 25), (current, 12)];
        app.scroll_back = 30; // offset = 0
        app.scroll_to_anchor();
        assert_eq!(
            app.scroll_back, 19,
            "the current article's anchor (line 12) moves to the second visible row"
        );
    }

    #[test]
    fn a_panel_growing_above_the_anchor_keeps_the_picture_still() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.draw_info.transcript = ratatui::layout::Rect::new(0, 0, 80, 10);
        app.draw_info.max_back = 30;
        app.scroll_back = 10; // 内容 40 行时，视野顶停在第 20 行

        // 说明块插在锚点上方：整篇长了 5 行，锚点也跟着下移 5 行。
        let message = app.reading.as_ref().unwrap().message.unwrap();
        app.prev_anchor = Some(12);
        app.draw_info.max_back = 35;
        app.draw_info.anchors = vec![(message, 17)];
        app.pin_view_on_screen();

        assert_eq!(app.scroll_back, 15, "视野顶仍停在第 20 行，画面不动");
    }

    #[test]
    fn selecting_a_word_keeps_the_reader_in_place_in_full_mode() {
        let Harness { mut app, _dir } = harness();
        reading_fixture(&mut app, &_dir);
        app.execute(Command::Mode("full".into())).unwrap();
        app.reveal_anchor = false; // /mode 自己请求过一次
        // 读者已经滚到文章中间。
        app.scroll_back = 12;
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))
            .unwrap();
        assert!(
            !app.reveal_anchor,
            "selection must not drag the paragraph back into view"
        );
        assert!(
            app.pin_view,
            "the panel grows in place, so the frame keeps the picture still"
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

    /// 写一个最小的 EPUB：container → OPF（spine 2 章 + 1 个扉页）。
    fn write_epub(path: &std::path::Path) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        let mut put = |name: &str, body: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };

        put(
            "mimetype",
            "application/epub+zip",
        );
        put(
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container><rootfiles>
               <rootfile full-path="epub/content.opf" media-type="application/oebps-package+xml"/>
               </rootfiles></container>"#,
        );
        put(
            "epub/content.opf",
            r#"<?xml version="1.0"?><package><metadata>
               <dc:title>Farm</dc:title><dc:creator>Someone</dc:creator>
               </metadata><manifest>
               <item id="tp" href="text/titlepage.xhtml" media-type="application/xhtml+xml"/>
               <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
               <item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
               </manifest><spine>
               <itemref idref="tp"/><itemref idref="c1"/><itemref idref="c2"/>
               </spine></package>"#,
        );
        put(
            "epub/text/titlepage.xhtml",
            r#"<html><body epub:type="frontmatter"><p>Farm</p><p>by Someone</p></body></html>"#,
        );
        put(
            "epub/text/ch1.xhtml",
            r#"<html><body epub:type="bodymatter"><h2>I</h2>
               <p>All animals are equal, and that is a long enough sentence to keep.</p>
               <p>But some animals are more equal than others, which is worth remembering.</p>
               </body></html>"#,
        );
        put(
            "epub/text/ch2.xhtml",
            r#"<html><body epub:type="bodymatter"><h2>II</h2>
               <p>Four legs good, two legs bad, and the sheep repeated it for hours.</p>
               </body></html>"#,
        );
        zip.finish().unwrap();
    }

    #[test]
    fn an_epub_imports_one_text_per_chapter_into_one_book() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("farm.epub");
        write_epub(&path);

        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();

        let books = app.store.text_books().unwrap();
        assert_eq!(books.len(), 1, "one book from one epub: {books:?}");
        assert_eq!(books[0].name, "Farm");
        assert_eq!(books[0].slug, "farm");

        let chapters = app.store.texts_in_book("farm").unwrap();
        assert_eq!(chapters.len(), 2, "the frontmatter page is skipped");
        assert_eq!(chapters[0].source.as_deref(), Some("Someone"));
        assert!(chapters[0].body.contains("All animals are equal"));

        // 导入后立刻打开第一章，`/read farm` 也能接上。
        assert_eq!(
            app.reading.as_ref().map(|s| s.text.title.as_str()),
            Some("Farm — 1")
        );
        app.finish_reading().unwrap();
        app.execute(Command::Read("farm".into())).unwrap();
        assert_eq!(
            app.reading.as_ref().map(|s| s.text.title.as_str()),
            Some("Farm — 1"),
            "the book slug opens the first unfinished chapter"
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

        // 读完第一篇（两段）：出总结停一下，等空格再接下一篇。
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        assert!(
            app.reading.is_none() && app.pending_next_text.as_deref() == Some("Chapter Two"),
            "finishing a chapter parks on the summary for space"
        );
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Tool(view) if view.badge.as_deref() == Some("done")
            )),
            "阅读总结要显示出来"
        );
        assert!(app.status.contains("space"), "状态栏提示空格接下一篇");

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.reading.as_ref().map(|s| s.text.title.as_str()),
            Some("Chapter Two"),
            "finishing a chapter continues with the next one after space"
        );
        assert_eq!(
            app.reading.as_ref().unwrap().paragraph,
            0,
            "下一篇从第一段开始"
        );
    }

    #[test]
    fn next_chapter_hides_translation_by_default() {
        let Harness { mut app, _dir } = harness();
        import_chapter(&mut app, &_dir, "ch1.txt", "Chapter One", "First.\n\nSecond.");
        import_chapter(&mut app, &_dir, "ch2.txt", "Chapter Two", "Third.\n\nFourth.");

        app.execute(Command::Read("animal-farm".into())).unwrap();
        app.reading
            .as_mut()
            .unwrap()
            .set_translation_shown(0, true);
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        // 总结等空格，按一下才进下一篇。
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(
            app.reading.as_ref().map(|session| session.text.title.as_str()),
            Some("Chapter Two")
        );
        assert!(
            !app.reading.as_ref().unwrap().current_translation_shown(),
            "换篇后译文显示状态从头开始"
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
        assert!(
            app.pending_next_text.is_none(),
            "没有下一篇可接，不该等空格"
        );
    }

    #[test]
    fn a_command_cancels_waiting_for_the_next_article() {
        let Harness { mut app, _dir } = harness();
        import_chapter(&mut app, &_dir, "ch1.txt", "Chapter One", "First.\n\nSecond.");
        import_chapter(&mut app, &_dir, "ch2.txt", "Chapter Two", "Third.\n\nFourth.");

        app.execute(Command::Read("animal-farm".into())).unwrap();
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        assert!(app.pending_next_text.is_some(), "总结在等空格");

        // 执行了别的命令就作废：之后的空格不该把人拽进下一篇。
        app.execute(Command::Books).unwrap();
        assert!(app.pending_next_text.is_none());
        assert!(app.reading.is_none(), "总结阶段没有正在读的文章");
    }

    #[test]
    fn stray_keys_do_not_cancel_and_enter_or_a_stray_space_still_advance() {
        let Harness { mut app, _dir } = harness();
        import_chapter(&mut app, &_dir, "ch1.txt", "Chapter One", "First.\n\nSecond.");
        import_chapter(&mut app, &_dir, "ch2.txt", "Chapter Two", "Third.\n\nFourth.");
        import_chapter(&mut app, &_dir, "ch3.txt", "Chapter Three", "Fifth.\n\nSixth.");

        app.execute(Command::Read("animal-farm".into())).unwrap();
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        assert!(app.pending_next_text.is_some(), "总结在等空格");

        // 手滑按了杂键：状态不作废，之后的空格仍然有效。
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert!(
            app.pending_next_text.is_some(),
            "杂键不作废「等空格接下一篇」"
        );

        // 输入行里残留一个空格（先前误敲的）也能过：识别为空白、清掉再进。
        app.editor.insert_char(' ');
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.reading.as_ref().map(|s| s.text.title.as_str()),
            Some("Chapter Two"),
            "残留空白不挡住过篇"
        );
        assert!(app.editor.is_empty(), "进去前把残留空白清掉");

        // 回车同样能接下一篇。
        app.reading_next().unwrap();
        app.reading_next().unwrap();
        assert!(app.pending_next_text.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.reading.as_ref().map(|s| s.text.title.as_str()),
            Some("Chapter Three"),
            "enter 和空格一样接下一篇"
        );
    }

    #[test]
    fn a_fresh_article_streams_its_text_character_by_character() {
        let Harness { mut app, _dir } = harness();
        let path = _dir.path().join("long.txt");
        // 超过 400 字符才值得流式；6 段 × ~90 字符。
        let body: String = (1..=6)
            .map(|i| format!("Paragraph number {i} holds enough words to read this slowly over time."))
            .collect::<Vec<_>>()
            .join("\n\n");
        std::fs::write(&path, &body).unwrap();
        let total_chars = body.chars().count();
        assert!(total_chars > 400, "fixture must be long enough to stream");

        app.execute(Command::Read(path.to_string_lossy().to_string()))
            .unwrap();
        assert!(
            app.reading_fade_started.is_some(),
            "从头开的新文章走流式过渡"
        );
        let shown = app
            .reading
            .as_ref()
            .unwrap()
            .stream_chars
            .expect("first frame already has a character budget");
        assert!(
            shown >= 40 && shown < total_chars,
            "起手先给一截但没到全文：{shown}/{total_chars}"
        );
        assert_eq!(app.reading.as_ref().unwrap().paragraph, 0);
        assert!(app.reveal_anchor, "视野请求对准第一段的锚点");

        // 短文不流式：直接全量。
        let short = _dir.path().join("short.txt");
        std::fs::write(&short, "First.\n\nSecond.").unwrap();
        app.execute(Command::Read(short.to_string_lossy().to_string()))
            .unwrap();
        assert!(app.reading_fade_started.is_none());
        assert!(app.reading.as_ref().unwrap().stream_chars.is_none());
    }

    #[test]
    fn read_completion_offers_the_book_not_its_chapters() {
        // 一本书几十章：补全要给出可输入的书名，而不是把每一章列一遍。
        let Harness { mut app, _dir } = harness();
        import_chapter(&mut app, &_dir, "ch1.txt", "Chapter One", "First.\n\nSecond.");
        import_chapter(&mut app, &_dir, "ch2.txt", "Chapter Two", "Third.\n\nFourth.");

        let candidates = app.arg_candidates("/read", "ani");
        let values: Vec<String> = candidates.iter().map(|c| c.value.clone()).collect();
        // 目录条目也会进来（`/read` 接受路径），所以看的是按前缀过滤后的命中。
        let hits = command::arg_hits(&values, "ani");
        assert_eq!(hits, vec!["animal-farm"], "the book slug, once");
        assert!(
            candidates[0].help.contains("Animal Farm"),
            "the panel names the book and its progress: {}",
            candidates[0].help
        );
        assert!(
            !values.iter().any(|value| value.starts_with("Chapter")),
            "chapters are reached through the book"
        );
    }

    #[test]
    fn the_daily_fetch_runs_at_most_once_a_day() {
        let Harness { mut app, _dir } = harness();
        // 今天已经抓过：不应该再起线程。
        app.store
            .set_setting(
                "news_fetched_on",
                &qe_core::time::format_day(qe_core::time::today()),
            )
            .unwrap();
        app.enable_daily_news(_dir.path().join("quietly.db")).unwrap();
        assert!(
            app.news_fetch.is_none(),
            "a second launch on the same day must not fetch again"
        );
    }

    #[test]
    fn a_finished_fetch_reports_what_it_saved() {
        let Harness { mut app, _dir } = harness();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.news_fetch = Some(receiver);
        sender
            .send(Ok(crate::news::FetchReport {
                sources: vec![
                    crate::news::SourceReport {
                        name: "TED Talks".to_string(),
                        book: "TED Talks".to_string(),
                        translated: true,
                        saved: 3,
                        skipped: 1,
                        failed: 0,
                        error: None,
                    },
                    crate::news::SourceReport {
                        name: "Xinhua English".to_string(),
                        book: "Xinhua English".to_string(),
                        translated: false,
                        saved: 1,
                        skipped: 0,
                        failed: 0,
                        error: None,
                    },
                ],
            }))
            .unwrap();

        app.poll_news_fetch();
        assert!(app.news_fetch.is_none(), "the channel is cleared");
        let block = app
            .messages
            .iter()
            .find_map(|message| match &message.body {
                Body::Tool(view) if view.name == "news" => Some(view),
                _ => None,
            })
            .expect("a news block reports the result");
        assert_eq!(block.result.as_deref(), Some("4 new · 1 skipped"));
        assert!(
            block.lines.iter().any(|line| matches!(
                line,
                ToolLine::Kv { key, .. } if key == "Xinhua English"
            )),
            "every source that saved something gets a line"
        );
        assert_eq!(
            app.store.setting("news_fetched_on").unwrap().as_deref(),
            Some(qe_core::time::format_day(qe_core::time::today()).as_str()),
            "the day is recorded only after a successful fetch"
        );
    }

    #[test]
    fn a_failed_fetch_floats_a_warning_not_an_error_block() {
        let Harness { mut app, _dir } = harness();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.news_fetch = Some(receiver);
        sender.send(Err("offline".to_string())).unwrap();

        app.poll_news_fetch();
        assert!(app.news_fetch.is_none());
        // 失败走右上角红色悬浮：仍然可见，但不占对话历史、更不是错误块。
        assert!(
            app.floating.iter().any(|note| note.warn
                && note.text.contains("news fetch failed")
                && note.text.contains("offline")),
            "the failure is visible: {:?}",
            app.floating
        );
        assert!(
            !app
                .messages
                .iter()
                .any(|message| matches!(&message.body, Body::Text(text) if text.contains("offline"))),
            "失败不写进对话"
        );
        assert!(
            app.store.setting("news_fetched_on").unwrap().is_none(),
            "a failure does not mark the day as done"
        );
    }

    #[test]
    fn practice_without_args_says_there_are_none_or_lists_collections() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::Practice(String::new())).unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Text(text) if text.contains("no exercises yet")
            )),
            "空库先说没有题，并给出出题入口"
        );

        // 手动建一组再列：走 practice 面板，名字和题数都在。
        let set = app
            .store
            .ensure_practice_set("测试文章", "web", Some("https://example.cn/a"))
            .unwrap();
        let id = app
            .store
            .create_translation_exercise(
                "测试的一道题，长度足够长可以出。",
                "EN test",
                "zh-en",
                "web:https://example.cn/a",
                None,
            )
            .unwrap();
        app.store.assign_exercise_set(id, set).unwrap();

        app.execute(Command::Practice("list".into())).unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Tool(view) if view.name == "practice"
            )),
            "/practice list 渲染合集面板"
        );
    }

    #[test]
    fn practice_without_a_url_or_subcommand_spells_out_usage() {
        let Harness { mut app, _dir } = harness();
        // 不是 URL 也不是子命令的入参按 URL 处理，最终给出用法提示。
        app.execute(Command::Practice("not-a-url".into())).unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Text(text) if text.contains("usage: /practice")
            )),
            "坏入参给用法"
        );
    }

    #[test]
    fn the_advice_prompt_carries_stats_and_asks_for_two_to_four_sentences() {
        let context = "本轮新学 5 词；忘记率 39%。今天反复忘的词：inspect×3。";
        let prompt = App::advice_prompt(context);
        assert!(prompt.contains(context), "数据原样进提示词");
        assert!(prompt.contains("2-4 句简体中文"), "句数约束写死");
        assert!(prompt.contains("不要列表"), "输出形状约束");
    }

    #[test]
    fn the_ai_advice_arrives_as_its_own_block_and_failures_float() {
        let Harness { mut app, _dir } = harness();
        // 成功：补一条 advice 面板，badge 是实际出活的模型。
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(Ok(crate::translator::Generated {
                text: "明天先把 inspect 过两遍。".into(),
                id: "deepseek".into(),
            }))
            .unwrap();
        app.advice_fetch = Some(receiver);
        app.poll_summary_advice();
        assert!(app.advice_fetch.is_none());
        assert!(app.messages.iter().any(|message| matches!(
            &message.body,
            Body::Tool(view) if view.name == "advice" && view.badge.as_deref() == Some("deepseek")
        )));

        // 失败：右上角红色悬浮提示，不打断统计、不占历史。
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(Err("所有模型都试过了 · Gemini 连不上".into()))
            .unwrap();
        app.advice_fetch = Some(receiver);
        app.poll_summary_advice();
        assert!(
            app.floating
                .iter()
                .any(|note| note.warn && note.text.contains("ai advice unavailable")),
            "失败走红色悬浮"
        );
        assert!(!app.messages.iter().any(|message| matches!(
            &message.body,
            Body::Text(text) if text.contains("ai advice unavailable")
        )));
    }

    #[test]
    fn model_status_lists_order_and_the_first_candidate() {
        let _guard_dead = crate::translator::tests::DEAD_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _guard = crate::translator::tests::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let previous_home = std::env::var_os("QUIETLY_HOME");
        std::env::set_var("QUIETLY_HOME", dir.path());
        std::fs::write(
            dir.path().join("models.json"),
            r#"[
                {"name":"Deepseek","key":"deepseek_api_key","apiType":"chat-completions",
                 "models":[{"id":"deepseek-flash","url":"https://api.deepseek.com/chat/completions"}]},
                {"name":"Gemini","key":"gemini_api_key","apiType":"chat-completions",
                 "models":[{"id":"gemini-flash-latest","url":"https://example.invalid/chat"}]}
            ]"#,
        )
        .unwrap();
        qe_store::keys::set("deepseek_api_key", "ds").unwrap();
        qe_store::keys::set("gemini_api_key", "g").unwrap();

        let Harness { mut app, _dir } = harness();
        app.execute(Command::Model(String::new())).unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Tool(view) if view.name == "model"
                    && view.lines.iter().any(|line| matches!(
                        line,
                        qe_core::view::ToolLine::Kv { key, value }
                            if key.starts_with("1. Deepseek") && value.contains("当前第一")
                    ))
                    && view.lines.iter().any(|line| matches!(
                        line,
                        qe_core::view::ToolLine::Kv { key, .. } if key.starts_with("2. Gemini")
                    ))
            )),
            "顺序、第一名家、候补都列出来"
        );

        // 排第一的被封禁：标 已封禁、不再称当前第一，第一个未封的才是当前第一。
        crate::translator::mark_dead("https://api.deepseek.com/chat/completions", "测试封禁");
        let before = app.messages.len();
        app.execute(Command::Model(String::new())).unwrap();
        assert!(
            app.messages[before..].iter().any(|message| matches!(
                &message.body,
                Body::Tool(view) if view.name == "model"
                    && view.lines.iter().any(|line| matches!(
                        line,
                        qe_core::view::ToolLine::Kv { key, value }
                            if key.starts_with("1. Deepseek")
                                && value.contains("已封禁")
                                && !value.contains("当前第一")
                    ))
                    && view.lines.iter().any(|line| matches!(
                        line,
                        qe_core::view::ToolLine::Kv { key, value }
                            if key.starts_with("2. Gemini") && value.contains("当前第一")
                    ))
            )),
            "被封的排头不算当前第一，落到第一个未封的"
        );
        crate::translator::clear_dead();

        match previous_home {
            Some(value) => std::env::set_var("QUIETLY_HOME", value),
            None => std::env::remove_var("QUIETLY_HOME"),
        }
    }

    #[test]
    fn model_reset_lifts_bans_and_reprobes() {
        let _guard = crate::translator::tests::DEAD_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let Harness { mut app, _dir } = harness();
        crate::translator::mark_dead("https://banned-test.invalid/chat", "启动探测：连不上");
        assert!(crate::translator::dead_reason("https://banned-test.invalid/chat").is_some());

        app.execute(Command::Model("reset".into())).unwrap();
        assert!(
            crate::translator::dead_reason("https://banned-test.invalid/chat").is_none(),
            "reset 解除封禁"
        );
        // 临时回执走右上角悬浮，不占对话历史。
        assert!(
            app.floating
                .iter()
                .any(|note| note.text.contains("model bans cleared")),
            "回执进了悬浮区：{:?}",
            app.floating
        );
        assert!(
            !app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Text(text) if text.contains("model bans cleared")
            )),
            "悬浮消息不写进对话"
        );
        // 到点自动清掉。
        app.floating[0].until = Instant::now() - Duration::from_secs(1);
        let live = app.live_toasts();
        assert!(live.is_empty(), "过期的悬浮提示被剔除");
        assert!(app.floating.is_empty());
    }

    #[test]
    fn apikey_shows_usage_instead_of_guessing() {
        let Harness { mut app, _dir } = harness();
        app.execute(Command::ApiKey("wibble".into())).unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Text(text) if text.contains("/apikey set <name> <value>")
            )),
            "unknown subcommand spells out the usage"
        );
        app.execute(Command::ApiKey("set".into())).unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Text(text) if text.contains("usage: /apikey set")
            )),
            "set without a name and value does not write anything"
        );
    }

    #[test]
    fn apikey_set_confirms_what_was_stored() {
        let _guard = crate::translator::tests::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("QUIETLY_HOME");
        std::env::set_var("QUIETLY_HOME", dir.path());

        let Harness { mut app, _dir } = harness();
        app.execute(Command::ApiKey("set gemini AIzaTestKey123".into()))
            .unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Text(text) if text.contains("saved gemini_api_key")
            )),
            "set confirms the stored name: {:?}",
            app.messages
                .iter()
                .filter_map(|message| match &message.body {
                    Body::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );

        app.execute(Command::ApiKey("list".into())).unwrap();
        assert!(
            app.messages.iter().any(|message| matches!(
                &message.body,
                Body::Tool(view) if view.name == "apikey"
            )),
            "list renders a tool block"
        );

        match previous {
            Some(value) => std::env::set_var("QUIETLY_HOME", value),
            None => std::env::remove_var("QUIETLY_HOME"),
        }
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
        // 每张卡开一个新槽（保留十张历史）；下一张卡出现在新槽里。
        assert_eq!(card_count(&app), 2, "next card appears in its own slot");
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
    fn apikey_tab_completes_the_subcommand_then_the_key_name() {
        let Harness { mut app, _dir } = harness();

        // 第一段：子命令；`set` 补完自带空格，停在等 key 名的位置。
        let (rows, _) = app.suggestions_for("/apikey s");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["set "]);
        app.editor.set_text("/apikey s");
        app.tab_complete();
        assert_eq!(app.editor.text(), "/apikey set ");

        // 第二段：key 名。候选带子命令前缀，命中、公共前缀、回填走通用逻辑。
        let (rows, _) = app.suggestions_for("/apikey set ba");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["set baidu_app_id", "set baidu_secret_key"]);
        assert_eq!(rows[0].help, "百度翻译 APP ID", "面板要说明这个 key 是干嘛的");

        app.editor.set_text("/apikey set ba");
        app.suggestion_index = 0;
        app.tab_complete();
        assert_eq!(app.editor.text(), "/apikey set baidu_", "公共前缀直接补上");

        // 灰字提示同样覆盖第二段参数（只剩一个命中时才显示）。
        assert_eq!(
            app.arg_ghost("/apikey set baidu_a").as_deref(),
            Some("pp_id")
        );
        assert_eq!(app.arg_ghost("/apikey clear gemini").as_deref(), Some("_api_key"));

        // 别名 `/key` 走同一条补全路径（arg_completion_target 回落到规范名）。
        let (rows, _) = app.suggestions_for("/key set ba");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["set baidu_app_id", "set baidu_secret_key"]);
        assert_eq!(app.arg_ghost("/key set baidu_a").as_deref(), Some("pp_id"));
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
