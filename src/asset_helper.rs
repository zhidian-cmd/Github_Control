//! 附件辅助：非 ASCII 文件名复制成 ASCII 临时名（gh release 上传不支持中文名）。
//! 与 Python 版 asset_helper.py 对应。

use crate::paths;
use std::time::{SystemTime, UNIX_EPOCH};

/// 非 ASCII 文件名复制成 ASCII 临时名，返回新路径；ASCII 名原样返回。
pub fn make_ascii_copy(path: &str) -> String {
    let p = std::path::Path::new(path);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.chars().all(|c| c.is_ascii()) {
        return path.to_string();
    }
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tmp = paths::tmp_dir().join(format!("asset_{millis}{ext}"));
    let _ = std::fs::copy(path, &tmp);
    tmp.to_string_lossy().into_owned()
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
