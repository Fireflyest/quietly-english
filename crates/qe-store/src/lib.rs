//! 持久化与导入层：SQLite 实现、内置种子词书、外部词库导入。

pub mod db;
pub mod import;
pub mod seed;

pub use db::SqliteStore;
