//! 回滚：软/混合/硬回退、反向提交、保险 tag、reflog 恢复。
//! 与 Python 版 git_rollback.py 对应。

use crate::paths;
use crate::runner;
use serde_json::{json, Value};

fn require_confirm(confirm: bool, op: &str) -> Result<(), String> {
    if !confirm {
        return Err(format!("{op} 需要显式 confirm=True"));
    }
    Ok(())
}

/// 硬回退：先自动打保险 tag，再 reset --hard。
/// 未提交改动会先自动储藏、回退后弹回。
pub async fn hard_reset(repo: &str, target_commit: &str, confirm: bool) -> Result<Value, String> {
    require_confirm(confirm, "hard_reset")?;

    let (rc, out) = runner::run_git(&["rev-parse".into(), "HEAD".into()], Some(repo), None).await;
    if rc != 0 {
        return Err("无法读取当前 HEAD".into());
    }
    let current_head = out.trim().to_string();

    let tag_name = format!("_backup_{}", paths::now_compact());
    let (rc, out) = runner::run_git(&["tag".into(), tag_name.clone(), current_head], Some(repo), None).await;
    if rc != 0 {
        return Err(format!("打保险 tag 失败：{}", out.trim()));
    }

    let (rc, st_out) = runner::run_git(&["status".into(), "--porcelain=v1".into()], Some(repo), None).await;
    let dirty = rc == 0 && !st_out.trim().is_empty();
    if dirty {
        runner::run_git(
            &["stash".into(), "push".into(), "-u".into(), "-m".into(), "auto before hard reset".into()],
            Some(repo),
            None,
        )
        .await;
    }

    let (rc, out) = runner::run_git(&["reset".into(), "--hard".into(), target_commit.into()], Some(repo), None).await;
    let mut result = json!({"ok": rc == 0, "backup_tag": tag_name, "output": out.trim()});

    if dirty && rc == 0 {
        let (rc2, _) = runner::run_git(&["stash".into(), "pop".into()], Some(repo), None).await;
        // 弹回冲突时 git 自动保留 stash，改动不会丢
        result["stash_kept"] = json!(rc2 != 0);
    }
    Ok(result)
}

pub async fn soft_reset(repo: &str, target_commit: &str) -> Value {
    let (rc, out) = runner::run_git(&["reset".into(), "--soft".into(), target_commit.into()], Some(repo), None).await;
    json!({"ok": rc == 0, "output": out.trim()})
}

pub async fn mixed_reset(repo: &str, target_commit: &str, confirm: bool) -> Result<Value, String> {
    require_confirm(confirm, "mixed_reset")?;
    let (rc, out) = runner::run_git(&["reset".into(), "--mixed".into(), target_commit.into()], Some(repo), None).await;
    Ok(json!({"ok": rc == 0, "output": out.trim()}))
}

pub async fn revert(repo: &str, target_commit: &str) -> Value {
    let (rc, out) = runner::run_git(&["revert".into(), "--no-edit".into(), target_commit.into()], Some(repo), None).await;
    json!({"ok": rc == 0, "output": out.trim()})
}

/// 保险 tag 列表 + reflog 最近 30 条
pub async fn list_recovery_points(repo: &str) -> Value {
    let (rc, out) = runner::run_git(
        &[
            "tag".into(), "-l".into(), "_backup_*".into(),
            "--format=%(refname:short)|%(objectname:short)|%(creatordate:iso)".into(),
        ],
        Some(repo),
        None,
    )
    .await;
    let mut backups = Vec::new();
    if rc == 0 {
        for line in out.trim().lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() == 3 {
                backups.push(json!({"tag": parts[0], "hash": parts[1], "date": parts[2]}));
            }
        }
    }

    let (rc, out) = runner::run_git(
        &["reflog".into(), "--pretty=format:%h|%gd|%gs".into(), "-n".into(), "30".into()],
        Some(repo),
        None,
    )
    .await;
    let mut reflog = Vec::new();
    if rc == 0 {
        for line in out.trim().lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() == 3 {
                reflog.push(json!({"hash": parts[0], "ref": parts[1], "action": parts[2]}));
            }
        }
    }
    json!({"backups": backups, "reflog": reflog})
}

pub async fn recover_to(repo: &str, commit_hash: &str, confirm: bool) -> Result<Value, String> {
    require_confirm(confirm, "recover_to")?;
    let (rc, out) = runner::run_git(&["reset".into(), "--hard".into(), commit_hash.into()], Some(repo), None).await;
    Ok(json!({"ok": rc == 0, "output": out.trim()}))
}
