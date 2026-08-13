// DeepSeek Harness desktop client.
//
// A native Windows desktop application that embeds a WebView2 browser control
// and loads the DeepSeek Harness Web GUI at http://127.0.0.1:3080/.
//
// Service lifecycle (managed automatically):
//   - on startup: if something already listens on port 3080, kill it first;
//     then start `npx @deepseek-ai/dsh web` (hidden) and wait for it to come up;
//   - on exit: kill the process(es) listening on port 3080.
//
// Built on `wry` (the lightweight webview library used by Tauri) with the
// official DeepSeek Harness black whale icon embedded as the app icon.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dsh_service;

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, WindowBuilder};
use wry::{WebContext, WebViewBuilder};

/// Target URL of the DeepSeek Harness Web GUI.
const APP_URL: &str = "http://127.0.0.1:3080/";
/// Window title.
const APP_TITLE: &str = "DeepSeek Harness";
/// Status page template (whale icon + message slot).
const FALLBACK_HTML: &str = include_str!("../assets/fallback.html");
/// Official DSH whale icon (multi-resolution ICO), embedded into the exe.
const WHALE_ICO: &[u8] = include_bytes!("../assets/dsh-whale.ico");

/// DSH light-theme page background (`--dsw-alias-bg-base`).
const BG_LIGHT: (u8, u8, u8, u8) = (0xf9, 0xfa, 0xfb, 0xff);
/// DSH dark-theme page background (`#0f1115`).
const BG_DARK: (u8, u8, u8, u8) = (0x0f, 0x11, 0x15, 0xff);

/// Events dispatched from worker threads to the main event loop.
enum UserEvent {
    /// The DSH service is listening; load the real GUI.
    DshReady,
    /// The DSH service failed to start (message shown in the status page).
    DshFailed(String),
}

/// Decodes the largest embedded ICO frame into a tao `Icon` for the window
/// (taskbar / titlebar / alt-tab).
fn window_icon() -> Option<Icon> {
    let dir = ico::IconDir::read(Cursor::new(WHALE_ICO)).ok()?;
    let mut best: Option<(u32, ico::IconImage)> = None;
    for entry in dir.entries() {
        if let Ok(img) = entry.decode() {
            let pixels = img.width() * img.height();
            if best.as_ref().map_or(true, |(size, _)| pixels > *size) {
                best = Some((pixels, img));
            }
        }
    }
    let (_, img) = best?;
    Icon::from_rgba(img.rgba_data().to_vec(), img.width(), img.height()).ok()
}

/// True when the Windows UI theme is light (matches `prefers-color-scheme` so
/// the webview background matches the page background before it loads).
fn system_light_theme() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    if let Ok(themes) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
    {
        if let Ok(value) = themes.get_value::<u32, _>("AppsUseLightTheme") {
            return value != 0;
        }
    }
    true
}

/// WebView2 user-data directory: an isolated per-user profile for this app.
///
/// Overridable with `DSH_DESKTOP_USER_DATA_DIR` (used for testing and as an
/// escape hatch).
fn user_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DSH_DESKTOP_USER_DATA_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("DeepSeekHarness")
}

/// Builds the whale status page with a spinner (starting) or a red dot + text.
fn status_page(indicator: &str, status: &str) -> String {
    let escaped = status
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    FALLBACK_HTML
        .replace("__INDICATOR__", indicator)
        .replace("__STATUS__", &escaped)
}

/// Shows a native error dialog (used when the WebView2 runtime is missing or
/// the webview cannot be created, so the user isn't left with a silent exit).
fn show_error(message: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
    };

    let to_wide = |s: &str| -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    };
    let msg = to_wide(message);
    let title = to_wide(APP_TITLE);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            msg.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

/// Background worker that manages the DSH service:
/// 1. kill whatever owns port 3080 (restart policy);
/// 2. start `npx @deepseek-ai/dsh web`;
/// 3. wait for it to listen, then notify the main loop.
fn bootstrap_dsh(proxy: EventLoopProxy<UserEvent>, child_pid: Arc<Mutex<Option<u32>>>) {
    // 1) Restart policy: an existing service on port 3080 is stopped first.
    if dsh_service::is_port_listening(dsh_service::DSH_PORT) {
        dsh_service::kill_port_owner(dsh_service::DSH_PORT);
        let _ = dsh_service::wait_port_closed(dsh_service::DSH_PORT, Duration::from_secs(8));
    }

    // 2) Start the service.
    let spawned = dsh_service::start_dsh();
    if let Some(pid) = spawned {
        *child_pid.lock().unwrap() = Some(pid);
    } else {
        let _ = proxy.send_event(UserEvent::DshFailed(
            "无法启动 npx @deepseek-ai/dsh web：请确认已安装 Node.js / npm 且在 PATH 中。"
                .to_string(),
        ));
        return;
    }

    // 3) Wait up to 120s for the service; afterwards keep checking slowly so a
    //    very slow start still connects automatically.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut announced_timeout = false;
    loop {
        if dsh_service::is_port_listening(dsh_service::DSH_PORT) {
            if proxy.send_event(UserEvent::DshReady).is_err() {
                cleanup_dsh(&child_pid);
            }
            return;
        }
        if !announced_timeout && Instant::now() >= deadline {
            announced_timeout = true;
            if proxy
                .send_event(UserEvent::DshFailed(
                    "DeepSeek Harness 服务启动超时（120 秒），将在后台继续等待…".to_string(),
                ))
                .is_err()
            {
                cleanup_dsh(&child_pid);
                return;
            }
        }
        thread::sleep(if announced_timeout {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(700)
        });
    }
}

/// Kill the DSH process we spawned (and its tree) plus the port 3080 owner.
fn cleanup_dsh(child_pid: &Arc<Mutex<Option<u32>>>) {
    if let Some(pid) = child_pid.lock().unwrap().take() {
        dsh_service::kill_pid_tree(pid);
    }
    dsh_service::kill_port_owner(dsh_service::DSH_PORT);
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let background = if system_light_theme() { BG_LIGHT } else { BG_DARK };

    let window = WindowBuilder::new()
        .with_title(APP_TITLE)
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(LogicalSize::new(960.0, 600.0))
        .with_window_icon(window_icon())
        .build(&event_loop)
        .expect("failed to create window");

    let mut web_context = WebContext::new(Some(user_data_dir()));
    let webview = match WebViewBuilder::new_with_web_context(&mut web_context)
        .with_background_color(background)
        .with_url("about:blank")
        .build(&window)
    {
        Ok(webview) => webview,
        Err(e) => {
            show_error(&format!(
                "无法启动浏览器控件（需要 Microsoft Edge WebView2 运行时）。\n\n错误详情：{e}\n\n请安装 WebView2 运行时后重试。"
            ));
            std::process::exit(1);
        }
    };

    // Show the "starting" status page immediately; the bootstrap worker swaps
    // it for the real GUI as soon as the DSH service is up.
    webview.load_html(&status_page(
        "<span class=\"spinner\"></span>",
        "正在启动 DeepSeek Harness 服务（npx @deepseek-ai/dsh web），请稍候…",
    ))?;

    // --- DSH service lifecycle (background) ---
    let dsh_child_pid: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    {
        let proxy = proxy.clone();
        let dsh_child_pid = dsh_child_pid.clone();
        thread::spawn(move || bootstrap_dsh(proxy, dsh_child_pid));
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // On exit: stop the DSH service we started.
                cleanup_dsh(&dsh_child_pid);
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::DshReady) => {
                let _ = webview.load_url(APP_URL);
            }
            Event::UserEvent(UserEvent::DshFailed(reason)) => {
                let _ = webview.load_html(&status_page(
                    "<span class=\"dot\"></span>",
                    &reason,
                ));
            }
            _ => {}
        }
    });
}
