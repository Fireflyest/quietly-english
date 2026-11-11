//! 配色与样式。
//!
//! 只用终端基础色，暗色/亮色主题都能读。
//!
//! 关于灰色的取舍：早期版本把次要文字设成 `DarkGray`（ANSI 8）再叠加 `DIM`，
//! 在深色终端上几近不可见；调成 `Gray`（ANSI 7）后又显得偏亮。现在统一到
//! ANSI 8 但**不叠加 DIM** —— 只靠颜色降低存在感，亮度仍足够阅读。
//!
//! * `text`  — 终端默认前景色，正文，对比度最高
//! * `label` — ANSI 8，块名/徽标这类"标题"
//! * `muted` — ANSI 8，次要信息（出处、译文、键列、状态栏）
//! * `faint` — ANSI 8 + DIM，仅装饰（竖线、分隔线、进度条底槽）

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::LightGreen;
pub const INFO: Color = Color::LightBlue;
pub const NOTE: Color = Color::LightMagenta;
pub const USER: Color = Color::Cyan;
pub const VALUE: Color = Color::LightCyan;
pub const WARN: Color = Color::Yellow;
pub const DANGER: Color = Color::LightRed;

/// 次要文字：比正文暗一档的灰。不叠加 DIM，否那么在深色终端上会糊成一片。
pub const MUTED: Color = Color::DarkGray;
/// 装饰线条：再压一档（靠 DIM）。
pub const FAINT: Color = Color::DarkGray;

pub fn accent() -> Style {
    Style::default()
        .fg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

pub fn accent_plain() -> Style {
    Style::default().fg(ACCENT)
}

pub fn info() -> Style {
    Style::default().fg(INFO)
}

pub fn note() -> Style {
    Style::default().fg(NOTE)
}

pub fn user() -> Style {
    Style::default().fg(USER)
}

pub fn value() -> Style {
    Style::default().fg(VALUE)
}

/// 正文：终端默认前景色，对比度最高，也不受主题配色影响。
pub fn text() -> Style {
    Style::default()
}

/// 强调：加粗的正文。
pub fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// 块名与徽标（`word`、`review · 2/54`）：同一档灰，不加 DIM —— 不加才读得清。
pub fn label() -> Style {
    Style::default().fg(FAINT)
}

/// 内联强调——例句里的目标词。换色加粗，和正文、竖线都区分得开。
pub fn mark() -> Style {
    Style::default()
        .fg(VALUE)
        .add_modifier(Modifier::BOLD)
}

/// 面板背景。像代码块：一整块底色把内容圈起来，不画边框。
pub const PANEL_BG: Color = Color::Indexed(236);

/// 面板的空白填充，用来把底色铺满整行。
pub fn panel_fill() -> Style {
    Style::default().bg(PANEL_BG)
}

// ------------------------------------------------------------------ code theme
//
// 词根块按"函数定义"呈现。配色取自常见深色主题的 256 色近似值，
// 比纯 ANSI 亮色柔和，在 Indexed(236) 底上不刺眼：
//
//   function   #e5c07b  暖黄     参数名 #61afef  天蓝
//   类型       #c678dd  紫       字符串 #98c379  草绿
//   标点       #7f848e  中灰     注释   #5c6370  暗灰斜体

const CODE_FN: Color = Color::Indexed(180);
const CODE_PARAM: Color = Color::Indexed(75);
const CODE_TYPE: Color = Color::Indexed(176);
const CODE_STRING: Color = Color::Indexed(114);
const CODE_PUNCT: Color = Color::Indexed(245);
const CODE_COMMENT: Color = Color::Indexed(242);

/// 函数名——词条本身。
pub fn code_fn() -> Style {
    Style::default()
        .fg(CODE_FN)
        .bg(PANEL_BG)
        .add_modifier(Modifier::BOLD)
}

/// 参数名——词根/词缀本身。
pub fn code_param() -> Style {
    Style::default().fg(CODE_PARAM).bg(PANEL_BG)
}

/// 类型——prefix / root / suffix。
pub fn code_type() -> Style {
    Style::default()
        .fg(CODE_TYPE)
        .bg(PANEL_BG)
        .add_modifier(Modifier::ITALIC)
}

/// 字符串——释义。
pub fn code_string() -> Style {
    Style::default().fg(CODE_STRING).bg(PANEL_BG)
}

/// 标点：括号、逗号、冒号、箭头、大括号。
pub fn code_punct() -> Style {
    Style::default().fg(CODE_PUNCT).bg(PANEL_BG)
}

/// 注释：出处。
pub fn code_comment() -> Style {
    Style::default()
        .fg(CODE_COMMENT)
        .bg(PANEL_BG)
        .add_modifier(Modifier::ITALIC)
}

/// 次要信息：出处、译文、键列。ANSI 8 但不加 DIM，比 `faint` 亮一点点。
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

/// 装饰：竖线、分隔线、进度条底槽。
pub fn faint() -> Style {
    Style::default().fg(FAINT).add_modifier(Modifier::DIM)
}

pub fn warn() -> Style {
    Style::default().fg(WARN)
}

pub fn danger() -> Style {
    Style::default().fg(DANGER)
}

/// 块光标。实心色块在明暗主题下都清楚，且不像 `▏` 那样挤动旁边的文字。
pub fn cursor() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// 保留旧名字，指向 `muted`：目前的调用处都是信息（占位提示、页脚、上下文），
/// 装饰行请直接用 `faint`。
pub fn dim() -> Style {
    muted()
}
