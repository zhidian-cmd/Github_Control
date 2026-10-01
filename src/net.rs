//! 网络探测：代理可达性检测。与 Python 版 net.py 对应。

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 检测代理端口能否建立 TCP 连接
pub fn is_proxy_reachable(proxy_url: &str, timeout: f64) -> bool {
    let parsed = match url::Url::parse(proxy_url) {
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

/// 联网操作前检查代理。返回错误消息，None 表示可用。
pub fn proxy_preflight() -> Option<String> {
    let cfg = crate::config::load_config(false);
    let enabled = cfg
        .pointer("/proxy/enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    let url = cfg
        .pointer("/proxy/url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
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
pub const NET_HINTS: [&str; 11] = [
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
];

pub fn is_net_hint(out: &str) -> bool {
    let low = out.to_lowercase();
    NET_HINTS.iter().any(|k| low.contains(k))
}
