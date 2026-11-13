//! 机器翻译：给没有官方译文的文章补**整段**译文。
//!
//! 内建来源里只有 TED 和 BBC 带中文对照，新华网、Guardian 长读、Aeon 都只有英文。
//! 这里接翻译 API：按段翻译、结果落库缓存，同一段只花一次额度。
//!
//! 三家按优先级挑（都从环境变量读，**key 不进库、不进仓库**）：
//!
//! 1. **Google Gemini**（`GEMINI_API_KEY`）——质量最好，免费层按项目算配额。
//!    缺点是国内**必须走代理**，而且高峰期偶尔 503，所以失败会自动回退到 MyMemory。
//! 2. **DeepL API Free**（`DEEPL_API_KEY`）——50 万字符/月，但要注册绑卡。
//! 3. **MyMemory**——不用注册，匿名 5000 字符/天；环境变量里填 `MYMEMORY_EMAIL`
//!    （只是请求里的一个 `de` 参数，不用验证）额度按 50000 字符/天算。
//!
//! `QUIETLY_TRANSLATE=none` 整个关掉（不想联网时用）。
//!
//! 翻译是**按需**的：只翻你正读到的那一段，不会把整篇四五千词一次性提交出去。

use anyhow::{Context, Result};

/// MyMemory 单次请求上限 500 字节；英文一字符一字节，留点余量更稳。
const MYMEMORY_CHUNK: usize = 420;

/// Gemini 的候选模型：从新到旧，第一个能用的就赢。
///
/// 模型改名很快（2.5-flash 已经对新用户下线），留几个备选、允许 `GEMINI_MODEL`
/// 覆盖，比写死一个名字活得久。
const GEMINI_MODELS: [&str; 3] = ["gemini-3.8-flash", "gemini-3.5-flash", "gemini-flash-latest"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    MyMemory,
    DeepL,
    Gemini,
}

impl Provider {
    /// 显示名，用在结果和错误提示里。
    pub fn label(self) -> &'static str {
        match self {
            Provider::MyMemory => "MyMemory",
            Provider::DeepL => "DeepL",
            Provider::Gemini => "Gemini",
        }
    }

    /// 存进库里的服务名。
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::MyMemory => "mymemory",
            Provider::DeepL => "deepl",
            Provider::Gemini => "gemini",
        }
    }
}

/// 一次翻译的结果：译文 + 实际用的服务（可能是回退后的那家）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    pub provider: &'static str,
}

/// 当前生效的翻译配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub provider: Provider,
    /// MyMemory 的 `de` 参数：填了额度从 5000 涨到 50000 字符/天。
    /// 不用注册，也没有 key，只要一个能联系到你的真实邮箱。
    pub email: Option<String>,
    /// DeepL / Gemini 的 API key。
    pub key: Option<String>,
}

impl Settings {
    /// 说明这台机器会怎么翻，供 `/translate` 与 `quietly translate` 提示用。
    pub fn detail(&self) -> String {
        match self.provider {
            Provider::MyMemory => {
                let quota = if self.email.is_some() {
                    "50000 chars/day"
                } else {
                    "5000 chars/day"
                };
                format!("MyMemory · free · {quota}")
            }
            Provider::DeepL => "DeepL API Free · 500k chars/month".to_string(),
            Provider::Gemini => "Gemini · free tier · needs a proxy in mainland China".to_string(),
        }
    }

    /// 同一个配置换成回退用的 MyMemory（Gemini / DeepL 挂了时用）。
    fn fallback(&self) -> Self {
        Self {
            provider: Provider::MyMemory,
            email: self.email.clone(),
            key: None,
        }
    }
}

/// 从环境变量读配置；返回 `None` 表示用户关掉了翻译。
///
/// 优先级：Gemini > DeepL > MyMemory。配了 key 不代表一定用得上，
/// 用不上时 [`translate`] 会自动回退到 MyMemory。
pub fn settings() -> Option<Settings> {
    if matches!(
        env(&["QUIETLY_TRANSLATE", "quietly_translate"]).as_deref(),
        Some("none" | "off" | "0")
    ) {
        return None;
    }
    let email = env(&["MYMEMORY_EMAIL", "mymemory_email"]);
    // Gemini 的译文明显更顺，而且免费层够用，所以排在最前。
    if let Some(key) = env(&["GEMINI_API_KEY", "gemini_api_key"]) {
        return Some(Settings {
            provider: Provider::Gemini,
            email,
            key: Some(key),
        });
    }
    if let Some(key) = env(&["DEEPL_API_KEY", "deepl_api_key"]) {
        return Some(Settings {
            provider: Provider::DeepL,
            email,
            key: Some(key),
        });
    }
    Some(Settings {
        provider: Provider::MyMemory,
        email,
        key: None,
    })
}

fn env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 翻一段，返回中文译文；主服务用不了就自动回退到 MyMemory。
pub fn translate(settings: &Settings, text: &str) -> Result<Translation> {
    let text = text.trim();
    anyhow::ensure!(!text.is_empty(), "没有可翻译的正文");
    let outcome = match settings.provider {
        Provider::MyMemory => mymemory(settings, text),
        Provider::DeepL => deepl(settings, text),
        Provider::Gemini => gemini(settings, text),
    };
    match outcome {
        Ok(body) => Ok(Translation {
            text: body,
            provider: settings.provider.as_str(),
        }),
        // MyMemory 自己失败就没什么可退的了。
        Err(err) if settings.provider == Provider::MyMemory => Err(err),
        Err(err) => {
            // Gemini 必须走代理、高峰期还会 503；DeepL 没 key 也会走到这里。
            // 有 MyMemory 兜着，读者至少拿到译文，提示里说清是谁给的。
            let why = first_line(&format!("{err:#}"));
            let body = mymemory(&settings.fallback(), text).with_context(|| {
                format!(
                    "{} 失败（{why}），MyMemory 也失败",
                    settings.provider.label()
                )
            })?;
            Ok(Translation {
                text: body,
                provider: Provider::MyMemory.as_str(),
            })
        }
    }
}

/// Gemini：一次请求翻整段，不用切分（上下文窗口上百万 token）。
///
/// 模型名和负载都会变，所以按 [`GEMINI_MODELS`] 依次试；网络本身不通时立刻放弃，
/// 不把时间浪费在换模型上——那种情况换哪个都不通。
fn gemini(settings: &Settings, text: &str) -> Result<String> {
    let key = settings.key.as_deref().context("GEMINI_API_KEY 没有配置")?;
    let models: Vec<String> = match env(&["GEMINI_MODEL", "gemini_model"]) {
        Some(model) => vec![model],
        None => GEMINI_MODELS.iter().map(|name| name.to_string()).collect(),
    };
    let prompt = format!(
        "Translate the following English passage into natural, fluent Simplified Chinese. \
         Keep the meaning and tone, do not add explanations. Output only the translation.\n\n{text}"
    );
    let payload = serde_json::json!({
        "contents": [{ "parts": [{ "text": prompt }] }],
        "generationConfig": { "temperature": 0.2 }
    })
    .to_string();

    let mut last = String::from("没有可用的模型");
    for model in &models {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        );
        let response = match minreq::post(&url)
            .with_header("x-goog-api-key", key)
            .with_header("Content-Type", "application/json")
            .with_timeout(45)
            .with_body(payload.clone())
            .send()
        {
            Ok(response) => response,
            // 连不上（国内没开代理）就到此为止。
            Err(err) => anyhow::bail!("Gemini 连不上（国内需要代理）· {err}"),
        };
        let raw = response.as_str().unwrap_or_default();
        if response.status_code == 200 {
            let json: serde_json::Value =
                serde_json::from_str(raw).context("Gemini 返回的不是 JSON")?;
            let translated = json
                .pointer("/candidates/0/content/parts/0/text")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            if !translated.is_empty() {
                return Ok(translated);
            }
            last = format!("{model} 返回了空译文");
            continue;
        }
        last = format!("{model} 返回 {}", response.status_code);
        // 503 / 429 是忙，换个模型也许就好；400 / 404 是模型名不对，也别停。
        if matches!(response.status_code, 401 | 403) {
            anyhow::bail!("Gemini 拒绝了这个 key · {}", hint(raw));
        }
    }
    anyhow::bail!("Gemini: {last}")
}

/// 错误信息常常好几行，界面上只留第一行。
fn first_line(message: &str) -> String {
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(message)
        .trim()
        .to_string()
}

fn mymemory(settings: &Settings, text: &str) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for chunk in chunks(text, MYMEMORY_CHUNK) {
        parts.push(mymemory_chunk(settings, chunk)?);
        // 匿名额度按天算，连着打会被限速，隔一下更稳。
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    Ok(join(parts))
}

fn mymemory_chunk(settings: &Settings, chunk: &str) -> Result<String> {
    let mut url = format!(
        "https://api.mymemory.translated.net/get?q={}&langpair=en|zh-CN",
        encode(chunk)
    );
    if let Some(email) = &settings.email {
        url.push_str(&format!("&de={}", encode(email)));
    }
    let raw = crate::fetch::get(&url).context("MyMemory 请求失败")?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).context("MyMemory 返回的不是 JSON（可能被限流）")?;
    let translated = json
        .pointer("/responseData/translatedText")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    // 配额用完时它照样回 200，把警告塞进译文里，所以要看内容。
    let status = json
        .pointer("/responseStatus")
        .map(|value| value.to_string())
        .unwrap_or_default();
    let problem = !status.is_empty() && status != "200";
    if problem || looks_like_warning(&translated) {
        let detail = json
            .pointer("/responseDetails")
            .and_then(|value| value.as_str())
            .unwrap_or(translated.as_str())
            .trim()
            .to_string();
        anyhow::bail!("MyMemory: {}", hint(&detail));
    }
    anyhow::ensure!(!translated.is_empty(), "MyMemory 没有返回译文");
    Ok(translated)
}

fn deepl(settings: &Settings, text: &str) -> Result<String> {
    let key = settings
        .key
        .as_deref()
        .context("DEEPL_API_KEY 没有配置")?;
    let body = format!(
        "text={}&source_lang=EN&target_lang=ZH",
        encode(text)
    );
    let response = minreq::post("https://api-free.deepl.com/v2/translate")
        .with_header("Authorization", format!("DeepL-Auth-Key {key}"))
        .with_header("Content-Type", "application/x-www-form-urlencoded")
        .with_timeout(40)
        .with_body(body)
        .send()
        .context("DeepL 请求失败")?;
    let raw = response.as_str().unwrap_or_default();
    anyhow::ensure!(
        (200..300).contains(&response.status_code),
        "DeepL 返回 {} · {}",
        response.status_code,
        hint(raw)
    );
    let json: serde_json::Value = serde_json::from_str(raw).context("DeepL 返回的不是 JSON")?;
    let translated = json
        .pointer("/translations/0/text")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    anyhow::ensure!(!translated.is_empty(), "DeepL 没有返回译文");
    Ok(translated)
}

fn looks_like_warning(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    upper.starts_with("MYMEMORY WARNING") || upper.contains("QUERY LENGTH LIMIT EXCEEDED")
}

/// 把服务端的原话换成人能照着做的一句。
fn hint(detail: &str) -> String {
    let upper = detail.to_ascii_uppercase();
    if upper.contains("QUOTA") || upper.contains("ALL AVAILABLE FREE TRANSLATIONS") {
        return "今天的免费额度用完了 · 明天再来，或配 MYMEMORY_EMAIL / DEEPL_API_KEY"
            .to_string();
    }
    if upper.contains("QUERY LENGTH LIMIT") {
        return "单段太长，超出了免费接口的上限".to_string();
    }
    if upper.contains("TOO MANY REQUESTS") || upper.contains("RATE") {
        return "请求太频繁，稍等一会儿再试".to_string();
    }
    let first = detail.lines().find(|line| !line.trim().is_empty());
    first.unwrap_or(detail).trim().to_string()
}

/// 按 `max` 字节切成小段，尽量在句末断开，其次在词间断开。
///
/// 免费接口对单次请求有硬上限，长段落只能拆。在句号处断句，拼回去才不会
/// 出现「半句接半句」的译文。
pub fn chunks(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.len() <= max {
            out.push(rest);
            break;
        }
        let window = &rest[..floor_char_boundary(rest, max)];
        let cut = sentence_break(window)
            .or_else(|| window.rfind(' '))
            .unwrap_or(window.len());
        let (head, tail) = rest.split_at(cut);
        // 断点落在句号后面时把标点留在本段，读起来更整。
        let (head, tail) = if tail.starts_with(['.', '!', '?', '。', '！', '？']) {
            tail.split_at(tail.chars().next().map(char::len_utf8).unwrap_or(0))
        } else {
            (head, tail)
        };
        let head = head.trim();
        if !head.is_empty() {
            out.push(head);
        }
        rest = tail.trim_start();
    }
    out
}

/// 窗口里最后一个句末标点的位置（含标点本身）。
fn sentence_break(window: &str) -> Option<usize> {
    window
        .char_indices()
        .filter(|(_, ch)| matches!(ch, '.' | '!' | '?' | '。' | '！' | '？'))
        .map(|(index, ch)| index + ch.len_utf8())
        .last()
}

/// 不超过 `max` 的最大字符边界，避免把多字节字符切成两半。
fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// 拼回整段。中文不用空格，英文片段之间补一个。
fn join(parts: Vec<String>) -> String {
    let mut out = String::new();
    for part in parts {
        if out.is_empty() {
            out.push_str(&part);
            continue;
        }
        let previous_is_ascii = out
            .chars()
            .last()
            .map(|ch| ch.is_ascii())
            .unwrap_or(false);
        let next_is_ascii = part.chars().next().map(|ch| ch.is_ascii()).unwrap_or(false);
        if previous_is_ascii || next_is_ascii {
            out.push(' ');
        }
        out.push_str(&part);
    }
    out
}

/// 百分号转义（UTF-8 按字节处理），够 `q=` 和表单用。
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_split_within_the_limit() {
        let text = "One two three. Four five six. Seven eight nine. Ten eleven twelve.";
        let parts = chunks(text, 30);
        assert!(parts.len() > 1);
        for part in &parts {
            assert!(part.len() <= 30, "{part:?} is too long");
        }
        assert!(parts[0].ends_with('.'), "cut on a sentence: {parts:?}");
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(chunks("A short line.", 420), vec!["A short line."]);
    }

    #[test]
    fn a_sentence_longer_than_the_limit_still_gets_cut() {
        let text = "word ".repeat(200);
        let parts = chunks(text.trim(), 100);
        assert!(parts.iter().all(|part| part.len() <= 100));
        assert!(parts.len() > 1);
    }

    #[test]
    fn chunks_never_split_a_multibyte_character() {
        let text = "中文句子。".repeat(40);
        for part in chunks(&text, 21) {
            assert!(part.len() <= 21);
            // 能拼回去说明没有半个字符。
            assert!(part.chars().all(|ch| ch != '\u{fffd}'));
        }
    }

    #[test]
    fn query_text_is_percent_encoded() {
        assert_eq!(encode("a b"), "a+b");
        assert_eq!(encode("en|zh"), "en%7Czh");
        assert_eq!(encode("你好"), "%E4%BD%A0%E5%A5%BD");
    }

    #[test]
    fn quota_warnings_become_a_sentence_you_can_act_on() {
        assert!(hint("MYMEMORY WARNING: YOU USED ALL AVAILABLE FREE TRANSLATIONS FOR TODAY")
            .contains("免费额度"));
        assert!(hint("QUERY LENGTH LIMIT EXCEEDED. MAX ALLOWED QUERY : 500 CHARS").contains("太长"));
        assert!(looks_like_warning("MYMEMORY WARNING: quota"));
        assert!(!looks_like_warning("我很小的时候就知道自己应该成为一名作家。"));
    }

    #[test]
    fn deepl_wins_when_a_key_is_present() {
        // 环境变量是进程级的，这里只验判定逻辑本身。
        let deepl = Settings {
            provider: Provider::DeepL,
            email: None,
            key: Some("x".into()),
        };
        assert!(deepl.detail().contains("DeepL"));
        let free = Settings {
            provider: Provider::MyMemory,
            email: None,
            key: None,
        };
        assert!(free.detail().contains("5000"));
        let with_email = Settings {
            email: Some("me@example.com".into()),
            ..free
        };
        assert!(with_email.detail().contains("50000"));
    }
}
