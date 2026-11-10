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
    /root <affix> 查词根       /summary     每日总结
    /import <p>  导入文件      /help        帮助"
    );
}
