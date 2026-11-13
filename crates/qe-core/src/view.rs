//! UI-agnostic view models.
//!
//! The app layer only produces messages; the TUI layer only paints them.
//! The look is deliberately modelled on a command-line agent: `⏺ name` followed by
//! `⎿ result`, with indented detail lines and no big borders.

use crate::model::{DailyStats, Example, Quiz, Rating, ReadingStats, Word};

/// 选词说明块右栏最多列几条词组：再多会把卡片撑长，剩下的留给完整卡片。
const MAX_PANEL_PHRASES: usize = 6;

/// 一个可以按序号打开的候选词（精读模式下段落里的词）。
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub lemma: String,
    /// 词性与中文释义；查不到词条时为空。
    pub gloss: String,
    /// 是否已经被读者标记。
    pub marked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    Neutral,
    /// Review cards and other informational blocks.
    Info,
    /// New cards and successful actions.
    Success,
    Warn,
    Danger,
    /// Daily summaries and reports.
    Note,
}

/// One conversation message.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub body: Body,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            body: Body::Text(text.into()),
        }
    }

    pub fn agent(text: impl Into<String>) -> Self {
        Self {
            role: Role::Agent,
            body: Body::Text(text.into()),
        }
    }

    pub fn hint(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            body: Body::Text(text.into()),
        }
    }

    pub fn tool(view: ToolView) -> Self {
        Self {
            role: Role::Agent,
            body: Body::Tool(Box::new(view)),
        }
    }

    /// A word card. Stored as state so the whole transcript can be re-rendered
    /// when the gloss fold setting changes.
    pub fn card(state: CardState) -> Self {
        Self {
            role: Role::Agent,
            body: Body::Card(Box::new(state)),
        }
    }

    /// The card state behind this message, when it is one.
    pub fn card_state(&self) -> Option<&CardState> {
        match &self.body {
            Body::Card(state) => Some(state),
            _ => None,
        }
    }

    pub fn card_state_mut(&mut self) -> Option<&mut CardState> {
        match &mut self.body {
            Body::Card(state) => Some(state),
            _ => None,
        }
    }

    pub fn table(view: TableView) -> Self {
        Self {
            role: Role::Agent,
            body: Body::Table(Box::new(view)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Body {
    Text(String),
    Tool(Box<ToolView>),
    /// A word card kept as state, not pixels, so `m` can fold or unfold every
    /// card already on screen without the app remembering what it drew.
    Card(Box<CardState>),
    Table(Box<TableView>),
}

/// A "tool call" style output block.
#[derive(Debug, Clone)]
pub struct ToolView {
    /// Block name, e.g. `lookup`, `word`, `summary`.
    pub name: String,
    /// Dimmed summary shown after the name.
    pub badge: Option<String>,
    /// First line after `⎿`.
    pub result: Option<String>,
    /// Secondary text on the result line, drawn grey: frequency stars, a word
    /// count, anything that qualifies the result without being part of it.
    pub result_meta: Option<String>,
    /// Indented detail lines.
    pub lines: Vec<ToolLine>,
    /// Optional right-hand column, rendered beside `lines` when the terminal is
    /// wide enough and stacked underneath it otherwise.
    pub aside: Vec<ToolLine>,
    pub accent: Accent,
}

/// 段落里选中的词：紧跟在段落（与译文）后面的一小块说明，不用另开一张卡片。
///
/// 只放「读这段时需要」的信息：音标、前两三条义项、标记状态。词根和例句仍在
/// `enter` 打开的完整卡片里，免得一块说明把正文顶出屏幕。
fn word_panel(
    mut view: ToolView,
    session: &crate::reading::ReadingSession,
    lemma: &str,
    word: Option<&Word>,
) -> ToolView {
    let marked = session.is_marked(lemma);
    view = view.spacer().muted(if marked {
        "word · marked".to_string()
    } else {
        "word".to_string()
    });

    let Some(word) = word else {
        // 词典里没有这个词条时也要给出反馈，否则读者会以为点击没生效。
        view = view.line(ToolLine::Accent(lemma.to_string()));
        return view.muted("(no definition in the dictionary) · /add takes it from here");
    };

    let mut head = word.lemma.clone();
    if let Some(phonetic) = &word.phonetic {
        if !phonetic.trim().is_empty() {
            head.push_str("  ");
            head.push_str(phonetic.trim());
        }
    }

    let mut left = vec![ToolLine::Accent(head)];
    let parsed = word.senses();
    let mut shown = 0;
    for sense in parsed.senses.iter().filter(|sense| !sense.is_secondary()) {
        if shown == 3 {
            break;
        }
        left.push(ToolLine::Sense {
            label: sense.pos.clone(),
            text: sense.text.clone(),
        });
        shown += 1;
    }
    if shown == 0 {
        // 词典里没有释义时也别空着：至少说明为什么只有词组。
        left.push(ToolLine::Muted("(no definition in the dictionary)".to_string()));
    }

    // 只提示**本段里真的写着**的词组：词条里登记但这段没用到的搭配不列出来，
    // 否则每个多义词都会拖出一长串与当前阅读无关的短语。
    let matched = match (session.focus_text(), session.focused_span()) {
        (Some(paragraph), Some(span)) => {
            crate::reading::phrases_covering(paragraph, span, &word.collocations)
        }
        _ => Vec::new(),
    };
    if matched.is_empty() {
        return view.lines(left);
    }

    let mut right: Vec<ToolLine> = matched
        .iter()
        .take(MAX_PANEL_PHRASES)
        .map(|phrase| ToolLine::PhraseInline {
            phrase: phrase.phrase.clone(),
            gloss: phrase.gloss.clone(),
        })
        .collect();
    let hidden = matched.len().saturating_sub(MAX_PANEL_PHRASES);
    if hidden > 0 {
        right.push(ToolLine::Muted(format!(
            "+{hidden} more · enter the full card"
        )));
    }

    // 右栏从第 0 行就开始写词组，不额外加标题：标题会把词组整体压下一行，
    // 第一行留空读起来像「词组没对上单词」。
    let view = view.split(left, right);
    view.muted("enter full card · /mark marks it · esc clears the selection")
}

impl ToolView {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            badge: None,
            result: None,
            result_meta: None,
            lines: Vec::new(),
            aside: Vec::new(),
            accent: Accent::Neutral,
        }
    }

    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    pub fn result(mut self, result: impl Into<String>) -> Self {
        self.result = Some(result.into());
        self
    }

    pub fn accent(mut self, accent: Accent) -> Self {
        self.accent = accent;
        self
    }

    pub fn line(mut self, line: ToolLine) -> Self {
        self.lines.push(line);
        self
    }

    /// Add a line to the right-hand column.
    pub fn aside(mut self, line: ToolLine) -> Self {
        self.aside.push(line);
        self
    }

    pub fn lines(mut self, lines: Vec<ToolLine>) -> Self {
        self.lines.extend(lines);
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.lines.push(ToolLine::Text(text.into()));
        self
    }

    pub fn muted(mut self, text: impl Into<String>) -> Self {
        self.lines.push(ToolLine::Muted(text.into()));
        self
    }

    pub fn accent_line(mut self, text: impl Into<String>) -> Self {
        self.lines.push(ToolLine::Accent(text.into()));
        self
    }

    pub fn kv(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.lines.push(ToolLine::Kv {
            key: key.into(),
            value: value.into(),
        });
        self
    }

    pub fn spacer(mut self) -> Self {
        self.lines.push(ToolLine::Spacer);
        self
    }

    /// 两栏内容：宽终端并排，窄终端把右栏折到下方。
    pub fn split(mut self, left: Vec<ToolLine>, right: Vec<ToolLine>) -> Self {
        self.lines.push(ToolLine::Split { left, right });
        self
    }

    /// 阅读卡片：原文（标记词高亮、选中词反显）+ 可选译文 + 选中词的说明块。
    ///
    /// 全篇模式一次给出全部段落，译文跟随各自段落；精读模式只给当前段。
    /// `show_translation` 由 `/translate` 与 `tab` 控制，两种模式都认。
    /// `selection` 是选中的词：`(lemma, 词条)`；词条为 `None` 表示词典里没有，
    /// 卡片仍会显示选中的是哪个词。
    pub fn reading_card(
        session: &crate::reading::ReadingSession,
        candidates: &[Candidate],
        show_translation: bool,
        selection: Option<(&str, Option<&Word>)>,
    ) -> Self {
        let text = &session.text;
        let mut badge = format!("{} · {}", session.mode.label(), session.progress());
        if session.total() > 1 {
            badge = format!(
                "{} · {}/{}",
                session.mode.label(),
                session.paragraph + 1,
                session.total()
            );
        }
        let mut view = Self::new("read")
            .badge(badge)
            .accent(Accent::Note)
            .result(text.title.clone());

        let marked: Vec<String> = session.marked.iter().map(|m| m.lemma.clone()).collect();
        let is_marked = |word: &str| marked.iter().any(|lemma| lemma == word);
        let focus = session.focus;
        // 选中词可以在别的段里（点击不移动阅读位置），高亮与说明块都跟着它走。
        let focused_in = session.focus_paragraph();

        match session.mode {
            crate::reading::ReadMode::Full => {
                // 全篇：连续输出，靠终端滚动阅读。段落之间空一行。
                for (index, paragraph) in text.paragraphs().iter().enumerate() {
                    if index > 0 {
                        view = view.spacer();
                    }
                    // 全篇模式没有「当前段」的独立画面，用一行暗色标记指出
                    // `next` / `goto` 把位置移到哪了，否则这两个键看不出效果。
                    // `Anchor` 让渲染层回报这一行的位置，滚动时据此把当前段带回视野。
                    if index == session.paragraph && text.paragraph_count() > 1 {
                        view = view.line(ToolLine::Anchor(format!(
                            "▸ {}/{}",
                            session.paragraph + 1,
                            text.paragraph_count()
                        )));
                    }
                    // 每段都标上段号：词序号只在段内有效，点击时要用段号定位。
                    let focus_here = if Some(index) == focused_in { focus } else { None };
                    view = view.line(ToolLine::Paragraph {
                        pieces: crate::reading::paragraph_pieces(
                            paragraph,
                            is_marked,
                            focus_here,
                        ),
                        index: Some(index),
                    });
                    if show_translation {
                        if let Some(translation) = session.translation_at(index) {
                            for line in translation.lines() {
                                view = view.line(ToolLine::Muted(format!("  {}", line.trim())));
                            }
                        }
                    }
                    // 选中词的说明块紧跟**选中词所在段**，不是整篇末尾：长文里否则
                    // 要滚到底才能看到释义，等于没看到。
                    if Some(index) == focused_in {
                        if let Some((lemma, word)) = selection {
                            view = word_panel(view, session, lemma, word);
                        }
                    }
                }
            }
            crate::reading::ReadMode::Intensive => {
                if let Some(paragraph) = session.current() {
                    let focus_here = if focused_in == Some(session.paragraph) {
                        focus
                    } else {
                        None
                    };
                    view = view.line(ToolLine::Paragraph {
                        pieces: crate::reading::paragraph_pieces(paragraph, is_marked, focus_here),
                        index: None,
                    });
                }
                if show_translation {
                    if let Some(translation) = session.current_translation() {
                        for line in translation.lines() {
                            // 缩进与原文对齐，读起来像同一段的两种语言。
                            view = view.line(ToolLine::Muted(format!("  {}", line.trim())));
                        }
                    }
                }
                if let Some((lemma, word)) = selection {
                    view = word_panel(view, session, lemma, word);
                }
                if !candidates.is_empty() {
                    view = view.spacer().muted(format!(
                        "words · {} marked · /mark <word> to mark any",
                        session.marked.len()
                    ));
                    for (index, candidate) in candidates.iter().enumerate() {
                        view = view.line(ToolLine::Pick {
                            index: index + 1,
                            lemma: candidate.lemma.clone(),
                            gloss: candidate.gloss.clone(),
                            marked: candidate.marked,
                        });
                    }
                }
            }
        }

        view
    }

/// 阅读结束时的统计块。
    pub fn reading_summary(
        text: &str,
        paragraphs: u32,
        opened: u32,
        added: u32,
        marked: &[String],
        seconds: u32,
    ) -> Self {
        let mut view = Self::new("read")
            .badge("done".to_string())
            .accent(Accent::Success)
            .result(text.to_string());
        view = view.kv(
            "read",
            format!("{paragraphs} paragraphs · {} min", (seconds + 30) / 60),
        );
        view = view.kv("opened", format!("{opened} words"));
        view = view.kv("added", format!("{added} to deck"));
        if !marked.is_empty() {
            view = view.kv("marked", marked.join(", "));
        }
        view
    }

    /// 每日阅读统计，供 `/summary` 与 `quietly stats` 使用。
    pub fn reading_stats(stats: &ReadingStats) -> Self {
        let mut view = Self::new("reading")
            .badge("today".to_string())
            .accent(Accent::Note)
            .result(format!(
                "{} paragraphs · {} min",
                stats.paragraphs,
                stats.minutes()
            ));
        if stats.texts > 0 {
            view = view.kv("texts", format!("{}", stats.texts));
        }
        if stats.words_opened > 0 {
            view = view.kv(
                "words",
                format!("{} opened · {} added", stats.words_opened, stats.words_added),
            );
        }
        if stats.quizzes > 0 {
            view = view.kv(
                "quiz",
                format!("{}/{} correct", stats.correct, stats.quizzes),
            );
        }
        view
    }

    /// 一道测验题。选择题把选项列出来，填空题只给题干。
    ///
    /// `answered` 是已判定的结果；`revealed` 用于填空题——它没有输入框，按
    /// enter 只揭示答案、不判对错，也不写 `quiz_attempts`。
    pub fn quiz_card(
        quiz: &Quiz,
        index: usize,
        total: usize,
        answered: Option<bool>,
        revealed: bool,
    ) -> Self {
        let mut view = Self::new("quiz")
            .badge(format!("{}/{}", index + 1, total))
            .accent(match answered {
                Some(true) => Accent::Success,
                Some(false) => Accent::Danger,
                None => Accent::Info,
            })
            .result(quiz.prompt.clone());
        if quiz.is_choice() {
            for (i, choice) in quiz.choices.iter().enumerate() {
                view = view.line(ToolLine::Kv {
                    key: format!("{})", i + 1),
                    value: choice.clone(),
                });
            }
        }
        view = view.muted(format!("source: {}", quiz.source));
        if let Some(correct) = answered {
            view = view.muted(if correct {
                format!("correct · {}", quiz.answer)
            } else {
                format!("answer: {}", quiz.answer)
            });
        } else if revealed {
            view = view.muted(format!("answer: {} · not graded", quiz.answer));
        } else if !quiz.is_choice() {
            view = view.muted("fill in the blank · enter reveals the answer".to_string());
        }
        view
    }

    /// A word block used by both study and lookup.
    ///
    /// `state` decides how much is shown, so the study card, the hint, the revealed
    /// card and the graded card all share one renderer.
    pub fn word_card(state: &CardState) -> Self {
        let word = &state.word;
        let mut result = word.lemma.clone();
        if let Some(phonetic) = &word.phonetic {
            result.push_str("  ");
            result.push_str(phonetic);
        }

        let mut view = Self::new(state.name.clone())
            .badge(state.badge.clone())
            .accent(state.accent)
            .result(result);

        // Frequency belongs next to the headword: it describes the word as a whole,
        // and under the glosses it read as if it qualified the last one. Grey, so
        // it sits behind the word itself.
        if let Some(freq) = crate::text::freq(&word.translation) {
            let mut meta = crate::text::stars(freq);
            if freq.oxford {
                meta.push_str(" oxford");
            }
            view.result_meta = Some(meta);
        }

        if !state.revealed {
            for line in state.hint_lines() {
                view = view.line(line);
            }
            view = view.spacer().muted(state.prompt());
            if let Some(next) = &state.footer {
                view = view.muted(next.clone());
            }
            return view;
        }

        let parsed = word.senses();
        if parsed.senses.is_empty() {
            view = view.text("(no definition yet)".to_string());
        } else {
            // Common senses come first, domain-tagged ones (`[医]`, `[古]`) are
            // grouped below, which is how the "rare meaning" case gets separated
            // from everyday usage.
            let (common, secondary): (Vec<&crate::text::Sense>, Vec<&crate::text::Sense>) =
                parsed.senses.iter().partition(|sense| !sense.is_secondary());

            for sense in common {
                view = view.line(ToolLine::Sense {
                    label: sense.pos.clone(),
                    text: sense.text.clone(),
                });
            }

            if !secondary.is_empty() {
                view = view.muted("rare / technical".to_string());
                for sense in &secondary {
                    view = view.line(ToolLine::Sense {
                        label: format!("[{}]", sense.label),
                        text: sense.text.clone(),
                    });
                }
            }

            if let Some(forms) = parsed.forms {
                view = view.muted(forms);
            }
        }

        // Wiktionary's English glosses fill in what ECDICT lacks: `sake` has no
        // "Japanese rice wine" in the Chinese source, so it shows up here.
        // Folded by default — it is supporting detail, and up to six lines of it
        // would push the Chinese senses off the screen.
        if !word.glosses.is_empty() {
            if state.expand_glosses {
                view = view.spacer().muted("more meanings".to_string());
                for gloss in &word.glosses {
                    view = view.line(ToolLine::Gloss {
                        text: gloss.text.clone(),
                        tags: gloss.tags.clone(),
                    });
                }
            } else {
                view = view.spacer().muted(format!(
                    "more meanings · {} · m",
                    word.glosses.len()
                ));
            }
        }

        if let Some(definition) = &word.definition {
            view = view.muted(definition.clone());
        }

        if !word.collocations.is_empty() {
            // No `phrases` label here: the table names its own columns, and a label
            // on top of a `phrase | meaning` header just says it twice.
            view = view.spacer();
            for collocation in &word.collocations {
                view = view.line(ToolLine::Phrase {
                    phrase: collocation.phrase.clone(),
                    gloss: collocation.gloss.clone(),
                });
            }
        }

        let roots = word.roots();
        if !roots.is_empty() {
            // Rendered like a function definition, because that is what a word's
            // morphology actually is: a name applied to typed parts.
            //
            //     hopeless(hope : root, -less : suffix) {
            //         hope  -> to hope
            //         -less -> lacking (something)
            //     }
            let parts: Vec<(String, String)> = roots
                .iter()
                .map(|root| (root.part.clone(), root.kind.label().to_string()))
                .collect();

            let body: Vec<(String, String)> = roots
                .iter()
                .map(|root| {
                    // Keep every part so the body mirrors the signature; a missing
                    // gloss becomes an em dash instead of a vanished line.
                    let gloss = if root.gloss.trim().is_empty() {
                        "—".to_string()
                    } else {
                        root.gloss.clone()
                    };
                    (root.part.clone(), gloss)
                })
                .collect();

            let sources: Vec<&str> = roots
                .iter()
                .filter_map(|r| r.source.as_deref())
                .collect();
            let source = sources.first().map(|first| {
                if sources.len() > 1 {
                    format!("{first} (+{} more)", sources.len() - 1)
                } else {
                    (*first).to_string()
                }
            });

            view = view.spacer().line(ToolLine::Code(CodeBlock {
                name: word.lemma.clone(),
                parts,
                body,
                source,
            }));
        }

        if !word.examples.is_empty() {
            // Examples live in the right-hand column so the meaning and the
            // etymology keep the left, and the headword inside each sentence is
            // highlighted against its surroundings.
            view = view.aside(ToolLine::Muted("examples".to_string()));
            for (index, example) in word.examples.iter().take(3).enumerate() {
                view = view.aside(ToolLine::Example {
                    index: index + 1,
                    en: example.en.clone(),
                    zh: example.zh.clone(),
                    highlight: Some(word.lemma.clone()),
                });
            }
        }

        if state.show_ratings {
            view = view.spacer().line(ToolLine::Rating {
                selected: state.graded,
            });
            if let Some(next) = &state.footer {
                view = view.muted(next.clone());
            }
        }

        view
    }
}

/// Everything needed to paint one word block.
#[derive(Debug, Clone)]
pub struct CardState {
    pub name: String,
    pub badge: String,
    pub word: Word,
    pub revealed: bool,
    /// Show roots while still hidden.
    pub hint: bool,
    /// Rating already recorded for this card (re-selectable).
    pub graded: Option<Rating>,
    /// Only show the rating row when grading is meaningful.
    pub show_ratings: bool,
    /// Small dim line under the block.
    pub footer: Option<String>,
    /// Rail colour: new cards vs review cards.
    pub accent: Accent,
    /// Show every Wiktionary gloss instead of a one-line count.
    ///
    /// The English glosses are a nice-to-have next to the Chinese senses, so they
    /// stay folded until asked for; `m` flips this on every card at once.
    pub expand_glosses: bool,
}

impl CardState {
    pub fn new(word: Word, badge: impl Into<String>) -> Self {
        Self {
            name: "word".to_string(),
            badge: badge.into(),
            word,
            revealed: false,
            hint: false,
            graded: None,
            show_ratings: false,
            footer: None,
            accent: Accent::Neutral,
            expand_glosses: false,
        }
    }

    pub fn accent(mut self, accent: Accent) -> Self {
        self.accent = accent;
        self
    }

    pub fn revealed(mut self) -> Self {
        self.revealed = true;
        self.show_ratings = true;
        self
    }

    /// Show the Wiktionary glosses right away. Lookup is an explicit "tell me
    /// everything" so it opts in; the study card stays folded.
    pub fn with_glosses(mut self) -> Self {
        self.expand_glosses = true;
        self
    }

    /// Drop the 1-4 rating row. Lookup shows the same revealed card but grading
    /// there would write a review, so the keys are not offered.
    pub fn without_ratings(mut self) -> Self {
        self.show_ratings = false;
        self
    }

    pub fn graded(mut self, rating: Rating) -> Self {
        self.graded = Some(rating);
        self
    }

    pub fn hint(mut self, hint: bool) -> Self {
        self.hint = hint;
        self
    }

    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Progressive hint shown before the answer is revealed.
    ///
    /// Ordered by how much it gives away: the morphological split first (it is
    /// already on the card once revealed, so it mainly jogs memory), then an
    /// example sentence for context, then only the part of speech.
    fn hint_lines(&self) -> Vec<ToolLine> {
        if !self.hint {
            return Vec::new();
        }

        let roots = self.word.roots();
        if !roots.is_empty() {
            let parts: Vec<String> = roots
                .iter()
                .map(|root| format!("{} ({}, {})", root.part, root.kind.label(), root.gloss))
                .collect();
            return vec![ToolLine::Muted(format!("hint: {}", parts.join("  +  ")))];
        }

        // No etymology on file: an example puts the word in context without
        // spelling out its meaning. The translation is withheld on purpose — it
        // would hand over the answer the hint is meant to nudge towards.
        if let Some(example) = self.word.examples.first() {
            let mut lines = vec![ToolLine::Muted("hint: example".to_string())];
            lines.push(ToolLine::Example {
                index: 1,
                en: example.en.clone(),
                zh: None,
                highlight: Some(self.word.lemma.clone()),
            });
            return lines;
        }

        // Nothing else: at least say what kind of word it is.
        let kinds = crate::text::parts_of_speech(&self.word.translation);
        if !kinds.is_empty() {
            return vec![ToolLine::Muted(format!("hint: {}", kinds.join(" / ")))];
        }

        vec![ToolLine::Muted("hint: nothing on file · space to peek".to_string())]
    }

    fn prompt(&self) -> String {
        if self.graded.is_some() {
            "enter/s to continue".to_string()
        } else if self.revealed {
            let mut line = "1 again · 2 hard · 3 good · 4 easy".to_string();
            // Only advertise the fold when there is something folded.
            if !self.word.glosses.is_empty() {
                line.push_str(if self.expand_glosses {
                    " · m fold"
                } else {
                    " · m more"
                });
            }
            line
        } else {
            "1-4 to grade · tab for a hint · space to peek".to_string()
        }
    }
}

#[derive(Debug, Clone)]
pub enum ToolLine {
    Text(String),
    Muted(String),
    Accent(String),
    Kv { key: String, value: String },
    Bar { label: String, value: u32, max: u32 },
    Example {
        /// 1-based position in the example list, shown as a gutter number.
        index: usize,
        en: String,
        zh: Option<String>,
        /// Word to emphasise inside `en`. Matched case-insensitively, including the
        /// common inflections (`-s`, `-es`, `-ed`, `-ing`, `-d`).
        highlight: Option<String>,
    },
    /// One dictionary sense. `index == 0` means no number (a secondary sense).
    Sense {
        /// Part of speech (`n.`) or a bracketed domain tag (`[医]`).
        label: String,
        text: String,
    },
    /// A reading paragraph: pre-split pieces so the renderer can draw marked and
    /// focused words differently, and report which word sits in which cell.
    ///
    /// `index` 是这段在全文里的段号：点击时据此判断点到的是不是当前段，
    /// 词序号只在段内有效，跨段必须带上段号才不会选错词。
    Paragraph {
        pieces: Vec<crate::reading::Piece>,
        index: Option<usize>,
    },
    /// 阅读时的当前段标记（`▸ 12/50`）。渲染层回报它的行号，滚动时用来把
    /// 当前段带回视野；它只是位置提示，没有别的含义。
    Anchor(String),
    /// 一个块内部的两栏：宽终端并排，窄终端把右栏折到下方。
    ///
    /// 选词说明块用它把词组放在释义右侧——`ToolView::aside` 贴着整张卡片顶部，
    /// 而说明块在段落下方，用 aside 会把词组画到段落旁边。
    Split {
        left: Vec<ToolLine>,
        right: Vec<ToolLine>,
    },
    /// One word from the current paragraph, selectable by its number.
    Pick {
        index: usize,
        lemma: String,
        gloss: String,
        marked: bool,
    },
    /// A fixed expression: `for the sake of → 为了`.
    ///
    /// Rendered as a boxed table, which suits the full word card.
    Phrase { phrase: String, gloss: String },
    /// 一行一条的 `phrase → gloss`，不画表格。
    ///
    /// 选词说明块的右栏只有二三十列，表格框线会把内容挤成一条窄缝；这里直接用
    /// 缩进+箭头，和左栏逐行对齐。
    PhraseInline { phrase: String, gloss: String },
    /// One English gloss from Wiktionary, with optional register tags.
    Gloss { text: String, tags: Vec<String> },
    /// Four rating options, the selected one highlighted.
    Rating { selected: Option<Rating> },
    /// Morphology rendered as a code block.
    Code(CodeBlock),
    Spacer,
}

/// A word's morphology, laid out like a function definition:
///
/// ```text
/// hopeless(hope : root, -less : suffix) {
///     hope  -> to hope
///     -less -> lacking (something)
/// }  // Wiktionary {{af}}
/// ```
#[derive(Debug, Clone)]
pub struct CodeBlock {
    /// Headword, shown as the function name.
    pub name: String,
    /// `(part, kind)` pairs in the signature.
    pub parts: Vec<(String, String)>,
    /// `(part, gloss)` pairs in the body.
    pub body: Vec<(String, String)>,
    /// Optional attribution, shown as a trailing comment.
    pub source: Option<String>,
}

/// Tabular output (books, search hits, affix lists).
#[derive(Debug, Clone, Default)]
pub struct TableView {
    pub caption: Option<String>,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub right_align: Vec<bool>,
    pub footer: Option<String>,
}

impl TableView {
    pub fn new(headers: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        let len = headers.len();
        Self {
            caption: None,
            headers,
            rows,
            right_align: vec![false; len],
            footer: None,
        }
    }

    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    pub fn right_align(mut self, index: usize) -> Self {
        if let Some(cell) = self.right_align.get_mut(index) {
            *cell = true;
        }
        self
    }

    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }
}

/// Daily summary block.
/// Extra counters the summary needs but `DailyStats` does not carry.
#[derive(Debug, Clone, Default)]
pub struct SummaryStatus {
    /// New cards still allowed today under the daily quota.
    pub new_left: u32,
    /// Cards still due right now.
    pub due_left: u32,
    /// Words in the active deck.
    pub deck_total: u32,
}

pub fn summary_tool(
    daily: &DailyStats,
    streak: u32,
    upcoming: &[(String, u32)],
    status: &SummaryStatus,
) -> ToolView {
    let day = daily
        .day
        .map(crate::time::format_day)
        .unwrap_or_else(|| crate::time::format_day(crate::time::today()));

    // New words: how many landed today, and how much of the quota is left.
    let new_line = if status.new_left == 0 {
        format!("{} learned · quota reached", daily.new_learned)
    } else {
        format!("{} learned · {} left today", daily.new_learned, status.new_left)
    };

    // Reviews: finished vs still waiting.
    let review_line = if status.due_left == 0 {
        format!("{} done · nothing due", daily.reviews)
    } else {
        format!("{} done · {} still due", daily.reviews, status.due_left)
    };

    let mut view = ToolView::new("summary")
        .accent(Accent::Note)
        .badge(day)
        .result(format!(
            "{} new · {} reviews · {} min focused",
            daily.new_learned,
            daily.reviews,
            daily.minutes()
        ))
        .kv("new today", new_line)
        .kv("reviews", review_line)
        .kv(
            "ratings",
            format!(
                "again {} · hard {} · good {} · easy {}",
                daily.again, daily.hard, daily.good, daily.easy
            ),
        )
        .kv("streak", format!("{streak} days"));

    if status.deck_total > 0 {
        view = view.kv("deck", format!("{} words", status.deck_total));
    }

    if !upcoming.is_empty() {
        view = view.spacer().muted("next 7 days");
        let max = upcoming.iter().map(|(_, n)| *n).max().unwrap_or(0).max(1);
        for (label, count) in upcoming {
            view = view.line(ToolLine::Bar {
                label: label.clone(),
                value: *count,
                max,
            });
        }
    }
    view
}

/// Convert examples into tool lines for the reading/translation modules later.
pub fn example_lines(examples: &[Example]) -> Vec<ToolLine> {
    examples
        .iter()
        .enumerate()
        .map(|(index, e)| ToolLine::Example {
            index: index + 1,
            en: e.en.clone(),
            zh: e.zh.clone(),
            highlight: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daily(new_learned: u32, reviews: u32) -> DailyStats {
        DailyStats {
            new_learned,
            reviews,
            again: 1,
            hard: 2,
            good: 3,
            easy: 4,
            seconds: 600,
            ..Default::default()
        }
    }

    fn kv_lines(view: &ToolView) -> Vec<String> {
        view.lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Kv { key, value } => Some(format!("{key}={value}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn summary_reports_new_and_review_progress() {
        let view = summary_tool(
            &daily(8, 12),
            4,
            &[],
            &SummaryStatus {
                new_left: 12,
                due_left: 3,
                deck_total: 40,
            },
        );
        assert_eq!(view.accent, Accent::Note, "summaries need their own colour");

        let kvs = kv_lines(&view);
        assert!(kvs
            .iter()
            .any(|line| line == "new today=8 learned · 12 left today"));
        assert!(kvs.iter().any(|line| line == "reviews=12 done · 3 still due"));
        assert!(kvs.iter().any(|line| line == "deck=40 words"));
        assert!(kvs.iter().any(|line| line == "streak=4 days"));
    }

    #[test]
    fn summary_flags_quota_and_clear_queue() {
        let view = summary_tool(
            &daily(20, 30),
            9,
            &[],
            &SummaryStatus {
                new_left: 0,
                due_left: 0,
                deck_total: 0,
            },
        );
        let kvs = kv_lines(&view);
        assert!(kvs
            .iter()
            .any(|line| line == "new today=20 learned · quota reached"));
        assert!(kvs
            .iter()
            .any(|line| line == "reviews=30 done · nothing due"));
        // No deck number when we do not know it.
        assert!(!kvs.iter().any(|line| line.starts_with("deck=")));
    }

    #[test]
    fn summary_headline_carries_counts() {
        let view = summary_tool(&daily(8, 12), 4, &[], &SummaryStatus::default());
        let headline = view.result.expect("headline");
        assert!(headline.contains("8 new"));
        assert!(headline.contains("12 reviews"));
        assert!(headline.contains("10 min"));
    }

    #[test]
    fn card_state_carries_accent() {
        let word = crate::model::Word::new("inspect", "v. check");
        let state = CardState::new(word, "new · 1/5").accent(Accent::Warn);
        assert_eq!(state.accent, Accent::Warn);
        let view = ToolView::word_card(&state);
        assert_eq!(view.accent, Accent::Warn, "rail colour must reach the block");
    }

    #[test]
    fn frequency_rides_on_the_headword_line() {
        // Stars describe the word, not one of its glosses, so they sit next to the
        // phonetic instead of under `more meanings` where they read as a gloss.
        let word = crate::model::Word::new("sake", "-K5\nn. 目的, 缘故");
        let mut state = CardState::new(word, "lookup").revealed();
        state.word.glosses = vec![crate::text::Gloss::new("Rice wine.")];

        let view = ToolView::word_card(&state);
        let headline = view.result.clone().expect("headline");
        assert!(headline.contains("sake"), "{headline}");
        let meta = view.result_meta.clone().expect("frequency meta");
        assert!(meta.contains("★★★★★"), "stars ride the headword: {meta}");
        assert!(meta.contains("oxford"), "{meta}");
        assert!(
            !headline.contains('★'),
            "the stars are meta, drawn grey and apart: {headline}"
        );

        // And they must not linger in the body.
        assert!(
            !view.lines.iter().any(|line| matches!(
                line,
                ToolLine::Muted(text) if text.contains('★')
            )),
            "stars must not repeat below"
        );
    }

    /// Only the `hint:` lines, so the card's own prompt does not get in the way.
    fn hint_texts(view: &ToolView) -> Vec<String> {
        view.lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Muted(text) if text.starts_with("hint:") => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hint_prefers_the_morphological_split() {
        let mut word = crate::model::Word::new("hopeless", "a. 无希望的");
        word.roots = vec![crate::model::WordRoot::new(
            "-less",
            "lacking",
            crate::model::RootKind::Suffix,
        )];
        word.examples = vec![crate::model::Example::new("You are hopeless.", None)];

        let view = ToolView::word_card(&CardState::new(word, "1/3").hint(true));
        let text = hint_texts(&view).join("\n");
        assert!(text.contains("-less (suffix, lacking)"), "{text}");
        assert!(!text.contains("You are hopeless."), "roots win over examples");
    }

    #[test]
    fn hint_falls_back_to_an_example_without_roots() {
        let mut word = crate::model::Word::new("serendipity", "n. 意外发现");
        word.examples = vec![crate::model::Example::new(
            "It was pure serendipity.",
            Some("纯属意外。".into()),
        )];

        let view = ToolView::word_card(&CardState::new(word, "1/3").hint(true));
        assert!(hint_texts(&view).iter().any(|t| t.contains("hint: example")));
        let example = view
            .lines
            .iter()
            .find_map(|line| match line {
                ToolLine::Example { en, zh, highlight, .. } => Some((en, zh, highlight)),
                _ => None,
            })
            .expect("hint should carry an example");
        assert!(example.0.contains("serendipity"));
        assert_eq!(example.2.as_deref(), Some("serendipity"));
        // The translation would give the answer away, so the hint omits it.
        assert!(example.1.is_none(), "hint example must not show the Chinese");
    }

    #[test]
    fn hint_falls_back_to_the_part_of_speech() {
        let word = crate::model::Word::new("abandon", "-K3\nvt. 放弃\nn. 放任");
        let view = ToolView::word_card(&CardState::new(word, "1/3").hint(true));
        let text = hint_texts(&view).join("\n");
        assert_eq!(text.trim(), "hint: vt / n");
        assert!(!text.contains("letters"), "letter count is no longer shown");
    }

    #[test]
    fn hint_says_so_when_nothing_is_known() {
        let word = crate::model::Word::new("zzz", "");
        let view = ToolView::word_card(&CardState::new(word, "1/3").hint(true));
        let text = hint_texts(&view).join("\n");
        assert!(text.contains("nothing on file"), "{text}");
    }

    #[test]
    fn hint_is_hidden_until_requested() {
        let word = crate::model::Word::new("abandon", "vt. 放弃");
        let view = ToolView::word_card(&CardState::new(word, "1/3"));
        assert!(hint_texts(&view).is_empty());
    }

    fn senses(view: &ToolView) -> Vec<(String, String)> {
        view.lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Sense { label, text } => Some((label.clone(), text.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn common_senses_come_first_and_rare_ones_follow() {
        let word = crate::model::Word::new(
            "bank",
            "-K1\nn. 银行, 堤, 岸\n[医] 库\n[时态] banked, banking, banks",
        );
        let view = ToolView::word_card(&CardState::new(word, "lookup").revealed());

        let rows = senses(&view);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "n.");
        assert_eq!(rows[0].1, "银行, 堤, 岸");
        assert_eq!(rows[1].0, "[医]");

        let texts: Vec<String> = view
            .lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Muted(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t == "rare / technical"));
        assert!(texts.iter().any(|t| t.contains("[时态] banked")));
    }

    #[test]
    fn collocations_render_below_the_senses() {
        let mut word = crate::model::Word::new("sake", "n. 目的, 缘故");
        word.collocations = vec![
            crate::model::Collocation::new("for the sake of", "为了"),
            crate::model::Collocation::new("for its own sake", "为了本身"),
        ];
        let view = ToolView::word_card(&CardState::new(word, "lookup").revealed());

        let phrases: Vec<(String, String)> = view
            .lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Phrase { phrase, gloss } => Some((phrase.clone(), gloss.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(phrases.len(), 2);
        assert_eq!(phrases[0].0, "for the sake of");
        assert_eq!(phrases[0].1, "为了");
    }

    #[test]
    fn word_without_roots_does_not_scold_the_reader() {
        // An empty etymology block simply does not render; the old placeholder made
        // 63% of cards carry a line of noise.
        let word = crate::model::Word::new("sake", "n. 目的");
        let view = ToolView::word_card(&CardState::new(word, "lookup").revealed());
        assert!(!view.lines.iter().any(|line| matches!(
            line,
            ToolLine::Muted(text) if text.contains("no etymology")
        )));
    }

    fn gloss_texts(view: &ToolView) -> Vec<String> {
        view.lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Gloss { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn muted_texts(view: &ToolView) -> Vec<String> {
        view.lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Muted(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn word_with_glosses() -> crate::model::Word {
        let mut word = crate::model::Word::new("sake", "n. 目的, 缘故");
        word.glosses = ["Cause, interest or account.", "Rice wine."]
            .iter()
            .map(|text| crate::text::Gloss::new(*text))
            .collect();
        word
    }

    #[test]
    fn more_meanings_are_folded_by_default() {
        // Six lines of English glosses push the Chinese senses off a short
        // terminal, so the card asks first.
        let view = ToolView::word_card(&CardState::new(word_with_glosses(), "1/9").revealed());
        assert!(gloss_texts(&view).is_empty(), "glosses stay hidden");
        assert!(
            muted_texts(&view)
                .iter()
                .any(|text| text.contains("more meanings · 2 · m")),
            "the fold advertises its size and the key that opens it"
        );
    }

    #[test]
    fn pressing_m_unfolds_the_glosses() {
        let mut state = CardState::new(word_with_glosses(), "1/9").revealed();
        state.expand_glosses = true;
        let view = ToolView::word_card(&state);
        assert_eq!(gloss_texts(&view).len(), 2);
        assert!(!muted_texts(&view).iter().any(|t| t.contains("more meanings ·")));
    }

    #[test]
    fn lookup_opens_with_the_glosses_already_shown() {
        let view = ToolView::word_card(
            &CardState::new(word_with_glosses(), "lookup")
                .revealed()
                .with_glosses()
                .without_ratings(),
        );
        assert_eq!(
            gloss_texts(&view).len(),
            2,
            "an explicit lookup shows everything"
        );
    }

    #[test]
    fn lookup_does_not_offer_grading() {
        // Grading is only meaningful inside a study session: the keys would write
        // a review for a word the reader merely looked up.
        let view = ToolView::word_card(
            &CardState::new(word_with_glosses(), "lookup")
                .revealed()
                .with_glosses()
                .without_ratings(),
        );
        assert!(
            !view
                .lines
                .iter()
                .any(|line| matches!(line, ToolLine::Rating { .. })),
            "no 1-4 rating row on a lookup card"
        );

        let study = ToolView::word_card(&CardState::new(word_with_glosses(), "1/9").revealed());
        assert!(
            study
                .lines
                .iter()
                .any(|line| matches!(line, ToolLine::Rating { .. })),
            "the study card keeps the rating row"
        );
    }
}
