//! 附件辅助：非 ASCII 文件名复制成 ASCII 临时名（gh release 上传不支持中文名）。
//! 与 Python 版 asset_helper.py 对应。

use crate::paths;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 临时名唯一序号：同毫秒多附件不再互相覆盖
static SEQ: AtomicU64 = AtomicU64::new(0);

/// 非 ASCII 文件名复制成 ASCII 临时名，返回新路径；ASCII 名原样返回。
/// 复制失败返回 Err——绝不能把一个不存在的临时路径塞给 gh 上传。
pub fn make_ascii_copy(path: &str) -> Result<String, String> {
    let p = std::path::Path::new(path);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.chars().all(|c| c.is_ascii()) {
        return Ok(path.to_string());
    }
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp = paths::tmp_dir().join(format!("asset_{pid}_{millis}_{seq}{ext}"));
    std::fs::copy(path, &tmp)
        .map_err(|e| format!("附件临时副本创建失败（{path}）：{e}"))?;
    Ok(tmp.to_string_lossy().into_owned())
}

/// 只清理 tmp 目录里的临时副本
pub fn cleanup(paths_list: &[String]) {
    let tmp = paths::tmp_dir();
    for p in paths_list {
        if p.is_empty() {
            continue;
        }
        let pp = std::path::Path::new(p);
        if pp.parent().map(|d| d == tmp).unwrap_or(false) {
            let _ = std::fs::remove_file(pp);
        }
    }
}
