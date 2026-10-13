//! git status --porcelain=v2 全字段解析。
//! 与 Python 版 git_status_v2.py 对应。

use crate::runner;
use serde_json::{json, Value};

pub async fn get_status_v2(repo: &str) -> Result<(Value, Vec<Value>), String> {
    let (rc, out) = runner::run_git(
        &[
            "status".into(),
            "--porcelain=v2".into(),
            "--branch".into(),
            "--show-stash".into(),
            "-z".into(),
        ],
        Some(repo),
        None,
    )
    .await;
    if rc != 0 {
        return Err(out.trim().to_string());
    }

    let mut meta = json!({"branch": "", "upstream": "", "ahead": 0, "behind": 0, "stash": 0});
    let mut files: Vec<Value> = Vec::new();
    let recs: Vec<&str> = out.split('\0').filter(|r| !r.is_empty()).collect();
    let mut i = 0usize;
    while i < recs.len() {
        let rec = recs[i];
        let kind = &rec[..1];

        if kind == "#" {
            let body = &rec[2..];
            if let Some(b) = body.strip_prefix("branch.head ") {
                meta["branch"] = json!(b);
            } else if let Some(b) = body.strip_prefix("branch.upstream ") {
                meta["upstream"] = json!(b);
            } else if let Some(b) = body.strip_prefix("branch.ab ") {
                let parts: Vec<&str> = b.split_whitespace().collect();
                let ahead = parts
                    .first()
                    .unwrap_or(&"+0")
                    .trim_start_matches('+')
                    .parse::<i64>()
                    .unwrap_or(0);
                let behind = parts
                    .get(1)
                    .unwrap_or(&"-0")
                    .trim_start_matches('-')
                    .parse::<i64>()
                    .unwrap_or(0);
                meta["ahead"] = json!(ahead);
                meta["behind"] = json!(behind);
            } else if let Some(b) = body.strip_prefix("stash ") {
                meta["stash"] = json!(b.trim().parse::<i64>().unwrap_or(0));
            }
            i += 1;
            continue;
        }

        if kind == "1" {
            // 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
            let f: Vec<&str> = rec[2..].splitn(8, ' ').collect();
            if f.len() >= 8 {
                let xy = f[0].as_bytes();
                files.push(json!({
                    "path": f[7], "x": (xy[0] as char).to_string(), "y": (xy[1] as char).to_string(),
                    "sub": f[1], "mode_head": f[2], "mode_index": f[3],
                    "mode_worktree": f[4], "hash_head": f[5], "hash_index": f[6],
                    "rename_from": Value::Null, "rename_score": Value::Null,
                }));
            }
            i += 1;
            continue;
        }

        if kind == "2" {
            // 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path> NUL <orig>
            let f: Vec<&str> = rec[2..].splitn(9, ' ').collect();
            let mut orig = String::new();
            if i + 1 < recs.len() {
                let next = recs[i + 1];
                let first = next.chars().next().unwrap_or('\0');
                if first != '#'
                    && first != '1'
                    && first != '2'
                    && first != 'u'
                    && first != '?'
                    && first != '!'
                    && !next.contains('\t')
                {
                    orig = next.to_string();
                    i += 1;
                }
            }
            let xy = f[0].as_bytes();
            let score = f[7]; // 形如 R100 / C85
            let rename_score =
                if score.len() > 1 && (score.starts_with('R') || score.starts_with('C')) {
                    score[1..].parse::<i64>().ok()
                } else {
                    None
                };
            files.push(json!({
                "path": f[8], "x": (xy[0] as char).to_string(), "y": (xy[1] as char).to_string(),
                "sub": f[1], "mode_head": f[2], "mode_index": f[3],
                "mode_worktree": f[4], "hash_head": f[5], "hash_index": f[6],
                "rename_from": if orig.is_empty() { Value::Null } else { json!(orig) },
                "rename_score": rename_score.map(|s| json!(s)).unwrap_or(Value::Null),
            }));
            i += 1;
            continue;
        }

        if kind == "u" {
            // u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>  共 9 个前导字段
            let f: Vec<&str> = rec[2..].splitn(10, ' ').collect();
            if f.len() >= 10 {
                let xy = f[0].as_bytes();
                files.push(json!({
                    "path": f[9], "x": (xy[0] as char).to_string(), "y": (xy[1] as char).to_string(),
                    "sub": f[1], "conflict": f[0],
                    "modes": [f[2], f[3], f[4]], "mode_worktree": f[5],
                    "hashes": [f[6], f[7], f[8]],
                    "rename_from": Value::Null, "rename_score": Value::Null,
                }));
            }
            i += 1;
            continue;
        }

        if kind == "?" {
            files.push(json!({
                "path": &rec[2..], "x": "?", "y": "?",
                "untracked": true, "sub": ".",
                "mode_head": "-", "mode_index": "-", "mode_worktree": "-",
                "hash_head": "", "hash_index": "",
                "rename_from": Value::Null, "rename_score": Value::Null,
            }));
            i += 1;
            continue;
        }

        if kind == "!" {
            // 已忽略：默认不进列表（避免噪音）
            i += 1;
            continue;
        }

        i += 1;
    }
    Ok((meta, files))
}
