//! 抓网页用的最小 GET。
//!
//! 只有一个要点：**自动走代理**。国内网络直连 BBC / 境外站点多半超时，而这类
//! 机器上通常开着本地代理（Clash / v2rayN 这种，`127.0.0.1:7888`）。代理有两种
//! 开法，我们都认：先是 `HTTPS_PROXY` / `HTTP_PROXY` 环境变量（`curl.exe` 认的
//! 就是它们），没有的话再读 Windows「系统代理」开关（代理软件写进注册表的值）。
//! 只认环境变量等于没做——大部分人在 Windows 上只是点了代理软件的开关。

use anyhow::{Context, Result};

const USER_AGENT: &str = "Mozilla/5.0 (compatible; quietly-reader/0.1; personal study)";

/// 抓一个 URL，返回正文。
pub fn get(url: &str) -> Result<String> {
    let request = with_proxy(
        minreq::get(url)
            .with_header("User-Agent", USER_AGENT)
            .with_timeout(30),
    );
    let response = request
        .send()
        .with_context(|| format!("抓取失败：{url}"))?;
    anyhow::ensure!(
        (200..300).contains(&response.status_code),
        "{url} 返回 {}",
        response.status_code
    );
    Ok(response.as_str().unwrap_or_default().to_string())
}

/// 给一个 minreq 请求挂上代理；没配代理就原样返回。
///
/// GET 和翻译接口的 POST 共用这一处，保证「开了代理就都走代理」。
pub fn with_proxy(request: minreq::Request) -> minreq::Request {
    match proxy() {
        Some(proxy) => request.with_proxy(proxy),
        None => request,
    }
}

/// 代理地址（`127.0.0.1:7888` 这种）；`None` 表示直连。
///
/// 优先级：`HTTPS_PROXY` → `HTTP_PROXY` → `ALL_PROXY` → Windows 系统代理。
/// 环境变量比系统代理靠前，因为显式配置应该赢过全局开关。
///
/// 只支持 HTTP CONNECT 代理，这正是本地代理工具的默认形态；`NO_PROXY` 里的
/// 直连名单不在这里处理，抓取目标都是外网站点。
pub fn proxy_address() -> Option<String> {
    proxy_var(&["HTTPS_PROXY", "https_proxy"])
        .or_else(|| proxy_var(&["HTTP_PROXY", "http_proxy"]))
        .or_else(|| proxy_var(&["ALL_PROXY", "all_proxy"]))
        .or_else(system_proxy)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 建给 minreq 用的代理；地址解析不了就当没配。
pub fn proxy() -> Option<minreq::Proxy> {
    proxy_address().and_then(|value| minreq::Proxy::new(&value).ok())
}

fn proxy_var(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

/// Windows「系统代理」开关（`HKCU\...\Internet Settings`）里配的地址。
///
/// 代理软件的「系统代理」开关写的就是这两个值：`ProxyEnable` 是总闸，
/// `ProxyServer` 是地址。这里只读不写。
#[cfg(windows)]
fn system_proxy() -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    let settings = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            KEY_READ,
        )
        .ok()?;
    let enabled: u32 = settings.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return None;
    }
    let server: String = settings.get_value("ProxyServer").ok()?;
    parse_proxy_server(&server)
}

#[cfg(not(windows))]
fn system_proxy() -> Option<String> {
    None
}

/// Windows `ProxyServer` 的两种写法：
///
/// * `127.0.0.1:7888`——整体一个地址；
/// * `http=127.0.0.1:7890;https=127.0.0.1:7888`——按协议分开。
///
/// 抓取目标全是 https，所以按 https > http 取；`ftp=`、`socks=` 这种
/// 不是 HTTP CONNECT 的条目直接跳过——宁可直连，也别挂一个必败的代理。
fn parse_proxy_server(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if !value.contains('=') {
        return Some(value.to_string());
    }
    let mut entries: Vec<(String, String)> = value
        .split(';')
        .filter_map(|part| part.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .filter(|(key, value)| matches!(key.as_str(), "http" | "https") && !value.is_empty())
        .collect();
    entries.sort_by_key(|(key, _)| if key == "https" { 0 } else { 1 });
    entries.first().map(|(_, value)| value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_and_port_is_a_proxy() {
        // `minreq::Proxy` 不实现 PartialEq 的 Debug 输出没法直接比，能建出来就够。
        assert!(minreq::Proxy::new("127.0.0.1:7888").is_ok());
        assert!(minreq::Proxy::new("http://127.0.0.1:7888").is_ok());
    }

    #[test]
    fn a_single_system_proxy_address_is_taken_as_is() {
        assert_eq!(
            parse_proxy_server(" 127.0.0.1:7888 "),
            Some("127.0.0.1:7888".to_string())
        );
    }

    #[test]
    fn a_per_protocol_system_proxy_prefers_https() {
        // 各协议地址不同时，抓 https 目标必须拿 https 那个。
        assert_eq!(
            parse_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7888"),
            Some("127.0.0.1:7888".to_string())
        );
        // 只配了 http 也认。
        assert_eq!(
            parse_proxy_server("http=127.0.0.1:7890;ftp=127.0.0.1:21"),
            Some("127.0.0.1:7890".to_string())
        );
    }

    #[test]
    fn an_empty_or_socks_only_system_proxy_is_no_proxy() {
        assert_eq!(parse_proxy_server("   "), None);
        // socks 不是 minreq 支持的形态，跳过它宁可直连。
        assert_eq!(parse_proxy_server("socks=127.0.0.1:1080"), None);
        assert_eq!(parse_proxy_server("https=;http=127.0.0.1:7888"), Some("127.0.0.1:7888".to_string()));
    }
}
