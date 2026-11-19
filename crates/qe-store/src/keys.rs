//! 翻译 API key 的本地配置：`<data_dir>/keys.json`。
//!
//! key **不进** `quietly.db`——换机器时只搬学习库，不想把密钥一起带走；
//! 也**不进**仓库，文件只落在本机数据目录（`QUIETLY_HOME` 可覆盖位置）。
//!
//! 环境变量仍然有效：文件里写了就用文件的，没写的字段回落到同名环境变量，
//! 所以老配置不用动。优先级由 [`crate`] 之外的调用方按 key 名取即可。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 一个可配置的 key：规范名（存进 `keys.json` 的键）、回退用的环境变量、用途说明。
pub struct Key {
    /// 文件里的键，也是 `/apikey set` 认的名字。
    pub name: &'static str,
    /// 读取顺序即回退顺序；第一个非空的胜出。
    pub env: &'static [&'static str],
    /// 给人看的说明，出现在 `/apikey list` 里。
    pub hint: &'static str,
}

/// 机器翻译能配的全部 key，顺序即 `/apikey list` 的展示顺序。
pub const KEYS: &[Key] = &[
    Key {
        name: "baidu_app_id",
        env: &[
            "BAIDU_TRANSLATE_APP_ID",
            "baidu_translate_app_id",
            "BAIDU_APP_ID",
            "baidu_app_id",
        ],
        hint: "百度翻译 APP ID",
    },
    Key {
        name: "baidu_secret_key",
        env: &[
            "BAIDU_TRANSLATE_SECRET_KEY",
            "baidu_translate_secret_key",
            "BAIDU_SECRET_KEY",
            "baidu_secret_key",
        ],
        hint: "百度翻译 SecretKey（签名用）",
    },
    Key {
        name: "tencent_secret_id",
        env: &["TENCENT_SECRET_ID", "tencent_secret_id"],
        hint: "腾讯云 SecretId",
    },
    Key {
        name: "tencent_secret_key",
        env: &["TENCENT_SECRET_KEY", "tencent_secret_key"],
        hint: "腾讯云 SecretKey",
    },
    Key {
        name: "deepseek_api_key",
        env: &["DEEPSEEK_API_KEY", "deepseek_api_key"],
        hint: "DeepSeek API key（国产 · 直连 · 翻译+批改）",
    },
    Key {
        name: "mimo_api_key",
        env: &["MIMO_API_KEY", "mimo_api_key", "XIAOMI_MIMO_API_KEY"],
        hint: "小米 MiMo API key（国产 · 直连 · 翻译+批改）",
    },
    Key {
        name: "gemini_api_key",
        env: &["GEMINI_API_KEY", "gemini_api_key"],
        hint: "Gemini API key（国内要走代理）",
    },
    Key {
        name: "deepl_api_key",
        env: &["DEEPL_API_KEY", "deepl_api_key"],
        hint: "DeepL API Free key",
    },
    Key {
        name: "mymemory_email",
        env: &["MYMEMORY_EMAIL", "mymemory_email"],
        hint: "MyMemory 邮箱（额度 5k → 50k 字符/天）",
    },
];

/// 一个 key 的当前状态，给 `/apikey list` 渲染用。
pub struct Status {
    pub name: &'static str,
    pub hint: &'static str,
    /// 已配置的值；`None` 表示文件和环境变量里都没有。
    pub value: Option<String>,
    /// `keys.json` / `env` / `unset`。
    pub origin: &'static str,
}

/// 把任意写法归一成规范名：忽略大小写与 `-` `_` `.`
/// （`BAIDU_TRANSLATE_APP_ID`、`baidu-app-id`、`baidu_app_id` 都算一个）。
fn normalize(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .collect()
}

/// 规范名；认不出的返回 `None`。
///
/// 认三种写法：规范名、环境变量名、以及无歧义的前缀短写
/// （`gemini` → `gemini_api_key`；`baidu` 同时命中两个 key，要求写全）。
pub fn canonical(name: &str) -> Option<&'static str> {
    let needle = normalize(name);
    let exact = KEYS.iter().find(|key| {
        normalize(key.name) == needle || key.env.iter().any(|env| normalize(env) == needle)
    });
    if let Some(key) = exact {
        return Some(key.name);
    }
    let mut hits = KEYS
        .iter()
        .filter(|key| normalize(key.name).starts_with(needle.as_str()));
    let first = hits.next()?;
    hits.next().is_none().then_some(first.name)
}

/// 默认数据目录：与 `quietly.db` 同一个（`QUIETLY_HOME` 可覆盖）。
pub fn default_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("QUIETLY_HOME") {
        return PathBuf::from(custom);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("quietly-english")
}

/// `keys.json` 的完整路径。
pub fn path() -> PathBuf {
    default_dir().join("keys.json")
}

/// 读文件；文件不存在或坏了都当空表，回落到环境变量即可。
pub fn load() -> BTreeMap<String, String> {
    load_at(&default_dir()).unwrap_or_default()
}

fn load_at(dir: &Path) -> Result<BTreeMap<String, String>> {
    let path = dir.join("keys.json");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(err).with_context(|| format!("无法读取 {}", path.display())),
    };
    serde_json::from_str(&raw).with_context(|| format!("{} 不是合法 JSON", path.display()))
}

fn save_at(dir: &Path, keys: &BTreeMap<String, String>) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("无法创建数据目录 {}", dir.display()))?;
    let path = dir.join("keys.json");
    let raw = serde_json::to_string_pretty(keys).context("无法序列化 keys.json")?;
    fs::write(&path, raw).with_context(|| format!("无法写入 {}", path.display()))?;
    Ok(())
}

/// 一个 key 的生效值：文件优先，其次环境变量。
///
/// `stored` 由 [`load`] 一次读出，避免每次取值都碰磁盘。
pub fn resolve(stored: &BTreeMap<String, String>, name: &str) -> Option<String> {
    let canonical = canonical(name)?;
    if let Some(value) = stored.get(canonical) {
        return Some(value.clone());
    }
    let key = KEYS.iter().find(|key| key.name == canonical)?;
    key.env
        .iter()
        .find_map(|env| std::env::var(env).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 每个 key 的当前值与来源，给 `/apikey list` 用。
pub fn status() -> Vec<Status> {
    let stored = load();
    KEYS.iter()
        .map(|key| {
            let (value, origin) = if let Some(value) = stored.get(key.name) {
                (Some(value.clone()), "keys.json")
            } else {
                let from_env = key
                    .env
                    .iter()
                    .find_map(|env| std::env::var(env).ok())
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty());
                match from_env {
                    Some(value) => (Some(value), "env"),
                    None => (None, "unset"),
                }
            };
            Status {
                name: key.name,
                hint: key.hint,
                value,
                origin,
            }
        })
        .collect()
}

/// 写入一个 key；名字认不出就报错。返回规范名。
pub fn set(name: &str, value: &str) -> Result<&'static str> {
    set_at(&default_dir(), name, value)
}

fn set_at(dir: &Path, name: &str, value: &str) -> Result<&'static str> {
    let canonical = canonical(name).with_context(|| {
        format!(
            "认不出的 key \"{}\" · /apikey list 看全部名字",
            name.trim()
        )
    })?;
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "{canonical} 的值不能为空");
    let mut keys = load_at(dir)?;
    keys.insert(canonical.to_string(), value.to_string());
    save_at(dir, &keys)?;
    Ok(canonical)
}

/// 删掉一个 key；`None` 清空整个文件。返回被删掉的规范名。
///
/// 只清 `keys.json`：环境变量是 shell 里设的，程序删不掉，提示里说清楚即可。
pub fn clear(name: Option<&str>) -> Result<Vec<String>> {
    clear_at(&default_dir(), name)
}

fn clear_at(dir: &Path, name: Option<&str>) -> Result<Vec<String>> {
    let mut keys = load_at(dir)?;
    let removed = match name {
        Some(name) => {
            let canonical = canonical(name).with_context(|| {
                format!(
                    "认不出的 key \"{}\" · /apikey list 看全部名字",
                    name.trim()
                )
            })?;
            keys.remove(canonical).map(|_| canonical.to_string()).into_iter().collect()
        }
        None => {
            let all = keys.keys().cloned().collect::<Vec<_>>();
            keys.clear();
            all
        }
    };
    save_at(dir, &keys)?;
    Ok(removed)
}

/// 打码显示：只留末四位（邮箱是身份不是密钥，调用方自己决定要不要打码）。
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        return "****".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_accepts_env_and_slug_spellings() {
        assert_eq!(canonical("baidu_app_id"), Some("baidu_app_id"));
        assert_eq!(canonical("BAIDU_TRANSLATE_APP_ID"), Some("baidu_app_id"));
        assert_eq!(canonical("baidu-app-id"), Some("baidu_app_id"));
        assert_eq!(canonical("TENCENT_SECRET_KEY"), Some("tencent_secret_key"));
        assert_eq!(canonical("gemini"), Some("gemini_api_key"), "无歧义的短写");
        assert_eq!(canonical("baidu"), None, "两个 key 都以 baidu 开头，必须写全");
        assert_eq!(canonical("deepseek"), Some("deepseek_api_key"), "国产模型的短写");
        assert_eq!(canonical("DEEPSEEK_API_KEY"), Some("deepseek_api_key"));
        assert_eq!(canonical("mimo"), Some("mimo_api_key"));
        assert_eq!(canonical("XIAOMI_MIMO_API_KEY"), Some("mimo_api_key"));
        assert_eq!(canonical("nonsense"), None);
    }

    #[test]
    fn set_load_clear_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        set_at(dir.path(), "BAIDU_TRANSLATE_APP_ID", " 2015066600 ").unwrap();
        set_at(dir.path(), "gemini", "abc123").unwrap();

        let keys = load_at(dir.path()).unwrap();
        assert_eq!(keys.get("baidu_app_id").unwrap(), "2015066600");
        assert_eq!(keys.get("gemini_api_key").unwrap(), "abc123");

        let removed = clear_at(dir.path(), Some("gemini_api_key")).unwrap();
        assert_eq!(removed, vec!["gemini_api_key".to_string()]);
        assert!(!load_at(dir.path()).unwrap().contains_key("gemini_api_key"));

        clear_at(dir.path(), None).unwrap();
        assert!(load_at(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn resolve_prefers_file_over_env() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MYMEMORY_EMAIL", "env@example.com");
        set_at(dir.path(), "mymemory_email", "file@example.com").unwrap();

        let keys = load_at(dir.path()).unwrap();
        assert_eq!(
            resolve(&keys, "mymemory_email").as_deref(),
            Some("file@example.com"),
            "文件里的值压过环境变量"
        );
        // 别的 key 没写文件，继续用环境变量。
        assert_eq!(
            resolve(&keys, "mymemory_email").is_some(),
            resolve(&BTreeMap::new(), "mymemory_email").is_some()
        );
        std::env::remove_var("MYMEMORY_EMAIL");
    }

    #[test]
    fn rejects_unknown_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(set_at(dir.path(), "who_knows", "x").is_err());
        assert!(set_at(dir.path(), "deepl_api_key", "   ").is_err());
        assert!(clear_at(dir.path(), Some("who_knows")).is_err());
        assert!(load_at(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn mask_keeps_only_the_tail() {
        assert_eq!(mask("abcd"), "****");
        assert_eq!(mask("2015066600"), "••••6600");
    }
}
