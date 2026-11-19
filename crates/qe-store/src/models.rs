//! 模型清单 `models.json`：哪家模型、什么端点、哪些型号，以及**调用顺序**。
//!
//! 模型会更新（型号改名、新版本上线、换端点），这些信息不写死在代码里：
//! 落在数据目录的 `models.json`（和 `keys.json` 同目录），**启动时读取**，
//! 文件缺失时自动写出内置默认。以后升级模型改文件即可，不用改代码、不用重编译。
//!
//! **列表顺序就是调用顺序**：翻译按它取第一个配了 key 的模型（都配了时谁在前
//! 谁先用），批改同样按它；模型全部没配才轮到机器翻译
//! （百度 > 腾讯 > DeepL > MyMemory，那段顺序在 `translator` 里）。
//!
//! 结构和常见的模型清单一致（`name` / `apiType` / `models[].id` / `models[].url`），
//! 从别的工具把配置搬过来直接能读；多余字段（`vision`、`thinking`、
//! `maxInputTokens` 之类）照样解析，只是这里用不上。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 一家模型服务（`models.json` 的一项）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// 显示名，例如 `Gemini`。
    #[serde(default)]
    pub name: String,
    /// 解锁这家用的 key 名：`/apikey set <key>` 里的规范名（如 `gemini_api_key`）。
    #[serde(default)]
    pub key: String,
    /// 接口类型；只认 `chat-completions`（缺省也当它是）。
    #[serde(rename = "apiType", default = "default_api_type")]
    pub api_type: String,
    /// 候选型号，按文件顺序依次尝试，第一个返回正文的赢。
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

/// 一个具体模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    /// 型号 id，进请求的 `model` 字段。
    pub id: String,
    /// 人看的型号名，例如 `DeepSeek 4.1 Flash`。
    #[serde(default)]
    pub name: String,
    /// chat-completions 地址。
    pub url: String,
}

fn default_api_type() -> String {
    "chat-completions".to_string()
}

impl ProviderEntry {
    /// 是不是 chat-completions 形态；空白（清单里没写这字段）也算。
    pub fn is_chat_completions(&self) -> bool {
        let kind = self.api_type.trim();
        kind.is_empty() || kind.eq_ignore_ascii_case("chat-completions")
    }
}

/// 内置默认。**顺序 = 调用顺序**：Gemini > 小米 MiMo > DeepSeek，
/// 都配了 key 时翻译和批改都按这个先后。文件缺失时写这份出来。
const DEFAULT: &str = r#"[
  {
    "name": "Gemini",
    "key": "gemini_api_key",
    "apiType": "chat-completions",
    "models": [
      { "id": "gemini-3.8-flash", "name": "Gemini 3.8 Flash", "url": "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions" },
      { "id": "gemini-3.5-flash", "name": "Gemini 3.5 Flash", "url": "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions" },
      { "id": "gemini-flash-latest", "name": "Gemini Flash (latest)", "url": "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions" }
    ]
  },
  {
    "name": "Xiaomi Mimo",
    "key": "mimo_api_key",
    "apiType": "chat-completions",
    "models": [
      { "id": "mimo-v2.6-flash", "name": "MiMo 2.6 Flash", "url": "https://token-plan-cn.xiaomimimo.com/v1/chat/completions" },
      { "id": "mimo-v2.6-pro", "name": "MiMo 2.6 Pro", "url": "https://token-plan-cn.xiaomimimo.com/v1/chat/completions" }
    ]
  },
  {
    "name": "Deepseek",
    "key": "deepseek_api_key",
    "apiType": "chat-completions",
    "models": [
      { "id": "deepseek-flash", "name": "DeepSeek 4.1 Flash", "url": "https://api.deepseek.com/chat/completions" },
      { "id": "deepseek-v4-pro", "name": "DeepSeek 4 Pro", "url": "https://api.deepseek.com/chat/completions" }
    ]
  }
]
"#;

/// `models.json` 的完整路径（与 `keys.json` 同目录）。
pub fn path() -> PathBuf {
    crate::keys::default_dir().join("models.json")
}

/// 读模型清单；文件缺失时先写出内置默认再读。
///
/// 坏文件会报错而不是悄悄覆盖——清单是用户手改的，弄丢比读失败更糟。
pub fn load() -> Result<Vec<ProviderEntry>> {
    load_at(&crate::keys::default_dir())
}

fn load_at(dir: &Path) -> Result<Vec<ProviderEntry>> {
    let path = dir.join("models.json");
    if !path.exists() {
        fs::create_dir_all(dir)
            .with_context(|| format!("无法创建数据目录 {}", dir.display()))?;
        fs::write(&path, DEFAULT)
            .with_context(|| format!("无法写出默认模型清单 {}", path.display()))?;
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("无法读取 {}", path.display()))?;
    parse(&raw).with_context(|| format!("{} 不是合法 JSON", path.display()))
}

fn parse(raw: &str) -> Result<Vec<ProviderEntry>> {
    serde_json::from_str(raw).context("不是合法的模型清单 JSON")
}

/// 按 key 名找那家模型（`gemini_api_key` → Gemini 条目）；清单里没有就报错，
/// 提示去改 `models.json`——这正是「加模型不改代码」的入口。
pub fn entry_for_key(key_name: &str) -> Result<ProviderEntry> {
    let entries = load()?;
    entry_in(&entries, key_name)
        .with_context(|| format!("{key_name} 不在 {} 里 · 加一条模型条目", path().display()))
}

fn entry_in(entries: &[ProviderEntry], key_name: &str) -> Option<ProviderEntry> {
    entries
        .iter()
        .find(|entry| entry.key == key_name)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_written_from_defaults_in_call_order() {
        let dir = tempfile::tempdir().unwrap();
        let entries = load_at(dir.path()).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Gemini", "Xiaomi Mimo", "Deepseek"],
            "默认清单的顺序就是翻译/批改的调用顺序"
        );
        // 文件真的落了盘，之后改文件就够，不用碰代码。
        assert!(dir.path().join("models.json").exists());
        assert_eq!(load_at(dir.path()).unwrap(), entries, "第二次读的是文件本身");
    }

    #[test]
    fn foreign_fields_from_another_tools_config_are_tolerated() {
        // 别的工具的清单（带 apiKey 占位、vision/thinking 等）搬过来也能读。
        let raw = r#"[{
            "name": "Deepseek",
            "vendor": "customendpoint",
            "apiKey": "${input:chat.lm.secret.-31abf65d}",
            "apiType": "chat-completions",
            "models": [{
                "id": "deepseek-flash",
                "name": "DeepSeek 4.1 Flash",
                "url": "https://api.deepseek.com/chat/completions",
                "toolCalling": true,
                "vision": true,
                "thinking": true,
                "supportsReasoningEffort": ["low", "medium", "high"],
                "maxInputTokens": 1048576,
                "maxOutputTokens": 393216
            }]
        }]"#;
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Deepseek");
        assert_eq!(entries[0].key, "", "没写 key 字段时不解锁任何服务");
        assert_eq!(entries[0].models[0].id, "deepseek-flash");
        assert_eq!(
            entries[0].models[0].url,
            "https://api.deepseek.com/chat/completions"
        );
        assert!(entries[0].is_chat_completions());
    }

    #[test]
    fn entries_are_found_by_key_name() {
        let entries = parse(DEFAULT).unwrap();
        assert!(entry_in(&entries, "mimo_api_key").is_some());
        assert_eq!(
            entry_in(&entries, "deepseek_api_key").unwrap().name,
            "Deepseek"
        );
        assert!(entry_in(&entries, "who_knows_api_key").is_none());
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_silently_replaced() {
        // 清单是用户手改的：读失败要报出来，不能悄悄拿默认盖掉。
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("models.json"), "{oops").unwrap();
        let err = load_at(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("不是合法 JSON"));
    }
}
