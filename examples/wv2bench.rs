//! WebView2 环境创建耗时基准（临时诊断工具，不属于应用功能）。
//!
//! 用法：cargo run --release --example wv2bench
//! 对比 wry 默认参数 vs 追加精简参数（禁后台联网/组件更新/同步）的
//! CreateCoreWebView2EnvironmentWithOptions 耗时，量化启动优化空间。

use std::time::Instant;

use webview2_com::{
    CreateCoreWebView2EnvironmentCompletedHandler, CoreWebView2EnvironmentOptions,
    Microsoft::Web::WebView2::Win32::{
        CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2EnvironmentOptions,
    },
};
use windows_core::{PCWSTR, HSTRING};

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *const core::ffi::c_void, dwcoinit: u32) -> i32;
}

const WRY_DEFAULT: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

fn create_env(args: &str, data_dir: &str) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(std::ptr::null(), 0x2); // COINIT_APARTMENTTHREADED
        let options = CoreWebView2EnvironmentOptions::default();
        options.set_additional_browser_arguments(args.to_string());
        let dir = HSTRING::from(data_dir);
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    &dir,
                    &ICoreWebView2EnvironmentOptions::from(options),
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)?;
                Ok(())
            }),
            Box::new(|_hresult, _env| Ok(())),
        )
        .map_err(|e| format!("{e:?}"))
    }
}

fn fresh_dir(tag: &str) -> String {
    let base = std::env::temp_dir().join(format!("wv2bench-{tag}"));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::create_dir_all(&base);
    base.to_string_lossy().into_owned()
}

fn bench_env(args: &str, data_dir: &str) -> u128 {
    let t = Instant::now();
    let r = create_env(args, data_dir);
    let ms = t.elapsed().as_millis();
    match r {
        Ok(()) => println!("    env: {ms}ms  ok"),
        Err(e) => println!("    env: {ms}ms  FAIL: {e}"),
    }
    ms
}

fn main() {
    println!("=== A. WebView2 环境创建（每轮全新数据目录，交叉进行抵消冷/热缓存）===");
    let slim = format!("{WRY_DEFAULT} --disable-background-networking --disable-component-update --disable-sync --no-pings --metrics-recording-only");
    for i in 0..3 {
        println!("  第 {} 轮", i + 1);
        let a = bench_env(WRY_DEFAULT, &fresh_dir("a"));
        let b = bench_env(&slim, &fresh_dir("b"));
        println!("    default={a}ms slim={b}ms diff={}ms", a as i64 - b as i64);
    }

    println!("\n=== B. 完整窗口链路：tao 窗口 + wry webview 附加（复刻 app 启动路径）===");
    for i in 0..3 {
        println!("  第 {} 轮", i + 1);
        let event_loop = tao::event_loop::EventLoop::new();
        let t_win = Instant::now();
        let window = tao::window::WindowBuilder::new()
            .with_title("probe")
            .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 820.0))
            .with_visible(true)
            .build(&event_loop)
            .expect("tao window");
        let win_ms = t_win.elapsed().as_millis();
        println!("    tao window: {win_ms}ms");

        let t_wv = Instant::now();
        let webview = wry::WebViewBuilder::new()
            .with_url("about:blank")
            .with_background_color((244, 242, 234, 255))
            .build(&window)
            .expect("wry webview");
        let wv_ms = t_wv.elapsed().as_millis();
        println!("    wry webview (env+controller+init): {wv_ms}ms");
        println!("    ---- 合计: {}ms", win_ms + wv_ms);

        drop(webview);
        drop(window);
    }

    println!("\n=== C. 真实配置目录副本（60MB profile）对 env 创建的影响 ===");
    let real = std::env::var("LOCALAPPDATA").unwrap_or_default()
        + "\\com.zhidian.github_control\\EBWebView";
    if std::path::Path::new(&real).exists() {
        let copy = std::env::temp_dir().join("wv2bench-real");
        let _ = std::fs::remove_dir_all(&copy);
        let t = Instant::now();
        let _ = copy_dir(&real, &copy);
        println!("    复制耗时 {}ms", t.elapsed().as_millis());
        let a = bench_env(WRY_DEFAULT, &copy.to_string_lossy());
        let _ = std::fs::remove_dir_all(&copy);
        println!("    default+realprofile={a}ms");
    }
}

fn copy_dir(src: &str, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path().to_string_lossy(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
