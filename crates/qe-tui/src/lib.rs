//! 终端渲染层：只认识 `qe_core::view` 里的视图模型，不碰数据库与业务逻辑。

pub mod lines;
pub mod render;
pub mod terminal;
pub mod theme;

pub use render::{draw, DrawInfo, Suggestion, View, ViewMode};
pub use terminal::TerminalSession;
