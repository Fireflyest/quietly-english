//! 机器翻译：给没有官方译文的文章补**整段**译文。
//!
//! 内建来源里只有 TED 和 BBC 带中文对照，新华网、Guardian 长读、Aeon 都只有英文。
//! 这里接翻译 API：按段翻译、结果落库缓存，同一段只花一次额度。
//!
//! 五家按优先级挑（`/apikey set` 存的 `keys.json` 优先，环境变量兜底；
//! **key 不进库、不进仓库**）：
//!
//! 1. **百度翻译**（`BAIDU_TRANSLATE_APP_ID` + `BAIDU_TRANSLATE_SECRET_KEY`）
//!    ——国内直连不用代理，签名是 `MD5(appid+q+salt+secretKey)`。
//! 2. **腾讯云机器翻译**（`TENCENT_SECRET_ID` + `TENCENT_SECRET_KEY`）
//!    ——同样国内直连，走腾讯云统一的 TC3-HMAC-SHA256 签名。
//! 3. **Google Gemini**（`GEMINI_API_KEY`）——译文最顺，免费层按项目算配额。
//!    缺点是国内**必须走代理**，而且高峰期偶尔 503，所以失败会自动回退到 MyMemory。
//! 4. **DeepL API Free**（`DEEPL_API_KEY`）——50 万字符/月，但要注册绑卡。
//! 5. **MyMemory**——不用注册，匿名 5000 字符/天；环境变量里填 `MYMEMORY_EMAIL`
//!    （只是请求里的一个 `de` 参数，不用验证）额度按 50000 字符/天算。
//!
//! `QUIETLY_TRANSLATE=none` 整个关掉（不想联网时用）。
//!
//! 翻译是**按需**的：只翻你正读到的那一段，不会把整篇四五千词一次性提交出去。

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use qe_store::keys;
use sha2::{Digest, Sha256};

/// MyMemory 单次请求上限 500 字节；英文一字符一字节，留点余量更稳。
const MYMEMORY_CHUNK: usize = 420;

/// 百度 `q` 上限 6000 字节，留点余量。
const BAIDU_CHUNK: usize = 5000;

/// Gemini 的候选模型：从新到旧，第一个能用的就赢。
///
/// 模型改名很快（2.5-flash 已经对新用户下线），留几个备选、允许 `GEMINI_MODEL`
/// 覆盖，比写死一个名字活得久。
const GEMINI_MODELS: [&str; 3] = [
    "gemini-3.8-flash",
    "gemini-3.5-flash",
    "gemini-flash-latest",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Baidu,
    Tencent,
    MyMemory,
    DeepL,
    Gemini,
}

impl Provider {
    /// 显示名，用在结果和错误提示里。
    pub fn label(self) -> &'static str {
        match self {
            Provider::Baidu => "Baidu",
            Provider::Tencent => "Tencent",
            Provider::MyMemory => "MyMemory",
            Provider::DeepL => "DeepL",
            Provider::Gemini => "Gemini",
        }
    }

    /// 存进库里的服务名。
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Baidu => "baidu",
            Provider::Tencent => "tencent",
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
    /// DeepL / Gemini 的 API key；百度的 APP_ID；腾讯的 SecretId。
    pub key: Option<String>,
    /// 百度的 SecretKey / 腾讯的 SecretKey，和 [`Settings::key`] 成对使用。
    pub secret: Option<String>,
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
            Provider::Baidu => "Baidu · 百度翻译开放平台 · 按账号免费额度 · 直连".to_string(),
            Provider::Tencent => "Tencent · 腾讯云机器翻译 TMT · 按账号免费额度 · 直连".to_string(),
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
            secret: None,
        }
    }
}

/// 从 `keys.json`（`/apikey set`）读配置，缺的字段回落到环境变量；
/// 返回 `None` 表示用户关掉了翻译。
///
/// 优先级：百度 > 腾讯 > Gemini > DeepL > MyMemory。配了 key 不代表一定用得上，
/// 用不上时 [`translate`] 会自动回退到 MyMemory。
pub fn settings() -> Option<Settings> {
    if matches!(
        env(&["QUIETLY_TRANSLATE", "quietly_translate"]).as_deref(),
        Some("none" | "off" | "0")
    ) {
        return None;
    }
    let stored = keys::load();
    let get = |name: &str| keys::resolve(&stored, name);
    let email = get("mymemory_email");
    // 百度 / 腾讯国内直连、不用代理，配了就排在最前。
    if let (Some(key), Some(secret)) = (get("baidu_app_id"), get("baidu_secret_key")) {
        return Some(Settings {
            provider: Provider::Baidu,
            email,
            key: Some(key),
            secret: Some(secret),
        });
    }
    if let (Some(key), Some(secret)) = (get("tencent_secret_id"), get("tencent_secret_key")) {
        return Some(Settings {
            provider: Provider::Tencent,
            email,
            key: Some(key),
            secret: Some(secret),
        });
    }
    // 再往后是 Gemini：译文明显更顺，免费层也够用，但国内要走代理。
    if let Some(key) = get("gemini_api_key") {
        return Some(Settings {
            provider: Provider::Gemini,
            email,
            key: Some(key),
            secret: None,
        });
    }
    if let Some(key) = get("deepl_api_key") {
        return Some(Settings {
            provider: Provider::DeepL,
            email,
            key: Some(key),
            secret: None,
        });
    }
    Some(Settings {
        provider: Provider::MyMemory,
        email,
        key: None,
        secret: None,
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
        Provider::Baidu => baidu(settings, text),
        Provider::Tencent => tencent(settings, text),
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
    let key = settings.key.as_deref().context("DEEPL_API_KEY 没有配置")?;
    let body = format!("text={}&source_lang=EN&target_lang=ZH", encode(text));
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

/// 百度通用翻译：`q` 按 [`BAIDU_CHUNK`] 切开，每段单独签名请求。
fn baidu(settings: &Settings, text: &str) -> Result<String> {
    let app_id = settings
        .key
        .as_deref()
        .context("BAIDU_TRANSLATE_APP_ID 没有配置")?;
    let secret = settings
        .secret
        .as_deref()
        .context("BAIDU_TRANSLATE_SECRET_KEY 没有配置")?;
    let mut parts = Vec::new();
    for chunk in chunks(text, BAIDU_CHUNK) {
        parts.push(baidu_chunk(app_id, secret, chunk)?);
    }
    Ok(join(parts))
}

fn baidu_chunk(app_id: &str, secret: &str, chunk: &str) -> Result<String> {
    // salt 每次不同即可，纳秒时间戳够用（它只参与签名，不是安全随机数）。
    let salt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    // 官方规则：sign = MD5(appid + q + salt + secretKey)，q 是原文，不是编码后的。
    let sign = md5_hex(&format!("{app_id}{chunk}{salt}{secret}"));
    let body = format!(
        "q={}&from=en&to=zh&appid={}&salt={}&sign={}",
        encode(chunk),
        encode(app_id),
        salt,
        sign
    );
    let response = minreq::post("https://fanyi-api.baidu.com/api/trans/vip/translate")
        .with_header(
            "Content-Type",
            "application/x-www-form-urlencoded; charset=UTF-8",
        )
        .with_timeout(30)
        .with_body(body)
        .send()
        .context("百度翻译请求失败")?;
    let raw = response.as_str().unwrap_or_default();
    anyhow::ensure!(
        (200..300).contains(&response.status_code),
        "百度翻译返回 {} · {}",
        response.status_code,
        hint(raw)
    );
    let json: serde_json::Value = serde_json::from_str(raw).context("百度翻译返回的不是 JSON")?;
    if let Some(code) = json.get("error_code") {
        // 百度把错误码当字符串或数字都发过，先统一成字符串。
        let code = code.to_string().trim_matches('"').to_string();
        let message = json
            .pointer("/error_msg")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        anyhow::bail!("百度翻译 {code} · {message}");
    }
    let results = json
        .pointer("/trans_result")
        .and_then(|value| value.as_array())
        .context("百度翻译没有返回译文")?;
    let mut parts = Vec::new();
    for item in results {
        let dst = item
            .pointer("/dst")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        if !dst.is_empty() {
            parts.push(dst.to_string());
        }
    }
    anyhow::ensure!(!parts.is_empty(), "百度翻译没有返回译文");
    Ok(join(parts))
}

/// 腾讯云机器翻译 TMT：一次请求翻整段（上限 6000 字符），走 TC3 签名。
fn tencent(settings: &Settings, text: &str) -> Result<String> {
    let secret_id = settings
        .key
        .as_deref()
        .context("TENCENT_SECRET_ID 没有配置")?;
    let secret_key = settings
        .secret
        .as_deref()
        .context("TENCENT_SECRET_KEY 没有配置")?;
    let payload = serde_json::json!({
        "SourceText": text,
        "Source": "en",
        "Target": "zh",
    })
    .to_string();
    let timestamp = chrono::Utc::now().timestamp();
    let authorization = tencent_sign(secret_id, secret_key, &payload, timestamp);
    let response = minreq::post("https://tmt.tencentcloudapi.com")
        .with_header("Content-Type", "application/json; charset=utf-8")
        .with_header("X-TC-Action", "TextTranslate")
        .with_header("X-TC-Version", "2018-03-21")
        .with_header("X-TC-Timestamp", timestamp.to_string())
        .with_header("Authorization", authorization)
        .with_timeout(30)
        .with_body(payload)
        .send()
        .context("腾讯云翻译请求失败")?;
    let raw = response.as_str().unwrap_or_default();
    // 出错时错误体也是 JSON（HTTP 常回 4xx），先认 Response.Error，
    // 拿到的是 `AuthFailure.SecretIdInvalid` 这种能照着修的码。
    let json: Option<serde_json::Value> = serde_json::from_str(raw).ok();
    if let Some(code) = json
        .as_ref()
        .and_then(|json| json.pointer("/Response/Error/Code"))
        .and_then(|value| value.as_str())
        .filter(|code| !code.is_empty())
    {
        let message = json
            .as_ref()
            .and_then(|json| json.pointer("/Response/Error/Message"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        anyhow::bail!("腾讯云翻译 {code} · {message}");
    }
    anyhow::ensure!(
        (200..300).contains(&response.status_code),
        "腾讯云翻译返回 {} · {}",
        response.status_code,
        hint(raw)
    );
    let json: serde_json::Value = json.context("腾讯云翻译返回的不是 JSON")?;
    let translated = json
        .pointer("/Response/TargetText")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    anyhow::ensure!(!translated.is_empty(), "腾讯云翻译没有返回译文");
    Ok(translated)
}

/// 腾讯云 TC3-HMAC-SHA256 签名，返回 `Authorization` 头的值。
///
/// 官方算法：把请求拼成 canonical request，哈希后和时间戳、凭证范围一起
/// 组成 string to sign，再用密钥逐层 HMAC（日期 → 服务 → tc3_request）。
fn tencent_sign(secret_id: &str, secret_key: &str, payload: &str, timestamp: i64) -> String {
    const HOST: &str = "tmt.tencentcloudapi.com";
    const SERVICE: &str = "tmt";
    const ACTION: &str = "texttranslate";
    let algorithm = "TC3-HMAC-SHA256";
    let date = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
        .map(|moment| moment.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let canonical_headers = format!(
        "content-type:application/json; charset=utf-8\nhost:{HOST}\nx-tc-action:{ACTION}\n"
    );
    let signed_headers = "content-type;host;x-tc-action";
    let canonical_request = format!(
        "POST\n/\n\n{canonical_headers}\n{signed_headers}\n{}",
        sha256_hex(payload)
    );
    let scope = format!("{date}/{SERVICE}/tc3_request");
    let string_to_sign = format!(
        "{algorithm}\n{timestamp}\n{scope}\n{}",
        sha256_hex(&canonical_request)
    );
    let k_date = hmac_hex(secret_key.as_bytes(), date.as_bytes());
    let k_service = hmac_hex(k_date.as_bytes(), SERVICE.as_bytes());
    let k_signing = hmac_hex(k_service.as_bytes(), b"tc3_request");
    let signature = hmac_hex(k_signing.as_bytes(), string_to_sign.as_bytes());
    format!(
        "{algorithm} Credential={secret_id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    )
}

fn md5_hex(text: &str) -> String {
    hex(&md5::Md5::digest(text.as_bytes()))
}

fn sha256_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC 接受任意长度的密钥");
    mac.update(message);
    hex(&mac.finalize().into_bytes())
}

/// 小写十六进制，几种签名算法都要求这个形式。
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn looks_like_warning(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    upper.starts_with("MYMEMORY WARNING") || upper.contains("QUERY LENGTH LIMIT EXCEEDED")
}

/// 把服务端的原话换成人能照着做的一句。
fn hint(detail: &str) -> String {
    let upper = detail.to_ascii_uppercase();
    if upper.contains("QUOTA") || upper.contains("ALL AVAILABLE FREE TRANSLATIONS") {
        return "今天的免费额度用完了 · 明天再来，或配 MYMEMORY_EMAIL / DEEPL_API_KEY".to_string();
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
        let previous_is_ascii = out.chars().last().map(|ch| ch.is_ascii()).unwrap_or(false);
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
pub(crate) mod tests {
    use super::*;

    /// 改 `QUIETLY_HOME` 的测试共用这把锁：环境变量是进程级的，
    /// 两个测试同时改会互相读到对方的目录。
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

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
        assert!(
            hint("MYMEMORY WARNING: YOU USED ALL AVAILABLE FREE TRANSLATIONS FOR TODAY")
                .contains("免费额度")
        );
        assert!(
            hint("QUERY LENGTH LIMIT EXCEEDED. MAX ALLOWED QUERY : 500 CHARS").contains("太长")
        );
        assert!(looks_like_warning("MYMEMORY WARNING: quota"));
        assert!(!looks_like_warning(
            "我很小的时候就知道自己应该成为一名作家。"
        ));
    }

    #[test]
    fn deepl_wins_when_a_key_is_present() {
        // 环境变量是进程级的，这里只验判定逻辑本身。
        let deepl = Settings {
            provider: Provider::DeepL,
            email: None,
            key: Some("x".into()),
            secret: None,
        };
        assert!(deepl.detail().contains("DeepL"));
        let free = Settings {
            provider: Provider::MyMemory,
            email: None,
            key: None,
            secret: None,
        };
        assert!(free.detail().contains("5000"));
        let with_email = Settings {
            email: Some("me@example.com".into()),
            ..free
        };
        assert!(with_email.detail().contains("50000"));
    }

    #[test]
    fn baidu_and_tencent_carry_a_key_and_a_secret() {
        let baidu = Settings {
            provider: Provider::Baidu,
            email: None,
            key: Some("2015066600".into()),
            secret: Some("secret".into()),
        };
        assert!(baidu.detail().contains("Baidu"));
        let tencent = Settings {
            provider: Provider::Tencent,
            email: None,
            key: Some("AKIDxxx".into()),
            secret: Some("secret".into()),
        };
        assert!(tencent.detail().contains("Tencent"));
        // 回退永远是 MyMemory，两家的凭证不能跟着漏过去。
        assert_eq!(baidu.fallback().secret, None);
        assert_eq!(tencent.fallback().key, None);
    }

    #[test]
    fn signature_primitives_match_published_vectors() {
        // 出版物里的经典向量，确认哈希用对了。
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // RFC 4231 测试向量 2。
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn tencent_signature_is_stable_and_tracks_key_and_utc_date() {
        let payload = r#"{"Source":"en","SourceText":"hi","Target":"zh"}"#;
        let first = tencent_sign("id", "key", payload, 1_700_000_000);
        assert_eq!(first, tencent_sign("id", "key", payload, 1_700_000_000));
        // 1700000000 = 2023-11-14 22:13:20 UTC，凭证范围里必须是 UTC 日期。
        assert!(first.contains("Credential=id/2023-11-14/tmt/tc3_request"));
        assert!(first.contains("SignedHeaders=content-type;host;x-tc-action"));
        assert_ne!(first, tencent_sign("id", "other", payload, 1_700_000_000));
        // 跨过 UTC 零点，日期和签名都要跟着换。
        assert_ne!(first, tencent_sign("id", "key", payload, 1_700_086_400));
    }

    #[test]
    fn keys_json_wins_over_environment() {
        // 环境变量是进程级的：只动 QUIETLY_HOME，把百度的一对 key 写进文件。
        let _guard = lock_env();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("QUIETLY_HOME");
        std::env::set_var("QUIETLY_HOME", dir.path());
        qe_store::keys::set("baidu_app_id", "2015066600").unwrap();
        qe_store::keys::set("baidu_secret_key", "s3cret").unwrap();

        let first = settings().expect("machine translation is on by default");
        assert_eq!(first.provider, Provider::Baidu, "文件里的 key 直接生效");
        assert_eq!(first.key.as_deref(), Some("2015066600"));

        // 清空文件后回落到环境变量，不再看得见文件里那把 key。
        qe_store::keys::clear(None).unwrap();
        let fallback = settings().expect("still on");
        assert_ne!(fallback.key.as_deref(), Some("2015066600"));

        match previous {
            Some(value) => std::env::set_var("QUIETLY_HOME", value),
            None => std::env::remove_var("QUIETLY_HOME"),
        }
    }
}
