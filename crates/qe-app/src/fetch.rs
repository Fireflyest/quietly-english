//! 抓网页用的最小 GET。
//!
//! 只有一个要点：**走系统代理**。国内网络直连 BBC / 境外站点多半超时，而这类
//! 机器上通常已经开着本地代理（`HTTP_PROXY=http://127.0.0.1:7888` 这种），
//! 直接连就会像「网络不通」一样失败。`curl.exe` 自动认这些变量，我们的抓取也认。

use anyhow::{Context, Result};

const USER_AGENT: &str = "Mozilla/5.0 (compatible; quietly-reader/0.1; personal study)";

/// 抓一个 URL，返回正文。
pub fn get(url: &str) -> Result<String> {
    let mut request = minreq::get(url)
        .with_header("User-Agent", USER_AGENT)
        .with_timeout(30);
    if let Some(proxy) = proxy_from_env() {
        request = request.with_proxy(proxy);
    }
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

/// `HTTPS_PROXY` / `HTTP_PROXY`（大小写都认）里配的代理。
///
/// 只支持 HTTP CONNECT 代理，这也正是本地代理工具的默认形态；`NO_PROXY` 里的
/// 直连名单不在这里处理，抓取目标都是外网站点。
pub fn proxy_from_env() -> Option<minreq::Proxy> {
    proxy_var(&["HTTPS_PROXY", "https_proxy"])
        .or_else(|| proxy_var(&["HTTP_PROXY", "http_proxy"]))
        .and_then(|value| minreq::Proxy::new(value.trim()).ok())
}

fn proxy_var(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_bare_host_and_port_is_a_proxy() {
        // `minreq::Proxy` 不实现 PartialEq 的 Debug 输出没法直接比，能建出来就够。
        assert!(minreq::Proxy::new("127.0.0.1:7888").is_ok());
        assert!(minreq::Proxy::new("http://127.0.0.1:7888").is_ok());
    }
}
