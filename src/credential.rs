//! 认证管理：gh 登录状态、PAT 登录/登出、.gitconfig 整文件写入。
//! 与 Python 版 credential.py 对应。

use crate::config;
use crate::paths;
use crate::runner;
use regex::Regex;
use serde_json::Value;
use std::sync::{Mutex, OnceLock};

fn auth_cache() -> &'static Mutex<(Option<String>, f64)> {
    static C: OnceLock<Mutex<(Option<String>, f64)>> = OnceLock::new();
    // 第二个字段语义：缓存过期时刻（时间戳），不是写入时刻
    C.get_or_init(|| Mutex::new((None, 0.0)))
}

fn hosts_yml_path() -> std::path::PathBuf {
    paths::gh_config_dir().join("hosts.yml")
}

/// 本地快速判断（不联网）：hosts.yml 有 github.com 主机条目即认为登录过。
pub fn get_auth_state_fast() -> String {
    let p = hosts_yml_path();
    if let Ok(text) = std::fs::read_to_string(&p) {
        let re = Regex::new(r"(?m)^github\.com:").unwrap();
        if re.is_match(&text) {
            return "logged_in".into();
        }
    }
    "not_logged_in".into()
}

/// 返回: "logged_in" | "expired" | "not_logged_in" | "unknown"。
/// - logged_in（联网验证通过）：缓存 30 分钟
/// - expired（GitHub 明确回复 401，token 真失效）：缓存 5 分钟
/// - unknown（联网验证本身失败：代理抽风/超时/连接中断）：只缓存 60 秒，
///   避免一次网络波动被误判成"未登录"后卡住半小时；网络恢复后快速自愈
pub async fn get_auth_state(force: bool) -> String {
    let now = chrono::Local::now().timestamp() as f64;
    {
        let c = auth_cache().lock().unwrap();
        if !force {
            if let Some(state) = &c.0 {
                if now < c.1 {
                    return state.clone();
                }
            }
        }
    }
    // 未登录过就不用联网验证了
    if get_auth_state_fast() == "not_logged_in" {
        let mut c = auth_cache().lock().unwrap();
        c.0 = Some("not_logged_in".into());
        c.1 = now + 1800.0;
        return "not_logged_in".into();
    }
    let (rc, out) = runner::run_gh(&["auth".into(), "status".into()], Some(15.0)).await;
    let (state, ttl): (String, f64) = if rc == 0 {
        ("logged_in".into(), 1800.0)
    } else {
        let low = out.to_lowercase();
        let real_reject = low.contains("bad credentials")
            || low.contains("http 401")
            || low.contains("returned error: 401")
            || (low.contains("token") && low.contains("invalid"));
        if real_reject {
            ("expired".into(), 300.0)
        } else {
            // 超时 / 连接失败 / 代理异常等：不算未登录，标记为"无法验证"
            ("unknown".into(), 60.0)
        }
    };
    let mut c = auth_cache().lock().unwrap();
    c.0 = Some(state.clone());
    c.1 = now + ttl;
    state
}

pub fn set_auth_cache(state: &str) {
    let mut c = auth_cache().lock().unwrap();
    c.0 = Some(state.to_string());
    c.1 = chrono::Local::now().timestamp() as f64 + 1800.0;
}

async fn gh_api_user() -> (String, String) {
    let (rc, out) = runner::run_gh(
        &["api".into(), "user".into(), "--jq".into(), "{id: .id, login: .login}".into()],
        Some(15.0),
    )
    .await;
    if rc != 0 {
        return (String::new(), String::new());
    }
    match serde_json::from_str::<Value>(&out) {
        Ok(v) => {
            let login = v.get("login").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let id = v
                .get("id")
                .and_then(|x| x.as_i64().map(|n| n.to_string()))
                .unwrap_or_default();
            (login, id)
        }
        Err(_) => (String::new(), String::new()),
    }
}

pub async fn get_username() -> String {
    gh_api_user().await.0
}

/// 登录：一次输入 token，git 和 gh 同时生效
pub async fn login(token: &str) -> Value {
    if token.trim().is_empty() {
        return serde_json::json!({"ok": false, "error": "请输入 token"});
    }
    let gh = paths::gh_exe().to_string_lossy().into_owned();
    let (rc, out) = runner::run(
        &[gh.clone(), "auth".into(), "login".into(), "--with-token".into()],
        Some(30.0),
        Some(token.trim().to_string()),
        None,
    )
    .await;
    if rc != 0 {
        return serde_json::json!({"ok": false, "error": out.trim().to_string()});
    }
    let (rc, out) = runner::run_gh(&["auth".into(), "setup-git".into()], Some(15.0)).await;
    if rc != 0 {
        return serde_json::json!({"ok": false, "error": out.trim().to_string()});
    }

    let (username, id) = gh_api_user().await;
    let email = if !id.is_empty() {
        format!("{id}+{username}@users.noreply.github.com")
    } else {
        String::new()
    };
    if !username.is_empty() {
        let git = paths::git_exe().to_string_lossy().into_owned();
        runner::run(
            &[git.clone(), "config".into(), "--global".into(), "user.name".into(), username.clone()],
            Some(10.0),
            None,
            None,
        )
        .await;
        runner::run(
            &[git, "config".into(), "--global".into(), "user.email".into(), email.clone()],
            Some(10.0),
            None,
            None,
        )
        .await;
    }

    write_gitconfig_defaults(&username, &email, true);
    set_auth_cache("logged_in");
    if !username.is_empty() {
        config::update_config(&serde_json::json!({"auth": {"username": username.clone()}}));
    }
    serde_json::json!({"ok": true, "username": username})
}

pub async fn logout() -> Value {
    let gh = paths::gh_exe().to_string_lossy().into_owned();
    runner::run(
        &[gh, "auth".into(), "logout".into(), "--hostname".into(), "github.com".into()],
        Some(15.0),
        Some("y\n".into()),
        None,
    )
    .await;
    write_gitconfig_without_credentials();
    set_auth_cache("not_logged_in");
    serde_json::json!({"ok": true})
}

fn gitconfig_path() -> std::path::PathBuf {
    paths::git_home_dir().join(".gitconfig")
}

/// 登出后保留常规配置，只移除凭据 helper 段
fn write_gitconfig_without_credentials() {
    let path = gitconfig_path();
    if !path.is_file() {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(&path) {
        let re = Regex::new(r#"(?s)\[credential "https://github\.com"\][^\[]*"#).unwrap();
        let out = re.replace_all(&text, "");
        let _ = std::fs::write(&path, out.as_bytes());
    }
}

/// 直接重写 .gitconfig 保证段落顺序：空 helper 必须排在 URL 专属 helper 之前
pub fn write_gitconfig_defaults(username: &str, email: &str, allow_network: bool) {
    let path = gitconfig_path();
    let (mut uname, mut umail) = (username.to_string(), email.to_string());
    if uname.is_empty() && allow_network {
        // 仅允许联网时才询问 GitHub —— 异步函数在同步路径里不方便，
        // 这里由调用方保证：登录流程已先取过身份；启动修正路径传 allow_network=false。
        if let Ok(text) = std::fs::read_to_string(&path) {
            let m_name = Regex::new(r"(?m)name\s*=\s*(.+)").unwrap().captures(&text);
            let m_mail = Regex::new(r"(?m)email\s*=\s*(.+)").unwrap().captures(&text);
            uname = m_name.and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            umail = m_mail.and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        }
    }

    // 单引号防 sh 拆空格；正斜杠避开 git config 的反斜杠转义
    let gh = paths::gh_exe().to_string_lossy().replace('\\', "/");
    let helper = format!("!'{gh}' auth git-credential");
    let mut lines: Vec<String> = Vec::new();
    if !uname.is_empty() {
        lines.push("[user]".into());
        lines.push(format!("\tname = {uname}"));
        lines.push(format!("\temail = {umail}"));
    }
    lines.extend([
        "[core]".into(),
        "\tquotepath = false".into(),
        "[safe]".into(),
        "\tdirectory = *".into(),
        "[i18n]".into(),
        "\tlogOutputEncoding = utf-8".into(),
        "[http]".into(),
        "\tsslBackend = schannel".into(),
        "\tschannelCheckRevoke = false".into(),
        "[credential]".into(),
        "\thelper =".into(),
        "[credential \"https://github.com\"]".into(),
        format!("\thelper = {helper}"),
    ]);
    let body = lines.join("\n") + "\n";
    let _ = std::fs::write(&path, body.as_bytes());
}

/// 启动时把旧版 git config 追加出来的错误顺序修正（不联网，保留已有身份）。
pub fn fix_gitconfig_order() {
    let path = gitconfig_path();
    if !path.is_file() {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(&path) {
        let uname = Regex::new(r"(?m)name\s*=\s*(.+)")
            .unwrap()
            .captures(&text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let umail = Regex::new(r"(?m)email\s*=\s*(.+)")
            .unwrap()
            .captures(&text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        // 启动路径禁止联网：没身份就先不写 user 段
        write_gitconfig_defaults(&uname, &umail, false);
    }
}
