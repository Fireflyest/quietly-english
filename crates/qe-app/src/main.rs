//! 命令行入口：默认进入 TUI，另外提供导入、路径查询等一次性命令。

mod app;
mod cli;
mod command;
mod editor;
mod epub;
mod reader;
mod session;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use qe_core::Store;
use qe_store::SqliteStore;

fn main() {
    if let Err(err) = run() {
        eprintln!("quietly: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut db_override: Option<PathBuf> = None;
    let mut command: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("quietly {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--db" => {
                let value = args.next().context("--db 需要一个路径")?;
                db_override = Some(PathBuf::from(value));
            }
            flag if flag.starts_with('-') => {
                // 子命令自带开关（如 `dict --merge`），交回给它们自己解析；
                // 顶层出现未知开关才算用法错误。
                if command.is_some() {
                    rest.push(flag.to_string());
                } else {
                    bail!("未知参数 {flag}（用 --help 查看用法）");
                }
            }
            other => {
                if command.is_none() {
                    command = Some(other.to_string());
                } else {
                    rest.push(other.to_string());
                }
            }
        }
    }

    let mut store = match &db_override {
        Some(path) => SqliteStore::open_at(path)?,
        None => SqliteStore::open_default()?,
    };

    match command.as_deref() {
        None => {
            cli::install_panic_hook();
            let mut terminal = cli::TerminalSession::new()?;
            let app = app::App::new(Box::new(store))?;
            app.run(&mut terminal)
        }
        Some("db") => {
            // 数据分两个文件，先说清楚各自是什么、多大。
            let core = store.path().to_path_buf();
            let words = store.words_path().to_path_buf();
            let size = |path: &std::path::Path| {
                std::fs::metadata(path)
                    .map(|meta| format!("{:.1} MB", meta.len() as f64 / 1_048_576.0))
                    .unwrap_or_else(|_| "missing".to_string())
            };
            println!("study   {}  {}", size(&core), core.display());
            println!("words   {}  {}", size(&words), words.display());
            println!();
            println!("study db 里是词书、卡片、复习流水、阅读与设置，几 MB，适合进 git。");
            println!("words db 是词典，几百 MB，随时能用 quietly dict / etymology 重建。");
            Ok(())
        }
        Some("import") => {
            let path = rest
                .first()
                .context("用法：quietly import <文件路径>")?;
            println!("importing {} …", path);
            let report = store.import_file(std::path::Path::new(path))?;
            println!("{}", cli::format_report(&report));
            println!("database: {}", store.path().display());
            Ok(())
        }
        Some("dict") => {
            // `--merge` layers this dictionary on top of what is already stored,
            // which is how you combine ECDICT + 21世纪 + 朗道 without losing data.
            let merge = rest.iter().any(|arg| arg == "--merge" || arg == "-m");
            let path = rest
                .iter()
                .find(|arg| !arg.starts_with('-'))
                .context("用法：quietly dict <dict.ifo> [--merge]")?;
            println!(
                "reading StarDict from {path} …{}",
                if merge { " (merging into existing senses)" } else { "" }
            );
            let started = std::time::Instant::now();
            let report =
                store.import_stardict_with(std::path::Path::new(path), merge, &mut |count| {
                    eprintln!("  {count} entries …");
                })?;
            println!(
                "added {} · updated {} · {}senses grown · {:.1}s",
                report.inserted,
                report.updated,
                if merge {
                    format!("{} ", report.grown)
                } else {
                    String::new()
                },
                started.elapsed().as_secs_f32()
            );
            if report.phrases > 0 {
                println!(
                    "{} phrases extracted · {} translated",
                    report.phrases, report.phrases_translated
                );
            }
            println!("database: {}", store.path().display());
            Ok(())
        }
        Some("examples") => {
            let cmn = rest
                .first()
                .context("用法：quietly examples <cmn_sentences.tsv> <eng_sentences.tsv> <links.csv>")?;
            let eng = rest
                .get(1)
                .context("缺少 eng_sentences.tsv")?;
            let links = rest
                .get(2)
                .context("缺少 links.csv")?;

            println!("collecting Tatoeba pairs for deck words …");
            let started = std::time::Instant::now();
            let vocabulary: std::collections::HashSet<String> =
                store.deck_lemmas()?.into_iter().collect();
            println!("candidate words: {}", vocabulary.len());

            let (examples, stats) = qe_store::tatoeba::collect_examples(
                std::path::Path::new(cmn),
                std::path::Path::new(eng),
                std::path::Path::new(links),
                &vocabulary,
                |stage, done| eprintln!("  {stage}: {done}"),
            )?;
            println!(
                "pairs seen {} · examples minted {} · words covered {} · {:.1}s",
                stats.pairs,
                stats.attached,
                stats.words,
                started.elapsed().as_secs_f32()
            );

            let added = store.add_examples(&examples)?;
            println!("attached {added} examples to existing words");
            println!("database: {}", store.path().display());
            Ok(())
        }
        Some("phrases") | Some("collocations") => {
            // `quietly phrases --translate` 不给文件时就只补缺失的中文释义。
            if rest.iter().any(|arg| arg == "--translate" || arg == "-t") {
                let filled = store.resolve_collocation_glosses()?;
                println!("{filled} phrases translated");
                println!("database: {}", store.path().display());
                return Ok(());
            }
            let path = rest
                .first()
                .context("用法：quietly phrases <lemma,phrase,gloss.tsv> | --translate")?;
            println!("reading phrases from {path} …");
            let (words, added) = store.import_phrases(std::path::Path::new(path))?;
            println!("{added} phrases added across {words} words");
            let filled = store.resolve_collocation_glosses()?;
            println!("{filled} phrases translated");
            println!("database: {}", store.path().display());
            Ok(())
        }
        Some("phrase") => {
            // 单条补录：`quietly phrase sake for the sake of = 为了`
            let spec = rest.join(" ").trim().to_string();
            let (head, gloss) = spec
                .split_once('=')
                .context("用法：quietly phrase <词> <搭配> = <释义>")?;
            let mut parts = head.split_whitespace();
            let lemma = parts.next().context("缺少词条，例如：quietly phrase sake for the sake of = 为了")?;
            let phrase = parts.collect::<Vec<_>>().join(" ");
            if phrase.is_empty() {
                bail!("缺少搭配，例如：quietly phrase sake for the sake of = 为了");
            }
            let added = store.add_collocation(lemma, &phrase, gloss.trim())?;
            if added {
                println!("added · {lemma} · {phrase} -> {}", gloss.trim());
            } else {
                println!("already on file · {lemma} · {phrase}");
            }
            Ok(())
        }
        Some("etymology") | Some("roots") => {
            let path = rest
                .first()
                .context("用法：quietly etymology <kaikki.jsonl | roots.tsv>")?;
            println!("reading etymology from {path} …");
            if path.to_ascii_lowercase().ends_with(".jsonl") {
                println!("(a full kaikki dump is ~1.5M lines; this can take a few minutes)");
            }
            let started = std::time::Instant::now();
            let (words, morphemes) = store.import_etymology_with(
                std::path::Path::new(path),
                &mut |scanned| {
                    eprintln!("  scanned {scanned} lines …");
                },
            )?;
            println!(
                "{words} words enriched · {morphemes} morphemes attached · {:.1}s",
                started.elapsed().as_secs_f32()
            );
            println!("database: {}", store.path().display());
            Ok(())
        }
        Some("where") | Some("path") => {
            println!("{}", store.path().display());
            Ok(())
        }
        Some("word") | Some("lookup") => {
            // 一次性查看词条，专供核对导入 / 合并结果，不进 TUI。
            let lemma = rest.first().context("用法：quietly word <lemma>")?;
            let Some(word) = store.word(lemma)? else {
                println!("没有收录 {lemma}");
                return Ok(());
            };
            println!("{}  {}", word.lemma, word.phonetic.as_deref().unwrap_or(""));
            let parsed = word.senses();
            for sense in &parsed.senses {
                // 词性（或领域标签）在前，译文在后，不编号：编号只有在这一行
                // 需要被引用时才有用，而查词不需要指代某条义项。
                let label = if sense.label.is_empty() {
                    sense.pos.clone()
                } else {
                    format!("[{}]", sense.label)
                };
                println!("{}{}", pad_label(&label), sense.text);
            }
            if let Some(forms) = &parsed.forms {
                println!("forms: {forms}");
            }
            for colloc in &word.collocations {
                if colloc.gloss.trim().is_empty() {
                    println!("phrase: {}", colloc.phrase);
                } else {
                    println!("phrase: {}  ->  {}", colloc.phrase, colloc.gloss);
                }
            }
            for gloss in &word.glosses {
                println!("gloss: {}", gloss.text);
            }
            for root in word.roots() {
                println!("root: {:<8} {:<6} {}", root.part, root.kind.as_str(), root.gloss);
            }
            for example in &word.examples {
                println!(
                    "eg: {} — {}",
                    example.en,
                    example.zh.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        Some("books") => {
            let books = store.list_books()?;
            if books.is_empty() {
                println!("（没有词书，先运行 quietly import <文件>）");
            }
            for book in books {
                let progress = store.progress(book.id)?;
                println!(
                    "{:<10} {:<20} {:>6}/{:<6} due {}",
                    book.slug, book.name, progress.learned, progress.total, progress.due
                );
            }
            Ok(())
        }
        Some("stats") => {
            let daily = store.daily(qe_core::time::today())?;
            println!("{}", cli::format_stats(&daily, store.streak()?));
            let reading = store.daily_reading(qe_core::time::today())?;
            if !reading.is_empty() {
                println!(
                    "reading: {} paragraphs · {} min · {} words opened · {} added",
                    reading.paragraphs,
                    reading.minutes(),
                    reading.words_opened,
                    reading.words_added
                );
                if reading.quizzes > 0 {
                    println!("quiz: {}/{} correct", reading.correct, reading.quizzes);
                }
            }
            Ok(())
        }
        Some("read") => {
            // `quietly read <file> [--book <name>]` 导入文本或 EPUB；不给文件就列出已导入的。
            let file = rest.iter().find(|arg| !arg.starts_with('-'));
            match file {
                Some(path) => {
                    let book_override = flag_value(&rest, "--book");
                    let mut imported =
                        reader::load_reading(std::path::Path::new(path))?;
                    if let Some(book) = book_override {
                        // `--book` 优先于文件头：批量导入同一本书时不必改每个文件头。
                        imported.book = Some(book.clone());
                        for text in &mut imported.texts {
                            text.book = Some(book.clone());
                        }
                    }

                    let mut first_title = String::new();
                    let mut paragraphs = 0usize;
                    let mut bilingual = false;
                    let mut source = None;
                    let mut license = None;
                    for text in &imported.texts {
                        store.save_text(text)?;
                        if first_title.is_empty() {
                            first_title = text.title.clone();
                            paragraphs = text.paragraph_count();
                            bilingual = !text.translation.trim().is_empty();
                            source = text.source.clone();
                            license = text.license.clone();
                        }
                    }

                    let count = imported.texts.len();
                    if count == 1 {
                        println!(
                            "imported \"{}\" · {paragraphs} paragraphs{}",
                            first_title,
                            if bilingual { " · bilingual" } else { "" }
                        );
                    } else {
                        // EPUB：列出书名而不是第一章的标题，避免误导。
                        let book_name = imported
                            .book
                            .clone()
                            .unwrap_or_else(|| first_title.clone());
                        println!(
                            "imported {count} chapters from \"{book_name}\" · {paragraphs} paragraphs in the first chapter"
                        );
                    }
                    if let Some(book) = &imported.book {
                        println!("book: {book} ({})", qe_core::reading::book_slug(book));
                    }
                    if let Some(source) = &source {
                        println!("source: {source}");
                    }
                    if let Some(license) = &license {
                        println!("license: {license}");
                    }
                    let open = imported
                        .book
                        .as_ref()
                        .map(|book| qe_core::reading::book_slug(book))
                        .unwrap_or_else(|| first_title.clone());
                    println!("open it with /read {open}");
                    Ok(())
                }
                None => {
                    let books = store.text_books()?;
                    for book in &books {
                        let progress = store.text_book_progress(&book.slug)?;
                        println!(
                            "{:<24} {}/{} texts · next: {}",
                            book.name,
                            progress.finished,
                            progress.total,
                            progress.next_title.as_deref().unwrap_or("-")
                        );
                    }
                    let loose: Vec<_> = store
                        .texts()?
                        .into_iter()
                        .filter(|text| text.book.is_none())
                        .collect();
                    if books.is_empty() && loose.is_empty() {
                        println!("（没有文本，先运行 quietly read <文件>）");
                    }
                    if !loose.is_empty() {
                        if !books.is_empty() {
                            println!();
                        }
                        println!(
                            "{} text{} without a book:",
                            loose.len(),
                            if loose.len() == 1 { "" } else { "s" }
                        );
                        for text in loose {
                            let progress = store
                                .reading_progress(text.id)?
                                .map(|(paragraph, mode)| {
                                    format!(
                                        "{}/{} {}",
                                        paragraph + 1,
                                        text.paragraph_count(),
                                        mode.label()
                                    )
                                })
                                .unwrap_or_else(|| {
                                    format!("{} paragraphs", text.paragraph_count())
                                });
                            println!("  {:<26} {}", text.title, progress);
                        }
                    }
                    Ok(())
                }
            }
        }
        Some("library") => {
            // `quietly library`：阅读书与进度。
            let books = store.text_books()?;
            if books.is_empty() {
                println!("（没有阅读书，先运行 quietly read <文件> --book <书名>）");
            }
            for book in &books {
                let progress = store.text_book_progress(&book.slug)?;
                println!(
                    "{:<10} {:<24} {:>3}/{:<3} next: {}",
                    book.slug,
                    book.name,
                    progress.finished,
                    progress.total,
                    progress.next_title.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        Some("drop") | Some("delete-text") => {
            // `quietly drop <text>`：删掉一篇及其进度、标记、题目。
            let title = rest
                .iter()
                .find(|arg| !arg.starts_with('-'))
                .context("用法：quietly drop <text>")?;
            let text = store
                .text_by_title(title)?
                .with_context(|| format!("没有这篇文本：{title}"))?;
            store.delete_text(text.id)?;
            if store.setting("last_text")?.as_deref() == Some(text.title.as_str()) {
                store.set_setting("last_text", "")?;
            }
            println!("deleted \"{}\" · {} paragraphs", text.title, text.paragraph_count());
            Ok(())
        }
        Some("quiz") => {
            // `quietly quiz <text-title> <file.tsv>` 往某篇里导入题目。
            let mut args = rest.iter().filter(|arg| !arg.starts_with('-'));
            let title = args.next().context("用法：quietly quiz <text> <file.tsv>")?;
            let path = args.next().context("用法：quietly quiz <text> <file.tsv>")?;
            let text = store
                .text_by_title(title)?
                .with_context(|| format!("没有这篇文本：{title}"))?;
            let raw = std::fs::read_to_string(path)?;
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
                let paragraph = cols[0]
                    .trim()
                    .parse::<usize>()
                    .unwrap_or(1)
                    .saturating_sub(1);
                let choices: Vec<String> = cols
                    .get(4)
                    .map(|raw| {
                        raw.split('|')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let mut quiz = qe_core::model::Quiz::new(cols[1].trim(), cols[2].trim())
                    .at_paragraph(paragraph)
                    .with_source(cols[3].trim());
                if !choices.is_empty() {
                    quiz = quiz.choices(choices);
                }
                quiz.text_id = text.id;
                if store.add_quiz(&quiz)? {
                    added += 1;
                } else {
                    skipped += 1;
                }
            }
            println!("{added} questions added · {skipped} skipped");
            println!("database: {}", store.path().display());
            Ok(())
        }
        Some("doctor") => {
            // 引用以 lemma 为准，所以换词典不会错位；只有词典里没有这个词才算悬空。
            let fix = rest.iter().any(|arg| arg == "--fix" || arg == "-f");
            let report = store.doctor(fix)?;
            println!("words     {} 条词条", report.words);
            println!("decks     {}", report.decks.len());
            for (slug, total) in &report.decks {
                println!("  {:<10} {:>6}", slug, total);
            }
            if report.is_clean() {
                println!("\n引用完整：词书、卡片、复习流水都能在词典里找到对应词条。");
            } else {
                println!("\n{} 条悬空引用：", report.orphans());
                if !report.missing_in_decks.is_empty() {
                    println!(
                        "  词书缺词 {} 条{}",
                        report.missing_in_decks.len(),
                        if report.missing_in_decks.len() > 10 {
                            "（只列前 10）"
                        } else {
                            ""
                        }
                    );
                    for (slug, lemma) in report.missing_in_decks.iter().take(10) {
                        println!("    {slug:<10} {lemma}");
                    }
                }
                if !report.missing_cards.is_empty() {
                    println!("  卡片缺词 {}: {}", report.missing_cards.len(),
                        report.missing_cards.iter().take(10).cloned().collect::<Vec<_>>().join(", "));
                }
                if report.missing_reviews > 0 {
                    println!("  复习流水缺词 {} 条", report.missing_reviews);
                }
                if fix {
                    if report.fixed > 0 {
                        println!("\n已清理 {} 条悬空引用。", report.fixed);
                    }
                } else if report.words == 0 {
                    println!("\n词典库是空的，先导入词典再考虑 --fix。");
                } else {
                    println!("\n用 quietly doctor --fix 清掉这些行。");
                }
            }
            if !report.empty_decks.is_empty() {
                println!("空词书: {}", report.empty_decks.join(", "));
            }
            Ok(())
        }
        Some(other) => bail!("未知子命令 {other}（用 --help 查看用法）"),
    }
}

fn print_help() {
    println!(
        "\
quietly · terminal english tutor

USAGE:
    quietly                       打开对话式终端界面
    quietly import <file>         导入词库（ECDICT csv / 通用 csv / json / txt）
    quietly dict <file> [--merge] 导入 StarDict 词典；--merge 追加义项而非覆盖
    quietly etymology <file>      导入有出处的词根（kaikki jsonl / roots.tsv）
    quietly phrases <file>        批量导入常用词组（lemma,phrase,gloss）
    quietly phrase <词> <搭配> = <释义>   单条补录词组
    quietly word <lemma>          查看词条（含义项、搭配、词根、例句）
    quietly read [file] [--book <name>]  导入一篇文本（用 --book 归入同名的书）
    quietly library               阅读书与进度
    quietly drop <text>           删除一篇文本及其进度、标记、题目
    quietly quiz <text> <file>    往某篇文本导入题目
    quietly books                 列出词书与进度
    quietly doctor [--fix]        核对词书/卡片引用的词条是否还在词典里
    quietly stats                 今日统计
    quietly db                    两个数据库文件的位置与体积
    quietly where                 打印数据库路径

OPTIONS:
    --db <path>                   使用指定数据库
    -h, --help                    显示帮助
    -V, --version                 显示版本

TUI 命令：
    /study [n]   开始学习      /books       词书列表
    /book <slug> 切换词书      /search <q>  搜索
    /root <affix> 查词根       /summary     每日总结
    /phrase <词> <搭配> = <释义>   补录固定搭配
    /import <p>  导入文件      /help        帮助

阅读：
    /read [file|title|book]  开始阅读   /texts  已导入的文本
    /library            阅读书与进度   /drop <title>  删除文本
    /mark <word> · /mark all      标记 / 取消标记生词
    /take <n>                     收第 n 个词进词书
    /next · /prev · /goto <n>     翻页
    /translate                    显示 / 隐藏译文
    /quiz [file]                  读后测验"
    );
}

/// `--flag value` 里的值；没有该开关时返回 `None`。
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 让译文从同一列开始。中文按两个字符宽度算，否则 `[医]` 之后会错位。
fn pad_label(label: &str) -> String {
    let width: usize = label
        .chars()
        .map(|ch| if ch.is_ascii() { 1 } else { 2 })
        .sum();
    format!("{label}{}", " ".repeat(7usize.saturating_sub(width)))
}
