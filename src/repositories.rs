//! 多仓库管理：config.json 的 repositories 数组 + active_repo。
//! 与 Python 版 repositories.py 对应。

use crate::config;
use crate::paths;
use serde_json::{json, Value};
use std::path::Path;

/// Windows 路径比较统一小写（不区分大小写）
fn norm(path: &str) -> String {
    let abs = paths::canonicalize_human(Path::new(path))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    abs.to_lowercase()
}

fn find(cfg: &Value, path: &str) -> Option<Value> {
    let key = norm(path);
    cfg["repositories"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .find(|r| norm(r["path"].as_str().unwrap_or("")) == key)
        .cloned()
}

/// 加入列表（已存在则更新 remote），并设为活动仓库。返回记录。
pub fn add_repo(path: &str, name: Option<&str>, remote: &str) -> Value {
    let mut cfg = config::load_config(false);
    let abs = paths::canonicalize_human(Path::new(path))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    if let Some(rec) = find(&cfg, &abs) {
        if !remote.is_empty() {
            let idx = cfg["repositories"]
                .as_array()
                .unwrap()
                .iter()
                .position(|r| r["path"].as_str() == rec["path"].as_str())
                .unwrap();
            cfg["repositories"][idx]["remote"] = json!(remote);
        }
    } else {
        let rec = json!({
            "path": abs,
            "name": name.map(|s| s.to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| {
                Path::new(&abs).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| abs.clone())
            }),
            "remote": remote,
            "last_opened": paths::now_iso_seconds(),
        });
        cfg["repositories"].as_array_mut().unwrap().push(rec);
    }
    cfg["active_repo"] = json!(abs);
    config::save_config(&cfg);
    find(&cfg, &abs).unwrap_or(Value::Null)
}

/// 只从列表移除，不删文件夹。
pub fn remove_repo(path: &str) {
    let mut cfg = config::load_config(false);
    let key = norm(path);
    if let Some(arr) = cfg["repositories"].as_array_mut() {
        arr.retain(|r| norm(r["path"].as_str().unwrap_or("")) != key);
    }
    if let Some(active) = cfg["active_repo"].as_str() {
        if norm(active) == key {
            cfg["active_repo"] = json!("");
        }
    }
    config::save_config(&cfg);
}

pub fn rename_repo(path: &str, name: &str) {
    let mut cfg = config::load_config(false);
    let key = norm(path);
    if !name.trim().is_empty() {
        if let Some(arr) = cfg["repositories"].as_array_mut() {
            if let Some(rec) = arr.iter_mut().find(|r| norm(r["path"].as_str().unwrap_or("")) == key) {
                rec["name"] = json!(name.trim());
            }
        }
    }
    config::save_config(&cfg);
}

pub fn activate(path: &str) -> Result<Value, String> {
    let mut cfg = config::load_config(false);
    let rec = find(&cfg, path).ok_or_else(|| "该仓库不在列表中".to_string())?;
    if let Some(arr) = cfg["repositories"].as_array_mut() {
        if let Some(r) = arr.iter_mut().find(|r| r["path"] == rec["path"]) {
            r["last_opened"] = json!(paths::now_iso_seconds());
        }
    }
    cfg["active_repo"] = rec["path"].clone();
    config::save_config(&cfg);
    Ok(rec)
}

/// get_status 时把最新 remote 写回缓存
pub fn sync_remote(path: &str, remote: &str) {
    let mut cfg = config::load_config(false);
    if let Some(arr) = cfg["repositories"].as_array_mut() {
        if let Some(rec) = arr.iter_mut().find(|r| norm(r["path"].as_str().unwrap_or("")) == norm(path)) {
            if rec.get("remote").and_then(|v| v.as_str()).unwrap_or("") != remote {
                rec["remote"] = json!(remote);
            }
        }
    }
    config::save_config(&cfg);
}

pub fn list_repos() -> Vec<Value> {
    config::load_config(false)["repositories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

pub fn get_active() -> String {
    config::load_config(false)["active_repo"]
        .as_str()
        .unwrap_or("")
        .to_string()
}
