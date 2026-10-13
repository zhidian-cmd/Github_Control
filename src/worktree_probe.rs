//! 文件系统层探测：每个文件只读前 8KB + 最后 1 字节，绝不读全文。
//! 与 Python 版 worktree_probe.py 对应。

use serde_json::{json, Value};

const SECRET_PATTERNS: [&str; 7] = [
    ".env",
    ".key",
    ".pem",
    ".p12",
    ".pfx",
    "id_rsa",
    "id_ed25519",
];

#[cfg(unix)]
fn mode_oct(mode: u32) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("0o{:o}", mode & 0o777)
}
#[cfg(windows)]
fn mode_oct(_mode: u32) -> String {
    // Python os.stat().st_mode & 0o777 在 Windows 上固定为 0o666（普通文件）
    "0o666".into()
}

pub fn probe(repo: &str, rel_path: &str, limit: usize) -> Value {
    use std::io::{Read, Seek, SeekFrom};
    let abs_path = std::path::Path::new(repo).join(rel_path);
    let st = match std::fs::metadata(&abs_path) {
        Ok(m) => m,
        Err(_) => return json!({"exists": false}),
    };

    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        mode_oct(st.permissions().mode())
    };
    #[cfg(windows)]
    let mode = mode_oct(0);

    let mut result = json!({
        "exists": true,
        "size": st.len(),
        "mtime": paths_mtime(&st),
        "mode": mode,
        "is_binary": false,
        "crlf_mixed": false,
        "pure_crlf": false,
        "final_newline": true,
        "suspicious_secret": false,
    });
    let name_lower = std::path::Path::new(rel_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if SECRET_PATTERNS.iter().any(|p| name_lower.contains(p)) {
        result["suspicious_secret"] = json!(true);
    }

    if st.len() == 0 {
        return result;
    }

    let mut f = match std::fs::File::open(&abs_path) {
        Ok(f) => f,
        Err(_) => return result,
    };
    let mut head = vec![0u8; limit];
    let n = f.read(&mut head).unwrap_or(0);
    head.truncate(n);
    let mut last = [0u8; 1];
    let last_ok = f.seek(SeekFrom::End(-1)).is_ok() && f.read(&mut last).is_ok();

    result["is_binary"] = json!(head.contains(&0u8));
    if !head.contains(&0u8) {
        let crlf = head.windows(2).filter(|w| w == b"\r\n").count();
        let lf = head.iter().filter(|&&b| b == b'\n').count() - crlf;
        result["pure_crlf"] = json!(crlf > 0 && lf == 0);
        result["crlf_mixed"] = json!(crlf > 0 && lf > 0);
        result["final_newline"] = json!(last_ok && last[0] == b'\n');
    }
    result
}

fn paths_mtime(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
