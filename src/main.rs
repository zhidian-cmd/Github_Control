//! Github_Control —— Rust/Tauri 移植版入口。
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

/// 启动各阶段耗时记录：写 data/logs/boot.log，用于诊断"启动慢"卡在哪一段
pub(crate) fn boot_log(stage: &str, t0: std::time::Instant) {
    let line = format!(
        "[boot] {} +{}ms {stage}",
        paths::now_iso(),
        t0.elapsed().as_millis()
    );
    let p = paths::logs_dir().join("boot.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}

fn main() {
    let t0 = std::time::Instant::now();
    boot_log("process start", t0);

    // 无头模式（GC_HEADLESS=1）不建窗口，只起服务供压测
    let headless = std::env::var("GC_HEADLESS").is_ok_and(|v| v == "1");

    // 自建 tokio runtime 驱动 axum 服务；tauri 事件循环在另一个线程
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            win::message_box(
                "Github_control 启动失败",
                &format!("无法初始化运行时：{e}"),
                true,
            );
            std::process::exit(1);
        }
    };
    let _guard = rt.enter();
    let rt_handle = rt.handle().clone();
    // setup 闭包专用克隆（rt_handle 本体已被 gc:// 协议闭包捕获）
    let rt_handle_setup = rt_handle.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 第二个实例：聚焦已有窗口并退出
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        // gc:// 自定义协议：webview 的全部请求（页面 + /api/*）同源桥接到
        // axum 共享 Router —— 前端零改动、无 CORS、无需感知随机端口。
        // 注意：此回调运行在 webview 事件线程，必须用显式 Handle 投递到
        // 自建 tokio runtime。
        .register_asynchronous_uri_scheme_protocol("gc", move |_ctx, req, responder| {
            let h = rt_handle.clone();
            h.spawn(async move {
                let resp = api::handle_protocol_request(req).await;
                responder.respond(resp);
            });
        })
        .setup(move |app| {
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
                    &format!(
                        "缺少以下文件，请确认 runtime 目录完整：\n{}",
                        missing.join("\n")
                    ),
                    true,
                );
                std::process::exit(1);
            }

            // 启动本地 HTTP 服务（127.0.0.1 随机端口，局域网内其他设备无法访问）。
            // 窗口已改走 gc:// 协议；HTTP 端口保留给无头压测与外部工具。
            // 关键路径外置：窗口创建要 ~600ms（WebView2 冷启动），服务绑定只要
            // 几十毫秒且页面根本不走它——同步等它纯属浪费，改为后台并行绑定，
            // 失败也不致命（窗口功能全走 gc://，不受影响），仅记录 error.log。
            api::set_app_handle(app.handle().clone());
            if headless {
                let port = match tokio::runtime::Handle::current().block_on(api::start_server()) {
                    Ok(p) => p,
                    Err(e) => {
                        win::message_box(
                            "Github_control 启动失败",
                            &format!("无法启动本地服务：{e}"),
                            true,
                        );
                        std::process::exit(1);
                    }
                };
                let _ = port;
                boot_log("server up", t0);
                return Ok(());
            }
            {
                let srv_t0 = t0;
                rt_handle_setup.spawn(async move {
                    match api::start_server().await {
                        Ok(_p) => boot_log("server up", srv_t0),
                        Err(e) => runner::log_error(
                            "server",
                            &format!("本地 HTTP 服务绑定失败（不影响窗口功能）：{e}"),
                        ),
                    }
                });
            }

            // 启动预取：把首屏要用的 6 条 git 命令（status_hardcore + log_table +
            // overview_extra）在进程启动这一刻就并发跑掉，整体藏进接下来
            // WebView2 冷启动（~640ms，浏览器/renderer/GPU 进程派生）的空档里。
            // 页面 DOMContentLoaded 时数据通常已到手，不再等 git。
            // 只读命令、无 UI 依赖；仓库不存在时内部各自失败，不影响窗口创建。
            let active = repositories::get_active();
            if !active.trim().is_empty() {
                api::prefetch_startup_bundle(active);
                boot_log("bundle prefetch", t0);
            }

            // 窗口状态恢复 / 创建（GC_HEADLESS=1 时跳过窗口，只起服务供压测）
            if !headless {
                // 恢复窗口几何（物理像素，与关窗时保存的坐标系完全一致，无漂移）
                let saved = config::load_config(false)["window"].clone();
                let geo = win::parse_saved_geo(&saved);
                let maximized = saved
                    .get("maximized")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let scale = app
                    .primary_monitor()
                    .ok()
                    .flatten()
                    .map(|m| m.scale_factor())
                    .unwrap_or(1.0);

                let gc_url = tauri::Url::parse("gc://localhost/").expect("非法 gc:// URL");
                let mut wb = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::CustomProtocol(gc_url),
                )
                .title("Github_Control")
                .min_inner_size(980.0, 620.0)
                // 窗口立即显示（保证"点了就有"的即时反馈），底色取应用主题的
                // 暖白 #f4f2ea，内容就绪前的瞬间是主题色而非白屏
                .background_color(tauri::utils::config::Color(244, 242, 234, 255))
                // 启动埋点：记录 webview 导航开始/完成时刻（不改变显示行为）。
                // page loaded 顺带把两项非关键初始化（自动填充关闭 + 窗口图标）
                // 搬到这里执行——它们不阻塞首屏，原先串在 build() 后面白等。
                .on_page_load(move |w, payload| match payload.event() {
                    tauri::webview::PageLoadEvent::Started => boot_log("page nav started", t0),
                    tauri::webview::PageLoadEvent::Finished => {
                        boot_log("page loaded", t0);
                        // 关闭 WebView2 通用自动填充：它会把提交摘要、仓库地址等输入
                        // 历史存进 WebView 配置文件，并在输入框下弹"保存的信息"下拉，
                        // 用户明确不需要这种历史记录。失败静默忽略，不影响功能。
                        let _ = w.with_webview(|wv| unsafe {
                            use webview2_com::Microsoft::Web::WebView2::Win32::{
                                ICoreWebView2Profile6, ICoreWebView2_13,
                            };
                            use windows_core::Interface;
                            let controller = wv.controller();
                            let Ok(core) = controller.CoreWebView2() else {
                                return;
                            };
                            let Ok(core13) = core.cast::<ICoreWebView2_13>() else {
                                return;
                            };
                            let Ok(profile) = core13.Profile() else {
                                return;
                            };
                            let Ok(profile6) = profile.cast::<ICoreWebView2Profile6>() else {
                                return;
                            };
                            let _ = profile6.SetIsGeneralAutofillEnabled(false);
                        });
                        // 窗口图标（标题栏 + 任务栏）；失败仅忽略，不影响功能
                        let ico = paths::assets_dir().join("favicon.ico");
                        if let Ok(bytes) = std::fs::read(&ico) {
                            if let Ok(img) = tauri::image::Image::from_bytes(&bytes) {
                                let _ = w.set_icon(img);
                            }
                        }
                    }
                });
                match geo {
                    // 构建参数只有逻辑坐标：先按当前主屏缩放比近似换算减少可见跳动，
                    // 窗口建好后再用物理坐标精确校正（见下方 set_size/set_position）
                    Some((x, y, w, h)) => {
                        wb = wb
                            .inner_size(w as f64 / scale, h as f64 / scale)
                            .position(x as f64 / scale, y as f64 / scale);
                    }
                    None => {
                        wb = wb.inner_size(1280.0, 820.0);
                    }
                }
                boot_log("build start", t0);
                let window = match wb.build() {
                    Ok(w) => w,
                    Err(e) => {
                        win::message_box(
                            "Github_control 启动失败",
                            &format!("创建窗口失败：{e}"),
                            true,
                        );
                        std::process::exit(1);
                    }
                };
                boot_log("window built", t0);
                if let Some((x, y, w, h)) = geo {
                    use tauri::{PhysicalPosition, PhysicalSize};
                    let _ = window.set_size(PhysicalSize::new(w as u32, h as u32));
                    let _ = window.set_position(PhysicalPosition::new(x, y));
                }
                if maximized {
                    let _ = window.maximize();
                }
                boot_log("window created", t0);
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            match &event {
                // 拖动/缩放：只更新内存缓存；关窗：写入 config.json。
                // 全部物理像素采样，恢复时 set_size/set_position 原样设回，不漂移。
                tauri::RunEvent::WindowEvent { label, event, .. } => match event {
                    tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                        if let Some(w) = app.get_webview_window(label) {
                            win::track_window(&w);
                        }
                    }
                    tauri::WindowEvent::CloseRequested { .. } => {
                        if let Some(w) = app.get_webview_window(label) {
                            win::persist_window(&w);
                        }
                    }
                    _ => {}
                },
                // headless 模式无窗口，阻止默认的"无窗口即退出"，让服务常驻
                tauri::RunEvent::ExitRequested { api, .. }
                    if std::env::var("GC_HEADLESS").is_ok_and(|v| v == "1") =>
                {
                    api.prevent_exit();
                }
                _ => {}
            }
        });
}
