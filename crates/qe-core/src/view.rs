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
    pub accent: Accent,
}

impl ToolView {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            badge: None,
            result: None,
            lines: Vec::new(),
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

        let translation = if word.translation.trim().is_empty() {
            "(no definition yet)".to_string()
        } else {
            word.translation.clone()
        };
        for line in translation.split('\n') {
            view = view.text(line.to_string());
        }
        if let Some(definition) = &word.definition {
            view = view.muted(definition.clone());
        }

        let (roots, guessed) = word.roots_or_guess();
        if roots.is_empty() {
            view = view
                .spacer()
                .muted("no roots on file · try /root <affix>");
        } else {
            // Flowing prose instead of an aligned table: a word usually has two or
            // three parts, and a sentence reads less like a dashboard.
            let parts: Vec<String> = roots
                .iter()
                .map(|root| format!("{} ({}, {})", root.part, root.kind.label(), root.gloss))
                .collect();
            view = view.spacer().text(parts.join("  +  "));

            let glosses: Vec<&str> = roots
                .iter()
                .filter(|r| !r.gloss.is_empty())
                .map(|r| r.gloss.as_str())
                .collect();
            if !glosses.is_empty() {
                let suffix = if guessed { "  (guessed)" } else { "" };
                view = view.muted(format!("→ {}{suffix}", glosses.join(" + ")));
            }
        }

        if !word.examples.is_empty() {
            view = view.spacer().muted("examples");
            for example in word.examples.iter().take(3) {
                view = view.line(ToolLine::Example {
                    en: example.en.clone(),
                    zh: example.zh.clone(),
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

    fn hint_lines(&self) -> Vec<ToolLine> {
        if !self.hint {
            return Vec::new();
        }
        let (roots, _) = self.word.roots_or_guess();
        if roots.is_empty() {
            let first = self.word.lemma.chars().next().unwrap_or('?');
            return vec![ToolLine::Muted(format!(
                "hint: starts with '{first}', {} letters",
                self.word.lemma.chars().count()
            ))];
        }
        let parts: Vec<String> = roots
            .iter()
            .map(|root| format!("{} ({}, {})", root.part, root.kind.label(), root.gloss))
            .collect();
        vec![ToolLine::Muted(format!("hint: {}", parts.join("  +  ")))]
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
    Example { en: String, zh: Option<String> },
    /// Four rating options, the selected one highlighted.
    Rating { selected: Option<Rating> },
    Spacer,
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
        .map(|e| ToolLine::Example {
            en: e.en.clone(),
            zh: e.zh.clone(),
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
}
