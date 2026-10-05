//! 子进程环境构建：HOME 重定向、gh 配置目录、PATH、代理、凭据行为、schannel 开关。
//! 与 Python 版 env_builder.py 逐项对应。

use crate::config;
use crate::paths;
use std::collections::HashMap;

pub fn build_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();

    // 1. HOME 重定向，git 的 .gitconfig 去 data 里
    let home = paths::git_home_dir();
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    env.insert("USERPROFILE".into(), home.to_string_lossy().into_owned());

    // 2. gh 配置目录重定向
    env.insert(
        "GH_CONFIG_DIR".into(),
        paths::gh_config_dir().to_string_lossy().into_owned(),
    );

    // 3. 让 gh 能找到便携版 git
    let git_cmd = paths::runtime_dir().join("git").join("cmd");
    let path = env.get("PATH").cloned().unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    env.insert(
        "PATH".into(),
        format!("{}{sep}{path}", git_cmd.to_string_lossy()),
    );

    // 4. 代理：只要填了地址，联网就优先走代理（失败由自动兜底转直连）；
    //    不填 = 纯直连。旧的 enabled 开关已废弃，填地址即启用。
    let cfg = config::load_config(false);
    if let Some(u) = cfg.pointer("/proxy/url").and_then(|v| v.as_str()) {
        let u = crate::net::normalize_proxy_url(u);
        if !u.is_empty() {
            env.insert("HTTP_PROXY".into(), u.clone());
            env.insert("HTTPS_PROXY".into(), u);
        }
    }

    // 5. 不弹窗要密码，失败就失败；禁 Windows 凭据管理器抢答
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env.insert("GCM_INTERACTIVE".into(), "Never".into());

    // 6. MITM 代理（dev-sidecar 等）下 schannel 证书吊销检查必失败。
    //    必须同时显式声明 sslBackend=schannel 才生效。
    env.insert("GIT_CONFIG_COUNT".into(), "2".into());
    env.insert("GIT_CONFIG_KEY_0".into(), "http.sslBackend".into());
    env.insert("GIT_CONFIG_VALUE_0".into(), "schannel".into());
    env.insert("GIT_CONFIG_KEY_1".into(), "http.schannelCheckRevoke".into());
    env.insert("GIT_CONFIG_VALUE_1".into(), "false".into());

    env
}

/// 强制直连环境：清掉继承来的代理变量，也不注入配置代理（自动兜底"改走直连"时用）
pub fn build_env_direct() -> HashMap<String, String> {
    let mut env = build_env();
    for k in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "ALL_PROXY",
    ] {
        env.remove(k);
    }
    env
}

/// 强制走指定代理的环境（忽略配置开关，自动兜底"改走代理"时用）
pub fn build_env_forced_proxy(url: &str) -> HashMap<String, String> {
    let mut env = build_env_direct();
    let u = crate::net::normalize_proxy_url(url);
    if !u.is_empty() {
        env.insert("HTTP_PROXY".into(), u.clone());
        env.insert("HTTPS_PROXY".into(), u);
    }
    env
}

/// 取 URL 的 host；解析失败返回空串
fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn is_github_host(host: &str) -> bool {
    host == "github.com" || host == "www.github.com"
}

/// 克隆专用：按主机名判断，只有真正的 github.com 走代理，
/// 中转站（URL 路径里才含 github.com）直连，避免双重代理
pub fn clone_env(url: &str) -> HashMap<String, String> {
    let mut env = build_env();
    if !is_github_host(&host_of(url)) {
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env.remove(k);
        }
    }
    env
}

/// 是否 github.com 官方地址
pub fn is_github_url(url: &str) -> bool {
    is_github_host(&host_of(url))
}
