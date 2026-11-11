//! 命令行入口：默认进入 TUI，另外提供导入、路径查询等一次性命令。

mod app;
mod cli;
mod command;
mod editor;
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
            flag if flag.starts_with('-') => bail!("未知参数 {flag}（用 --help 查看用法）"),
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
    let seeded = qe_store::seed::seed_if_empty(&mut store)?;

    match command.as_deref() {
        None => {
            if seeded {
                eprintln!("quietly: 已写入内置词书 {}", store.path().display());
            }
            cli::install_panic_hook();
            let mut terminal = cli::TerminalSession::new()?;
            let app = app::App::new(Box::new(store))?;
            app.run(&mut terminal)
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
            let (inserted, updated) =
                store.import_stardict_with(std::path::Path::new(path), merge, &mut |count| {
                    eprintln!("  {count} entries …");
                })?;
            println!(
                "added {inserted} · updated {updated} · {:.1}s",
                started.elapsed().as_secs_f32()
            );
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
            let path = rest
                .first()
                .context("用法：quietly phrases <lemma,phrase,gloss.tsv>")?;
            println!("reading phrases from {path} …");
            let (words, added) = store.import_phrases(std::path::Path::new(path))?;
            println!("{added} phrases added across {words} words");
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
    quietly books                 列出词书与进度
    quietly stats                 今日统计
    quietly where                 打印数据库路径

OPTIONS:
    --db <path>                   使用指定数据库
    -h, --help                    显示帮助
    -V, --version                 显示版本

TUI 命令：
    /study [n]   开始学习      /books       词书列表
    /book <slug> 切换词书      /search <q>  搜索
    /root <affix> 查词根       /summary     每日总结    /phrase <词> <搭配> = <释义>   补录固定搭配    /import <p>  导入文件      /help        帮助"
    );
}
