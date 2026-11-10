//! 单行编辑器：光标移动、历史记录。故意保持极简。

const MAX_HISTORY: usize = 200;

pub struct Editor {
    chars: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    browsing: Option<usize>,
    draft: String,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            chars: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            browsing: None,
            draft: String::new(),
        }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.browsing = None;
    }

    pub fn set_text(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn insert_char(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    // ---- word-level editing (ctrl + arrows / backspace) ----

    fn is_word_char(ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '/' || ch == '.'
    }

    /// Start index of the word before `cursor`.
    fn word_start(&self, from: usize) -> usize {
        let mut idx = from;
        while idx > 0 && !Self::is_word_char(self.chars[idx - 1]) {
            idx -= 1;
        }
        while idx > 0 && Self::is_word_char(self.chars[idx - 1]) {
            idx -= 1;
        }
        idx
    }

    /// End index of the word after `cursor`.
    fn word_end(&self, from: usize) -> usize {
        let mut idx = from;
        while idx < self.chars.len() && !Self::is_word_char(self.chars[idx]) {
            idx += 1;
        }
        while idx < self.chars.len() && Self::is_word_char(self.chars[idx]) {
            idx += 1;
        }
        idx
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start(self.cursor);
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end(self.cursor);
    }

    /// Ctrl+Backspace: delete the word before the caret.
    pub fn delete_word_back(&mut self) {
        let start = self.word_start(self.cursor);
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Ctrl+Delete: delete the word after the caret.
    pub fn delete_word_forward(&mut self) {
        let end = self.word_end(self.cursor);
        self.chars.drain(self.cursor..end);
    }

    /// Ctrl+U: drop everything before the caret.
    pub fn kill_to_start(&mut self) {
        self.chars.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Ctrl+K: drop everything after the caret.
    pub fn kill_to_end(&mut self) {
        self.chars.truncate(self.cursor);
    }

    /// Ctrl+W: drop the word before the caret (same as ctrl+backspace).
    pub fn kill_word(&mut self) {
        self.delete_word_back();
    }

    /// 取出内容并写入历史。返回去除首尾空白的字符串。
    pub fn take(&mut self) -> String {
        let text = self.text();
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() && self.history.last() != Some(&trimmed) {
            self.history.push(trimmed.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.clear();
        trimmed
    }

    /// 复制当前历史，用于落盘到设置表。
    pub fn history(&self) -> Vec<String> {
        self.history.clone()
    }

    /// 启动时用上次会话的历史填充。
    pub fn set_history(&mut self, entries: Vec<String>) {
        self.history = entries;
        if self.history.len() > MAX_HISTORY {
            let drop = self.history.len() - MAX_HISTORY;
            self.history.drain(..drop);
        }
        self.browsing = None;
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.browsing {
            None => {
                self.draft = self.text();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(idx) => idx - 1,
        };
        self.browsing = Some(next);
        self.set_text(&self.history[next].clone());
    }

    pub fn history_next(&mut self) {
        let Some(idx) = self.browsing else { return };
        if idx + 1 < self.history.len() {
            let next = idx + 1;
            self.browsing = Some(next);
            self.set_text(&self.history[next].clone());
        } else {
            self.browsing = None;
            let draft = self.draft.clone();
            self.set_text(&draft);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_at_cursor() {
        let mut e = Editor::new();
        e.set_text("abc");
        e.left();
        e.insert_char('X');
        assert_eq!(e.text(), "abXc");
        e.backspace();
        assert_eq!(e.text(), "abc");
        e.delete();
        assert_eq!(e.text(), "ab");
    }

    #[test]
    fn history_roundtrip() {
        let mut e = Editor::new();
        e.set_text("one");
        assert_eq!(e.take(), "one");
        e.set_text("two");
        e.take();
        e.history_prev();
        assert_eq!(e.text(), "two");
        e.history_prev();
        assert_eq!(e.text(), "one");
        e.history_next();
        assert_eq!(e.text(), "two");
        e.history_next();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn blank_input_not_stored() {
        let mut e = Editor::new();
        e.set_text("   ");
        assert_eq!(e.take(), "");
        e.history_prev();
        assert_eq!(e.text(), "");
    }
}
