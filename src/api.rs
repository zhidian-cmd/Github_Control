//! HTTP API 层：与 Python 版 bridge/*.py 全部路由一一对应。
//! 前端保持原样（web/ 目录），通过同源 /api/* 调用。

use axum::extract::Query;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use tokio::time::Duration;

use crate::config;
use crate::credential;
use crate::env;
use crate::gh_runner;
use crate::git_repo;
use crate::git_rollback;
use crate::git_status_v2;
use crate::net;
use crate::paths;
use crate::repositories;
use crate::runner;
use crate::worktree_probe;

#[derive(Deserialize, Default)]
pub struct Q {
    pub path: Option<String>,
    pub file: Option<String>,
    pub commit: Option<String>,
    pub word_diff: Option<String>,
    pub n: Option<String>,
    pub id: Option<String>,
}

// ---------------- 全局状态 ----------------

/// 原生对话框需要 AppHandle（pick_folder / pick_files）
static APP_HANDLE: OnceLock<Mutex<Option<tauri::AppHandle>>> = OnceLock::new();

pub fn set_app_handle(h: tauri::AppHandle) {
    *APP_HANDLE.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(h);
}

fn app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
}

/// 克隆任务注册表 {clone_id: log_path}
static CLONE_JOBS: OnceLock<Mutex<HashMap<String, std::path::PathBuf>>> = OnceLock::new();

fn clone_jobs() -> &'static Mutex<HashMap<String, std::path::PathBuf>> {
    CLONE_JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------- 网络重试 ----------------

/// 网络抖动/间歇性 500 自动重试（间隔 2s/8s/20s），非网络类失败不重试
async fn with_net_retry<F, Fut>(mut f: F) -> (i32, String)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = (i32, String)>,
{
    let (mut rc, mut out) = f().await;
    let mut delay = 2u64;
    for _ in 0..3 {
        if rc == 0 || !net::is_net_hint(&out) {
            return (rc, out);
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 4).min(20);
        let r = f().await;
        rc = r.0;
        out = r.1;
    }
    (rc, out)
}

fn auth_hint(out: &str) -> String {
    let out = out.trim().to_string();
    if out.contains("Authentication failed") || out.contains("403") {
        return format!("{out}\n提示：可能未登录或 token 过期，请到设置页检查。");
    }
    out
}

// ---------------- 静态文件 / 首页 ----------------

const SHIM: &str = r#"<script>
/* Rust 移植版注入：把 pywebview API 桥接到本地 HTTP 接口 */
(function () {
  if (window.pywebview) return;
  window.pywebview = { api: {
    pick_folder: async function () {
      try { const r = await fetch('/api/native/pick_folder', { method: 'POST' }); const d = await r.json(); return d.path || ''; }
      catch (e) { return ''; }
    },
    pick_files: async function () {
      try { const r = await fetch('/api/native/pick_files', { method: 'POST' }); const d = await r.json(); return Array.isArray(d.files) ? d.files : []; }
      catch (e) { return []; }
    },
    save_window_state: async function (w, h, x, y) {
      try { await fetch('/api/config', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ window: { width: w, height: h, x: x, y: y } }) }); } catch (e) {}
      return true;
    }
  } };

  /* target=_blank 链接：Tauri WebView 不弹新窗口，转交系统默认浏览器打开 */
  document.addEventListener('click', function (e) {
    var a = e.target && e.target.closest ? e.target.closest('a[target="_blank"]') : null;
    if (!a) return;
    var href = a.getAttribute('href') || '';
    if (!href) return;
    e.preventDefault();
    fetch('/api/gh/open_url', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ url: href }) })
      .then(function (r) { return r.json(); })
      .then(function (d) { if (!d.ok) alert('打开失败：' + (d.error || '未知错误')); })
      .catch(function () {});
  });
})();
</script>"#;

/// 首页：注入 pywebview 桥接 shim（不修改 web/ 目录里的源文件）
async fn index() -> Response {
    let html_path = paths::web_dir().join("index.html");
    match std::fs::read_to_string(&html_path) {
        Ok(html) => {
            let out = if html.contains("</head>") {
                html.replacen("</head>", &format!("{SHIM}</head>"), 1)
            } else {
                format!("{SHIM}{html}")
            };
            no_store_response(out, "text/html; charset=utf-8")
        }
        Err(_) => json_response(StatusCode::NOT_FOUND, json!({"ok": false, "error": "index.html 不存在"})),
    }
}

async fn favicon() -> Response {
    let ico = paths::assets_dir().join("favicon.ico");
    match std::fs::read(&ico) {
        Ok(b) => no_store_response_bytes(b, "image/x-icon"),
        Err(_) => json_response(StatusCode::NOT_FOUND, json!({"ok": false, "error": "favicon 不存在"})),
    }
}

fn content_type(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if lower.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if lower.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if lower.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if lower.ends_with(".ico") {
        "image/x-icon"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "application/octet-stream"
    }
}

/// 静态文件（路径穿越防护：解析后必须位于 web 目录内）。
/// 作为 fallback 处理所有未匹配路径，路径直接取自 Request URI，
/// 不依赖 axum wildcard 的捕获语义。
async fn static_files(req: axum::extract::Request) -> Response {
    let path = req
        .uri()
        .path()
        .trim_start_matches('/')
        .to_string();
    let web = paths::web_dir();
    let candidate = web.join(&path);
    let canon_c = std::fs::canonicalize(&candidate);
    let canon_w = std::fs::canonicalize(&web);
    let ok = match (canon_c.as_ref(), canon_w.as_ref()) {
        (Ok(c), Ok(w)) => c.starts_with(w),
        _ => false,
    };
    if !ok {
        return json_response(StatusCode::NOT_FOUND, json!({"ok": false, "error": "文件不存在"}));
    }
    match std::fs::read(&candidate) {
        Ok(b) => {
            // ETag = mtime + 长度：文件没变就 304，WebView2 不再重复下载
            // app.js/style.css（启动提速）；no-cache 保证改动立即生效。
            let (mtime, flen) = std::fs::metadata(&candidate)
                .map(|m| {
                    (
                        m.modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        m.len(),
                    )
                })
                .unwrap_or((0, 0));
            let etag = format!("\"{mtime:x}-{flen:x}\"");
            let matches = req
                .headers()
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(',').any(|t| t.trim() == etag))
                .unwrap_or(false);
            if matches {
                let mut resp = Response::new(axum::body::Body::empty());
                if let Ok(v) = HeaderValue::from_str(&etag) {
                    resp.headers_mut().insert(header::ETAG, v);
                }
                if let Ok(v) = HeaderValue::from_str("no-cache") {
                    resp.headers_mut().insert(header::CACHE_CONTROL, v);
                }
                return StatusCode::NOT_MODIFIED.into_response();
            }
            let mut resp = no_store_response_bytes(b, content_type(&path));
            if let Ok(v) = HeaderValue::from_str(&etag) {
                resp.headers_mut().insert(header::ETAG, v);
            }
            if let Ok(v) = HeaderValue::from_str("no-cache") {
                resp.headers_mut().insert(header::CACHE_CONTROL, v);
            }
            resp
        }
        Err(_) => json_response(StatusCode::NOT_FOUND, json!({"ok": false, "error": "文件不存在"})),
    }
}

fn no_store_response(body: String, ct: &str) -> Response {
    no_store_response_bytes(body.into_bytes(), ct)
}

fn no_store_response_bytes(body: Vec<u8>, ct: &str) -> Response {
    let mut resp = Response::new(axum::body::Body::from(body));
    if let Ok(v) = HeaderValue::from_str(ct) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str("no-store") {
        resp.headers_mut().insert(header::CACHE_CONTROL, v);
    }
    resp
}

fn json_response(status: StatusCode, v: Value) -> Response {
    (status, Json(v)).into_response()
}

fn ok(v: Value) -> Response {
    json_response(StatusCode::OK, v)
}

// ---------------- 原生对话框（替代 pywebview） ----------------

async fn native_pick_folder() -> Response {
    let app = app_handle();
    let path = app
        .and_then(|a| {
            use tauri_plugin_dialog::DialogExt;
            a.dialog()
                .file()
                .set_title("选择文件夹")
                .blocking_pick_folder()
                .and_then(|p| p.as_path().map(|pp| pp.to_string_lossy().into_owned()))
        })
        .unwrap_or_default();
    ok(json!({"ok": true, "path": path}))
}

async fn native_pick_files() -> Response {
    let app = app_handle();
    let files: Vec<String> = app
        .and_then(|a| {
            use tauri_plugin_dialog::DialogExt;
            a.dialog()
                .file()
                .set_title("选择要上传的文件")
                .blocking_pick_files()
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| p.as_path().map(|pp| pp.to_string_lossy().into_owned()))
                        .collect()
                })
        })
        .unwrap_or_default();
    ok(json!({"ok": true, "files": files}))
}

// ---------------- /api/health & /api/client_log ----------------

async fn health() -> Response {
    ok(json!({"ok": true}))
}

async fn client_log(Json(d): Json<Value>) -> Response {
    let msg = d.get("msg").and_then(|v| v.as_str()).unwrap_or("");
    let stack = d.get("stack").and_then(|v| v.as_str()).unwrap_or("");
    runner::log_error("frontend", &format!("{msg}\n{stack}"));
    ok(json!({"ok": true}))
}

// ---------------- /api/auth/* ----------------

async fn auth_status() -> Response {
    let state = credential::get_auth_state_fast();
    let mut resp = json!({"ok": true, "state": state});
    if state == "logged_in" {
        let username = config::load_config(false)["auth"]["username"]
            .as_str()
            .unwrap_or("")
            .to_string();
        resp["username"] = json!(username);
    }
    ok(resp)
}

async fn auth_verify() -> Response {
    let state = credential::get_auth_state(true).await;
    let username = if state == "logged_in" {
        credential::get_username().await
    } else {
        String::new()
    };
    if !username.is_empty() {
        config::update_config(&json!({"auth": {"username": username.clone()}}));
    }
    ok(json!({"ok": true, "state": state, "username": username}))
}

async fn auth_login(Json(d): Json<Value>) -> Response {
    let token = d.get("token").and_then(|v| v.as_str()).unwrap_or("");
    ok(credential::login(token).await)
}

async fn auth_logout() -> Response {
    ok(credential::logout().await)
}

// ---------------- /api/config/* ----------------

async fn config_get() -> Response {
    ok(json!({"ok": true, "config": config::load_config(false)}))
}

async fn config_set(Json(d): Json<Value>) -> Response {
    ok(json!({"ok": true, "config": config::update_config(&d)}))
}

async fn config_versions() -> Response {
    let git = paths::git_exe().to_string_lossy().into_owned();
    let gh = paths::gh_exe().to_string_lossy().into_owned();
    let (grc, gout) = runner::run(&[git], Some(10.0), None, None).await;
    let (hrc, hout) = runner::run(&[gh], Some(10.0), None, None).await;
    let git_v = if grc == 0 && !gout.is_empty() {
        gout.lines().next().unwrap_or("不可用").to_string()
    } else {
        "不可用".into()
    };
    let gh_v = if hrc == 0 && !hout.is_empty() {
        hout.lines().next().unwrap_or("不可用").to_string()
    } else {
        "不可用".into()
    };
    ok(json!({"ok": true, "git": git_v, "gh": gh_v}))
}

async fn config_proxy_test() -> Response {
    let cfg = config::load_config(false);
    let url = cfg["proxy"]["url"].as_str().unwrap_or("").to_string();
    let timeout = cfg["proxy"]["test_timeout"].as_f64().unwrap_or(2.0);
    let reachable = net::is_proxy_reachable(&url, timeout);
    if !reachable {
        return ok(json!({"ok": false, "error": format!("代理 {url} 无法连接")}));
    }
    let (rc, out) = runner::run_gh(&["auth".into(), "status".into()], Some(20.0)).await;
    if rc == 0 {
        ok(json!({"ok": true, "gh_ok": true}))
    } else {
        let err: String = out.chars().take(300).collect();
        ok(json!({"ok": true, "gh_ok": false, "error": format!("代理端口可达，但 gh 联网失败：{err}")}))
    }
}

// ---------------- /api/gh/* ----------------

async fn gh_release(Json(d): Json<Value>) -> Response {
    let tag = d.get("tag").and_then(|v| v.as_str()).unwrap_or("");
    let files: Vec<String> = d
        .get("files")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let title = d.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let notes = d.get("notes").and_then(|v| v.as_str()).unwrap_or("");
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    ok(gh_runner::release_create(tag, &files, title, notes, repo).await)
}

async fn gh_release_delete(Json(d): Json<Value>) -> Response {
    let tag = d.get("tag").and_then(|v| v.as_str()).unwrap_or("");
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    ok(gh_runner::release_delete(tag, repo).await)
}

async fn gh_releases(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    ok(json!({"ok": true, "releases": gh_runner::release_list(&repo).await}))
}

async fn gh_open_web(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !std::path::Path::new(&path).is_dir() {
        return ok(json!({"ok": false, "error": "仓库目录不存在"}));
    }
    let gh = paths::gh_exe().to_string_lossy().into_owned();
    let mut cmd = tokio::process::Command::new(&gh);
    cmd.args(["repo", "view", "--web"])
        .current_dir(&path)
        .envs(env::build_env());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let result = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await;
    match result {
        Ok(Ok(o)) => {
            if o.status.success() {
                ok(json!({"ok": true}))
            } else {
                let err = String::from_utf8_lossy(&o.stderr).into_owned();
                let err2 = String::from_utf8_lossy(&o.stdout).into_owned();
                let combined = format!("{}{}", err2, err).trim().to_string();
                ok(json!({"ok": false, "error": if combined.is_empty() { "打开失败（可能没有设置远程仓库）".to_string() } else { combined }}))
            }
        }
        Ok(Err(e)) => ok(json!({"ok": false, "error": e.to_string()})),
        Err(_) => ok(json!({"ok": false, "error": "打开超时"})),
    }
}

/// 在系统默认浏览器中打开外部 URL。
/// Tauri WebView 默认不弹新窗口，前端 target=_blank 链接由 SHIM 拦截后走这里。
async fn gh_open_url(Json(d): Json<Value>) -> Response {
    let url = d.get("url").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() > 2048 {
        return ok(json!({"ok": false, "error": "仅支持 http/https 链接"}));
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let mut u: Vec<u16> = url.encode_utf16().collect();
        u.push(0);
        let mut op: Vec<u16> = "open".encode_utf16().collect();
        op.push(0);
        let r = unsafe {
            ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), u.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL as i32)
        };
        if r as isize <= 32 {
            return ok(json!({"ok": false, "error": "系统未找到可用的浏览器"}));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
    ok(json!({"ok": true}))
}

// ---------------- /api/rollback/* ----------------

async fn rollback_execute(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let commit = d.get("commit").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let mode = d.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    if commit.is_empty() || mode.is_empty() {
        return ok(json!({"ok": false, "error": "缺少参数"}));
    }
    match mode {
        "hard" => match git_rollback::hard_reset(&repo, &commit, true).await {
            Ok(v) => ok(v),
            Err(e) => ok(json!({"ok": false, "error": e})),
        },
        "soft" => ok(git_rollback::soft_reset(&repo, &commit).await),
        "mixed" => match git_rollback::mixed_reset(&repo, &commit, true).await {
            Ok(v) => ok(v),
            Err(e) => ok(json!({"ok": false, "error": e})),
        },
        "revert" => ok(git_rollback::revert(&repo, &commit).await),
        _ => ok(json!({"ok": false, "error": format!("未知回退方式 {mode}")})),
    }
}

async fn rollback_recovery(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let r = git_rollback::list_recovery_points(&repo).await;
    ok(json!({"ok": true, "backups": r["backups"].clone(), "reflog": r["reflog"].clone()}))
}

async fn rollback_recover(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let commit = d.get("commit").and_then(|v| v.as_str()).unwrap_or("").to_string();
    match git_rollback::recover_to(&repo, &commit, true).await {
        Ok(v) => ok(v),
        Err(e) => ok(json!({"ok": false, "error": e})),
    }
}

// ---------------- /api/git/* ----------------

async fn git_commit(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let msg = d
        .get("summary")
        .or_else(|| d.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if msg.is_empty() {
        return ok(json!({"ok": false, "error": "请填写提交摘要"}));
    }
    // 无暂存概念：提交即包含全部改动
    runner::run_git(&["add".into(), "-A".into()], Some(&repo), None).await;
    let desc = d.get("description").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let mut args = vec!["commit".into(), "-m".into(), msg];
    if !desc.is_empty() {
        args.push("-m".into());
        args.push(desc);
    }
    let (rc, out) = runner::run_git(&args, Some(&repo), None).await;
    ok(json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { json!(out.trim()) }}))
}

async fn git_restore(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let file = d.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (rc, out) = runner::run_git(&["restore".into(), "--".into(), file], Some(&repo), None).await;
    ok(json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { json!(out.trim()) }}))
}

async fn git_diff(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let file = q.file.unwrap_or_default();
    let wd = q.word_diff.as_deref() == Some("1");
    let text = git_repo::get_diff(&repo, if file.is_empty() { None } else { Some(&file) }, wd).await;
    let lines = if text.is_empty() { 0 } else { text.matches('\n').count() + 1 };
    ok(json!({"ok": true, "diff": text, "lines": lines}))
}

async fn git_commit_detail(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let commit = q.commit.unwrap_or_default();
    match git_repo::get_commit_detail(&repo, &commit).await {
        Ok(mut v) => {
            v["ok"] = json!(true);
            ok(v)
        }
        Err(e) => ok(json!({"ok": false, "error": e})),
    }
}

async fn git_has_origin(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let (rc, _) = runner::run_git(&["remote".into(), "get-url".into(), "origin".into()], Some(&repo), None).await;
    ok(json!({"ok": true, "has_origin": rc == 0}))
}

async fn git_pull(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if let Some(err) = net::proxy_preflight() {
        return ok(json!({"ok": false, "error": err}));
    }
    // 显式指定 origin <branch>：分支还没关联上游时，裸 `git pull` 会直接报
    // "no tracking information"，而推送又要求先更新 —— 形成死循环。
    let branch = git_repo::get_current_branch(&repo).await;
    if branch.is_empty() {
        return ok(json!({"ok": false, "error": "无法确定当前分支"}));
    }
    let repo2 = repo.clone();
    let branch2 = branch.clone();
    let (rc, out) = with_net_retry(|| {
        let repo = repo2.clone();
        let branch = branch2.clone();
        async move {
            runner::run_git(
                &[
                    "pull".into(),
                    "--no-edit".into(),
                    "--no-rebase".into(),
                    "origin".into(),
                    branch,
                ],
                Some(&repo),
                Some(600.0),
            )
            .await
        }
    })
    .await;
    if rc == 0 {
        return ok(json!({"ok": true}));
    }
    let low = out.to_lowercase();
    // 远端还没有这个分支（空仓库或未推送过）：无需更新，直接推送即可
    if low.contains("couldn't find remote ref") {
        return ok(json!({"ok": false, "error": format!("远程还没有 {branch} 分支，无需更新，直接推送即可")}));
    }
    // 远端是网页端独立创建的（如自带 README 的首次提交）：与本地无共同祖先
    if low.contains("refusing to merge unrelated histories") {
        let repo3 = repo.clone();
        let branch3 = branch.clone();
        let (rc2, out2) = with_net_retry(|| {
            let repo = repo3.clone();
            let branch = branch3.clone();
            async move {
                runner::run_git(
                    &[
                        "pull".into(),
                        "--no-edit".into(),
                        "--allow-unrelated-histories".into(),
                        "origin".into(),
                        branch,
                    ],
                    Some(&repo),
                    Some(600.0),
                )
                .await
            }
        })
        .await;
        return ok(json!({"ok": rc2 == 0, "error": if rc2 == 0 { Value::Null } else { json!(auth_hint(&out2)) }}));
    }
    ok(json!({"ok": false, "error": auth_hint(&out)}))
}

async fn git_push(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let create = d.get("create").cloned();
    let branch = match d
        .get("branch")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
    {
        Some(b) => b,
        None => git_repo::get_current_branch(&repo).await,
    };
    if branch.is_empty() {
        return ok(json!({"ok": false, "error": "无法确定当前分支"}));
    }

    let (rc, _) = runner::run_git(&["remote".into(), "get-url".into(), "origin".into()], Some(&repo), None).await;
    let mut has_origin = rc == 0;
    if create.is_some() && has_origin {
        // 旧的远程是死地址（仓库已被删），先移除再重建
        runner::run_git(&["remote".into(), "remove".into(), "origin".into()], Some(&repo), None).await;
        has_origin = false;
    }
    if !has_origin {
        let create = match &create {
            Some(c) => c.clone(),
            None => {
                return ok(json!({"ok": false, "error": "本地仓库还没有远程地址，无法推送"}));
            }
        };
        let name = create
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let re = regex::Regex::new(r"^[A-Za-z0-9._-]+$").unwrap();
        if !re.is_match(&name) {
            return ok(json!({"ok": false, "error": "仓库名只能用英文、数字、-、_、.（GitHub 不接受中文名）"}));
        }
        if let Some(err) = net::proxy_preflight() {
            return ok(json!({"ok": false, "error": err}));
        }
        if credential::get_auth_state(false).await != "logged_in" {
            return ok(json!({"ok": false, "error": "创建远程仓库需要先登录 GitHub"}));
        }
        let vis = if create.get("private").and_then(|v| v.as_bool()).unwrap_or(false) {
            "--private"
        } else {
            "--public"
        };
        let (rc, out) = runner::run_gh(
            &[
                "repo".into(), "create".into(), name.clone(), "--source".into(), repo.clone(),
                "--remote".into(), "origin".into(), vis.into(), "--push".into(),
            ],
            Some(600.0),
        )
        .await;
        if rc != 0 {
            if out.to_lowercase().contains("already exists") {
                // 账号下已有同名仓库：不创建，直接把它设为 origin 推送
                let (rc2, login_out) = runner::run_gh(
                    &["api".into(), "user".into(), "-q".into(), ".login".into()],
                    Some(60.0),
                )
                .await;
                let login = if rc2 == 0 { login_out.trim().to_string() } else { String::new() };
                if login.is_empty() {
                    return ok(json!({"ok": false, "error": "远程已有同名仓库，但获取账号名失败，请到设置里手动填写远程地址"}));
                }
                let url = format!("https://github.com/{login}/{name}.git");
                let (rca, _) = runner::run_git(&["remote".into(), "add".into(), "origin".into(), url.clone()], Some(&repo), None).await;
                if rca != 0 {
                    runner::run_git(&["remote".into(), "set-url".into(), "origin".into(), url], Some(&repo), None).await;
                }
                let repo2 = repo.clone();
                let branch2 = branch.clone();
                let (rc3, out3) = with_net_retry(|| {
                    let repo = repo2.clone();
                    let branch = branch2.clone();
                    async move {
                        runner::run_git(
                            &["push".into(), "--force-with-lease".into(), "-u".into(), "origin".into(), branch],
                            Some(&repo),
                            Some(600.0),
                        )
                        .await
                    }
                })
                .await;
                if rc3 != 0 {
                    return ok(json!({"ok": false, "error": auth_hint(&out3)}));
                }
                return ok(json!({"ok": true, "hint": format!("远程仓库 {login}/{name} 已存在，已直接推送")}));
            }
            return ok(json!({"ok": false, "error": out.trim()}));
        }
        let vis_cn = if create.get("private").and_then(|v| v.as_bool()).unwrap_or(false) {
            "私有"
        } else {
            "公开"
        };
        return ok(json!({"ok": true, "hint": format!("已创建远程仓库 {name}（{vis_cn}）并推送")}));
    }

    if let Some(err) = net::proxy_preflight() {
        return ok(json!({"ok": false, "error": err}));
    }
    let upstream = git_repo::has_upstream(&repo, &branch).await;
    let repo2 = repo.clone();
    let branch2 = branch.clone();
    let (rc, out) = with_net_retry(|| {
        let repo = repo2.clone();
        let branch = branch2.clone();
        async move {
            if upstream {
                runner::run_git(
                    &["push".into(), "--force-with-lease".into(), "origin".into(), branch],
                    Some(&repo),
                    Some(600.0),
                )
                .await
            } else {
                runner::run_git(
                    &["push".into(), "--force-with-lease".into(), "-u".into(), "origin".into(), branch],
                    Some(&repo),
                    Some(600.0),
                )
                .await
            }
        }
    })
    .await;
    ok(json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { json!(auth_hint(&out)) }}))
}

/// 历史重写前把未提交改动（含未跟踪）自动储藏。返回是否储藏了。
async fn stash_if_dirty(repo: &str) -> bool {
    let st = match git_repo::get_status(repo).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    if st["files"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return false;
    }
    let (rc, _) = runner::run_git(
        &["stash".into(), "push".into(), "-u".into(), "-m".into(), "auto before history rewrite".into()],
        Some(repo),
        None,
    )
    .await;
    rc == 0
}

/// 把自动储藏弹回工作区；弹回冲突时 git 会自动保留 stash，不丢数据。
async fn pop_stash(repo: &str, stashed: bool) {
    if stashed {
        runner::run_git(&["stash".into(), "pop".into()], Some(repo), None).await;
    }
}

async fn finish_delete(repo: &str, stashed: bool, base_hint: String) -> Response {
    let mut hint = base_hint;
    if stashed {
        let (rc, _) = runner::run_git(&["stash".into(), "pop".into()], Some(repo), None).await;
        if rc == 0 {
            hint += "；未提交改动已自动恢复";
        } else {
            hint += "；未提交改动在储藏里（右键历史提交时的「恢复改动」或手动 stash pop 可找回）";
        }
    }
    ok(json!({"ok": true, "hint": hint}))
}

/// 历史提交右键操作：检出（游离查看）/ 清空当前历史（保留文件）
async fn git_commit_ops(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let op = d.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let commit = d.get("commit").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();

    if op == "checkout" {
        if commit.is_empty() {
            return ok(json!({"ok": false, "error": "缺少 commit"}));
        }
        let status = git_repo::get_status(&repo).await.unwrap_or_else(|_| json!({"files": []}));
        let mut stashed = false;
        if !status["files"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
            runner::run_git(&["stash".into(), "push".into(), "-m".into(), "auto before checkout".into()], Some(&repo), None).await;
            stashed = true;
        }
        let (rc, out) = runner::run_git(&["checkout".into(), commit], Some(&repo), None).await;
        if rc != 0 {
            return ok(json!({"ok": false, "error": out.trim()}));
        }
        let hint = if stashed { "已自动储藏改动，切回分支后可恢复".to_string() } else { String::new() };
        return ok(json!({"ok": true, "hint": hint}));
    }

    if op == "delete_one" {
        // 只删除选中的这一个提交，其余重放（rebase --onto）；硬删除前打保险 tag
        let (rc, _) = runner::run_git(&["rev-parse".into(), "--verify".into(), "HEAD".into()], Some(&repo), None).await;
        if rc != 0 {
            return ok(json!({"ok": false, "error": "仓库还没有提交"}));
        }
        let (rc, _) = runner::run_git(&["merge-base".into(), "--is-ancestor".into(), commit.clone(), "HEAD".into()], Some(&repo), None).await;
        if rc != 0 {
            return ok(json!({"ok": false, "error": "只能删除当前分支历史中的提交"}));
        }
        let (_, head) = runner::run_git(&["rev-parse".into(), "HEAD".into()], Some(&repo), None).await;
        let is_head = commit == head.trim();
        let (rc, parent) = runner::run_git(&["rev-parse".into(), format!("{commit}~1")], Some(&repo), None).await;
        let is_root = rc != 0;
        let parent = parent.trim().to_string();
        let cur = git_repo::get_current_branch(&repo).await;
        let branch = if cur.is_empty() { "main".to_string() } else { cur };

        if is_head && is_root {
            // 不打任何 tag：仓库原封不动，只拒绝
            return ok(json!({"ok": false, "error": "这是仓库唯一的提交，删除它等于清空全部历史；请改用硬回退或在确认后手动处理"}));
        }

        if is_head {
            // 删最新一条：退到上一位。保险 tag 由 hard_reset 内部统一打，避免同秒撞名
            return match git_rollback::hard_reset(&repo, &parent, true).await {
                Ok(r) => ok(json!({"ok": true, "hint": format!("已删除该提交，保险 tag {} 可在紧急恢复中找回", r["backup_tag"].as_str().unwrap_or(""))})),
                Err(e) => ok(json!({"ok": false, "error": e})),
            };
        }

        // rebase 类路径：这里才打本路径自己的保险 tag
        let tag = format!("_backup_{}", paths::now_compact());
        runner::run_git(&["tag".into(), tag.clone(), "HEAD".into()], Some(&repo), None).await;

        if is_root {
            // 删根提交：把第二条提交重铸为无父新根，其余提交重放到它上面
            let (rc, child) = runner::run_git(
                &["rev-list".into(), "--ancestry-path".into(), "--reverse".into(), format!("{commit}..HEAD")],
                Some(&repo),
                None,
            )
            .await;
            if rc != 0 || child.trim().is_empty() {
                return ok(json!({"ok": false, "error": "无法定位根提交的下一条"}));
            }
            let child = child.lines().next().unwrap_or("").trim().to_string();
            let (rc, tree) = runner::run_git(&["log".into(), "--format=%T".into(), "-1".into(), child.clone()], Some(&repo), None).await;
            let (rc2, msg) = runner::run_git(&["log".into(), "--format=%s".into(), "-1".into(), child.clone()], Some(&repo), None).await;
            if rc != 0 || rc2 != 0 {
                return ok(json!({"ok": false, "error": "读取下一条提交信息失败"}));
            }
            let (rc, new_root) = runner::run_git(
                &["commit-tree".into(), tree.trim().to_string(), "-m".into(), msg.trim().to_string()],
                Some(&repo),
                None,
            )
            .await;
            if rc != 0 {
                return ok(json!({"ok": false, "error": if new_root.trim().is_empty() { "重铸新根失败" } else { new_root.trim() }}));
            }
            let stashed = stash_if_dirty(&repo).await;
            let (rc, _out) = runner::run_git(
                &["rebase".into(), "--onto".into(), new_root.trim().to_string(), child, branch],
                Some(&repo),
                Some(300.0),
            )
            .await;
            if rc != 0 {
                runner::run_git(&["rebase".into(), "--abort".into()], Some(&repo), None).await;
                pop_stash(&repo, stashed).await;
                return ok(json!({"ok": false, "error": "删除该提交会引发内容冲突，已自动恢复原状，什么都没有改"}));
            }
            return finish_delete(&repo, stashed, format!("已删除根提交，下一条提交成为新根；保险 tag {tag} 可找回")).await;
        }

        // 中间的提交：rebase --onto 重放后续提交
        let stashed = stash_if_dirty(&repo).await;
        let (rc, _out) = runner::run_git(
            &["rebase".into(), "--onto".into(), parent, commit, branch],
            Some(&repo),
            Some(300.0),
        )
        .await;
        if rc != 0 {
            runner::run_git(&["rebase".into(), "--abort".into()], Some(&repo), None).await;
            pop_stash(&repo, stashed).await;
            return ok(json!({"ok": false, "error": "删除该提交会引发内容冲突，已自动恢复原状，什么都没有改"}));
        }
        return finish_delete(&repo, stashed, format!("已删除该提交，其余提交已重放；保险 tag {tag} 可找回")).await;
    }
    ok(json!({"ok": false, "error": format!("未知操作 {op}")}))
}

async fn git_branch(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let action = d.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let (rc, out) = match action {
        "switch" => {
            // 未提交改动时先 stash
            let status = git_repo::get_status(&repo).await.unwrap_or_else(|_| json!({"files": []}));
            let stashed = !status["files"].as_array().map(|a| a.is_empty()).unwrap_or(true);
            if stashed {
                runner::run_git(&["stash".into(), "push".into(), "-m".into(), "auto before switch".into()], Some(&repo), None).await;
            }
            let (rc, out) = runner::run_git(&["switch".into(), name], Some(&repo), None).await;
            if rc == 0 && stashed {
                return ok(json!({"ok": true, "hint": "已自动储藏改动，稍后可点 [恢复储藏] 找回"}));
            }
            (rc, out)
        }
        "delete" => {
            let force = d.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let flag = if force { "-D" } else { "-d" };
            runner::run_git(&["branch".into(), flag.into(), name], Some(&repo), None).await
        }
        "create" => {
            let start = d.get("start").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if start.is_empty() {
                runner::run_git(&["branch".into(), name], Some(&repo), None).await
            } else {
                runner::run_git(&["branch".into(), name, start], Some(&repo), None).await
            }
        }
        "stash" => {
            let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("stash").to_string();
            runner::run_git(&["stash".into(), "push".into(), "-m".into(), msg], Some(&repo), None).await
        }
        "stash_pop" => runner::run_git(&["stash".into(), "pop".into()], Some(&repo), None).await,
        "tag" => runner::run_git(&["tag".into(), name], Some(&repo), None).await,
        _ => return ok(json!({"ok": false, "error": format!("未知操作 {action}")})),
    };
    ok(json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { json!(out.trim()) }}))
}

async fn git_commit_file_diff(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let commit = q.commit.unwrap_or_default();
    let file = q.file.unwrap_or_default();
    let (rc, out) = runner::run_git(
        &["show".into(), commit, "--format=".into(), "--".into(), file],
        Some(&repo),
        None,
    )
    .await;
    ok(json!({
        "ok": rc == 0,
        "diff": if rc != 0 { Value::Null } else { json!(out.chars().take(git_repo::MAX_DIFF).collect::<String>()) }
    }))
}

async fn git_file_stats(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let file = q.file.unwrap_or_default();
    let mut v = git_repo::get_file_stats(&repo, &file).await;
    v["ok"] = json!(true);
    ok(v)
}

async fn git_commit_stats(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let commit = q.commit.unwrap_or_default();
    match git_repo::get_commit_stats(&repo, &commit).await {
        Ok(mut v) => {
            v["ok"] = json!(true);
            ok(v)
        }
        Err(e) => ok(json!({"ok": false, "error": e})),
    }
}

/// 提交历史 numstat 缓存：按 (repo, n) 键 + HEAD 校验。
/// HEAD 未变时历史数据必然不变（工作区改动不影响已提交的 numstat），
/// 直接返回缓存；HEAD 变了才重跑 git log --numstat。
static LOG_TABLE_CACHE: OnceLock<Mutex<HashMap<(String, i64), (String, Vec<Value>)>>> = OnceLock::new();

async fn cached_log_table(repo: &str, n: i64) -> Vec<Value> {
    // HEAD 快查（rev-parse 是毫秒级，远便宜于 log --numstat）
    let (hrc, hout) = runner::run_git(&["rev-parse".into(), "HEAD".into()], Some(repo), None).await;
    let head = if hrc == 0 { hout.trim().to_string() } else { String::new() };
    let key = (repo.to_string(), n);
    let cache = LOG_TABLE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache.lock().unwrap();
        if let Some((h, commits)) = guard.get(&key) {
            if *h == head {
                return commits.clone();
            }
        }
    }
    let commits = git_repo::get_log_table(repo, n).await;
    let mut guard = cache.lock().unwrap();
    if guard.len() >= 32 {
        guard.clear();
    }
    guard.insert(key, (head, commits.clone()));
    commits
}

async fn git_log_table(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    let n = q.n.unwrap_or_else(|| "50".into()).parse::<i64>().unwrap_or(50);
    ok(json!({"ok": true, "commits": cached_log_table(&repo, n).await}))
}

async fn git_branches(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    ok(json!({"ok": true, "branches": git_repo::get_branches(&repo).await}))
}

// ---------------- 进行中操作检测 / 冲突解决 ----------------

const IN_PROGRESS_CHECKS: [(&str, &str); 6] = [
    ("MERGE_HEAD", "merge"),
    ("rebase-merge", "rebase"),
    ("rebase-apply", "rebase"),
    ("CHERRY_PICK_HEAD", "cherry-pick"),
    ("REVERT_HEAD", "revert"),
    ("BISECT_LOG", "bisect"),
];

/// 读 .git 目录判断是否有进行中的操作，不跑 git，毫秒级。
pub fn detect_in_progress(repo: &str) -> Option<String> {
    let mut gitdir = std::path::Path::new(repo).join(".git");
    if gitdir.is_file() {
        // worktree：.git 是指针文件
        if let Ok(text) = std::fs::read_to_string(&gitdir) {
            if let Some(target) = text.split("gitdir:").nth(1) {
                let target = target.trim();
                gitdir = std::path::Path::new(repo).join(target);
            }
        }
    }
    if !gitdir.is_dir() {
        return None;
    }
    for (name, state) in IN_PROGRESS_CHECKS {
        if gitdir.join(name).exists() {
            return Some(state.to_string());
        }
    }
    None
}

async fn git_in_progress(Query(q): Query<Q>) -> Response {
    let repo = q.path.unwrap_or_default();
    ok(json!({"ok": true, "state": detect_in_progress(&repo)}))
}

async fn git_in_progress_op(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let op = d.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let state = match detect_in_progress(&repo) {
        Some(s) => s,
        None => return ok(json!({"ok": false, "error": "当前没有进行中的操作"})),
    };
    let args: Vec<String> = match (op, state.as_str()) {
        ("abort", "merge") => vec!["merge".into(), "--abort".into()],
        ("abort", "rebase") => vec!["rebase".into(), "--abort".into()],
        ("abort", "cherry-pick") => vec!["cherry-pick".into(), "--abort".into()],
        ("abort", "revert") => vec!["revert".into(), "--abort".into()],
        ("abort", "bisect") => vec!["bisect".into(), "reset".into()],
        ("continue", "merge") => vec!["-c".into(), "core.editor=true".into(), "merge".into(), "--continue".into()],
        ("continue", "rebase") => vec!["-c".into(), "core.editor=true".into(), "rebase".into(), "--continue".into()],
        ("continue", "cherry-pick") => vec!["-c".into(), "core.editor=true".into(), "cherry-pick".into(), "--continue".into()],
        ("continue", "revert") => vec!["-c".into(), "core.editor=true".into(), "revert".into(), "--continue".into()],
        _ => return ok(json!({"ok": false, "error": format!("{state} 不支持继续，请用中止")})),
    };
    let (rc, out) = runner::run_git(&args, Some(&repo), Some(300.0)).await;
    ok(json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { json!(auth_hint(&out)) }}))
}

const CONFLICT_ACTION: [(&str, &str, &str); 12] = [
    ("UU", "ours", "checkout"),
    ("UU", "theirs", "checkout"),
    ("AA", "ours", "checkout"),
    ("AA", "theirs", "checkout"),
    ("DU", "ours", "remove"),
    ("DU", "theirs", "checkout"),
    ("UD", "ours", "checkout"),
    ("UD", "theirs", "remove"),
    ("AU", "ours", "checkout"),
    ("AU", "theirs", "remove"),
    ("UA", "ours", "remove"),
    ("UA", "theirs", "checkout"),
];

async fn git_conflict_resolve(Json(d): Json<Value>) -> Response {
    let repo = d.get("repo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let file = d.get("file").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let side = d.get("side").and_then(|v| v.as_str()).unwrap_or("");
    if file.is_empty() || (side != "ours" && side != "theirs") {
        return ok(json!({"ok": false, "error": "参数缺失"}));
    }
    let st = git_repo::get_status(&repo).await.unwrap_or_else(|_| json!({"files": []}));
    let conflict = st["files"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .find(|f| f["path"].as_str() == Some(file.as_str()))
        .map(|f| f["xy"].as_str().unwrap_or("UU").to_string())
        .unwrap_or_else(|| "UU".into());
    let action = CONFLICT_ACTION
        .iter()
        .find(|(c, s, _)| *c == conflict && *s == side)
        .map(|(_, _, a)| *a)
        .unwrap_or("checkout");
    let (rc, out) = if action == "remove" {
        runner::run_git(&["rm".into(), "-f".into(), "--".into(), file.clone()], Some(&repo), None).await
    } else {
        let flag = format!("--{side}");
        let (rc, out) = runner::run_git(&["checkout".into(), flag, "--".into(), file.clone()], Some(&repo), None).await;
        if rc == 0 {
            runner::run_git(&["add".into(), "--".into(), file], Some(&repo), None).await
        } else {
            (rc, out)
        }
    };
    if rc != 0 {
        let err = out.trim().to_string();
        return ok(json!({"ok": false, "error": if err.is_empty() { "解决失败".to_string() } else { err }}));
    }
    let remaining = detect_in_progress(&repo);
    let (_, out2) = runner::run_git(&["status".into(), "--porcelain=v2".into(), "-z".into()], Some(&repo), None).await;
    let left = out2.split('\0').filter(|r| r.starts_with("u ")).count();
    let hint = if left > 0 {
        format!("冲突已解决，还剩 {left} 个")
    } else {
        "冲突已解决，全部解决！".to_string()
    };
    ok(json!({"ok": true, "hint": hint, "remaining": left, "in_progress": remaining}))
}

// ---------------- /api/repo/* ----------------

async fn repo_status(Query(q): Query<Q>) -> Response {
    let path = q.path.unwrap_or_default();
    match git_repo::get_status(&path).await {
        Ok(mut v) => {
            v["ok"] = json!(true);
            ok(v)
        }
        Err(e) => ok(json!({"ok": false, "error": e})),
    }
}

/// 工作区硬核视图：porcelain=v2 全字段 + 逐文件 +/- + 文件系统探测
async fn repo_status_hardcore(Query(q): Query<Q>) -> Response {
    let path = q.path.unwrap_or_default();
    // 四个互相独立的 git 子进程并行跑，延迟收敛到最慢的那个
    let wt_args = vec!["diff".into(), "--numstat".into()];
    let idx_args = vec!["diff".into(), "--cached".into(), "--numstat".into()];
    let (st, wt, idx, remote) = tokio::join!(
        git_status_v2::get_status_v2(&path),
        runner::run_git(&wt_args, Some(&path), None),
        runner::run_git(&idx_args, Some(&path), None),
        git_repo::get_remote(&path),
    );
    let (meta, mut files) = match st {
        Ok(v) => v,
        Err(e) => return ok(json!({"ok": false, "error": e})),
    };
    let wt_map = git_repo::parse_numstat(if wt.0 == 0 { &wt.1 } else { "" });
    let idx_map = git_repo::parse_numstat(if idx.0 == 0 { &idx.1 } else { "" });

    // 文件系统探测（stat + 前 8KB 读）和未跟踪行数统计是同步 IO，
    // 包 spawn_blocking 避免占死 tokio worker
    let probe_limit = if files.len() > 500 { 200 } else { files.len() };
    let pb_files = std::mem::take(&mut files);
    let pb_path = path.clone();
    let pb = tokio::task::spawn_blocking(move || {
        let mut out_files = Vec::with_capacity(pb_files.len());
        for (i, mut f) in pb_files.into_iter().enumerate() {
            if i < probe_limit {
                let p = f["path"].as_str().unwrap_or("").to_string();
                f["fs"] = worktree_probe::probe(&pb_path, &p, 8192);
            }
            if f["x"].as_str() == Some("?") {
                let exists = f
                    .get("fs")
                    .and_then(|fs| fs.get("exists"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let is_bin = f
                    .get("fs")
                    .and_then(|fs| fs.get("is_binary"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let p = f["path"].as_str().unwrap_or("").to_string();
                // 只读前 2MB 数换行，绝不全量读入；binary 已由 probe 的 is_binary 先行排除
                let (lines, truncated) = if exists && !is_bin {
                    git_repo::count_lines(&pb_path, &p, Some(2 * 1024 * 1024))
                } else {
                    (0, false)
                };
                f["plus"] = json!(lines);
                f["minus"] = json!(0);
                if truncated {
                    // 大文件：标记行数只是前 2MB 的统计
                    f["plus_truncated"] = json!(true);
                }
            } else {
                let p = f["path"].as_str().unwrap_or("").to_string();
                let (pw0, pw1) = wt_map.get(&p).copied().unwrap_or((0, 0));
                let (pi0, pi1) = idx_map.get(&p).copied().unwrap_or((0, 0));
                f["plus"] = json!(pw0 + pi0);
                f["minus"] = json!(pw1 + pi1);
            }
            out_files.push(f);
        }
        out_files
    })
    .await
    .unwrap_or_default();
    files = pb;

    let mut summary = json!({
        "total": files.len(),
        "staged": 0i64, "unstaged": 0i64, "untracked": 0i64,
        "conflicts": 0i64, "plus": 0i64, "minus": 0i64,
    });
    for f in &files {
        let x = f["x"].as_str().unwrap_or("");
        let y = f["y"].as_str().unwrap_or("");
        if !". ?!".contains(x) {
            summary["staged"] = json!(summary["staged"].as_i64().unwrap_or(0) + 1);
        }
        if !". ?!".contains(y) {
            summary["unstaged"] = json!(summary["unstaged"].as_i64().unwrap_or(0) + 1);
        }
        if x == "?" {
            summary["untracked"] = json!(summary["untracked"].as_i64().unwrap_or(0) + 1);
        }
        if f.get("conflict").is_some() {
            summary["conflicts"] = json!(summary["conflicts"].as_i64().unwrap_or(0) + 1);
        }
        summary["plus"] = json!(summary["plus"].as_i64().unwrap_or(0) + f.get("plus").and_then(|v| v.as_i64()).unwrap_or(0));
        summary["minus"] = json!(summary["minus"].as_i64().unwrap_or(0) + f.get("minus").and_then(|v| v.as_i64()).unwrap_or(0));
    }

    ok(json!({
        "ok": true,
        "meta": meta,
        "summary": summary,
        "files": files,
        "remote": remote,
        "in_progress": detect_in_progress(&path),
        "probed": probe_limit.min(files.len()),
    }))
}

async fn repo_sync_remote(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let remote = d.get("remote").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !path.is_empty() && !remote.is_empty() {
        repositories::sync_remote(&path, &remote);
    }
    ok(json!({"ok": true}))
}

async fn repo_overview(Query(q): Query<Q>) -> Response {
    let path = q.path.unwrap_or_default();
    match git_repo::get_overview(&path).await {
        Ok(mut v) => {
            v["ok"] = json!(true);
            ok(v)
        }
        Err(e) => ok(json!({"ok": false, "error": e})),
    }
}

async fn repo_list() -> Response {
    ok(json!({
        "ok": true,
        "repositories": repositories::list_repos(),
        "active": repositories::get_active(),
    }))
}

async fn repo_open(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    ok(git_repo::open_repo(&path).await)
}

async fn repo_add(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if !std::path::Path::new(&path).is_dir() {
        return ok(json!({"ok": false, "error": "目录不存在"}));
    }
    let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let remote = d.get("remote").and_then(|v| v.as_str()).unwrap_or("");
    let rec = repositories::add_repo(&path, Some(name), remote);
    ok(json!({"ok": true, "repo": rec}))
}

async fn repo_remove(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    repositories::remove_repo(&path);
    ok(json!({"ok": true}))
}

async fn repo_activate(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    match repositories::activate(&path) {
        Ok(_) => match git_repo::get_status(&path).await {
            Ok(mut v) => {
                v["ok"] = json!(true);
                ok(v)
            }
            Err(e) => ok(json!({"ok": false, "error": e})),
        },
        Err(e) => ok(json!({"ok": false, "error": e})),
    }
}

async fn repo_rename(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    repositories::rename_repo(&path, &name);
    ok(json!({"ok": true}))
}

async fn repo_init(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if !std::path::Path::new(&path).is_dir() {
        return ok(json!({"ok": false, "error": "目录不存在"}));
    }
    let r = git_repo::init_repo(&path).await;
    if r["ok"].as_bool() != Some(true) {
        return ok(r);
    }
    let rec = repositories::add_repo(&path, None, "");
    ok(json!({"ok": true, "repo": rec}))
}

async fn repo_init_and_publish(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let create_remote = d.get("create_remote").and_then(|v| v.as_bool()).unwrap_or(false);
    let private = d.get("private").and_then(|v| v.as_bool()).unwrap_or(false);
    let visibility = if private { "--private" } else { "--public" };
    if !std::path::Path::new(&path).is_dir() {
        return ok(json!({"ok": false, "error": "目录不存在"}));
    }
    if create_remote {
        if let Some(err) = net::proxy_preflight() {
            return ok(json!({"ok": false, "error": err}));
        }
    }
    // 1. git init
    let (rc, out) = runner::run_git(&["init".into(), "-b".into(), "main".into()], Some(&path), None).await;
    if rc != 0 {
        return ok(json!({"ok": false, "error": out.trim()}));
    }
    // 2. 初始提交（目录为空时静默跳过）
    runner::run_git(&["add".into(), "-A".into()], Some(&path), None).await;
    runner::run_git(&["commit".into(), "-m".into(), "初始提交".into()], Some(&path), None).await;
    let (rc, _) = runner::run_git(&["rev-parse".into(), "--verify".into(), "HEAD".into()], Some(&path), None).await;
    let has_commit = rc == 0;
    // 3. 创建远程、关联并推送（空仓库没有可推的提交，先不 push）
    if create_remote {
        if name.is_empty() {
            return ok(json!({"ok": false, "error": "请填写仓库名"}));
        }
        let mut args = vec![
            "repo".into(), "create".into(), name.clone(), "--source".into(), path.clone(),
            "--remote".into(), "origin".into(), visibility.into(),
        ];
        if has_commit {
            args.push("--push".into());
        }
        let (rc, out) = runner::run_gh(&args, Some(600.0)).await;
        if rc != 0 {
            return ok(json!({"ok": false, "error": out.trim(), "hint": "本地仓库已初始化，远程创建失败"}));
        }
        let rec = repositories::add_repo(&path, Some(&name), "");
        if !has_commit {
            return ok(json!({"ok": true, "repo": rec, "hint": "目录是空的，远程仓库已创建但未推送；首次提交后点 [推送] 即可"}));
        }
        return ok(json!({"ok": true, "repo": rec}));
    }
    let rec = repositories::add_repo(&path, Some(&name), "");
    ok(json!({"ok": true, "repo": rec}))
}

/// 用户可能粘贴整条命令：剥掉 git clone 前缀和引号
fn clean_clone_url(u: &str) -> String {
    let mut u = u.trim().trim_matches('"').trim_matches('\'').to_string();
    let re = regex::Regex::new(r"^git\s+clone\s+(?:--\S+\s+)*(.+)$").unwrap();
    if let Some(c) = re.captures(&u) {
        if let Some(g) = c.get(1) {
            u = g.as_str().trim().to_string();
        }
    }
    u
}

/// 克隆 worker：流式写 stderr 到日志、超时 kill、成功后登记仓库并补建跟踪分支
fn spawn_clone_worker(
    clone_id: String,
    args: Vec<String>,
    target_path: String,
    log_path: std::path::PathBuf,
    env_map: HashMap<String, String>,
) {
    {
        let mut jobs = clone_jobs().lock().unwrap();
        jobs.insert(clone_id.clone(), log_path.clone());
    }
    // 任务结束 60 秒后自动清理，不依赖前端轮询触发（防泄漏）
    let cid = clone_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        clone_jobs().lock().unwrap().remove(&cid);
    });

    let timeout_secs = config::load_config(false)["git"]["cmd_timeout"]
        .as_f64()
        .unwrap_or(300.0);

    tokio::spawn(async move {
        use std::io::Write;
        let mut f = match std::fs::File::create(&log_path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let git = paths::git_exe().to_string_lossy().into_owned();
        let mut cmd = tokio::process::Command::new(&git);
        cmd.args(&args)
            .envs(&env_map)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(f, "EXIT:-1\nREPO:{}", e);
                return;
            }
        };
        let stderr = child.stderr.take().unwrap();
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        let start = std::time::Instant::now();
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    match line {
                        Ok(Some(l)) => {
                            let _ = writeln!(f, "{l}");
                            let _ = f.flush();
                        }
                        _ => break,
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if start.elapsed().as_secs_f64() > timeout_secs {
                        let _ = child.kill().await;
                        let _ = writeln!(f, "超时（{timeout_secs} 秒）");
                        break;
                    }
                }
            }
        }
        let status = child.wait().await;
        let rc = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        if rc == 0 {
            repositories::add_repo(&target_path, None, "");
            // clone 只给默认分支建了本地分支，其余远程头补建跟踪分支
            let (er_rc, er_out) = runner::run_git_sync(
                &["for-each-ref".into(), "refs/remotes/origin".into(), "--format=%(refname:lstrip=3)".into()],
                None,
                Some(&target_path),
            );
            let _ = er_rc;
            for line in er_out.lines() {
                let short = line.trim().to_string();
                if short.is_empty() || short == "HEAD" || short.ends_with("/HEAD") {
                    continue;
                }
                let check_arg = format!("refs/heads/{short}");
                let (chk, _) = runner::run_git_sync(
                    &["rev-parse".into(), "--verify".into(), "--quiet".into(), check_arg],
                    None,
                    Some(&target_path),
                );
                if chk == 0 {
                    continue;
                }
                let track_arg = format!("origin/{short}");
                let (br_rc, _) = runner::run_git_sync(
                    &["branch".into(), "--track".into(), short.clone(), track_arg],
                    None,
                    Some(&target_path),
                );
                let _ = writeln!(f, "branch {short}: rc={br_rc}");
                let _ = f.flush();
            }
        }
        let _ = writeln!(f, "EXIT:{rc}");
        let _ = writeln!(f, "REPO:{target_path}");
    });
}

/// 目录存在且非空（read_dir 取不到首个条目视为空）
fn dir_nonempty(p: &str) -> bool {
    std::path::Path::new(p).is_dir()
        && !std::fs::read_dir(p).map(|mut d| d.next().is_none()).unwrap_or(false)
}

async fn repo_clone(Json(d): Json<Value>) -> Response {
    let url = clean_clone_url(d.get("url").and_then(|v| v.as_str()).unwrap_or(""));
    let mut path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let all_history = d.get("all_history").and_then(|v| v.as_bool()).unwrap_or(true);
    let all_branches = d.get("all_branches").and_then(|v| v.as_bool()).unwrap_or(true);

    if url.is_empty() || path.is_empty() {
        return ok(json!({"ok": false, "error": "请填写仓库地址和保存位置"}));
    }
    if env::is_github_url(&url) {
        if let Some(err) = net::proxy_preflight() {
            return ok(json!({"ok": false, "error": err}));
        }
    }

    // 目标目录非空处理：clone 到子目录
    let repo_name = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .strip_suffix(".git")
        .unwrap_or_else(|| url.trim_end_matches('/').rsplit('/').next().unwrap_or(""))
        .to_string();
    if dir_nonempty(&path) {
        path = std::path::Path::new(&path).join(&repo_name).to_string_lossy().into_owned();
    }
    if dir_nonempty(&path) {
        return ok(json!({"ok": false, "error": format!("目标目录 {path} 非空，请换一个位置")}));
    }
    // 规范化路径：与仓库列表逐字符一致，否则顶栏下拉框显示不出仓库名
    let p = std::path::Path::new(&path);
    path = crate::paths::canonicalize_human(p.parent().unwrap_or_else(|| std::path::Path::new(".")))
        .map(|par| par.join(p.file_name().unwrap_or_default()).to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    // 记住这次用的保存位置
    config::update_config(&json!({"ui": {"last_clone_dir": path}}));

    let mut args = vec!["clone".into()];
    if !all_history {
        args.push("--depth".into());
        args.push("1".into());
    }
    if !all_branches {
        args.push("--single-branch".into());
    } else if !all_history {
        args.push("--no-single-branch".into());
    }
    let env_map = env::clone_env(&url);
    args.push(url);
    args.push(path.clone());

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let clone_id = millis.to_string();
    let log_path = paths::logs_dir().join(format!("clone_{clone_id}.log"));
    spawn_clone_worker(clone_id.clone(), args, path, log_path, env_map);
    ok(json!({"ok": true, "clone_id": clone_id}))
}

async fn repo_clone_progress(Query(q): Query<Q>) -> Response {
    let cid = q.id.unwrap_or_default();
    let log_path = {
        let jobs = clone_jobs().lock().unwrap();
        match jobs.get(&cid) {
            Some(p) => p.clone(),
            None => return ok(json!({"ok": false, "error": "克隆任务不存在"})),
        }
    };
    let content = std::fs::read_to_string(&log_path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let done = content.contains("EXIT:");
    let exit_line = lines
        .iter()
        .find(|l| l.starts_with("EXIT:"))
        .map(|l| l.to_string())
        .unwrap_or_default();
    let repo_line = lines
        .iter()
        .find(|l| l.starts_with("REPO:"))
        .map(|l| l.to_string())
        .unwrap_or_default();
    let tail: Vec<String> = lines.iter().rev().take(20).rev().map(|l| l.to_string()).collect();
    ok(json!({
        "ok": true,
        "lines": tail,
        "done": done,
        "success": done && exit_line == "EXIT:0",
        "repo_path": if repo_line.is_empty() { "" } else { &repo_line[5..] },
    }))
}

async fn repo_open_explorer(Json(d): Json<Value>) -> Response {
    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !std::path::Path::new(&path).is_dir() {
        return ok(json!({"ok": false, "error": "仓库目录不存在"}));
    }
    let _ = std::process::Command::new("explorer").arg(&path).spawn();
    ok(json!({"ok": true}))
}

// ---------------- Router ----------------

pub fn build_router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(favicon))
        .route("/api/health", get(health))
        .route("/api/client_log", post(client_log))
        .route("/api/native/pick_folder", post(native_pick_folder))
        .route("/api/native/pick_files", post(native_pick_files))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/verify", get(auth_verify))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/config", get(config_get).post(config_set))
        .route("/api/config/versions", get(config_versions))
        .route("/api/config/proxy_test", post(config_proxy_test))
        .route("/api/gh/release", post(gh_release))
        .route("/api/gh/release_delete", post(gh_release_delete))
        .route("/api/gh/releases", get(gh_releases))
        .route("/api/gh/open_web", post(gh_open_web))
        .route("/api/gh/open_url", post(gh_open_url))
        .route("/api/rollback/execute", post(rollback_execute))
        .route("/api/rollback/recovery", get(rollback_recovery))
        .route("/api/rollback/recover", post(rollback_recover))
        .route("/api/git/commit", post(git_commit))
        .route("/api/git/restore", post(git_restore))
        .route("/api/git/diff", get(git_diff))
        .route("/api/git/commit_detail", get(git_commit_detail))
        .route("/api/git/has_origin", get(git_has_origin))
        .route("/api/git/pull", post(git_pull))
        .route("/api/git/push", post(git_push))
        .route("/api/git/commit_ops", post(git_commit_ops))
        .route("/api/git/branch", post(git_branch))
        .route("/api/git/commit_file_diff", get(git_commit_file_diff))
        .route("/api/git/file_stats", get(git_file_stats))
        .route("/api/git/commit_stats", get(git_commit_stats))
        .route("/api/git/log_table", get(git_log_table))
        .route("/api/git/branches", get(git_branches))
        .route("/api/git/in_progress", get(git_in_progress))
        .route("/api/git/in_progress_op", post(git_in_progress_op))
        .route("/api/git/conflict_resolve", post(git_conflict_resolve))
        .route("/api/repo/status", get(repo_status))
        .route("/api/repo/status_hardcore", get(repo_status_hardcore))
        .route("/api/repo/sync_remote", post(repo_sync_remote))
        .route("/api/repo/overview", get(repo_overview))
        .route("/api/repo/list", get(repo_list))
        .route("/api/repo/open", post(repo_open))
        .route("/api/repo/add", post(repo_add))
        .route("/api/repo/remove", post(repo_remove))
        .route("/api/repo/activate", post(repo_activate))
        .route("/api/repo/rename", post(repo_rename))
        .route("/api/repo/init", post(repo_init))
        .route("/api/repo/init_and_publish", post(repo_init_and_publish))
        .route("/api/repo/clone", post(repo_clone))
        .route("/api/repo/clone_progress", get(repo_clone_progress))
        .route("/api/repo/open_explorer", post(repo_open_explorer))
        .fallback(static_files)
}

/// 在 127.0.0.1 随机端口上启动 HTTP 服务，返回实际端口
pub async fn start_server() -> Result<u16, String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let router = build_router();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(port)
}

