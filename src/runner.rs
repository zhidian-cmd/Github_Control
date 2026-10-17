//! 命令执行统一入口：注入环境、显式编码容错、超时控制、写操作日志。
//! 与 Python 版 git_runner.py 对应。

use crate::config;
use crate::env;
use crate::messages;
use crate::net;
use crate::paths;
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::Duration;

/// 统一执行入口。返回 (返回码, 输出)。返回码 -1 表示超时或启动失败。
/// env_override=Some 时用调用方给的环境（如去代理直连重试），否则走 build_env()。
pub async fn run(
    cmd: &[String],
    timeout: Option<f64>,
    input: Option<String>,
    env_override: Option<&std::collections::HashMap<String, String>>,
) -> (i32, String) {
    let timeout_secs = timeout.unwrap_or_else(|| {
        config::load_config(false)
            .pointer("/git/cmd_timeout")
            .and_then(|v| v.as_f64())
            .unwrap_or(300.0)
    });
    // 命令耗时埋点：写进 run.log 的 DUR 字段，诊断"卡"时无歧义区分
    // "命令本身慢"与"用户空闲"（日志时间戳是命令完成后才写的，相邻条目
    // 间隔里混有用户思考时间，历史上多次因此误判）。
    let t_cmd = std::time::Instant::now();
    let env_map;
    let envs = match env_override {
        Some(m) => m,
        None => {
            env_map = env::build_env();
            &env_map
        }
    };

    let mut command = Command::new(&cmd[0]);
    if cmd.len() > 1 {
        command.args(&cmd[1..]);
    }
    if env_override.is_some() {
        // 调用方给了完整环境（如"去代理直连"）：必须先清空再注入。
        // envs() 本身是增量覆盖，子进程仍会继承本进程的 HTTP_PROXY 等变量，
        // 只从 map 里删 key 根本删不掉——直连重试会继续走代理，等于没兜底。
        command.env_clear();
    }
    command.envs(envs);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let (rc, out) = (-1, e.to_string());
            log_command(cmd, rc, &out, t_cmd.elapsed().as_millis());
            return (rc, out);
        }
    };

    if let Some(i) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(i.as_bytes()).await;
        }
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let wait = async {
        let mut so = Vec::new();
        let mut se = Vec::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_end(&mut so).await;
        }
        if let Some(mut s) = stderr {
            let _ = s.read_to_end(&mut se).await;
        }
        let status = child.wait().await;
        (status, so, se)
    };
    let result = tokio::time::timeout(Duration::from_secs_f64(timeout_secs.max(0.1)), wait).await;

    let (rc, mut out) = match result {
        Ok((status, so, se)) => {
            let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            (code, format!("{}{}", text(&so), text(&se)))
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (-1, format!("命令超时（{timeout_secs} 秒）"))
        }
    };

    if rc != 0 {
        out = messages::translate(&out);
    }
    log_command(cmd, rc, &out, t_cmd.elapsed().as_millis());
    (rc, out)
}

pub fn run_git_sync(args: &[String], repo: Option<&str>, cwd: Option<&str>) -> (i32, String) {
    let env_map = env::build_env();
    let mut cmd = std::process::Command::new(paths::git_exe());
    if let Some(r) = repo {
        cmd.arg("-C").arg(r);
    }
    cmd.args(args);
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    cmd.envs(&env_map).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(o) => {
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            (
                o.status.code().unwrap_or(-1),
                format!("{}{}", text(&o.stdout), text(&o.stderr)),
            )
        }
        Err(e) => (-1, e.to_string()),
    }
}

pub async fn run_git(args: &[String], repo: Option<&str>, timeout: Option<f64>) -> (i32, String) {
    let mut cmd = vec![paths::git_exe().to_string_lossy().into_owned()];
    if let Some(r) = repo {
        cmd.push("-C".into());
        cmd.push(r.to_string());
    }
    cmd.extend(args.iter().cloned());
    run_fb(&cmd, timeout, None).await
}

pub async fn run_gh(args: &[String], timeout: Option<f64>) -> (i32, String) {
    let mut cmd = vec![paths::gh_exe().to_string_lossy().into_owned()];
    cmd.extend(args.iter().cloned());
    run_fb(&cmd, timeout, None).await
}

/// 不带兜底的 gh 执行：给"专门测某一条链路"的场景用（代理测试/直连测试），
/// 否则测试会把兜底重试的结果混进去，失去检测意义。
#[allow(dead_code)]
pub async fn run_gh_nb(args: &[String], timeout: Option<f64>) -> (i32, String) {
    let mut cmd = vec![paths::gh_exe().to_string_lossy().into_owned()];
    cmd.extend(args.iter().cloned());
    run(&cmd, timeout, None, None).await
}

/// 自动兜底执行：填了代理地址 → 主路径为代理，网络类失败自动改走直连重试一次；
/// 没填代理 → 纯直连，无从兜底。成功时输出头部注明实际走的链路。
/// 兜底恒开：链路诊断交给「测试代理」「测试直连」按钮（它们不兜底）。
pub async fn run_fb(cmd: &[String], timeout: Option<f64>, input: Option<String>) -> (i32, String) {
    let (rc, out) = run(cmd, timeout, input.clone(), None).await;
    if rc == 0 {
        return (rc, out);
    }
    if !net::is_net_hint(&out) {
        return (rc, out);
    }
    let url = net::normalize_proxy_url(
        config::load_config(false)
            .pointer("/proxy/url")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    if url.is_empty() {
        // 没配代理：纯直连，无处可兜
        return (rc, out);
    }
    // 转直连前先花至多 1.2s 体检 TCP：直连根本不通就别白跑一次完整命令
    // （典型环境：代理软件没开 + 直连被墙 —— 跑直连也必败，只会让用户多等）。
    // 体检测的是 api.github.com:443，与本应用全部 GitHub 链路一致。
    // 同步 TCP 探测必须走 spawn_blocking，否则阻塞 axum 所在的事件循环。
    let direct_reachable = tokio::task::spawn_blocking(|| net::direct_github_reachable(1.2))
        .await
        .unwrap_or(false);
    if !direct_reachable {
        return (
            rc,
            format!("{out}\n\n（已检测到本机直连也不通，未再盲目重试；需要代理请先开启代理软件）"),
        );
    }
    let env_map = env::build_env_direct();
    let (rc2, out2) = run(cmd, timeout, input, Some(&env_map)).await;
    if rc2 == 0 {
        (
            rc2,
            format!("[自动兜底] 代理网络失败，已自动改走直连重试成功。\n\n{out2}"),
        )
    } else {
        (rc, format!("{out}\n\n（已自动尝试直连，同样失败）"))
    }
}

/// 兜底版 git：本地命令不含网络关键词时行为与 run_git 完全一致
#[allow(dead_code)]
pub async fn run_git_fb(
    args: &[String],
    repo: Option<&str>,
    timeout: Option<f64>,
) -> (i32, String) {
    let mut cmd = vec![paths::git_exe().to_string_lossy().into_owned()];
    if let Some(r) = repo {
        cmd.push("-C".into());
        cmd.push(r.to_string());
    }
    cmd.extend(args.iter().cloned());
    run_fb(&cmd, timeout, None).await
}

/// 日志脱敏：屏蔽可能经由命令输出/参数写入 run.log 的明文凭据。
/// 覆盖 GitHub 令牌（ghp_/gho_/ghu_/ghs_/ghr_/github_pat_）与 URL 内 Basic Auth。
/// 正则编译一次复用（log_command 每次命令调 3 次 redact，重复编译纯浪费）。
fn redact(s: &str) -> String {
    static RE: OnceLock<(regex::Regex, regex::Regex)> = OnceLock::new();
    let (re_token, re_basic) = RE.get_or_init(|| {
        (
            regex::Regex::new(r"(gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{20,})")
                .expect("token 脱敏正则必须编译通过"),
            regex::Regex::new(r"(?i)(https?://)[^/\s:@]+:[^/\s:@]+@")
                .expect("basic auth 脱敏正则必须编译通过"),
        )
    });
    let mut out = s.to_string();
    out = re_token.replace_all(&out, "***REDACTED***").into_owned();
    out = re_basic.replace_all(&out, "$1***:***@").into_owned();
    out
}

/// 写操作日志 run.log，超过 1MB 轮转为 run.old.log。
/// dur_ms 为命令自身耗时（spawn 到输出读完），与条目时间戳相互独立。
fn log_command(cmd: &[String], returncode: i32, output: &str, dur_ms: u128) {
    let log_dir = paths::logs_dir();
    let log_file = log_dir.join("run.log");
    // 超过 1MB 轮转
    if let Ok(m) = std::fs::metadata(&log_file) {
        if m.len() > 1024 * 1024 {
            let _ = std::fs::rename(&log_file, log_dir.join("run.old.log"));
        }
    }
    let head: String = redact(&output.chars().take(2000).collect::<String>());
    let line = format!(
        "[{}]\nCMD: {}\nRC:  {}\nDUR: {}ms\nOUT: {}\n{}\n",
        paths::now_iso(),
        redact(&cmd.join(" ")),
        returncode,
        dur_ms,
        head,
        "-".repeat(60)
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

/// 写 error.log（后端异常 / 前端未捕获异常）
pub fn log_error(prefix: &str, body: &str) {
    let log_dir = paths::logs_dir();
    let log_file = log_dir.join("error.log");
    let line = format!(
        "[{prefix}] {}\n{body}\n{}\n",
        paths::now_iso(),
        "-".repeat(60)
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}
