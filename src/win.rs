//! Windows 辅助：虚拟屏幕钳窗、原生消息框。
//! 与 Python 版 main.pyw 中 _clamp_window / tkinter.messagebox 对应。

use serde_json::Value;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

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
