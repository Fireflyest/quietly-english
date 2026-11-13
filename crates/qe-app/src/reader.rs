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
    let mut book = None;

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
            if let Some(rest) = trimmed.strip_prefix("# book:") {
                book = Some(rest.trim().to_string());
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

    // 没有 `# book:` 时，用来源归书：同源的多篇读起来就是一本。
    // 都没有就留作散篇，不硬凑一个「来源未知」的书。
    if book.is_none() {
        book = source.clone();
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
        book,
        rank: 0,
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

/// 从一个路径导入的阅读材料。
pub struct ImportedReading {
    /// 归到哪本书（EPUB 用 `dc:title`，TXT 用 `# book:`）。
    pub book: Option<String>,
    /// 拆出来的篇目；TXT 只有一篇，EPUB 一章一篇。
    pub texts: Vec<Text>,
}

/// 读一个文件：EPUB 按章拆开，其它按纯文本读成一篇。
///
/// 两种格式最终都产出同一种 `Text`，所以导入、归书、进度都不区分来源格式。
pub fn load_reading(path: &std::path::Path) -> Result<ImportedReading> {
    if crate::epub::is_epub(path) {
        let epub = crate::epub::load(path)?;
        return Ok(ImportedReading {
            book: Some(epub.title),
            texts: epub.texts,
        });
    }
    let text = load_text(path)?;
    Ok(ImportedReading {
        book: text.book.clone(),
        texts: vec![text],
    })
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
            // 全篇模式把整篇画成一条消息，重画时保持当前滚动位置：选词、切译文
            // 只换内容不换位置。真的换了段落（翻页、跳段）由那几个入口自己请求
            // `reveal_anchor`，所以这里只要求视野别因为内容变长/变短而滑动。
            self.pin_view = true;
        } else {
            self.scroll_back = 0;
        }
        self.refresh_status();
        Ok(())
    }

    /// 列出已导入的文本：书按本汇总，散篇逐条列出。
    ///
    /// 一本书动辄几十章，逐章列出来会把别的书挤出屏幕；想看某一章可以用
    /// `/library` 看进度、`/read <slug>` 打开下一篇。
    pub fn list_texts(&mut self) -> Result<()> {
        let texts = self.store.texts()?;
        if texts.is_empty() {
            self.note("no texts yet · /read <file> to import one");
            return Ok(());
        }
        let books = self.store.text_books()?;
        let last = self.store.setting("last_text")?.unwrap_or_default();
        let mut view = ToolView::new("texts")
            .accent(qe_core::view::Accent::Note)
            .result(format!(
                "{} books · {} texts",
                books.len(),
                texts.len()
            ));
        for book in &books {
            let progress = self.store.text_book_progress(&book.slug)?;
            let current = progress
                .next_title
                .as_deref()
                .map(|next| next.eq_ignore_ascii_case(&last))
                .unwrap_or(false);
            view = view.kv(
                format!("{}{}", book.slug, if current { " ·" } else { "" }),
                format!(
                    "{} · {}/{} · next {}",
                    book.name,
                    progress.finished,
                    progress.total,
                    progress.next_title.as_deref().unwrap_or("-")
                ),
            );
        }
        for text in texts.iter().filter(|text| text.book.is_none()) {
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
        view = view.muted("· = book of the last read · /read <slug> continues it");
        self.push(Message::tool(view));
        Ok(())
    }

    /// 列出阅读书：每本书的进度与下一篇。
    pub fn list_library(&mut self) -> Result<()> {
        let books = self.store.text_books()?;
        let texts = self.store.texts()?;
        if books.is_empty() && texts.is_empty() {
            self.note("no texts yet · /read <file> imports one");
            return Ok(());
        }

        let loose = texts.iter().filter(|text| text.book.is_none()).count();
        let mut view = ToolView::new("library")
            .accent(qe_core::view::Accent::Note)
            .result(format!(
                "{} books · {} texts{}",
                books.len(),
                texts.len(),
                if loose > 0 {
                    format!(" · {loose} on their own")
                } else {
                    String::new()
                }
            ));

        let last = self.store.setting("last_text")?.unwrap_or_default();
        for book in &books {
            let progress = self.store.text_book_progress(&book.slug)?;
            let marker = if progress.is_done() { "done" } else { "next" };
            let next = progress.next_title.clone().unwrap_or_default();
            let current = next.eq_ignore_ascii_case(&last);
            // key 是可输入的那一半：`/read <slug>` 直接续读，不用去猜书名。
            view = view.kv(
                format!("{}{}", book.slug, if current { " ·" } else { "" }),
                format!(
                    "{} · {}/{} · {marker} {next}",
                    book.name, progress.finished, progress.total
                ),
            );
        }
        if loose > 0 {
            view = view.muted(format!("{loose} texts without a book · /texts lists them"));
        }
        view = view.muted("/read <slug> continues it · /read <file> imports txt/epub");
        self.push(Message::tool(view));
        Ok(())
    }

    /// 显示 / 隐藏译文；没有译文时按需机翻**当前段**。
    ///
    /// 按段翻而不是整篇：免费额度按字符算（MyMemory 匿名 5000 字符/天），
    /// 一篇 Guardian 长读四五千词，一次性提交就把一个月的额度花光了。
    pub fn toggle_translation(&mut self) -> Result<()> {
        let Some(session) = self.reading.as_ref() else {
            self.note("not reading · /read to start");
            return Ok(());
        };
        let paragraph = session.paragraph;
        let text_id = session.text.id;

        // 已经显示着：关掉就好，不发任何请求。
        if self.show_translation {
            self.show_translation = false;
            return self.render_reading();
        }
        self.show_translation = true;

        // 官方译文（TED / BBC）或这次会话里已经翻过的段，直接显示。
        if session.translation_at(paragraph).is_some() {
            return self.render_reading();
        }
        // 以前翻过的段落，不再花第二次额度。
        if let Some(cached) = self.store.text_translation(text_id, paragraph)? {
            if let Some(session) = self.reading.as_mut() {
                session.add_translation(paragraph, &cached);
            }
            return self.render_reading();
        }
        self.spawn_translation(text_id, paragraph)
    }

    /// 后台翻一段。网络请求不能卡在界面线程里（一段要请求一到几次）。
    fn spawn_translation(&mut self, text_id: i64, paragraph: usize) -> Result<()> {
        let Some(settings) = crate::translator::settings() else {
            self.show_translation = false;
            self.note("machine translation is off · unset QUIETLY_TRANSLATE to use it");
            return Ok(());
        };
        if self.translation_fetch.is_some() {
            self.note("translating another paragraph · one moment");
            return Ok(());
        }
        let Some(body) = self
            .reading
            .as_ref()
            .and_then(|session| session.text.paragraphs().get(paragraph).copied())
            .map(str::to_string)
        else {
            self.show_translation = false;
            self.note("nothing to translate here");
            return Ok(());
        };
        let detail = settings.detail();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = crate::translator::translate(&settings, &body)
                .map_err(|err| format!("{err:#}"));
            let _ = sender.send((paragraph, outcome));
        });
        self.translation_fetch = Some((text_id, receiver));
        self.note(format!("translating paragraph {} · {detail}", paragraph + 1));
        Ok(())
    }

    /// 翻译线程有结果了就补进当前会话；每帧调一次。
    pub fn poll_translation(&mut self) {
        let Some((text_id, receiver)) = self.translation_fetch.as_ref() else {
            return;
        };
        let text_id = *text_id;
        match receiver.try_recv() {
            Ok((paragraph, Ok(translation))) => {
                self.translation_fetch = None;
                let _ = self.store.set_text_translation(
                    text_id,
                    paragraph,
                    &translation.text,
                    translation.provider,
                );
                if let Some(session) = self.reading.as_mut() {
                    if session.text.id == text_id {
                        session.add_translation(paragraph, &translation.text);
                    }
                }
                let _ = self.render_reading();
            }
            Ok((_, Err(error))) => {
                self.translation_fetch = None;
                // 没译出来就别留一个「已开启但没有译文」的状态。
                self.show_translation = false;
                self.note(format!("translation failed · {error}"));
                let _ = self.render_reading();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.translation_fetch = None;
            }
        }
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
    ///
    /// 参数可以是文件路径、篇名，或一本阅读书的 slug/书名——最后一种会接着读
    /// 书里下一篇没读完的文章，这就是「按书推进」。
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
            // TXT 一篇、EPUB 一章一篇，导入后从第一篇开始读。
            let imported = load_reading(std::path::Path::new(query))?;
            let mut first: Option<Text> = None;
            let mut paragraphs = 0usize;
            let mut count = 0usize;
            for mut text in imported.texts {
                paragraphs += text.paragraph_count();
                let id = self.store.save_text(&text)?;
                text.id = id;
                count += 1;
                first.get_or_insert(text);
            }
            let Some(text) = first else {
                self.note("that file has no paragraphs");
                return Ok(());
            };
            self.store.set_setting("last_text", &text.title)?;
            let book = imported
                .book
                .as_ref()
                .map(|book| format!(" · book: {book}"))
                .unwrap_or_default();
            if count > 1 {
                self.note(format!(
                    "imported {count} chapters · {paragraphs} paragraphs{book} · /library"
                ));
            } else {
                self.note(format!(
                    "imported \"{}\" · {paragraphs} paragraphs{book}",
                    text.title
                ));
            }
            text
        } else if let Some(text) = self.store.text_by_title(query)? {
            text
        } else if let Some(text) = self.book_next_text(query)? {
            text
        } else {
            let books: Vec<String> = self
                .store
                .text_books()?
                .into_iter()
                .map(|book| book.slug)
                .collect();
            self.note(if books.is_empty() {
                format!(
                    "no text or book \"{query}\" · /read <file> imports a file (txt or epub)"
                )
            } else {
                format!(
                    "no text or book \"{query}\" · books: {} · /read <file> imports",
                    books.join(", ")
                )
            });
            return Ok(());
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
        // 已经在一篇里，又被要求打开另一篇（换书、书内续读）：先把当前这篇收尾，
        // 否则阅读时长和段落数会算到下一篇头上。
        if self.reading.is_some() {
            self.finish_reading()?;
        }

        let (paragraph, mode) = self
            .store
            .reading_progress(text.id)?
            .unwrap_or((0, ReadMode::default()));
        let paragraph = paragraph.min(text.paragraph_count().saturating_sub(1));
        let mut session = ReadingSession::new(text, mode, paragraph);
        // 翻译显示是当前文章的阅读偏好，不应延续到下一篇。
        self.show_translation = false;
        self.translation_fetch = None;
        for lemma in self.store.reading_marks(session.text.id)? {
            // 只恢复字符串标记；卡片正文按需再查。
            session.mark(&lemma, None);
        }
        self.store.set_setting("last_text", &session.text.title)?;
        self.reading = Some(session);
        self.reveal_anchor = true;
        self.render_reading()
    }

    /// 一本书里下一篇要读的文章：按 slug 或书名匹配，然后取进度里的 `next_title`。
    ///
    /// 全部读完时返回最后一篇（复习用），并在提示里说清楚。
    fn book_next_text(&mut self, query: &str) -> Result<Option<Text>> {
        let slug = qe_core::reading::book_slug(query);
        let books = self.store.text_books()?;
        let Some(book) = books.into_iter().find(|book| {
            book.slug.eq_ignore_ascii_case(&slug)
                || book.name.eq_ignore_ascii_case(query)
                || qe_core::reading::book_slug(&book.name).eq_ignore_ascii_case(&slug)
        }) else {
            return Ok(None);
        };
        let progress = self.store.text_book_progress(&book.slug)?;
        let Some(title) = progress.next_title.clone() else {
            self.note(format!("book \"{}\" has no texts", book.name));
            return Ok(None);
        };
        let text = self.store.text_by_title(&title)?;
        let position = if progress.is_done() {
            format!("{} finished · re-reading the last one", book.name)
        } else {
            format!(
                "{} · {} {}/{}",
                book.name,
                if progress.started == 0 { "start" } else { "next" },
                (progress.finished + 1).min(progress.total),
                progress.total
            )
        };
        self.note(position);
        Ok(text)
    }

    /// 读完一篇后接着读书里的下一篇；散篇或没有下一篇时返回 `false`。
    fn advance_within_book(&mut self) -> Result<bool> {
        let Some(session) = self.reading.as_ref() else {
            return Ok(false);
        };
        let Some(book) = session.text.book.clone() else {
            return Ok(false);
        };
        let current = session.text.title.clone();
        let slug = qe_core::reading::book_slug(&book);
        let progress = self.store.text_book_progress(&slug)?;
        let Some(next) = progress.next_title else {
            return Ok(false);
        };
        if next.eq_ignore_ascii_case(&current) {
            self.note(format!(
                "book \"{book}\" finished · {}/{} · /read {slug} re-reads it",
                progress.finished, progress.total
            ));
            return Ok(false);
        }
        self.start_reading(&next)?;
        Ok(true)
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
        self.reveal_anchor = true;
        self.render_reading()
    }

    /// 下一页。读到最后一段时，书里还有下一篇就自动接着读。
    pub fn reading_next(&mut self) -> Result<()> {
        let advanced = {
            let Some(session) = self.reading.as_mut() else {
                return Ok(());
            };
            if session.next() {
                Some((session.text.id, session.paragraph, session.mode))
            } else {
                None
            }
        };
        if let Some((id, paragraph, mode)) = advanced {
            self.store.save_reading_progress(id, paragraph, mode)?;
            self.reveal_anchor = true;
            return self.render_reading();
        }

        // 到头了：书里还有下一篇就接上，这是「按书推进」的核心。
        let Some((last, has_book)) = self
            .reading
            .as_ref()
            .map(|session| (session.total(), session.text.book.is_some()))
        else {
            return Ok(());
        };
        if !has_book {
            self.note(format!("last paragraph ({last}/{last}) · /read to pick another text"));
            return Ok(());
        }
        self.note(format!("text finished ({last}/{last})"));
        self.advance_within_book()?;
        Ok(())
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
        self.reveal_anchor = true;
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
        self.reveal_anchor = true;
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

    /// 选中第 `paragraph` 段的第 `index` 个词。
    ///
    /// 全篇模式下一屏里能看到好几段，点击落在哪一段都得选对：词序号只在段内
    /// 有效，所以选中词要记住它所在的是哪一段。但阅读位置和进度**不动**——
    /// 点一下词就把读者的进度挪走（连视野一起跳）是最烦人的那种行为。
    pub fn select_word_in(&mut self, paragraph: Option<usize>, index: usize) -> Result<()> {
        let Some(session) = self.reading.as_mut() else {
            return Ok(());
        };
        let paragraph = paragraph.unwrap_or(session.paragraph);
        if paragraph >= session.total() {
            return Ok(());
        }
        if !session.focus_in(paragraph, index) {
            return Ok(());
        }
        self.render_reading()
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
