//! 一次性命令的输出格式，TUI 与 CLI 共用。

use qe_core::model::{DailyStats, ImportReport};
use qe_core::view::TableView;

pub use qe_tui::TerminalSession;

pub fn install_panic_hook() {
    qe_tui::terminal::install_panic_hook();
}

/// 终端里打印的导入结果。
pub fn format_report(report: &ImportReport) -> String {
    let mut out = format!(
        "added {} · updated {} · skipped {}",
        report.inserted, report.updated, report.skipped
    );
    if !report.books.is_empty() {
        out.push_str("\n词书：");
        for (slug, count) in &report.books {
            out.push_str(&format!("\n  {slug:<10} {count} 词"));
        }
    }
    out
}

/// TUI 里展示的导入结果。
pub fn report_table(report: &ImportReport) -> TableView {
    let rows = report
        .books
        .iter()
        .map(|(slug, count)| vec![slug.clone(), count.to_string()])
        .collect::<Vec<_>>();
    let mut table = TableView::new(
        vec!["book".into(), "words".into()],
        if rows.is_empty() {
            vec![vec!["—".into(), "0".into()]]
        } else {
            rows
        },
    )
    .caption(format!(
        "import · added {} · updated {} · skipped {}",
        report.inserted, report.updated, report.skipped
    ));
    table = table.right_align(1);
    table.footer("用 /books 查看，/book <slug> 切换")
}

pub fn format_stats(daily: &DailyStats, streak: u32) -> String {
    format!(
        "{}  new {} · reviews {} · again {} · hard {} · good {} · easy {} · {} min · streak {}",
        daily
            .day
            .map(qe_core::time::format_day)
            .unwrap_or_else(|| qe_core::time::format_day(qe_core::time::today())),
        daily.new_learned,
        daily.reviews,
        daily.again,
        daily.hard,
        daily.good,
        daily.easy,
        daily.minutes(),
        streak
    )
}
