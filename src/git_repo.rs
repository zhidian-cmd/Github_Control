//! Git 仓库业务封装。与 Python 版 git_repo.py 逐函数对应。

use crate::paths;
use crate::runner;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub const MAX_DIFF: usize = 300 * 1024; // 300KB，防止前端卡死

fn trim(s: &str) -> String {
    s.trim().to_string()
}

pub async fn get_current_branch(repo: &str) -> String {
    let (rc, out) = runner::run_git(&["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()], Some(repo), None).await;
    let name = trim(&out);
    if rc == 0 && name == "HEAD" {
        // 游离状态：显示具体提交更友好
        let (rc2, h) = runner::run_git(&["rev-parse".into(), "--short".into(), "HEAD".into()], Some(repo), None).await;
        let h = trim(&h);
        if rc2 == 0 && !h.is_empty() {
            return format!("游离 HEAD {h}");
        }
        return "游离 HEAD".into();
    }
    name
}

pub async fn get_remote(repo: &str) -> String {
    let (rc, out) = runner::run_git(&["remote".into(), "-v".into()], Some(repo), None).await;
    if rc != 0 {
        return String::new();
    }
    // 正则只编译一次（热路径）
    static REMOTE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = REMOTE_RE
        .get_or_init(|| regex::Regex::new(r"github\.com[:/](.+?)(?:\.git)?/?$").unwrap());
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
    let re = Regex::new(
        r"^(.+?)(?:\.\.\.\S+)?(?:\s+\[(?:ahead (\d+))?(?:, )?(?:behind (\d+))?\])?$",
    )
    .unwrap();
    for line in out.lines() {
        if let Some(body) = line.strip_prefix("## ") {
            if let Some(b) = body.strip_prefix("No commits yet on ") {
                branch = b.to_string();
            } else if !body.is_empty() {
                if let Some(c) = re.captures(body) {
                    branch = c.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
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
    let (rc, out) = runner::run_git_sync(&["ls-files".into(), "--".into(), path.into()], Some(repo), None);
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
        let (rc, _) = runner::run_git(&["rev-parse".into(), "--verify".into(), "HEAD".into()], Some(repo), None).await;
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
        if rc == 0 && trim(&out).is_empty() && !is_tracked(repo, p) && paths::file_mtime(&std::path::Path::new(repo).join(p)) > 0 {
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
            "show".into(), "-s".into(), "--format=%H%n%an%n%ad%n%s%n%b".into(),
            "--date=iso".into(), commit.into(),
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

    let (rc, out) = runner::run_git(&["show".into(), commit.into(), "--format=".into()], Some(repo), None).await;
    let _ = rc;
    meta["patch"] = Value::String(out.chars().take(MAX_DIFF).collect());

    let (rc, out) = runner::run_git(
        &["diff-tree".into(), "--no-commit-id".into(), "--name-status".into(), "-r".into(), commit.into()],
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
    let (rc, out) = runner::run_git(&["rev-parse".into(), "--show-toplevel".into()], Some(path), None).await;
    if rc == 0 {
        let r = trim(&out);
        if !r.is_empty() {
            return Some(r);
        }
    }
    None
}

pub async fn init_repo(path: &str) -> Value {
    let (rc, out) = runner::run_git(&["init".into(), "-b".into(), "main".into()], Some(path), None).await;
    json!({"ok": rc == 0, "error": if rc == 0 { Value::Null } else { Value::String(trim(&out)) }})
}

/// git branch -vv → [{name, current, hash, upstream, msg}]
pub async fn get_branches(repo: &str) -> Vec<Value> {
    let (rc, out) = runner::run_git(&["branch".into(), "-vv".into()], Some(repo), None).await;
    if rc != 0 {
        return vec![];
    }
    let re = Regex::new(r"^(\*|\s)\s+(\S+)\s+([0-9a-f]+)\s+(?:\[(\S+?)(?:: gone \d+/\d+)?\]\s+)?(.*)$").unwrap();
    let mut branches = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.contains("(HEAD detached") || line.contains("(no branch") {
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
    let arg = format!("{branch}@{{upstream}}");
    let (rc, _) = runner::run_git(&["rev-parse".into(), "--abbrev-ref".into(), arg], Some(repo), None).await;
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
    let per_file = if rc == 0 { parse_numstat(&out) } else { BTreeMap::new() };
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
    loop {
        let n = match f.read(&mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
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

/// 仓库概览：状态 + 改动统计 + 最近 7 天活动
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
    let act_args = vec![
        "log".into(), "--since=7 days ago".into(), "--pretty=format:%ad".into(), "--date=short".into(),
    ];
    let cnt_args = vec!["rev-list".into(), "--count".into(), "HEAD".into()];
    let last_args = vec!["log".into(), "-1".into(), "--format=%cI".into()];
    let f_num = numstat(repo, &head_args);
    let f_act = runner::run_git(&act_args, Some(repo), None);
    let f_cnt = runner::run_git(&cnt_args, Some(repo), None);
    let f_last = runner::run_git(&last_args, Some(repo), None);
    let ((mut plus, minus, _per), (act_rc, act_out), (cnt_rc, cnt_out), (last_rc, last_out)) =
        tokio::join!(f_num, f_act, f_cnt, f_last);

    for f in &files {
        if f["xy"].as_str() == Some("??") {
            plus += count_lines(repo, f["path"].as_str().unwrap_or(""), None).0;
        }
    }

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
    let total = if cnt_rc == 0 { cnt_out.trim().parse::<i64>().unwrap_or(0) } else { 0 };
    let last = if last_rc == 0 { trim(&last_out) } else { String::new() };

    Ok(json!({
        "path": repo,
        "remote": s["remote"],
        "branch": s["branch"],
        "ahead": s["ahead"],
        "behind": s["behind"],
        "workspace_files": files.len(),
        "changed_summary": json!({"M": summary["M"], "A": summary["A"], "D": summary["D"], "R": summary["R"]}),
        "diff_total": {"plus": plus, "minus": minus},
        "activity": activity,
        "total_commits": total,
        "last_commit_time": last,
    }))
}

/// 单个文件的改动统计：+N -M、大小、mtime
pub async fn get_file_stats(repo: &str, file: &str) -> Value {
    let s = get_status(repo).await.unwrap_or_else(|_| json!({"files": []}));
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
        &["show".into(), "-s".into(), "--format=%H%n%an%n%ad%n%s".into(), "--date=iso".into(), commit.into()],
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
        &["diff-tree".into(), "--no-commit-id".into(), "--name-status".into(), "-r".into(), "-M".into(), commit.into()],
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
        &["show".into(), "--numstat".into(), "--format=".into(), "-M".into(), commit.into()],
        Some(repo),
        None,
    )
    .await;
    if rc == 0 {
        for line in out.lines() {
            let p: Vec<&str> = line.split('\t').collect();
            if p.len() >= 3 {
                let pl = if p[0] == "-" { 0 } else { p[0].parse::<i64>().unwrap_or(0) };
                let mi = if p[1] == "-" { 0 } else { p[1].parse::<i64>().unwrap_or(0) };
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

/// 历史表格数据：一次 git log --numstat 拿全部提交的增删统计。
/// branch 为空 = 当前 HEAD；否则查看指定分支的历史（各分支历史相互独立）。
pub async fn get_log_table(repo: &str, n: i64, branch: &str) -> Vec<Value> {
    let branch = if branch.starts_with('-') { "" } else { branch };
    let mut args = vec!["log".into(), "--numstat".into(), "-M".into()];
    if !branch.is_empty() {
        args.push(branch.to_string());
    }
    args.extend([
        "--pretty=format:\x01%H|%an|%cI|%s".into(),
        "-n".into(), n.to_string(),
    ]);
    let (rc, out) = runner::run_git(&args, Some(repo), None).await;
    if rc != 0 {
        return vec![];
    }
    let mut commits: Vec<Value> = Vec::new();
    let mut cur: Option<Value> = None;
    for line in out.lines() {
        if let Some(body) = line.strip_prefix('\u{1}') {
            let parts: Vec<&str> = body.splitn(4, '|').collect();
            let mut c = json!({
                "hash": parts.get(0).unwrap_or(&""),
                "short": parts.get(0).map(|s| s.chars().take(7).collect::<String>()).unwrap_or_default(),
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
                        c["plus"] = Value::Number(c["plus"].as_i64().unwrap_or(0).saturating_add(pl).into());
                        c["minus"] = Value::Number(c["minus"].as_i64().unwrap_or(0).saturating_add(mi).into());
                    }
                }
            }
        }
    }
    commits
}

// ---------------- 提交图（lane 布局移植自 oil-git src/graph.ts） ----------------

const GRAPH_ROW_H: f64 = 28.0;   // 每行高度（oil-git 56，这里紧凑一倍）
const GRAPH_LANE_W: f64 = 20.0;  // 泳道宽度
const GRAPH_COLORS: usize = 6;   // 颜色循环数（前端 --lane-0..5）

/// 全分支提交图：branches+remotes+tags+HEAD 最近 n 条提交，
/// Rust 侧完成泳道分配（oil-git layoutGraph 算法），前端只画 SVG。
/// 返回 {head, width, commits:[{...refs,lane,x,y}], edges:[{x1,y1,x2,y2,c,pending}]}。
pub async fn get_graph(repo: &str, n: i64) -> Result<Value, String> {
    // HEAD（当前 checkout，占 0 号泳道）
    let (hrc, hout) = runner::run_git(&["rev-parse".into(), "HEAD".into()], Some(repo), None).await;
    let head = if hrc == 0 { hout.trim().to_string() } else { String::new() };

    // refs：本地分支 / 远程分支 / 标签（for-each-ref 毫秒级）。
    // %(*objectname) 是 peel 后的 commit：annotated tag 的 objectname 是 tag 对象
    // 本身，直接用会挂不到提交上（oil-git 同款处理：peel 优先，否则裸 objectname）
    let (rrc, rout) = runner::run_git(
        &[
            "for-each-ref".into(),
            "--format=%(refname)%00%(objectname)%00%(*objectname)".into(),
            "refs/heads".into(), "refs/remotes".into(), "refs/tags".into(),
        ],
        Some(repo),
        None,
    )
    .await;
    let mut refs: Vec<(String, String, String)> = Vec::new(); // (hash, name, kind)
    if rrc == 0 {
        for line in rout.lines() {
            let p: Vec<&str> = line.split('\u{0}').collect();
            if p.len() != 3 {
                continue;
            }
            let (full, hash) = (p[0], if p[2].is_empty() { p[1] } else { p[2] });
            let (name, kind) = if let Some(nm) = full.strip_prefix("refs/heads/") {
                (nm.to_string(), "branch")
            } else if let Some(nm) = full.strip_prefix("refs/tags/") {
                (nm.to_string(), "tag")
            } else if let Some(nm) = full.strip_prefix("refs/remotes/") {
                if nm.ends_with("/HEAD") {
                    continue; // origin/HEAD 无展示价值
                }
                (nm.to_string(), "remote")
            } else {
                continue;
            };
            refs.push((hash.to_string(), name, kind.to_string()));
        }
    }

    // 提交：topo+date 序，全分支（oil-git history 的 reference="all" 取法）
    let (lrc, lout) = runner::run_git(
        &[
            "log".into(), "--topo-order".into(), "--date-order".into(),
            "--branches".into(), "--remotes".into(), "--tags".into(),
            "-n".into(), n.to_string(),
            "--format=%H%x00%P%x00%an%x00%cI%x00%s".into(),
            "--".into(),
        ],
        Some(repo),
        None,
    )
    .await;
    if lrc != 0 {
        // 空仓库（还没有任何提交）：返回空图而不是报错
        return Ok(json!({"head": head, "width": 72, "commits": [], "edges": []}));
    }

    struct RawCommit {
        hash: String,
        parents: Vec<String>,
        author: String,
        date: String,
        msg: String,
    }
    let mut raw: Vec<RawCommit> = Vec::new();
    for line in lout.lines() {
        let p: Vec<&str> = line.split('\u{0}').collect();
        if p.len() < 5 {
            continue;
        }
        raw.push(RawCommit {
            hash: p[0].to_string(),
            parents: p[1].split_whitespace().map(String::from).collect(),
            author: p[2].to_string(),
            date: p[3].to_string(),
            msg: p[4].to_string(),
        });
    }
    if raw.is_empty() {
        return Ok(json!({"head": head, "width": 72, "commits": [], "edges": []}));
    }

    // ---- lane 分配（oil-git layoutGraph 核心移植）----
    // lanes[i] = Some(hash) 表示该泳道"将流向"这个提交；None = 空槽可复用
    let mut lanes: Vec<Option<String>> = if head.is_empty() { vec![] } else { vec![Some(head.clone())] };
    let mut positions: HashMap<String, (f64, f64, usize)> = HashMap::new(); // hash → (x, y, lane)
    let mut max_lane = 0usize;
    let reserve = |lanes: &mut Vec<Option<String>>, hash: &str, max_lane: &mut usize| -> usize {
        if let Some(i) = lanes.iter().position(|l| l.as_deref() == Some(hash)) {
            return i;
        }
        let i = match lanes.iter().position(|l| l.is_none()) {
            Some(i) => {
                lanes[i] = Some(hash.to_string());
                i
            }
            None => {
                lanes.push(Some(hash.to_string()));
                lanes.len() - 1
            }
        };
        *max_lane = (*max_lane).max(i);
        i
    };
    let mut rows: Vec<Value> = Vec::with_capacity(raw.len());
    for (index, c) in raw.iter().enumerate() {
        let lane = reserve(&mut lanes, &c.hash, &mut max_lane);
        let x = 20.0 + lane as f64 * GRAPH_LANE_W;
        let y = index as f64 * GRAPH_ROW_H + GRAPH_ROW_H / 2.0;
        positions.insert(c.hash.clone(), (x, y, lane));
        lanes[lane] = None;
        // 第一个父提交继承本泳道，其余父提交另开泳道（分叉线）
        for (i, parent) in c.parents.iter().enumerate() {
            if lanes.iter().any(|l| l.as_deref() == Some(parent.as_str())) {
                continue;
            }
            if i == 0 && lanes[lane].is_none() {
                lanes[lane] = Some(parent.clone());
            } else {
                reserve(&mut lanes, parent, &mut max_lane);
            }
        }
        let short: String = c.hash.chars().take(7).collect();
        let crefs: Vec<Value> = refs
            .iter()
            .filter(|(h, _, _)| h == &c.hash)
            .map(|(_, name, kind)| json!({"name": name, "kind": kind}))
            .collect();
        rows.push(json!({
            "hash": c.hash, "short": short,
            "author": c.author, "date": c.date, "msg": c.msg,
            "lane": lane, "x": x, "y": y,
            "head": c.hash == head,
            "refs": crefs,
        }));
    }

    // ---- 边（贝塞尔端点）----
    // 父提交不在取回范围（被 -n 截断）→ pending 虚线延伸到列表末尾
    let mut continuations: HashMap<String, f64> = HashMap::new(); // hash → x
    for (i, l) in lanes.iter().enumerate() {
        if let Some(h) = l {
            continuations.insert(h.clone(), 20.0 + i as f64 * GRAPH_LANE_W);
        }
    }
    let bottom_y = raw.len() as f64 * GRAPH_ROW_H + 16.0;
    let mut edges: Vec<Value> = Vec::new();
    for c in &raw {
        let Some(&(x1, y1, lane)) = positions.get(&c.hash) else { continue };
        for parent in &c.parents {
            let (x2, y2, pending) = match positions.get(parent) {
                Some(&(x, y, _)) => (x, y, false),
                None => (
                    continuations.get(parent).copied().unwrap_or(x1),
                    bottom_y,
                    true,
                ),
            };
            edges.push(json!({
                "x1": x1, "y1": y1, "x2": x2, "y2": y2,
                "c": lane % GRAPH_COLORS, "pending": pending,
            }));
        }
    }

    let width = (72.0f64).max((max_lane + 1) as f64 * GRAPH_LANE_W + 20.0);
    Ok(json!({"head": head, "width": width, "commits": rows, "edges": edges}))
}
