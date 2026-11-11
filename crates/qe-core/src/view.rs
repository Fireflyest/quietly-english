//! UI-agnostic view models.
//!
//! The app layer only produces messages; the TUI layer only paints them.
//! The look is deliberately modelled on a command-line agent: `⏺ name` followed by
//! `⎿ result`, with indented detail lines and no big borders.

use crate::model::{DailyStats, Example, Rating, Word};

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
    /// Indented detail lines.
    pub lines: Vec<ToolLine>,
    /// Optional right-hand column, rendered beside `lines` when the terminal is
    /// wide enough and stacked underneath it otherwise.
    pub aside: Vec<ToolLine>,
    pub accent: Accent,
}

impl ToolView {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            badge: None,
            result: None,
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
            // Common senses are numbered so the card reads like a dictionary entry;
            // domain-tagged ones (`[医]`, `[古]`) are grouped below, which is how the
            // "rare meaning" case gets separated from everyday usage.
            let (common, secondary): (Vec<&crate::text::Sense>, Vec<&crate::text::Sense>) =
                parsed.senses.iter().partition(|sense| !sense.is_secondary());

            for (index, sense) in common.iter().enumerate() {
                view = view.line(ToolLine::Sense {
                    index: index + 1,
                    label: sense.pos.clone(),
                    text: sense.text.clone(),
                });
            }

            if !secondary.is_empty() {
                view = view.muted("rare / technical".to_string());
                for sense in &secondary {
                    view = view.line(ToolLine::Sense {
                        index: 0,
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
        if !word.glosses.is_empty() {
            view = view.spacer().muted("more meanings".to_string());
            for gloss in &word.glosses {
                view = view.line(ToolLine::Gloss {
                    text: gloss.text.clone(),
                    tags: gloss.tags.clone(),
                });
            }
        }

        if let Some(freq) = crate::text::freq(&word.translation) {
            let mut label = crate::text::stars(freq);
            if freq.oxford {
                label.push_str("  oxford");
            }
            view = view.muted(label);
        }
        if let Some(definition) = &word.definition {
            view = view.muted(definition.clone());
        }

        if !word.collocations.is_empty() {
            view = view.spacer().muted("phrases".to_string());
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
            "1 again · 2 hard · 3 good · 4 easy".to_string()
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
        index: usize,
        /// Part of speech (`n.`) or a bracketed domain tag (`[医]`).
        label: String,
        text: String,
    },
    /// A fixed expression: `for the sake of → 为了`.
    Phrase { phrase: String, gloss: String },
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

    fn senses(view: &ToolView) -> Vec<(usize, String, String)> {
        view.lines
            .iter()
            .filter_map(|line| match line {
                ToolLine::Sense { index, label, text } => {
                    Some((*index, label.clone(), text.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn common_senses_are_numbered_and_rare_ones_follow() {
        let word = crate::model::Word::new(
            "bank",
            "-K1\nn. 银行, 堤, 岸\n[医] 库\n[时态] banked, banking, banks",
        );
        let view = ToolView::word_card(&CardState::new(word, "lookup").revealed());

        let rows = senses(&view);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 1, "common sense is numbered from 1");
        assert_eq!(rows[0].1, "n.");
        assert_eq!(rows[1].0, 0, "secondary sense is unnumbered");
        assert_eq!(rows[1].1, "[医]");

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
}
