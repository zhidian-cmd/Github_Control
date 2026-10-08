//! 网络探测：代理可达性检测。与 Python 版 net.py 对应。

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 代理地址归一化：用户可能只填 host:port（如 127.0.0.1:7810）。
/// url::Url::parse 对无 scheme 的输入直接报错（scheme 不能以数字开头），
/// git/curl 拿到无 scheme 的 HTTP_PROXY 也会失效 —— 统一补 http://。
pub fn normalize_proxy_url(u: &str) -> String {
    let t = u.trim();
    if t.is_empty() || t.contains("://") {
        return t.to_string();
    }
    format!("http://{t}")
}

/// 检测代理端口能否建立 TCP 连接
pub fn is_proxy_reachable(proxy_url: &str, timeout: f64) -> bool {
    let parsed = match url::Url::parse(&normalize_proxy_url(proxy_url)) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let host = parsed.host_str().unwrap_or("127.0.0.1").to_string();
    let port = parsed.port().unwrap_or(8080);
    let addr = match (host.as_str(), port).to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None => return false,
        },
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, Duration::from_secs_f64(timeout)).is_ok()
}

/// 直连体检：不走任何代理，TCP 直摸 api.github.com:443。
/// 回答"这台机器不挂代理到底能不能连 GitHub"——被墙环境下直连 TCP 会超时。
/// 返回 true=直连可达（可酌情关代理）；false=直连不通（需要代理）。
/// 注意：仅 TCP 层结论；直连能通不代表 gh 认证请求一定不被干扰，但足够做开关代理的参考。
pub fn direct_github_reachable(timeout: f64) -> bool {
    let addrs = match ("api.github.com", 443).to_socket_addrs() {
        Ok(it) => it.collect::<Vec<_>>(),
        Err(_) => return false,
    };
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, Duration::from_secs_f64(timeout)).is_ok() {
            return true;
        }
    }
    false
}

/// 联网操作前检查代理。返回错误消息，None 表示可用。
/// 语义与 env::build_env 一致：配置了 url 即视为启用（旧的 enabled 开关已废弃）。
pub fn proxy_preflight() -> Option<String> {
    let cfg = crate::config::load_config(false);
    let url = normalize_proxy_url(
        cfg.pointer("/proxy/url")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    if url.is_empty() {
        return None; // 没配代理 = 纯直连，无需预检
    }
    let timeout = cfg
        .pointer("/proxy/test_timeout")
        .and_then(|v| v.as_f64())
        .unwrap_or(2.0);
    if !is_proxy_reachable(&url, timeout) {
        return Some(format!(
            "代理 {url} 无法连接，请检查代理软件是否运行，或在设置里关闭代理。"
        ));
    }
    None
}

/// 网络抖动/间歇性故障关键词：命中才重试（git pull/push 重试与 gh 直连重试共用）
pub const NET_HINTS: [&str; 23] = [
    "could not resolve host",
    "network is unreachable",
    "rpc failed",
    "connection reset",
    "timed out",
    "couldn't connect",
    "error: 500",
    "error: 502",
    "error: 503",
    "internal server error",
    "unexpected status",
    // —— 实机踩坑补充：schannel 断连 / 隧道失败 / TLS 握手失败 ——
    "schannel",
    "server closed abruptly",
    "early eof",
    "failed to connect",
    "connect tunnel failed",
    "proxy connect aborted",
    "recv failure",
    "ssl connect error",
    "curl 56",
    "curl 35",
    "curl 28",
    // runner 层超时是中文文案，同样值得兜底/重试
    "命令超时",
];

pub fn is_net_hint(out: &str) -> bool {
    let low = out.to_lowercase();
    NET_HINTS.iter().any(|k| low.contains(k))
}
