//! Git 仓库业务封装。与 Python 版 git_repo.py 逐函数对应。

use crate::paths;
use crate::runner;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const MAX_DIFF: usize = 300 * 1024; // 300KB，防止前端卡死

fn trim(s: &str) -> String {
    s.trim().to_string()
}

/// 解析仓库的 .git 目录：普通仓库 <repo>/.git 是目录；
/// worktree / submodule 场景 .git 是文件，内容 "gitdir: <path>" 指向真正的 git 目录。
/// 读不到返回 None（调用方应回落 git 命令）。
pub fn git_dir_of(repo: &str) -> Option<std::path::PathBuf> {
    let dot = std::path::Path::new(repo).join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    if dot.is_file() {
        let text = std::fs::read_to_string(&dot).ok()?;
        let p = text.trim().strip_prefix("gitdir:").map(str::trim)?;
        let path = std::path::PathBuf::from(p);
        // gitdir 可能是相对路径（相对 repo）
        if path.is_absolute() {
            Some(path)
        } else {
            Some(std::path::Path::new(repo).join(path))
        }
    } else {
        None
    }
}

/// worktree 的共享配置目录（remote 等在 common config 里）；
/// 普通仓库 git_dir/config 即配置，直接返回 git_dir/config。
fn common_config_path(git_dir: &std::path::Path) -> std::path::PathBuf {
    common_git_dir(git_dir).join("config")
}

/// 零进程 rev-parse：只支持 app 内部用到的形态（HEAD / 40-hex / refs 全名简写），
/// 读 loose ref 文件与 packed-refs。读不到返回 None，调用方回落 git 命令。
pub fn rev_parse_fast(repo: &str, rev: &str) -> Option<String> {
    let r = rev.trim();
    if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(r.to_string());
    }
    let git_dir = git_dir_of(repo)?;
    if r == "HEAD" {
        // HEAD 可能是 symbolic（ref: refs/heads/xxx）或游离（直接是哈希）
        let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
        let h = head.trim();
        if let Some(target) = h.strip_prefix("ref:") {
            return ref_value(&git_dir, target.trim());
        }
        if h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(h.to_string());
        }
        return None;
    }
    // 短名补全为 refs 全名（与 git 的解析优先级一致：本地分支 > 远程跟踪 > tag 从简，app 只用前两类）
    let full = if let Some(rest) = r.strip_prefix("refs/") {
        format!("refs/{rest}")
    } else if r.contains('/') {
        // origin/main → refs/remotes/origin/main；heads/main → refs/heads/main
        if r.starts_with("remotes/") || r.starts_with("heads/") || r.starts_with("tags/") {
            format!("refs/{r}")
        } else {
            format!("refs/remotes/{r}")
        }
    } else {
        format!("refs/heads/{r}")
    };
    ref_value(&git_dir, &full)
}

/// worktree 的共享 git 目录（refs / packed-refs 实际所在处）；
/// 普通仓库无 commondir 文件，返回 git_dir 自身。
fn common_git_dir(git_dir: &std::path::Path) -> std::path::PathBuf {
    let cd = git_dir.join("commondir");
    if let Ok(text) = std::fs::read_to_string(&cd) {
        let p = std::path::PathBuf::from(text.trim());
        if p.is_absolute() {
            return p;
        }
        return git_dir.join(p);
    }
    git_dir.to_path_buf()
}

/// 零进程 ref 查询是否可信：能定位 git 目录（普通仓库的 .git 目录，
/// 或 worktree 经 commondir 落到共享目录）时为 true——refs 与 packed-refs
/// 都已掌握，查不到即不存在。定位不到（.git 缺失、选了仓库子目录等）
/// 返回 false，调用方应回落 git 命令。
fn refs_readable(repo: &str) -> bool {
    match git_dir_of(repo) {
        Some(gd) => common_git_dir(&gd).is_dir(),
        None => false,
    }
}

/// 按 refs 全名取值：先 loose 文件，再 packed-refs。
/// worktree 场景共享 refs（heads/remotes/tags、packed-refs）在 commondir
/// 指向的主仓库目录，本地 git_dir 只放 HEAD 等每工作区私有引用——两处都要查。
fn ref_value(git_dir: &std::path::Path, full_ref: &str) -> Option<String> {
    let common = common_git_dir(git_dir);
    for base in [git_dir, common.as_path()] {
        let loose = base.join(full_ref);
        if let Ok(text) = std::fs::read_to_string(&loose) {
            let h = text.trim();
            if h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Some(h.to_string());
            }
            return None; // loose 存在但内容非法：与 git 一致，不再查 packed
        }
    }
    // packed-refs 普通仓库只在 git_dir；worktree 的本地 git_dir 没有，去重后只读一份
    let packed_bases: Vec<&std::path::Path> = if common == git_dir {
        vec![git_dir]
    } else {
        vec![git_dir, common.as_path()]
    };
    for base in packed_bases {
        let packed = match std::fs::read_to_string(base.join("packed-refs")) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for line in packed.lines() {
            if let Some((hash, name)) = line.split_once(' ') {
                if name.trim() == full_ref.trim() {
                    let h = hash.trim();
                    if h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Some(h.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 零进程取当前分支短名：HEAD symbolic 的目标；游离 HEAD 返回 "HEAD"
/// （与 rev-parse --abbrev-ref 的游离输出一致，供基准同线判断用）。
/// 读不到返回 None（调用方回落 git 命令）。
pub fn current_branch_fast(repo: &str) -> Option<String> {
    let git_dir = git_dir_of(repo)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let h = head.trim();
    if let Some(target) = h.strip_prefix("ref:") {
        return Some(
            target
                .trim()
                .strip_prefix("refs/heads/")
                .unwrap_or_else(|| target.trim())
                .to_string(),
        );
    }
    if h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some("HEAD".to_string());
    }
    None
}

/// 零进程读 remote 的 GitHub 短名（github.com/owner/repo）或原始 URL：
/// 解析 common config 的 [remote "origin"] url。读不到返回 None。
pub fn remote_fast(repo: &str) -> Option<String> {
    let git_dir = git_dir_of(repo)?;
    let text = std::fs::read_to_string(common_config_path(&git_dir)).ok()?;
    let url = parse_remote_url(&text, "origin")?;
    static REMOTE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = REMOTE_RE.get_or_init(|| {
        regex::Regex::new(r"github\.com[:/](.+?)(?:\.git)?/?$").unwrap()
    });
    if let Some(g) = re.captures(&url).and_then(|c| c.get(1)) {
        Some(format!("github.com/{}", g.as_str()))
    } else {
        Some(url)
    }
}

/// 从 git config 文本里取指定 remote 的 url（origin 缺失时返回 None，
/// 由调用方决定是否兜底其他远程——本函数只认 origin，其他远程的兜底
/// 仍走 git remote -v 进程路径，那是低频场景）。
fn parse_remote_url(config_text: &str, remote: &str) -> Option<String> {
    let section = format!("[remote \"{remote}\"]");
    let mut in_section = false;
    for line in config_text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t == section;
            continue;
        }
        if in_section {
            if let Some(v) = t.strip_prefix("url") {
                let v = v.trim_start();
                if let Some(v) = v.strip_prefix('=') {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// 零进程判断 git 身份是否已配置：读全局 .gitconfig 与仓库本地 config 的 [user] email。
/// 读不到文件返回 None（调用方回落 git 命令）。
pub fn git_identity_configured(repo: &str) -> Option<bool> {
    let mut found = false;
    let global = paths::git_home_dir().join(".gitconfig");
    if let Ok(text) = std::fs::read_to_string(&global) {
        if config_has_user_email(&text) {
            found = true;
        }
    }
    if !found {
        if let Some(git_dir) = git_dir_of(repo) {
            if let Ok(text) = std::fs::read_to_string(common_config_path(&git_dir)) {
                if config_has_user_email(&text) {
                    found = true;
                }
            }
        }
    }
    // 全局文件不存在 = git 会去系统级配置找，这种情况别下结论，回落 git 命令
    if !global.is_file() && !found {
        return None;
    }
    Some(found)
}

fn config_has_user_email(config_text: &str) -> bool {
    let mut in_user = false;
    for line in config_text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_user = t == "[user]";
            continue;
        }
        if in_user && t.starts_with("email") {
            return true;
        }
    }
    false
}

pub async fn get_current_branch(repo: &str) -> String {
    // 快路径：读 .git/HEAD（零进程）；读不到再问 git
    if let Some(name) = current_branch_fast(repo) {
        if name != "HEAD" {
            return name;
        }
        // 游离状态：短哈希也从 HEAD 文件直接截，不必再起进程
        if let Some(h) = rev_parse_fast(repo, "HEAD") {
            return format!("游离 HEAD {}", &h[..7.min(h.len())]);
        }
        return "游离 HEAD".into();
    }
    let (rc, out) = runner::run_git(
        &["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()],
        Some(repo),
        None,
    )
    .await;
    let name = trim(&out);
    if rc == 0 && name == "HEAD" {
        // 游离状态：显示具体提交更友好
        let (rc2, h) = runner::run_git(
            &["rev-parse".into(), "--short".into(), "HEAD".into()],
            Some(repo),
            None,
        )
        .await;
        let h = trim(&h);
        if rc2 == 0 && !h.is_empty() {
            return format!("游离 HEAD {h}");
        }
        return "游离 HEAD".into();
    }
    name
}

pub async fn get_remote(repo: &str) -> String {
    // 快路径：读 .git/config 的 origin（零进程）；origin 缺失/异常回落 git remote -v
    if let Some(short) = remote_fast(repo) {
        return short;
    }
    let (rc, out) = runner::run_git(&["remote".into(), "-v".into()], Some(repo), None).await;
    if rc != 0 {
        return String::new();
    }
    // 正则只编译一次（热路径）
    static REMOTE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re =
        REMOTE_RE.get_or_init(|| regex::Regex::new(r"github\.com[:/](.+?)(?:\.git)?/?$").unwrap());
    // origin 优先（行为不变）；origin 缺失时兜底用其他远程里能解析出的 github 地址
    let mut other_github: Option<String> = None;
    for line in out.lines() {
        if !line.contains("(fetch)") {
            continue;
        }
        let is_origin = line.starts_with("origin");
        if let Some(url) = line.split_whitespace().nth(1) {
            if let Some(g) = re.captures(url).and_then(|c| c.get(1)) {
                let short = format!("github.com/{}", g.as_str());
                if is_origin {
                    return short;
                }
                other_github.get_or_insert(short);
                continue;
            }
            if is_origin {
                return url.to_string();
            }
        }
    }
    other_github.unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct StatusFile {
    pub path: String,
    pub xy: String,
    #[allow(dead_code)] // 序列化给前端用
    pub mtime: i64,
}

/// 解析 git status --porcelain=v1 --branch
pub fn parse_status_output(out: &str) -> (String, i64, i64, Vec<StatusFile>) {
    let mut branch = String::new();
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut files: Vec<StatusFile> = Vec::new();
    let re =
        Regex::new(r"^(.+?)(?:\.\.\.\S+)?(?:\s+\[(?:ahead (\d+))?(?:, )?(?:behind (\d+))?\])?$")
            .unwrap();
    for line in out.lines() {
        if let Some(body) = line.strip_prefix("## ") {
            if let Some(b) = body.strip_prefix("No commits yet on ") {
                branch = b.to_string();
            } else if !body.is_empty() {
                if let Some(c) = re.captures(body) {
                    branch = c
                        .get(1)
                        .map(|m| m.as_str().trim().to_string())
                        .unwrap_or_default();
                    ahead = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
                    behind = c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
                }
            }
            continue;
        }
        if line.len() >= 3 {
            let xy = line[..2].to_string();
            let mut path = line[3..].to_string();
            if let Some(idx) = path.find("->") {
                // 重命名/拷贝：R old -> new，取新路径（原路径已不存在）
                path = path[idx + 2..].trim().to_string();
            }
            files.push(StatusFile { path, xy, mtime: 0 });
        }
    }
    (branch, ahead, behind, files)
}

pub async fn get_status(repo: &str) -> Result<Value, String> {
    let (rc, out) = runner::run_git(
        &["status".into(), "--porcelain=v1".into(), "--branch".into()],
        Some(repo),
        None,
    )
    .await;
    if rc != 0 {
        return Err(trim(&out));
    }
    let (branch, ahead, behind, files) = parse_status_output(&out);
    // mtime 是逐文件系统调用，只取前 30 个（大仓库限流）
    let file_list: Vec<Value> = files
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let mtime = if i < 30 {
                paths::file_mtime(&std::path::Path::new(repo).join(&f.path))
            } else {
                0
            };
            json!({"path": f.path, "xy": f.xy, "mtime": mtime})
        })
        .collect();
    Ok(json!({
        "branch": branch,
        "ahead": ahead,
        "behind": behind,
        "files": file_list,
        "remote": get_remote(repo).await,
        "path": repo,
    }))
}

fn is_tracked(repo: &str, path: &str) -> bool {
    let (rc, out) = runner::run_git_sync(
        &["ls-files".into(), "--".into(), path.into()],
        Some(repo),
        None,
    );
    rc == 0 && !trim(&out).is_empty()
}

/// 前 8000 字节里出现 \x00 即视为二进制
fn is_binary_file(repo: &str, path: &str) -> bool {
    let p = std::path::Path::new(repo).join(path);
    if let Ok(mut f) = std::fs::File::open(&p) {
        use std::io::Read;
        let mut buf = [0u8; 8000];
        let n = f.read(&mut buf).unwrap_or(0);
        return buf[..n].contains(&0u8);
    }
    false
}

/// 未跟踪文件的伪 diff：全文渲染成新增行；二进制文件不输出内容
fn pseudo_diff(repo: &str, path: &str) -> String {
    if is_binary_file(repo, path) {
        return format!(
            "diff --git a/{path} b/{path}\nBinary files a/{path} and b/{path} differ\n（二进制文件，无法显示差异）"
        );
    }
    let p = std::path::Path::new(repo).join(path);
    match std::fs::read_to_string(&p) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().take(500).collect();
            let body: String = lines.iter().map(|l| format!("+{l}\n")).collect();
            let mut out = format!(
                "diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n{body}",
                lines.len()
            );
            if lines.len() == 500 {
                out += "\n… 文件过长，已截断";
            }
            out
        }
        Err(_) => format!("（无法读取文件：{path}）"),
    }
}

/// 工作区相对 HEAD 的统一 diff；未跟踪文件造一个全绿 diff。
pub async fn get_diff(repo: &str, path: Option<&str>, word_diff: bool) -> String {
    let wd: Vec<String> = if word_diff {
        vec!["--word-diff=plain".into()]
    } else {
        vec![]
    };
    if let Some(p) = path {
        let (rc, _) = runner::run_git(
            &["rev-parse".into(), "--verify".into(), "HEAD".into()],
            Some(repo),
            None,
        )
        .await;
        if rc != 0 {
            // 空仓库（无 HEAD）：所有文件都按未跟踪处理
            if paths::file_mtime(&std::path::Path::new(repo).join(p)) > 0 {
                let d = pseudo_diff(repo, p);
                return d.chars().take(MAX_DIFF).collect();
            }
            return String::new();
        }
        let mut args = vec!["diff".into(), "HEAD".into()];
        args.extend(wd);
        args.push("--".into());
        args.push(p.into());
        let (rc, mut out) = runner::run_git(&args, Some(repo), None).await;
        if rc == 0
            && trim(&out).is_empty()
            && !is_tracked(repo, p)
            && paths::file_mtime(&std::path::Path::new(repo).join(p)) > 0
        {
            out = pseudo_diff(repo, p);
        }
        return out.chars().take(MAX_DIFF).collect();
    }
    let (rc, out) = runner::run_git(&["diff".into(), "HEAD".into()], Some(repo), None).await;
    let _ = rc;
    out.chars().take(MAX_DIFF).collect()
}

/// 提交元信息 + 完整 patch + 改动文件列表
pub async fn get_commit_detail(repo: &str, commit: &str) -> Result<Value, String> {
    let (rc, out) = runner::run_git(
        &[
            "show".into(),
            "-s".into(),
            "--format=%H%n%an%n%ad%n%s%n%b".into(),
            "--date=iso".into(),
            commit.into(),
        ],
        Some(repo),
        None,
    )
    .await;
    if rc != 0 {
        return Err(trim(&out));
    }
    let lines: Vec<&str> = out.trim().lines().collect();
    let mut meta = json!({
        "hash": lines.first().unwrap_or(&""),
        "short": lines.first().map(|s| s.chars().take(7).collect::<String>()).unwrap_or_default(),
        "author": lines.get(1).unwrap_or(&""),
        "date": lines.get(2).unwrap_or(&""),
        "msg": lines.get(3).unwrap_or(&""),
        "body": lines.get(4..).unwrap_or(&[]).join("\n").trim(),
    });

    let (rc, out) = runner::run_git(
        &["show".into(), commit.into(), "--format=".into()],
        Some(repo),
        None,
    )
    .await;
    let _ = rc;
    meta["patch"] = Value::String(out.chars().take(MAX_DIFF).collect());

    let (rc, out) = runner::run_git(
        &[
            "diff-tree".into(),
            "--no-commit-id".into(),
            "--name-status".into(),
            "-r".into(),
            commit.into(),
        ],
        Some(repo),
        None,
    )
    .await;
    if rc == 0 {
        let files: Vec<Value> = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let parts: Vec<&str> = l.split('\t').collect();
                let xy = parts.first().unwrap_or(&"").to_string();
                let path = parts.last().unwrap_or(&"").to_string();
                json!({"xy": xy, "path": path})
            })
            .collect();
        meta["files"] = Value::Array(files);
    }
    Ok(meta)
}

/// 仓库根；不是 git 仓库返回 None。子目录会向上找到根。
pub async fn find_root(path: &str) -> Option<String> {
    let (rc, out) = runner::run_git(
        &["rev-parse".into(), "--show-toplevel".into()],
        Some(path),
        None,
    )
    .await;
    if rc == 0 {
        let r = trim(&out);
        if !r.is_empty() {
            return Some(r);
        }
    }
    None
}

pub async fn init_repo(path: &str) -> Value {
    let (rc, out) = runner::run_git(
        &["init".into(), "-b".into(), "main".into()],
        Some(path),
        None,
    )
    .await;
    json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { Value::String(trim(&out)) }})
}

/// git branch -vv → [{name, current, hash, upstream, msg}]
pub async fn get_branches(repo: &str) -> Vec<Value> {
    let (rc, out) = runner::run_git(&["branch".into(), "-vv".into()], Some(repo), None).await;
    if rc != 0 {
        return vec![];
    }
    let re =
        Regex::new(r"^(\*|\s)\s+(\S+)\s+([0-9a-f]+)\s+(?:\[(\S+?)(?:: gone \d+/\d+)?\]\s+)?(.*)$")
            .unwrap();
    let mut branches = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.contains("(HEAD detached") || line.contains("(no branch")
        {
            continue;
        }
        if let Some(c) = re.captures(line) {
            branches.push(json!({
                "current": c.get(1).map(|m| m.as_str() == "*").unwrap_or(false),
                "name": c.get(2).map(|m| m.as_str()).unwrap_or(""),
                "hash": c.get(3).map(|m| m.as_str()).unwrap_or(""),
                "upstream": c.get(4).map(|m| m.as_str()).unwrap_or(""),
                "msg": c.get(5).map(|m| m.as_str().trim()).unwrap_or(""),
            }));
        }
    }
    branches
}

pub async fn has_upstream(repo: &str, branch: &str) -> bool {
    // 快路径：config 里 [branch "xxx"] 有 remote/merge 即有关联上游（零进程）
    if let Some(git_dir) = git_dir_of(repo) {
        if let Ok(text) = std::fs::read_to_string(common_config_path(&git_dir)) {
            let section = format!("[branch \"{branch}\"]");
            let mut in_section = false;
            let mut has_remote = false;
            let mut has_merge = false;
            for line in text.lines() {
                let t = line.trim();
                if t.starts_with('[') {
                    in_section = t == section;
                    continue;
                }
                if in_section {
                    if t.starts_with("remote") {
                        has_remote = true;
                    }
                    if t.starts_with("merge") {
                        has_merge = true;
                    }
                }
            }
            if has_remote && has_merge {
                return true;
            }
            // config 读到了但该分支没有上游配置：git 还会看 $GIT_DIR/refs/remotes
            // 的隐式默认，极少见；保险起见仍回落 git 命令确认
        }
    }
    let arg = format!("{branch}@{{upstream}}");
    let (rc, _) = runner::run_git(
        &["rev-parse".into(), "--abbrev-ref".into(), arg],
        Some(repo),
        None,
    )
    .await;
    rc == 0
}

/// 打开仓库：定位仓库根 → 加入列表 → 设为活动。
/// 非 git 目录不算错误，返回 is_git_repo=false 交由前端询问是否初始化。
pub async fn open_repo(path: &str) -> Value {
    if !std::path::Path::new(path).is_dir() {
        return json!({"ok": false, "error": "目录不存在"});
    }
    match find_root(path).await {
        Some(root) => {
            let rec = crate::repositories::add_repo(&root, None, "");
            json!({"ok": true, "repo": rec})
        }
        None => json!({"ok": true, "is_git_repo": false}),
    }
}

// ---------------- v3.0 统计接口 ----------------

/// 解析 numstat 输出 → {path: (plus, minus)}；二进制行（增删为 "-"）跳过。
/// 供 numstat 聚合与 /repo/status_hardcore 共用。
pub fn parse_numstat(out: &str) -> BTreeMap<String, (i64, i64)> {
    let mut d = BTreeMap::new();
    for line in out.lines() {
        let p: Vec<&str> = line.split('\t').collect();
        if p.len() >= 3 && p[0] != "-" {
            if let (Ok(pl), Ok(mi)) = (p[0].parse::<i64>(), p[1].parse::<i64>()) {
                d.insert(p[2].to_string(), (pl, mi));
            }
        }
    }
    d
}

/// git diff --numstat 聚合，返回 {plus, minus, per_file}
async fn numstat(repo: &str, args: &[String]) -> (i64, i64, BTreeMap<String, (i64, i64)>) {
    let mut cmd = vec!["diff".into(), "--numstat".into()];
    cmd.extend(args.iter().cloned());
    let (rc, out) = runner::run_git(&cmd, Some(repo), None).await;
    let per_file = if rc == 0 {
        parse_numstat(&out)
    } else {
        BTreeMap::new()
    };
    let plus = per_file.values().map(|(p, _)| *p).sum();
    let minus = per_file.values().map(|(_, m)| *m).sum();
    (plus, minus, per_file)
}

/// 数文件行数。cap=Some(limit) 时读到 limit 字节为止即返回 (行数, true)，
/// 防大文件全量读入内存；二进制文件行数无意义，按 0 处理。
pub fn count_lines(repo: &str, rel_path: &str, cap: Option<u64>) -> (i64, bool) {
    if is_binary_file(repo, rel_path) {
        return (0, false);
    }
    let p = std::path::Path::new(repo).join(rel_path);
    let mut f = match std::fs::File::open(&p) {
        Ok(f) => f,
        Err(_) => return (0, false),
    };
    use std::io::Read;
    let mut buf = vec![0u8; 8192];
    let mut total = 0u64;
    let mut count = 0i64;
    while let Ok(n) = f.read(&mut buf) {
        if n == 0 {
            break;
        }
        count += buf[..n].iter().filter(|&&b| b == b'\n').count() as i64;
        total += n as u64;
        if let Some(limit) = cap {
            if total >= limit {
                return (count, true);
            }
        }
    }
    (count, false)
}

/// 概览面板的独有数据：7 天活动 / 总提交数 / 最后提交时间。
/// 状态与改动统计由前端从 status_hardcore 派生（同一份 diff --numstat HEAD），
/// 端点自身不再跑 status + numstat——启动路径上省两条 ~250ms 的 git spawn。
pub async fn get_overview_extra(repo: &str) -> Value {
    let act_args = vec![
        "log".into(),
        "--since=7 days ago".into(),
        "--pretty=format:%ad".into(),
        "--date=short".into(),
    ];
    let cnt_args = vec!["rev-list".into(), "--count".into(), "HEAD".into()];
    let last_args = vec!["log".into(), "-1".into(), "--format=%cI".into()];
    let ((act_rc, act_out), (cnt_rc, cnt_out), (last_rc, last_out)) = tokio::join!(
        runner::run_git(&act_args, Some(repo), None),
        runner::run_git(&cnt_args, Some(repo), None),
        runner::run_git(&last_args, Some(repo), None),
    );
    let mut dates: BTreeMap<String, i64> = BTreeMap::new();
    if act_rc == 0 {
        for x in act_out.split_whitespace() {
            *dates.entry(x.to_string()).or_insert(0) += 1;
        }
    }
    let activity: Vec<Value> = dates
        .iter()
        .map(|(d, c)| json!({"date": d, "count": c}))
        .collect();
    let total = if cnt_rc == 0 {
        cnt_out.trim().parse::<i64>().unwrap_or(0)
    } else {
        0
    };
    let last = if last_rc == 0 {
        trim(&last_out)
    } else {
        String::new()
    };
    json!({
        "activity": activity,
        "total_commits": total,
        "last_commit_time": last,
    })
}

/// 仓库概览：状态 + 改动统计 + 最近 7 天活动（完整自包含版；
/// 启动路径走 /api/repo/overview_extra + 前端派生，不经过这里）。
pub async fn get_overview(repo: &str) -> Result<Value, String> {
    let s = get_status(repo).await?;
    let files = s["files"].as_array().cloned().unwrap_or_default();
    let mut summary = BTreeMap::from([("M", 0i64), ("A", 0i64), ("D", 0i64), ("R", 0i64)]);
    for f in &files {
        let xy = f["xy"].as_str().unwrap_or("");
        if xy == "??" {
            *summary.get_mut("A").unwrap() += 1;
        } else {
            for ch in ["M", "A", "D", "R"] {
                if xy.contains(ch) {
                    *summary.get_mut(ch).unwrap() += 1;
                }
            }
        }
    }

    // 只读命令并行跑
    let head_args = vec!["HEAD".into()];
    let ((mut plus, minus, _per), extra) =
        tokio::join!(numstat(repo, &head_args), get_overview_extra(repo));

    for f in &files {
        if f["xy"].as_str() == Some("??") {
            plus += count_lines(repo, f["path"].as_str().unwrap_or(""), None).0;
        }
    }

    Ok(json!({
        "path": repo,
        "remote": s["remote"],
        "branch": s["branch"],
        "ahead": s["ahead"],
        "behind": s["behind"],
        "workspace_files": files.len(),
        "changed_summary": json!({"M": summary["M"], "A": summary["A"], "D": summary["D"], "R": summary["R"]}),
        "diff_total": {"plus": plus, "minus": minus},
        "activity": extra["activity"],
        "total_commits": extra["total_commits"],
        "last_commit_time": extra["last_commit_time"],
    }))
}

/// 单个文件的改动统计：+N -M、大小、mtime
pub async fn get_file_stats(repo: &str, file: &str) -> Value {
    let s = get_status(repo)
        .await
        .unwrap_or_else(|_| json!({"files": []}));
    let xy = s["files"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .find(|f| f["path"].as_str() == Some(file))
        .map(|f| f["xy"].as_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let (plus, minus) = if xy == "??" {
        (count_lines(repo, file, None).0, 0)
    } else {
        let (pl, mi, _) = numstat(repo, &["HEAD".into(), "--".into(), file.into()]).await;
        (pl, mi)
    };
    let p = std::path::Path::new(repo).join(file);
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    json!({
        "file": file,
        "xy": xy,
        "plus": plus,
        "minus": minus,
        "size": size,
        "mtime": paths::file_mtime(&p),
    })
}

/// 提交统计：元信息 + 每文件状态和增删行数
pub async fn get_commit_stats(repo: &str, commit: &str) -> Result<Value, String> {
    let (rc, out) = runner::run_git(
        &[
            "show".into(),
            "-s".into(),
            "--format=%H%n%an%n%ad%n%s".into(),
            "--date=iso".into(),
            commit.into(),
        ],
        Some(repo),
        None,
    )
    .await;
    if rc != 0 {
        return Err(trim(&out));
    }
    let lines: Vec<&str> = out.trim().lines().collect();
    let mut result = json!({
        "hash": lines.first().unwrap_or(&""),
        "short": lines.first().map(|s| s.chars().take(7).collect::<String>()).unwrap_or_default(),
        "author": lines.get(1).unwrap_or(&""),
        "date": lines.get(2).unwrap_or(&""),
        "msg": lines.get(3).unwrap_or(&""),
        "files": [],
    });

    let mut status: BTreeMap<String, String> = BTreeMap::new();
    let (rc, out) = runner::run_git(
        &[
            "diff-tree".into(),
            "--no-commit-id".into(),
            "--name-status".into(),
            "-r".into(),
            "-M".into(),
            commit.into(),
        ],
        Some(repo),
        None,
    )
    .await;
    if rc == 0 {
        for line in out.lines() {
            let p: Vec<&str> = line.split('\t').collect();
            if p.len() >= 2 {
                status.insert(p.last().unwrap().to_string(), p[0].to_string());
            }
        }
    }

    let mut plus = 0i64;
    let mut minus = 0i64;
    let mut file_list: Vec<Value> = Vec::new();
    let (rc, out) = runner::run_git(
        &[
            "show".into(),
            "--numstat".into(),
            "--format=".into(),
            "-M".into(),
            commit.into(),
        ],
        Some(repo),
        None,
    )
    .await;
    if rc == 0 {
        for line in out.lines() {
            let p: Vec<&str> = line.split('\t').collect();
            if p.len() >= 3 {
                let pl = if p[0] == "-" {
                    0
                } else {
                    p[0].parse::<i64>().unwrap_or(0)
                };
                let mi = if p[1] == "-" {
                    0
                } else {
                    p[1].parse::<i64>().unwrap_or(0)
                };
                let path = p[2].to_string();
                plus += pl;
                minus += mi;
                file_list.push(json!({
                    "path": path,
                    "xy": status.get(&path).cloned().unwrap_or_else(|| "M".into()),
                    "plus": pl,
                    "minus": mi,
                }));
            }
        }
    }
    result["files"] = Value::Array(file_list);
    result["total"] = json!({"plus": plus, "minus": minus});
    Ok(result)
}

/// 候选基准与当前分支是否同指一条支线：origin/main ↔ main、origin/master ↔ master
/// （remote-tracking 引用与本地同名分支本就是同一条线）。同线时不能当基准——
/// 推送后 HEAD 与 origin/<当前分支> 同点，`HEAD ^base` 退化成空集，
/// 整条提交历史凭空消失（典型：在 main 上本地提交并推送，历史页从此空白）。
fn same_branch_line(cand: &str, current: &str) -> bool {
    cand.strip_prefix("origin/") == Some(current)
        || current.strip_prefix("origin/") == Some(cand)
}

/// 探测"主基准"分支：origin/main → origin/master → main → master，
/// 第一个存在、不是当前分支、且与当前分支不同线的。找不到返回 None
/// （如当前就在 main/master 上——此时显示全部历史，不做独有过滤）。
pub async fn detect_base_branch(repo: &str, current: &str) -> Option<String> {
    // refs 文件全在手里时（普通仓库 / worktree 共享目录可读），快路径的否定
    // 结果即是终审——不存在就是不存在。早期这里对每个不存在的候选都白挨一发
    // rev-parse 兜底（冷 spawn 230ms+，启动路径上 origin/master + master
    // 连挨两发），实际 git 也只会确认"不存在"。
    let trust_fast = refs_readable(repo);
    for cand in ["origin/main", "origin/master", "main", "master"] {
        if cand == current || same_branch_line(cand, current) {
            continue;
        }
        // 快路径：读 refs 文件 / packed-refs 判断存在性（零进程）
        if rev_parse_fast(repo, cand).is_some() {
            return Some(cand.to_string());
        }
        if trust_fast {
            continue;
        }
        let (rc, _) = runner::run_git(
            &[
                "rev-parse".into(),
                "--verify".into(),
                "--quiet".into(),
                cand.into(),
            ],
            Some(repo),
            None,
        )
        .await;
        if rc == 0 {
            return Some(cand.to_string());
        }
    }
    None
}

/// 历史表格数据：一次 git log --numstat 拿全部提交的增删统计。
/// --first-parent 只沿当前分支第一父链走；若调用方探测到主基准分支
/// （main/master 家族，且不是当前支线自己），则只显示当前支线**相对基准的
/// 独有提交**（HEAD ^base，GitHub 分支页语义）——各支线历史严格分开，
/// 共享的分叉点之前历史不混入。当前支线即主线时 base 为 None，显示全部历史。
/// 返回 (commits, base)：base = 所用基准（None = 显示当前分支全部历史）。
/// base 由调用方探测后传入：cached_log_table 要用同一次探测结果做缓存校验，
/// 顺带省掉这里重复的 rev-parse。
pub async fn get_log_table(
    repo: &str,
    n: i64,
    branch: &str,
    base: Option<String>,
) -> (Vec<Value>, Option<String>) {
    let branch = if branch.starts_with('-') { "" } else { branch };
    let mut args = vec![
        "log".into(),
        "--first-parent".into(),
        "--numstat".into(),
        "-M".into(),
    ];
    if !branch.is_empty() {
        args.push(branch.to_string());
    }
    args.extend([
        "-n".into(),
        n.to_string(),
        "--pretty=format:\x01%H|%an|%cI|%s".into(),
    ]);
    if let Some(b) = &base {
        args.push(format!("^{b}")); // 排除基准分支可达的提交：只看本支线独有
    }
    args.push("HEAD".into());
    args.push("--".into());
    let (rc, out) = runner::run_git(&args, Some(repo), None).await;
    if rc != 0 {
        return (vec![], base);
    }
    let mut commits: Vec<Value> = Vec::new();
    let mut cur: Option<Value> = None;
    for line in out.lines() {
        if let Some(body) = line.strip_prefix('\u{1}') {
            let parts: Vec<&str> = body.splitn(4, '|').collect();
            let mut c = json!({
                "hash": parts.first().unwrap_or(&""),
                "short": parts.first().map(|s| s.chars().take(7).collect::<String>()).unwrap_or_default(),
                "author": parts.get(1).unwrap_or(&""),
                "date": parts.get(2).unwrap_or(&""),
                "msg": parts.get(3).unwrap_or(&""),
                "plus": 0i64,
                "minus": 0i64,
            });
            let _ = &mut c;
            cur = Some(c);
            commits.push(cur.clone().unwrap());
        } else if let Some(c) = &mut cur {
            if !line.trim().is_empty() {
                let p: Vec<&str> = line.split('\t').collect();
                if p.len() >= 3 && p[0] != "-" {
                    if let (Ok(pl), Ok(mi)) = (p[0].parse::<i64>(), p[1].parse::<i64>()) {
                        c["plus"] = Value::Number(
                            c["plus"].as_i64().unwrap_or(0).saturating_add(pl).into(),
                        );
                        c["minus"] = Value::Number(
                            c["minus"].as_i64().unwrap_or(0).saturating_add(mi).into(),
                        );
                    }
                }
            }
        }
    }
    (commits, base)
}

#[cfg(test)]
mod tests {
    use super::{current_branch_fast, refs_readable, rev_parse_fast, same_branch_line};

    /// 回归：推送后历史消失的 bug——origin/main 与本地 main 是同一条支线，
    /// 绝不能被 detect_base_branch 选为基准（否则 HEAD ^base 成空集）。
    #[test]
    fn same_branch_line_detects_remote_tracking_of_current() {
        // 同线：remote-tracking ↔ 本地同名分支（两个方向都要认）
        assert!(same_branch_line("origin/main", "main"));
        assert!(same_branch_line("main", "origin/main"));
        assert!(same_branch_line("origin/master", "master"));
        assert!(same_branch_line("master", "origin/master"));
        // 不同线：普通支线 vs 主线；main vs master 也是两条线
        assert!(!same_branch_line("origin/main", "feat"));
        assert!(!same_branch_line("origin/main", "master"));
        assert!(!same_branch_line("main", "feat"));
        assert!(!same_branch_line("origin/main", "origin/main")); // 相等由 cand == current 兜底
        // 空分支名（探测失败）不与任何候选同线
        assert!(!same_branch_line("origin/main", ""));
    }

    /// 零进程 rev-parse：拿工程仓库本身（本身就是 git 仓库）与系统 git 对拍。
    /// 这些函数替代原来的 spawn git 进程，读错就等于界面数据错，必须锁死。
    /// （注意不能用 runner::run_git_sync 对拍：它走 app 内置 runtime git 路径，
    /// 开发机上该文件可能不存在。）
    #[test]
    fn rev_parse_fast_matches_real_git() {
        let repo = env!("CARGO_MANIFEST_DIR");
        let sys_git = |args: &[&str]| -> Option<String> {
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        // HEAD：40-hex，且与 git rev-parse HEAD 一致
        let fast = rev_parse_fast(repo, "HEAD").expect("工程仓库应能零进程解析 HEAD");
        assert_eq!(fast.len(), 40);
        if let Some(real) = sys_git(&["rev-parse", "HEAD"]) {
            assert_eq!(fast, real);
        }
        // 当前分支短名：非 HEAD（除非恰好在游离状态，CI 上不会）
        let branch = current_branch_fast(repo).expect("应能零进程读取当前分支");
        assert!(!branch.is_empty());
        assert_ne!(branch, "HEAD");
        // 分支名的全称形式与 git 一致
        let fast_branch = rev_parse_fast(repo, &branch).expect("分支名应可解析");
        if let Some(real) = sys_git(&["rev-parse", &branch]) {
            assert_eq!(fast_branch, real);
        }
        // 不存在的 ref 返回 None（不能编造哈希）
        assert!(rev_parse_fast(repo, "refs/heads/no-such-branch-xyz").is_none());
    }

    /// 工程仓库（普通 .git 目录）的 refs 文件完全可读——快路径否定结果
    /// 即是终审，detect_base_branch 无需再 spawn git 逐个确认。
    #[test]
    fn refs_readable_true_for_project_repo() {
        assert!(refs_readable(env!("CARGO_MANIFEST_DIR")));
    }

    /// worktree 回归：linked worktree 的 .git 是文件，refs/heads/* 与
    /// packed-refs 实际在主仓库目录（commondir）。旧实现只读 worktree
    /// 本地 git_dir，任何分支都解析不到；现在经 commondir 回落共享目录。
    #[test]
    fn rev_parse_fast_reads_shared_refs_in_worktree() {
        let tmp = std::env::temp_dir().join(format!(
            "gc-wt-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| -> bool {
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        let ok = git(&["init", "."])
            && std::fs::write(repo.join("f.txt"), "x").is_ok()
            && git(&["add", "."])
            && git(&[
                "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "init",
            ])
            && git(&["branch", "-M", "main"]);
        let wt = tmp.join("wt");
        let ok = ok && git(&["worktree", "add", wt.to_string_lossy().as_ref(), "-b", "feat"]);
        if !ok {
            let _ = std::fs::remove_dir_all(&tmp);
            return; // 环境无 git / 版本过老：跳过（不视为失败）
        }
        let wt_s = wt.to_string_lossy().to_string();
        // worktree 内解析主仓库分支：经 commondir 找到共享 refs
        let fast = rev_parse_fast(&wt_s, "main").expect("worktree 内应能零进程解析共享分支 main");
        assert_eq!(fast.len(), 40);
        let real = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["rev-parse", "main"])
            .output()
            .unwrap();
        assert_eq!(fast, String::from_utf8_lossy(&real.stdout).trim());
        // worktree 自身的分支也能解析
        assert!(rev_parse_fast(&wt_s, "feat").is_some());
        // commondir 可定位 → refs_readable 为 true（detect_base_branch 信任快路径）
        assert!(refs_readable(&wt_s));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
