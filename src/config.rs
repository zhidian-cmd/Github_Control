//! 配置管理：config.json 的加载 / 合并 / 保存，进程内缓存。
//! 与 Python 版 config_manager.py 语义一致。

use crate::paths;
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

pub fn defaults() -> Value {
    json!({
        "proxy": {"enabled": true, "url": "http://127.0.0.1:7810", "test_timeout": 2},
        "git": {"cmd_timeout": 300, "default_branch": "main"},
        "gh": {"default_repo": ""},
        "editor": "",
        "auth": {"username": ""},
        "repositories": [],
        "active_repo": "",
        "window": {},
        "ui": {"expanded_dirs": {}, "last_clone_dir": ""},
    })
}

/// 递归合并：两边都是对象时深合并，否则覆盖
pub fn merge(base: &Value, override_v: &Value) -> Value {
    match (base, override_v) {
        (Value::Object(b), Value::Object(o)) => {
            let mut out = b.clone();
            for (k, v) in o {
                if let Some(bv) = out.get(k) {
                    if bv.is_object() && v.is_object() {
                        out.insert(k.clone(), merge(bv, v));
                    } else {
                        out.insert(k.clone(), v.clone());
                    }
                } else {
                    out.insert(k.clone(), v.clone());
                }
            }
            Value::Object(out)
        }
        (_, o) => o.clone(),
    }
}

fn cache() -> &'static Mutex<Option<Value>> {
    static C: OnceLock<Mutex<Option<Value>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// 加载配置。force=true 强制重新读盘；损坏时备份旧文件并按默认配置继续。
pub fn load_config(force: bool) -> Value {
    let mut c = cache().lock().unwrap();
    if !force && c.is_some() {
        return c.clone().unwrap();
    }
    let cfg = match std::fs::read_to_string(paths::config_file()) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => merge(&defaults(), &v),
            Err(_) => {
                // config.json 损坏：备份旧文件，用默认配置继续跑
                let path = paths::config_file();
                if path.is_file() {
                    let bak = paths::config_file()
                        .with_file_name(format!("config.json.bak.{}", paths::now_compact()));
                    let _ = std::fs::rename(paths::config_file(), &bak);
                }
                defaults()
            }
        },
        Err(_) => defaults(),
    };
    *c = Some(cfg.clone());
    cfg
}

pub fn save_config(cfg: &Value) {
    let mut c = cache().lock().unwrap();
    *c = Some(cfg.clone());
    let text = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".into());
    if let Err(e) = std::fs::write(paths::config_file(), text) {
        eprintln!("保存配置失败: {e}");
    }
}

/// 合并补丁并保存，返回新配置
pub fn update_config(patch: &Value) -> Value {
    let cfg = merge(&load_config(false), patch);
    save_config(&cfg);
    cfg
}
