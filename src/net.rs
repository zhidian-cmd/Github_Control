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

/// 联网关键词判定见下方 NET_HINTS / is_net_hint。
///
/// 2026-10-18 移除 proxy_preflight（曾是 api/gh_runner 5 处接口的硬闸门）：
/// 代理端口不通就整体判死，但直连可用时 runner::run_fb 明明能兜底转直连，
/// 闸门把"本可以成功的操作"直接堵死（用户实测：代理软件没开，推送/更新全废）。
/// 链路决策统一收敛到 run_fb：代理失败命中 NET_HINTS → 自动转直连重试；
/// 直连也不通时 run_fb 自带"（已检测到本机直连也不通，未再盲目重试…）"报错，
/// 比闸门的 fail-fast 更准。测试按钮走 is_proxy_reachable /
/// direct_github_reachable，不受影响。

/// 网络抖动/间歇性故障关键词：命中才重试（git pull/push 重试与 gh 直连重试共用）
pub const NET_HINTS: [&str; 38] = [
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
    // —— Go 网络栈（gh CLI 本体）实测文本，2026-10-18 补 ——
    // 实测输出（Go 1.26 + 失效代理，2026-10-18 复现）：
    //   Get "https://api.github.com/user": proxyconnect tcp: dial tcp 127.0.0.1:7810:
    //   connectex: No connection could be made because the target machine actively refused it.
    // git 的 libcurl 输出（"Failed to connect to …"）能命中上方旧词，
    // 但 Go 的错误一个都不命中 → gh 侧操作代理挂了直接失败、不转直连。
    "connectex",
    "no connection could be made",
    "connection refused",
    "actively refused",
    "could not connect",
    "dial tcp",
    "dial udp",
    "proxyconnect",
    "no such host",
    "no route to host",
    "i/o timeout",
    "context deadline exceeded",
    "tls handshake timeout",
    "unexpected eof",
    "eof",
];

pub fn is_net_hint(out: &str) -> bool {
    let low = out.to_lowercase();
    NET_HINTS.iter().any(|k| low.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-18 实测复现：gh（Go）走失效代理的真实报错，修复前 NET_HINTS 一个都不命中，
    /// 导致 run_fb 判定"非网络错误"直接返回失败、不转直连。
    #[test]
    fn is_net_hint_matches_go_network_stack_errors() {
        let proxy_refused = r#"Get "https://api.github.com/user": proxyconnect tcp: dial tcp 127.0.0.1:7810: connectex: No connection could be made because the target machine actively refused it."#;
        assert!(is_net_hint(proxy_refused));
        let dns_fail = r#"Get "https://api.github.com/user": dial tcp: lookup api.github.com: no such host"#;
        assert!(is_net_hint(dns_fail));
        let timeout = "Get \"https://api.github.com/user\": context deadline exceeded (Client.Timeout exceeded while awaiting headers)";
        assert!(is_net_hint(timeout));
        let bare_eof = "Post \"https://api.github.com/user\": EOF";
        assert!(is_net_hint(bare_eof));
    }

    /// 2026-10-18 实测：git 2.55 libcurl 走失效代理的输出（原有 "failed to connect" 命中）。
    #[test]
    fn is_net_hint_matches_git_curl_errors() {
        let git_proxy = "fatal: unable to access 'https://github.com/x/y.git/': Failed to connect to github.com:443 over proxy 127.0.0.1 after 2065 ms: Could not connect to server";
        assert!(is_net_hint(git_proxy));
    }

    /// 非网络类失败不应触发兜底重试（认证/校验类错误兜了也没用）。
    #[test]
    fn is_net_hint_ignores_non_network_errors() {
        assert!(!is_net_hint("fatal: not a git repository (or any of the parent directories)"));
        assert!(!is_net_hint("error: Your local changes to the following files would be overwritten by merge"));
        assert!(!is_net_hint("gh: HTTP 422: Validation Failed (bad credentials)"));
        assert!(!is_net_hint(""));
    }
}
