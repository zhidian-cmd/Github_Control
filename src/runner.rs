//! 命令执行统一入口：注入环境、显式编码容错、超时控制、写操作日志。
//! 与 Python 版 git_runner.py 对应。

use crate::config;
use crate::env;
use crate::messages;
use crate::paths;
use std::process::Stdio;
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
            log_command(cmd, rc, &out);
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
    log_command(cmd, rc, &out);
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
            (o.status.code().unwrap_or(-1), format!("{}{}", text(&o.stdout), text(&o.stderr)))
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
    run(&cmd, timeout, None, None).await
}

pub async fn run_gh(args: &[String], timeout: Option<f64>) -> (i32, String) {
    let mut cmd = vec![paths::gh_exe().to_string_lossy().into_owned()];
    cmd.extend(args.iter().cloned());
    run(&cmd, timeout, None, None).await
}

/// 写操作日志 run.log，超过 1MB 轮转为 run.old.log
fn log_command(cmd: &[String], returncode: i32, output: &str) {
    let log_dir = paths::logs_dir();
    let log_file = log_dir.join("run.log");
    // 超过 1MB 轮转
    if let Ok(m) = std::fs::metadata(&log_file) {
        if m.len() > 1024 * 1024 {
            let _ = std::fs::rename(&log_file, log_dir.join("run.old.log"));
        }
    }
    let head: String = output.chars().take(2000).collect();
    let line = format!(
        "[{}]\nCMD: {}\nRC:  {}\nOUT: {}\n{}\n",
        paths::now_iso(),
        cmd.join(" "),
        returncode,
        head,
        "-".repeat(60)
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_file) {
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
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_file) {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}
