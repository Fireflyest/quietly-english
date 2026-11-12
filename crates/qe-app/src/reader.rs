//! 阅读交互：把 `qe_core::reading` 的会话状态接到 App 的处理逻辑上。
//!
//! 这里只做「读文本该有的动作」：翻页、选词、标记、切模式、收词、出题。
//! 查词、加词书、写日志都调已有的 `Store` 方法，不重复实现。

use anyhow::Result;
use qe_core::model::{Quiz, ReadingLog, Word};
use qe_core::reading::{ReadMode, ReadingSession, Text};use qe_core::view::{Accent, Candidate, Message, ToolView};

use crate::app::App;

/// 精读模式下最多列出几个候选词（对应数字键 1-9）。
pub const MAX_CANDIDATES: usize = 9;

/// 把文本文件读成一篇 `Text`。
///
/// 支持两种格式：
///
/// * 单文件：空行分段，整篇是原文；
/// * 双语：`---` 分隔两半，前半原文、后半译文，段数对齐。
///
/// 标题取文件名（去掉扩展名），来源与授权优先读文件开头的 `# source:` /
/// `# license:` 注释。
pub fn load_text(path: &std::path::Path) -> Result<Text> {
    let raw = std::fs::read_to_string(path)?;
    let mut title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".to_string());
    let mut source = None;
    let mut license = None;

    let mut body = String::new();
    let mut in_header = true;
    for line in raw.lines() {
        if in_header {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("# source:") {
                source = Some(rest.trim().to_string());
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("# license:") {
                license = Some(rest.trim().to_string());
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("# title:") {
                title = rest.trim().to_string();
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            in_header = false;
        }
        body.push_str(line);
        body.push('\n');
    }

    // `---` 独占一行时视为原文与译文的分界。
    let (original, translation) = match split_bilingual(&body) {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (body.clone(), String::new()),
    };

    Ok(Text {
        id: 0,
        title,
        source,
        license,
        body: original,
        translation,
    })
}

fn split_bilingual(body: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    for line in body.split('\n') {
        if line.trim() == "---" {
            let head = &body[..offset];
            let tail = &body[(offset + line.len()).min(body.len())..];
            return Some((head.trim(), tail.trim()));
        }
        offset += line.len() + 1;
    }
    None
}

impl App {
    /// 当前段里可作为候选的词（还没标记、库里有词条的那些）。
    pub fn reading_candidates(&self) -> Vec<Candidate> {
        let Some(session) = self.reading.as_ref() else {
            return Vec::new();
        };
        let tokens = session.current_tokens();
        let mut out = Vec::new();
        for token in tokens {
            if out.len() >= MAX_CANDIDATES {
                break;
            }
            let marked = session.is_marked(&token.lower);
            let gloss = self
                .store
                .word(&token.lower)
                .ok()
                .flatten()
                .map(|word| gloss_of(&word))
                .unwrap_or_default();
            out.push(Candidate {
                lemma: token.lower,
                gloss,
                marked,
            });
        }
        out
    }

    /// 阅读时显示选中的词（状态栏用），没选词时为 `None`。
    pub(crate) fn focused_lemma(&self) -> Option<String> {
        self.reading.as_ref()?.focused_lemma()
    }

    /// 重画阅读卡片（原地替换那条消息）。
    pub fn render_reading(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            return Ok(());
        };
        let candidates = self.reading_candidates();
        // 选中词的说明块就在卡片里，不另开一张卡：查不到词条时把 lemma 也传进去，
        // 卡片会显示「未收录」，读者仍能看出选中的是哪个词。
        let focused = session
            .focused_lemma()
            .map(|lemma| {
                let word = self.store.word(&lemma).ok().flatten();
                (lemma, word)
            });
        let selection = focused
            .as_ref()
            .map(|(lemma, word)| (lemma.as_str(), word.as_ref()));
        let view = ToolView::reading_card(
            session,
            &candidates,
            self.show_translation,
            selection,
        );
        let message = Message::tool(view);
        let full_mode = session.mode == ReadMode::Full;
        match session.message {
            Some(index) if index < self.messages.len() => self.messages[index] = message,
            _ => {
                self.messages.push(message);
                let index = self.messages.len() - 1;
                if let Some(session) = self.reading.as_mut() {
                    session.message = Some(index);
                }
            }
        }
        if full_mode {
            // 全篇模式把整篇画成一条消息：保持当前滚动位置，交给下一帧的
            // `scroll_to_anchor` 判断当前段是否需要被带回视野，避免先跳到全文
            // 末尾再跳回来。
            self.reveal_anchor = true;
        } else {
            self.scroll_back = 0;
        }
        self.refresh_status();
        Ok(())
    }

    /// 列出已导入的文本。
    pub fn list_texts(&mut self) -> Result<()> {
        let texts = self.store.texts()?;
        if texts.is_empty() {
            self.note("no texts yet · /read <file> to import one");
            return Ok(());
        }
        let last = self.store.setting("last_text")?.unwrap_or_default();
        let mut view = ToolView::new("texts")
            .accent(qe_core::view::Accent::Note)
            .result(format!("{} imported", texts.len()));
        for text in texts {
            let progress = self
                .store
                .reading_progress(text.id)?
                .map(|(paragraph, mode)| {
                    format!("{}/{} {}", paragraph + 1, text.paragraph_count(), mode.label())
                })
                .unwrap_or_else(|| format!("{} paragraphs", text.paragraph_count()));
            let current = if text.title.eq_ignore_ascii_case(&last) {
                " ·"
            } else {
                ""
            };
            view = view.kv(format!("{}{}", text.title, current), progress);
        }
        view = view.muted("· = last read · /read <title> resumes");
        self.push(Message::tool(view));
        Ok(())
    }

    /// 显示 / 隐藏译文。
    pub fn toggle_translation(&mut self) -> Result<()> {
        if self.reading.is_none() {
            self.note("not reading · /read to start");
            return Ok(());
        }
        self.show_translation = !self.show_translation;
        self.note(if self.show_translation {
            "translation: shown"
        } else {
            "translation: hidden"
        });
        self.render_reading()
    }

    /// 把当前段的**全部**候选词标为已标记（批量）。
    ///
    /// 这里不能复用 `reading_candidates()`：那个列表被 `MAX_CANDIDATES` 截断到
    /// 9 条（对应数字键），`/mark all` 用到它就会漏掉长段落后面的词。
    pub fn mark_all_candidates(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            self.note("not reading · /read to start");
            return Ok(());
        };
        let text_id = session.text.id;
        let paragraph = session.paragraph;
        let lemmas: Vec<String> = session
            .current_tokens()
            .into_iter()
            .filter(|token| token.is_word())
            .map(|token| token.lower)
            .collect();
        let mut count = 0u32;
        for lemma in lemmas {
            let already = self
                .reading
                .as_ref()
                .map(|session| session.is_marked(&lemma))
                .unwrap_or(false);
            if already {
                continue;
            }
            self.store.add_reading_mark(text_id, &lemma, paragraph)?;
            let word = self.store.word(&lemma).ok().flatten();
            if let Some(session) = self.reading.as_mut() {
                session.mark(&lemma, word);
            }
            count += 1;
        }
        let marked = self
            .reading
            .as_ref()
            .map(|session| session.marked.len())
            .unwrap_or(0);
        self.note(format!("marked {count} words · {marked} in this text"));
        self.render_reading()
    }

    /// 打开一篇文章并进入阅读。
    pub fn start_reading(&mut self, query: &str) -> Result<()> {
        let query = query.trim();
        let text = if query.is_empty() {
            // 不给参数：续读上次那篇。
            let previous = self
                .store
                .setting("last_text")?
                .filter(|title| !title.trim().is_empty());
            let Some(previous) = previous else {
                self.note("no text to resume · /read <file> to import one");
                return Ok(());
            };
            match self.store.text_by_title(&previous)? {
                Some(text) => text,
                None => {
                    self.note("the last text is gone · /read <file> to import one");
                    return Ok(());
                }
            }
        } else if std::path::Path::new(query).exists() {
            let text = load_text(std::path::Path::new(query))?;
            let id = self.store.save_text(&text)?;
            self.store.set_setting("last_text", &text.title)?;
            let mut text = text;
            text.id = id;
            self.note(format!("imported \"{}\" · {} paragraphs", text.title, text.paragraph_count()));
            text
        } else {
            match self.store.text_by_title(query)? {
                Some(text) => text,
                None => {
                    self.note(format!("no text \"{query}\" · /read <file> imports a file"));
                    return Ok(());
                }
            }
        };

        if text.paragraph_count() == 0 {
            self.note("that text has no paragraphs");
            return Ok(());
        }

        // 学习会话与阅读互斥：先收尾，避免两套队列抢同一批按键。
        if self.session.is_some() {
            self.finish_session()?;
            self.note("study session closed before reading");
        }

        let (paragraph, mode) = self
            .store
            .reading_progress(text.id)?
            .unwrap_or((0, ReadMode::default()));
        let paragraph = paragraph.min(text.paragraph_count().saturating_sub(1));
        let mut session = ReadingSession::new(text, mode, paragraph);
        for lemma in self.store.reading_marks(session.text.id)? {
            // 只恢复字符串标记；卡片正文按需再查。
            session.mark(&lemma, None);
        }
        self.reading = Some(session);
        self.render_reading()
    }

    /// 切换模式。全篇 ↔ 精读。
    pub fn set_reading_mode(&mut self, arg: &str) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            self.note("not reading · /read to start");
            return Ok(());
        };
        match arg.trim() {
            "" => {
                let next = match session.mode {
                    ReadMode::Full => ReadMode::Intensive,
                    ReadMode::Intensive => ReadMode::Full,
                };
                session.mode = next;
                self.note(format!("mode: {}", next.label()));
            }
            raw => match ReadMode::parse(raw) {
                Some(mode) => {
                    session.mode = mode;
                    self.note(format!("mode: {}", mode.label()));
                }
                None => {
                    self.note("usage: /mode full | intensive");
                    return Ok(());
                }
            },
        }
        let (id, paragraph, mode) = {
            let session = self.reading.as_ref().unwrap();
            (session.text.id, session.paragraph, session.mode)
        };
        self.store.save_reading_progress(id, paragraph, mode)?;
        self.render_reading()
    }

    /// 下一页。
    pub fn reading_next(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        if !session.next() {
            let last = session.total();
            self.note(format!("last paragraph ({last}/{last}) · /read to pick another text"));
            return Ok(());
        }
        let (id, paragraph, mode) = (session.text.id, session.paragraph, session.mode);
        self.store.save_reading_progress(id, paragraph, mode)?;
        self.render_reading()
    }

    /// 上一页。
    pub fn reading_prev(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        if !session.prev() {
            self.note("already at the first paragraph");
            return Ok(());
        }
        let (id, paragraph, mode) = (session.text.id, session.paragraph, session.mode);
        self.store.save_reading_progress(id, paragraph, mode)?;
        self.render_reading()
    }

    /// 跳到第 `index` 段（0 基）。
    pub fn reading_goto(&mut self, index: usize) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        if !session.go_to(index) {
            let total = session.total();
            self.note(format!("no paragraph {} of {total}", index + 1));
            return Ok(());
        }
        let (id, paragraph, mode) = (session.text.id, session.paragraph, session.mode);
        self.store.save_reading_progress(id, paragraph, mode)?;
        self.render_reading()
    }

    /// 标记或取消标记一个词。不带参数时标记「光标处」——这里取当前段的第一个
    /// 未标记候选，方便纯键盘操作。
    pub fn mark_word(&mut self, arg: &str) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            self.note("not reading · /read to start");
            return Ok(());
        };
        let lemma = if arg.trim().is_empty() {
            // 有选中词就标它；否则退回到「第一个还没标记的候选」。
            if let Some(focused) = session.focused_lemma() {
                focused
            } else {
                self.reading_candidates()
                    .into_iter()
                    .find(|candidate| !candidate.marked)
                    .map(|candidate| candidate.lemma)
                    .unwrap_or_default()
            }
        } else if arg.trim().eq_ignore_ascii_case("all") {
            return self.mark_all_candidates();
        } else {
            arg.trim().to_ascii_lowercase()
        };
        if lemma.is_empty() {
            self.note("nothing to mark · /mark <word>");
            return Ok(());
        }

        let text_id = session.text.id;
        let paragraph = session.paragraph;
        let already = session.is_marked(&lemma);
        if already {
            self.store.remove_reading_mark(text_id, &lemma)?;
        } else {
            self.store.add_reading_mark(text_id, &lemma, paragraph)?;
        }

        if let Some(session) = self.reading.as_mut() {
            if already {
                session.unmark(&lemma);
            } else {
                let word = self.store.word(&lemma).ok().flatten();
                session.mark(&lemma, word);
            }
        }
        self.note(if already {
            format!("unmarked {lemma}")
        } else {
            format!("marked {lemma}")
        });
        self.render_reading()
    }

    /// 打开当前段的第 `index` 个候选词（1 基）。
    pub fn open_word_at(&mut self, index: usize) -> Result<()> {
        let candidates = self.reading_candidates();
        let Some(candidate) = candidates.get(index.saturating_sub(1)) else {
            self.note(format!(
                "no word {index} · 1-{} available",
                candidates.len()
            ));
            return Ok(());
        };
        self.lookup(&candidate.lemma)?;
        if let Some(session) = self.reading.as_mut() {
            session.opened += 1;
            session.message = None; // 卡片沉到历史里，下次翻页再起一张
        }
        Ok(())
    }

    /// 把当前段的候选词全部列出来（不限 9 个）。
    pub fn list_reading_words(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            self.note("not reading · /read to start");
            return Ok(());
        };
        let tokens = session.current_tokens();
        if tokens.is_empty() {
            self.note("no words in this paragraph");
            return Ok(());
        }
        let mut view = ToolView::new("words")
            .badge(format!("{}/{}", session.paragraph + 1, session.total()))
            .accent(qe_core::view::Accent::Note)
            .result(format!("{} unique words", tokens.len()));
        for token in tokens {
            let marked = session.is_marked(&token.lower);
            view = view.kv(
                token.lower,
                if marked { "marked" } else { "" }.to_string(),
            );
        }
        self.push(Message::tool(view));
        Ok(())
    }

    /// 结束阅读，写日志。
    pub fn finish_reading(&mut self) -> Result<()> {
        let Some(session) = self.reading.take() else {
            return Ok(());
        };
        let now = chrono::Utc::now();
        let paragraphs = ((session.paragraph + 1) as u32).max(1);
        self.store.record_reading(&ReadingLog {
            text_id: session.text.id,
            started_at: session.started_at,
            ended_at: now,
            paragraphs,
            words_opened: session.opened,
            words_added: session.added,
        })?;
        self.store
            .save_reading_progress(session.text.id, session.paragraph, session.mode)?;
        self.store.set_setting("last_text", &session.text.title)?;

        let seconds = (now - session.started_at).num_seconds().clamp(0, 3600) as u32;
        let marked: Vec<String> = session.marked.iter().map(|m| m.lemma.clone()).collect();
        self.push(Message::tool(ToolView::reading_summary(
            &session.text.title,
            paragraphs,
            session.opened,
            session.added,
            &marked,
            seconds,
        )));
        self.refresh_status();
        Ok(())
    }

    /// 把选中的候选词收进当前词书。
    pub fn add_reading_word(&mut self, index: usize) -> Result<()> {
        let candidates = self.reading_candidates();
        let Some(candidate) = candidates.get(index.saturating_sub(1)) else {
            self.note("no such word in this paragraph");
            return Ok(());
        };
        let lemma = candidate.lemma.clone();
        self.add_word(&lemma)?;
        // `add_word` 会推一条消息；这里只累加计数，不重复输出。
        if let Some(session) = self.reading.as_mut() {
            session.added += 1;
            session.message = None;
        }
        Ok(())
    }

    /// 删除一篇已导入的文本，连同进度、标记与题目。
    ///
    /// 删除前要求标题精确匹配（不区分大小写）：`/drop` 是不可撤销的，
    /// 用「包含」匹配很容易误删同名开头的另一篇。
    pub fn drop_text(&mut self, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            self.note("usage: /drop <title> · /texts lists them");
            return Ok(());
        }
        let Some(text) = self
            .store
            .texts()?
            .into_iter()
            .find(|text| text.title.eq_ignore_ascii_case(title))
        else {
            self.note(format!("no text \"{title}\" · /texts lists them"));
            return Ok(());
        };

        // 正在读这一篇时先收尾，否则会话还指着已经不存在的行。
        let reading_this = self
            .reading
            .as_ref()
            .map(|session| session.text.id == text.id)
            .unwrap_or(false);
        if reading_this {
            self.finish_reading()?;
        }
        if self.store.setting("last_text")?.as_deref() == Some(text.title.as_str()) {
            // 记住的「上次读的」跟着消失的话，`/read` 无参会一直报错。
            let _ = self.store.set_setting("last_text", "");
        }

        self.store.delete_text(text.id)?;
        self.push(Message::tool(
            ToolView::new("drop")
                .accent(Accent::Warn)
                .result(text.title.clone())
                .muted("text, progress, marks and quizzes deleted"),
        ));
        self.refresh_status();
        Ok(())
    }

    /// 选中当前段第 `index` 个词（0 基），并重画卡片。
    pub fn select_word(&mut self, index: usize) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        if !session.focus_on(index) {
            return Ok(());
        }
        self.render_reading()
    }

    /// 选中第 `paragraph` 段的第 `index` 个词。
    ///
    /// 全篇模式下一屏里能看到好几段，点击落在哪一段都得选对：词序号只在段内
    /// 有效，所以点到别的段时先把阅读位置移过去（并落库），再选词。
    pub fn select_word_in(&mut self, paragraph: Option<usize>, index: usize) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        let Some(paragraph) = paragraph.filter(|&paragraph| paragraph != session.paragraph) else {
            return self.select_word(index);
        };
        if paragraph >= session.total() {
            return Ok(());
        }
        session.paragraph = paragraph;
        session.cursor = None;
        if !session.focus_on(index) {
            return Ok(());
        }
        let (id, paragraph, mode) = (session.text.id, session.paragraph, session.mode);
        self.store.save_reading_progress(id, paragraph, mode)?;
        self.render_reading()?;
        Ok(())
    }

    /// `,` / `.`：按词移动选中位置。`delta` 为 −1 时向前。
    pub fn move_focus(&mut self, delta: isize) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        if !session.focus_move(delta) {
            return Ok(());
        }
        self.render_reading()
    }

    pub fn clear_focus(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        if !session.clear_focus() {
            return Ok(());
        }
        self.render_reading()
    }

    /// 打开选中词的完整卡片（词根、例句、搭配都在那里）。
    pub fn open_focused(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            return Ok(());
        };
        let Some(lemma) = session.focused_lemma() else {
            self.note("no word selected · , and . move between words · click one too");
            return Ok(());
        };
        self.lookup(&lemma)?;
        if let Some(session) = self.reading.as_mut() {
            session.opened += 1;
            // 卡片沉到历史里，下次翻页再起一张。
            session.message = None;
        }
        Ok(())
    }

    /// 把选中的词收进词书。
    pub fn add_focused(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            self.note("not reading · /read to start");
            return Ok(());
        };
        let Some(lemma) = session.focused_lemma() else {
            self.note("no word selected · , and . move between words");
            return Ok(());
        };
        self.add_word(&lemma)?;
        if let Some(session) = self.reading.as_mut() {
            session.added += 1;
            session.message = None;
        }
        Ok(())
    }

    /// 开始这篇的测验。
    pub fn start_quiz(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            self.note("not reading · /read to start");
            return Ok(());
        };
        let text_id = session.text.id;
        let title = session.text.title.clone();
        let quizzes = self.store.quizzes(text_id)?;
        if quizzes.is_empty() {
            self.note(format!(
                "no questions for \"{title}\" · import a quiz file with /quiz <file>"
            ));
            return Ok(());
        }
        self.quizzes = quizzes;
        self.quiz_index = 0;
        self.quiz_answered = None;
        self.quiz_revealed = false;
        // 每轮从零分起算，否则第二轮会出现 `5/4 correct`。
        self.quiz_correct = 0;
        self.render_quiz()?;
        Ok(())
    }

    /// 回答当前题。`choice` 是 1 基选项号（选择题）或空（填空题）。
    pub fn answer_quiz(&mut self, choice: Option<usize>) -> Result<()> {
        let Some(quiz) = self.quizzes.get(self.quiz_index).cloned() else {
            self.note("no active quiz · /quiz to start");
            return Ok(());
        };
        if self.quiz_answered.is_some() || self.quiz_revealed {
            // 已答过：再按一次就翻到下一题。
            self.quiz_index += 1;
            self.quiz_answered = None;
            self.quiz_revealed = false;
            if self.quiz_index >= self.quizzes.len() {
                let correct = self.quiz_correct;
                let total = self.quizzes.len();
                self.quizzes.clear();
                self.note(format!("quiz done · {correct}/{total} correct"));
                return Ok(());
            }
            return self.render_quiz();
        }

        let Some(index) = choice.filter(|_| quiz.is_choice()) else {
            // 填空题没有输入框：只揭示答案，不判对错。写成 `false` 会把
            // `quiz_attempts` 污染成一片错误，统计里的正确率也就失去意义。
            self.quiz_revealed = true;
            return self.render_quiz();
        };

        let correct = quiz
            .choices
            .get(index.saturating_sub(1))
            .map(|picked| picked.trim().eq_ignore_ascii_case(quiz.answer.trim()))
            .unwrap_or(false);
        self.store.record_attempt(quiz.id, correct)?;
        if correct {
            self.quiz_correct += 1;
        }
        self.quiz_answered = Some(correct);
        self.render_quiz()
    }

    fn render_quiz(&mut self) -> Result<()> {
        let Some(quiz) = self.quizzes.get(self.quiz_index).cloned() else {
            return Ok(());
        };
        let view = ToolView::quiz_card(
            &quiz,
            self.quiz_index,
            self.quizzes.len(),
            self.quiz_answered,
            self.quiz_revealed,
        );
        self.push(Message::tool(view));
        Ok(())
    }

    /// 导入测验文件：TSV `paragraph<TAB>prompt<TAB>answer<TAB>source[<TAB>choices]`。
    pub fn import_quiz(&mut self, path: &str) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            self.note("open a text first · /read <file>");
            return Ok(());
        };
        let text_id = session.text.id;
        let raw = std::fs::read_to_string(path.trim())?;
        let mut added = 0u32;
        let mut skipped = 0u32;
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 4 {
                skipped += 1;
                continue;
            }
            let paragraph = cols[0].trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
            let choices: Vec<String> = cols
                .get(4)
                .map(|raw| {
                    raw.split('|')
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let mut quiz = Quiz::new(cols[1].trim(), cols[2].trim())
                .at_paragraph(paragraph)
                .with_source(cols[3].trim());
            if !choices.is_empty() {
                quiz = quiz.choices(choices);
            }
            quiz.text_id = text_id;
            if self.store.add_quiz(&quiz)? {
                added += 1;
            } else {
                skipped += 1;
            }
        }
        self.note(format!("{added} questions added · {skipped} skipped"));
        Ok(())
    }
}

/// 卡片上显示的一行释义：词性 + 第一条常用义。
fn gloss_of(word: &Word) -> String {
    let parsed = word.senses();
    let Some(sense) = parsed
        .senses
        .iter()
        .find(|sense| sense.label.is_empty() && !sense.text.trim().is_empty())
    else {
        return String::new();
    };
    if sense.pos.is_empty() {
        sense.text.trim().to_string()
    } else {
        format!("{} {}", sense.pos.trim(), sense.text.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("article.txt");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        (dir, path)
    }

    #[test]
    fn loads_paragraphs_and_title_from_a_plain_file() {
        let (_dir, path) = temp_file("First one.\n\nSecond one.\n");
        let text = load_text(&path).unwrap();
        assert_eq!(text.title, "article");
        assert_eq!(text.paragraph_count(), 2);
        assert!(text.translation.is_empty());
    }

    #[test]
    fn reads_header_metadata() {
        let (_dir, path) = temp_file(
            "# title: Animal Farm\n# source: Project Gutenberg #74\n# license: public domain\n\nAll animals are equal.\n",
        );
        let text = load_text(&path).unwrap();
        assert_eq!(text.title, "Animal Farm");
        assert_eq!(text.source.as_deref(), Some("Project Gutenberg #74"));
        assert_eq!(text.license.as_deref(), Some("public domain"));
        assert_eq!(text.paragraph_count(), 1);
    }

    #[test]
    fn splits_a_bilingual_file_on_the_rule() {
        let (_dir, path) = temp_file(
            "One.\n\nTwo.\n\n---\n\n一。\n\n二。\n",
        );
        let text = load_text(&path).unwrap();
        assert_eq!(text.paragraph_count(), 2);
        assert_eq!(text.translation_at(0), Some("一。"));
        assert_eq!(text.translation_at(1), Some("二。"));
    }

    #[test]
    fn gloss_shows_pos_then_meaning() {
        let word = Word::new("tyranny", "-K1\nn. 专制, 暴政");
        assert_eq!(gloss_of(&word), "n. 专制, 暴政");
    }
}
