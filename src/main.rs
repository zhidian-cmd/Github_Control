//! Github Control —— Rust/Tauri 移植版入口。
//! 与 Python 版 main.pyw 对应：单实例、runtime 检查、本地 HTTP 服务、窗口创建。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod asset_helper;
mod config;
mod credential;
mod env;
mod gh_runner;
mod git_repo;
mod git_rollback;
mod git_status_v2;
mod messages;
mod net;
mod paths;
mod repositories;
mod runner;
mod win;
mod worktree_probe;

use tauri::{Manager, WebviewUrl};

fn main() {
    // 自建 tokio runtime 驱动 axum 服务；tauri 事件循环在另一个线程
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            win::message_box("Github_control 启动失败", &format!("无法初始化运行时：{e}"), true);
            std::process::exit(1);
        }
    };
    let _guard = rt.enter();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 第二个实例：聚焦已有窗口并退出
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            paths::ensure_dirs();
            // 修正旧版 git config 追加出来的 .gitconfig 段落顺序（凭据 helper 被清零的 bug）
            credential::fix_gitconfig_order();

            // runtime 完整性检查
            let missing: Vec<String> = [paths::git_exe(), paths::gh_exe()]
                .iter()
                .filter(|p| !p.is_file())
                .map(|p| p.display().to_string())
                .collect();
            if !missing.is_empty() {
                win::message_box(
                    "Github_control 启动失败",
                    &format!("缺少以下文件，请确认 runtime 目录完整：\n{}", missing.join("\n")),
                    true,
                );
                std::process::exit(1);
            }

            // 启动本地 HTTP 服务（127.0.0.1 随机端口，局域网内其他设备无法访问）
            let port = match tokio::runtime::Handle::current().block_on(api::start_server()) {
                Ok(p) => p,
                Err(e) => {
                    win::message_box("Github_control 启动失败", &format!("无法启动本地服务：{e}"), true);
                    std::process::exit(1);
                }
            };
            let url = format!("http://127.0.0.1:{port}/");

            // 窗口状态恢复 / 创建（GC_HEADLESS=1 时跳过窗口，只起服务供压测）
            let headless = std::env::var("GC_HEADLESS").is_ok_and(|v| v == "1");
            if !headless {
                // 恢复窗口状态（钳回虚拟屏幕内）
                let saved = config::load_config(false)["window"].clone();
                let (w, h, x, y) = win::clamp_window(&saved);

                let mut wb = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::External(url.parse().expect("非法本地 URL")),
                )
                .title("Github Control")
                .inner_size(w, h)
                .min_inner_size(980.0, 620.0)
                // 先隐藏：页面加载完成再显示，消除启动白屏闪烁
                .visible(false)
                // 页面加载完成 → 显示窗口并聚焦
                .on_page_load(|w, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                });
                if let (Some(x), Some(y)) = (x, y) {
                    wb = wb.position(x, y);
                }
                let window = match wb.build() {
                    Ok(w) => w,
                    Err(e) => {
                        win::message_box("Github_control 启动失败", &format!("创建窗口失败：{e}"), true);
                        std::process::exit(1);
                    }
                };

                // 窗口图标（标题栏 + 任务栏）；失败仅忽略，不影响功能
                let ico = paths::assets_dir().join("favicon.ico");
                if let Ok(bytes) = std::fs::read(&ico) {
                    if let Ok(img) = tauri::image::Image::from_bytes(&bytes) {
                        let _ = window.set_icon(img);
                    }
                }
            }

            api::set_app_handle(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // headless 模式无窗口，阻止默认的"无窗口即退出"，让服务常驻
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if std::env::var("GC_HEADLESS").is_ok_and(|v| v == "1") {
                    api.prevent_exit();
                }
            }
        });
}
