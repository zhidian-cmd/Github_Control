//! gh Release 管理：创建 / 列表 / 删除，含代理失败直连重试。
//! 与 Python 版 gh_runner.py 对应。

use crate::asset_helper;
use crate::env;
use crate::git_repo;
use crate::net;
use crate::paths;
use crate::runner;
use regex::Regex;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Duration;

/// 发布/上传专用：代理失败 → 直连重试（uploads.githubusercontent.com
/// 是国内通常可以直连的 CDN，避开代理对大 POST 的中转）。
async fn run_gh_resilient(args: &[String], timeout: f64) -> (i32, String) {
    let mut cmd = vec![paths::gh_exe().to_string_lossy().into_owned()];
    cmd.extend(args.iter().cloned());
    let (rc, out) = runner::run(&cmd, Some(timeout), None, None).await;
    if rc == 0 || !net::is_net_hint(&out) {
        return (rc, out);
    }
    // 代理通道失败：去代理直连重试
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut no_proxy_env = env::build_env();
    for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        no_proxy_env.remove(k);
    }
    runner::run(&cmd, Some(timeout), None, Some(&no_proxy_env)).await
}

async fn guess_remote(repo_dir: &str) -> Option<String> {
    let r = git_repo::get_remote(repo_dir).await;
    if let Some(stripped) = r.strip_prefix("github.com/") {
        return Some(stripped.to_string());
    }
    None
}

fn friendly(out: &str) -> String {
    let out = out.trim().to_string();
    let low = out.to_lowercase();
    if out.contains("Bad credentials") || out.contains("401") {
        return "token 无效或已过期，请重新登录".into();
    }
    if out.contains("To get started with GitHub CLI") || out.contains("gh auth login") {
        return "gh 未登录或登录已失效，请到设置页重新登录 GitHub".into();
    }
    if low.contains("not found") || out.contains("404") {
        return "未找到该仓库或 Release：仓库可能已删除，或私有仓库未登录无权访问".into();
    }
    if out.contains("502") || out.contains("503") || out.contains("504") {
        return "GitHub 服务暂时不可用（网关错误），已自动重试仍失败，请稍后再试".into();
    }
    if out.contains("already_exists") {
        return "该 tag 的 Release 已存在".into();
    }
    if out.is_empty() {
        "gh 命令执行失败".into()
    } else {
        out
    }
}

pub async fn release_create(tag: &str, files: &[String], title: &str, notes: &str, repo_dir: &str) -> Value {
    let remote = match guess_remote(repo_dir).await {
        Some(r) => r,
        None => return json!({"ok": false, "error": "该仓库没有设置远程地址，请先推送一次"}),
    };
    if let Some(err) = net::proxy_preflight() {
        return json!({"ok": false, "error": err});
    }

    let mut tmp_files: Vec<String> = Vec::new();
    let mut args = vec!["release".into(), "create".into(), tag.into(), "-R".into(), remote];
    for f in files {
        if !std::path::Path::new(f).is_file() {
            asset_helper::cleanup(&tmp_files);
            return json!({"ok": false, "error": format!("文件不存在：{f}")});
        }
        let real = asset_helper::make_ascii_copy(f);
        if real != *f {
            tmp_files.push(real.clone());
        }
        args.push(real);
    }

    if !notes.trim().is_empty() {
        let pid = std::process::id();
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let notes_file = paths::tmp_dir()
            .join(format!("release_notes_{pid}_{millis}.md"))
            .to_string_lossy()
            .into_owned();
        if std::fs::write(&notes_file, notes).is_ok() {
            tmp_files.push(notes_file.clone());
            args.push("--notes-file".into());
            args.push(notes_file);
        }
    }
    if !title.trim().is_empty() {
        args.push("--title".into());
        args.push(title.trim().to_string());
    }

    let result = async {
        let (rc, out) = run_gh_resilient(&args, 1800.0).await; // 上传大文件放宽超时
        if rc != 0 {
            return json!({"ok": false, "error": friendly(&out)});
        }
        let m = Regex::new(r"https://\S+/tag/\S+").unwrap().find(&out)
            .or_else(|| Regex::new(r"https://\S+/releases/\S+").unwrap().find(&out));
        json!({"ok": true, "url": m.map(|m| m.as_str().to_string()).unwrap_or_default()})
    }
    .await;
    asset_helper::cleanup(&tmp_files);
    result
}

pub async fn release_list(repo_dir: &str) -> Vec<Value> {
    let remote = match guess_remote(repo_dir).await {
        Some(r) => r,
        None => return vec![], // 没有远程仓库就没有 Release
    };
    let (rc, out) = runner::run_gh(
        &[
            "release".into(), "list".into(), "-R".into(), remote,
            "--json".into(), "tagName,name,isLatest,publishedAt".into(),
        ],
        None,
    )
    .await;
    if rc != 0 {
        return vec![];
    }
    match serde_json::from_str::<Value>(&out) {
        Ok(Value::Array(arr)) => arr
            .iter()
            .map(|r| {
                json!({
                    "tag": r.get("tagName").and_then(|v| v.as_str()).unwrap_or(""),
                    "title": r.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "latest": r.get("isLatest").and_then(|v| v.as_bool()).unwrap_or(false),
                    "date": r.get("publishedAt").and_then(|v| v.as_str()).unwrap_or(""),
                })
            })
            .collect(),
        _ => vec![],
    }
}

pub async fn release_delete(tag: &str, repo_dir: &str) -> Value {
    let remote = match guess_remote(repo_dir).await {
        Some(r) => r,
        None => return json!({"ok": false, "error": "该仓库没有设置远程地址"}),
    };
    let (rc, out) = runner::run_gh(
        &["release".into(), "delete".into(), tag.into(), "-R".into(), remote, "--yes".into()],
        None,
    )
    .await;
    json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { json!(if out.trim().is_empty() { "删除失败".to_string() } else { friendly(&out) }) }})
}
