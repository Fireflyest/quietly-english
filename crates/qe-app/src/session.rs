//! One study session's state.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use qe_core::model::{Book, Rating, SrsCard, StudyCard};

pub struct StudySession {
    pub book: Book,
    pub queue: VecDeque<StudyCard>,
    pub current: Option<StudyCard>,
    /// Answer is on screen.
    pub revealed: bool,
    /// Roots are on screen while the answer is still hidden.
    pub hint: bool,
    /// Rating already written for the current card; may be changed with 1-4 again.
    pub graded: Option<Rating>,
    /// Card state computed from the last rating, used for the "next ..." footer.
    pub last_after: Option<SrsCard>,
    /// Number of new words already graded in this session.
    pub new_done: u32,
    /// Number of reviews already graded in this session.
    pub review_done: u32,
    pub started_at: DateTime<Utc>,
    pub new_total: u32,
    pub review_total: u32,
    /// Index of the card message inside `App::messages`.
    pub card_msg: Option<usize>,
}

impl StudySession {
    pub fn new(book: Book, queue: VecDeque<StudyCard>, new_total: u32, review_total: u32) -> Self {
        Self {
            book,
            queue,
            current: None,
            revealed: false,
            hint: false,
            graded: None,
            last_after: None,
            new_done: 0,
            review_done: 0,
            started_at: Utc::now(),
            new_total,
            review_total,
            card_msg: None,
        }
    }

    pub fn total(&self) -> usize {
        (self.new_total + self.review_total) as usize
    }

    pub fn done(&self) -> u32 {
        self.new_done + self.review_done
    }

    pub fn is_new_current(&self) -> bool {
        self.current.as_ref().map(|c| c.is_new).unwrap_or(false)
    }

    /// `new · 3/12` style badge for the card header.
    pub fn badge(&self) -> String {
        let kind = if self.is_new_current() { "new" } else { "review" };
        let position = (self.done() + 1).min(self.total() as u32);
        format!("{kind} · {position}/{}", self.total())
    }

    pub fn set_current(&mut self, card: StudyCard) {
        self.current = Some(card);
        self.revealed = false;
        self.hint = false;
        self.graded = None;
        self.last_after = None;
        self.card_msg = None;
    }
}
