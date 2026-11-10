//! 领域层：纯数据模型、记忆曲线算法与 UI 无关的视图模型。
//!
//! 这一层不依赖数据库、不依赖终端，可单独做单元测试。

pub mod affix;
pub mod model;
pub mod srs;
pub mod store;
pub mod time;
pub mod view;

pub use model::*;
pub use store::Store;
pub use view::*;
