//! 持久化与导入层：SQLite 实现、内置种子词书、外部词库导入。

pub mod db;
pub mod etymology;
pub mod import;
pub mod seed;
pub mod stardict;
pub mod tatoeba;

pub use db::SqliteStore;
pub use etymology::EtymologyReport;
