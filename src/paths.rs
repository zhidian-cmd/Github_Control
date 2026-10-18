//! 路径管理：绿色便携 —— config.json / data/ / runtime/ / web/ 都在可执行文件旁边。
//! 与 Python 版语义一致：BASE_DIR 优先取 `GC_BASE_DIR` 环境变量（测试用），
//! 否则取可执行文件所在目录。

use std::path::PathBuf;
use std::sync::OnceLock;

pub fn base_dir() -> &'static PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        if let Ok(p) = std::env::var("GC_BASE_DIR") {
            if !p.trim().is_empty() {
                return PathBuf::from(p);
            }
        }
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        // 开发布局：<root>\target\release\Github_Control.exe → 上溯到 <root>
        // （runtime/、web/、config.json 都在工程根）；发布时 exe 单独拷到
        // 任意目录与 runtime/、web/ 同级即可，保持 Python 版"绿色便携"语义。
        let stem = exe_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if stem == "release" || stem == "debug" {
            if let Some(root) = exe_dir.parent().and_then(|p| p.parent()) {
                return root.to_path_buf();
            }
        }
        exe_dir
    })
}

pub fn data_dir() -> PathBuf {
    base_dir().join("data")
}
pub fn runtime_dir() -> PathBuf {
    base_dir().join("runtime")
}
pub fn web_dir() -> PathBuf {
    base_dir().join("web")
}
pub fn assets_dir() -> PathBuf {
    base_dir().join("assets")
}
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}
pub fn tmp_dir() -> PathBuf {
    data_dir().join("tmp")
}
pub fn git_home_dir() -> PathBuf {
    data_dir().join("git_home")
}
pub fn gh_config_dir() -> PathBuf {
    data_dir().join("gh_config")
}

/// 配置文件路径；支持 `GC_CONFIG_FILE` 环境变量覆盖（测试用，避免污染用户数据）
pub fn config_file() -> &'static PathBuf {
    static CFG: OnceLock<PathBuf> = OnceLock::new();
    CFG.get_or_init(|| {
        if let Ok(p) = std::env::var("GC_CONFIG_FILE") {
            if !p.trim().is_empty() {
                return PathBuf::from(p);
            }
        }
        base_dir().join("config.json")
    })
}

pub fn git_exe() -> PathBuf {
    runtime_dir().join("git").join("cmd").join("git.exe")
}
pub fn gh_exe() -> PathBuf {
    runtime_dir().join("gh.exe")
}

pub fn ensure_dirs() {
    for d in [
        data_dir(),
        logs_dir(),
        tmp_dir(),
        git_home_dir(),
        gh_config_dir(),
    ] {
        let _ = std::fs::create_dir_all(&d);
    }
}

/// 规范化路径，并剥掉 Windows verbatim 前缀（`\\?\`、`\\?\UNC\`），
/// 返回人类可读形式。`std::fs::canonicalize` 在 Windows 上返回的 verbatim
/// 路径子进程（git/gh）不认，做克隆目标会直接失败。
pub fn canonicalize_human(path: &std::path::Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok().map(|p| {
        let s = p.to_string_lossy().into_owned();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            PathBuf::from(rest)
        } else {
            p
        }
    })
}

/// 当前时间 ISO 格式（毫秒级，与 Python datetime.now().isoformat() 对应）
pub fn now_iso() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

/// 当前时间 ISO 格式（秒级）
pub fn now_iso_seconds() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// 紧凑时间戳（保险 tag / 备份文件用）：20260930_172523
pub fn now_compact() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

/// 文件 mtime（unix 秒）；文件不存在返回 0
pub fn file_mtime(path: &std::path::Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
