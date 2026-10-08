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
    // 代理通道失败：去代理直连重试。
    // 必须用 build_env_direct（连 all_proxy 在内 6 个代理变量全清），
    // 且 runner::run 对外部环境是"清空后注入"，确保子进程真拿到直连环境。
    tokio::time::sleep(Duration::from_secs(2)).await;
    let no_proxy_env = env::build_env_direct();
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
        let real = match asset_helper::make_ascii_copy(f) {
            Ok(r) => r,
            Err(e) => {
                asset_helper::cleanup(&tmp_files);
                return json!({"ok": false, "error": e});
            }
        };
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

/// 查询 Release 列表。失败必须报错而不是返回空数组——
/// 未登录/代理故障时，"静默空列表"会让用户误以为远程没有 Release。
/// 实现说明：不用 `gh release list --json` —— gh 2.101+ 把该子命令改成了
/// GraphQL 端点，在部分代理链路下会被 406 Not Acceptable 拒绝（实测）；
/// 而 REST 端点（gh api）在同一链路下验证可用，因此直接打 REST。
pub async fn release_list(repo_dir: &str) -> Result<Vec<Value>, String> {
    let remote = match guess_remote(repo_dir).await {
        Some(r) => r,
        None => return Err("该仓库没有可用的 GitHub 远程地址，无法查询 Release".into()),
    };
    // 通道一：gh-proxy 镜像透传 GitHub API——匿名、直连、免登录免代理，
    // 公开仓库零流量走 GitHub 本体。私有仓库镜像匿名拿不到（404），走通道二。
    if let Some(slug) = remote.strip_prefix("github.com/") {
        if let Ok(list) = mirror_release_list(slug).await {
            return Ok(list);
        }
    }
    // 通道二：gh api（带已保存 token，直连优先、失败自动兜底走代理）
    let (rc, out) = runner::run_gh(
        &[
            "api".into(),
            format!("repos/{remote}/releases?per_page=100"),
        ],
        None,
    )
    .await;
    if rc != 0 {
        return Err(if out.trim().is_empty() {
            "查询 Release 列表失败（gh 未登录或网络异常）".into()
        } else {
            friendly(&out)
        });
    }
    parse_releases_json(&out).ok_or_else(|| "gh 返回的 Release 数据无法解析".into())
}

/// 经 gh-proxy.com 镜像请求 GitHub API 的 Release 列表（匿名）。
/// 用系统 curl 直连（清空代理变量）；非 200 或解析失败一律返回 Err，由调用方回落 gh api。
async fn mirror_release_list(slug: &str) -> Result<Vec<Value>, String> {
    let url = format!("https://gh-proxy.com/https://api.github.com/repos/{slug}/releases?per_page=100");
    let env_map = env::build_env_direct();
    let (rc, out) = runner::run(
        &[
            "curl".into(),
            "-sL".into(),
            "--max-time".into(),
            "30".into(),
            "-w".into(),
            "\n%{http_code}".into(),
            url,
        ],
        Some(35.0),
        None,
        Some(&env_map),
    )
    .await;
    if rc != 0 {
        return Err("镜像请求失败".into());
    }
    // curl -w 把状态码附在最后一行
    let (body, code) = match out.rsplit_once('\n') {
        Some((b, c)) => (b.trim(), c.trim()),
        None => return Err("镜像响应异常".into()),
    };
    if code != "200" {
        return Err(format!("镜像返回 {code}"));
    }
    parse_releases_json(body).ok_or_else(|| "镜像返回的 Release 数据无法解析".into())
}

/// 解析 GitHub REST releases JSON（gh api 与镜像透传的返回格式一致）。
/// latest = 最新一个非 draft 且非 prerelease 的 Release（数组按时间倒序）。
fn parse_releases_json(out: &str) -> Option<Vec<Value>> {
    let arr: Vec<Value> = match serde_json::from_str(out.trim()) {
        Ok(Value::Array(a)) => a,
        _ => return None,
    };
    let mut latest_done = false;
    let list = arr
        .iter()
        .map(|r| {
            let draft = r.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
            let pre = r.get("prerelease").and_then(|v| v.as_bool()).unwrap_or(false);
            let latest = !draft && !pre && !latest_done;
            if latest {
                latest_done = true;
            }
            let assets: Vec<Value> = r
                .get("assets")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            json!({
                                "name": x.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                "url": x.get("browser_download_url").and_then(|v| v.as_str()).unwrap_or(""),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "tag": r.get("tag_name").and_then(|v| v.as_str()).unwrap_or(""),
                "title": r.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "latest": latest,
                "date": r.get("published_at").and_then(|v| v.as_str()).unwrap_or(""),
                "assets": assets,
            })
        })
        .collect();
    Some(list)
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
