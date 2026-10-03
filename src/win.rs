//! Windows 辅助：虚拟屏幕钳窗、原生消息框、窗口几何记忆。
//! 与 Python 版 main.pyw 中 _clamp_window / tkinter.messagebox 对应。

use serde_json::{json, Value};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::{Mutex, OnceLock};

pub struct VirtualScreen {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub fn virtual_screen() -> VirtualScreen {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics;
    unsafe {
        VirtualScreen {
            x: GetSystemMetrics(76),  // SM_XVIRTUALSCREEN
            y: GetSystemMetrics(77),  // SM_YVIRTUALSCREEN
            w: GetSystemMetrics(78),  // SM_CXVIRTUALSCREEN
            h: GetSystemMetrics(79),  // SM_CYVIRTUALSCREEN
        }
    }
}

/// 恢复窗口位置前，把坐标钳回当前虚拟桌面内。
/// 返回 (width, height, Option<x>, Option<y>)，与 Python _clamp_window 一致。
/// （已被 parse_saved_geo 取代，保留至确认无引用后删除）
#[allow(dead_code)]
pub fn clamp_window(win: &Value) -> (f64, f64, Option<f64>, Option<f64>) {
    let vs = virtual_screen();
    let width = win.get("width").and_then(|v| v.as_i64()).unwrap_or(1280);
    let height = win.get("height").and_then(|v| v.as_i64()).unwrap_or(820);
    // min(max(w, 980), vw) —— vw 可能小于 980，不能用 clamp
    let w = (width.max(980)).min(vs.w as i64).max(1);
    let h = (height.max(620)).min(vs.h as i64).max(1);
    let x = win.get("x").and_then(|v| v.as_i64());
    let y = win.get("y").and_then(|v| v.as_i64());
    match (x, y) {
        (Some(x), Some(y)) => {
            let xc = (x as i64).max(vs.x as i64 - w / 2).min(vs.x as i64 + vs.w as i64 - w / 2);
            let yc = (y as i64).max(vs.y as i64 - 40).min(vs.y as i64 + vs.h as i64 - 100);
            (w as f64, h as f64, Some(xc as f64), Some(yc as f64))
        }
        _ => (w as f64, h as f64, None, None),
    }
}

/// 原生消息框（替代 tkinter.messagebox）
pub fn message_box(title: &str, msg: &str, is_error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK};
    let to_wide = |s: &str| -> Vec<u16> { OsStr::new(s).encode_wide().chain(Some(0)).collect() };
    let t = to_wide(title);
    let m = to_wide(msg);
    let flags = if is_error { MB_OK | MB_ICONERROR } else { MB_OK | MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(std::ptr::null_mut(), m.as_ptr(), t.as_ptr(), flags);
    }
}

// ---------------- 窗口几何记忆（物理像素，杜绝跨 DPI/内外框漂移） ----------------
//
// 旧实现由前端每 2 秒采 outerWidth/outerHeight/screenX/screenY（CSS 逻辑像素、
// 视口原点），恢复时却当作 Tauri 的 inner_size / position（外框原点）——
// 内外框差一个标题栏+边框，每开关一次就累积漂移一次，DPI 缩放下还会叠加舍入误差。
// 现在全部走物理像素：outer_position（外框原点）+ inner_size（客户区尺寸），
// 与恢复路径的 set_position / set_size 严格互逆，整数级精确还原。

/// 窗口几何缓存：Moved/Resized 事件只更新内存，关窗时才写盘
fn win_state_cache() -> &'static Mutex<Option<(i32, i32, u32, u32)>> {
    static C: OnceLock<Mutex<Option<(i32, i32, u32, u32)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// 拖动/缩放时采样当前几何（物理像素）。
/// 最小化（Windows 会把窗口挪到 -32000）和最大化状态不覆盖缓存：
/// 最大化时关闭，记住的仍是上一次正常状态的几何。
pub fn track_window(w: &tauri::WebviewWindow) {
    let Ok(pos) = w.outer_position() else { return };
    let Ok(size) = w.inner_size() else { return };
    if pos.x <= -30000 || pos.y <= -30000 || size.width == 0 || size.height == 0 {
        return;
    }
    if w.is_maximized().unwrap_or(false) {
        return;
    }
    *win_state_cache().lock().unwrap() = Some((pos.x, pos.y, size.width, size.height));
}

/// 关窗时把缓存几何写进 config.json（物理像素）
pub fn persist_window(w: &tauri::WebviewWindow) {
    let maximized = w.is_maximized().unwrap_or(false);
    if !maximized {
        track_window(w);
    }
    let mut payload = json!({ "maximized": maximized });
    if let Some((x, y, iw, ih)) = *win_state_cache().lock().unwrap() {
        payload["x"] = json!(x);
        payload["y"] = json!(y);
        payload["width"] = json!(iw);
        payload["height"] = json!(ih);
    }
    crate::config::update_config(&json!({ "window": payload }));
}

/// 解析保存的窗口几何（物理像素）并钳回当前虚拟桌面。
/// 四个键齐全且尺寸合法才返回 Some；首次运行（window 为空）返回 None 走默认居中。
pub fn parse_saved_geo(win: &Value) -> Option<(i32, i32, i32, i32)> {
    let x = win.get("x").and_then(|v| v.as_i64())?;
    let y = win.get("y").and_then(|v| v.as_i64())?;
    let w = win.get("width").and_then(|v| v.as_i64())?;
    let h = win.get("height").and_then(|v| v.as_i64())?;
    if w <= 0 || h <= 0 {
        return None;
    }
    let vs = virtual_screen();
    // 尺寸钳制：至少 980x620（应用下限），不超过虚拟屏幕
    let wc = (w.max(980)).min(vs.w as i64) as i32;
    let hc = (h.max(620)).min(vs.h as i64) as i32;
    // 位置钳制：标题栏允许露出 40px，不允许整窗跑出屏幕
    let xc = (x.max(vs.x as i64 - (wc / 2) as i64).min(vs.x as i64 + vs.w as i64 - (wc / 2) as i64)) as i32;
    let yc = (y.max(vs.y as i64 - 40).min(vs.y as i64 + vs.h as i64 - 100)) as i32;
    Some((xc, yc, wc, hc))
}
