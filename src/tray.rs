//! 系统托盘核心（M11 phase 1）。
//!
//! 提供 `lorag tray` 命令所需的托盘图标 + 菜单 + 事件循环。
//!
//! ## 平台差异
//!
//! - **Windows**：tray-icon 在创建线程注册 hidden window 接收 menu 事件，必须显式
//!   pump Win32 message queue（`PeekMessageW` / `DispatchMessageW`），否则菜单点击不触发。
//!   本模块在 `run_tray_loop` 内部跑"pump + channel poll + 短 sleep"的混合循环。
//! - **macOS**：tray-icon 要求 `NSApplication` run loop 在 main thread；本任务只在
//!   Windows 验证。macOS 需要额外调 `tray_icon::platform::macos::init_ns_app()`，留到后续。
//! - **Linux**：tray-icon 依赖 GTK / libappindicator；本任务不验证 Linux。
//!
//! ## 菜单
//!
//! - `Open Web UI` (id=`open`) — 打开浏览器到 `http://localhost:{port}`
//! - `─────────────` (separator)
//! - `Quit` (id=`quit`) — 通过 `shutdown_tx` 通知 axum 优雅关闭 → 进程退出
//!
//! ## 设计约束
//!
//! - **不**碰 aha / lancedb / sqlite——只接收 `port` + `shutdown_tx`，业务解耦。
//! - **不**用 `webbrowser` crate——直接 `std::process::Command` 跨平台调 `start` / `open` / `xdg-open`。
//! - **不**用 `tao` / `winit`——用 `windows-sys` 直接 pump message queue（轻量）。

use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::oneshot;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

/// 托盘菜单触发的命令。
///
/// 由 `menu_id_to_command` 从菜单项 id 映射得到，主循环据此分发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// 打开 Web UI 浏览器（`http://localhost:{port}`）
    OpenBrowser,
    /// 优雅退出进程（发 `shutdown_tx` 信号 → axum graceful shutdown → main 返回）
    Quit,
}

/// 把托盘菜单项 id 映射到 `TrayCommand`。
///
/// 返回 `None` 表示不认识的 id（主循环忽略）。
/// 单独抽出来是为了单元测试 menu id 匹配逻辑（不依赖真实托盘）。
fn menu_id_to_command(id: &str) -> Option<TrayCommand> {
    match id {
        "open" => Some(TrayCommand::OpenBrowser),
        "quit" => Some(TrayCommand::Quit),
        _ => None,
    }
}

/// 跨平台打开浏览器到指定 URL。
///
/// - Windows：`cmd /C start "" <url>`（`start` 第一个引号参数是窗口标题，必须留空）
/// - macOS：`open <url>`
/// - Linux：`xdg-open <url>`
///
/// 失败时返回 `Err`（不 panic），调用方决定是向上传播还是降级提示用户手动打开。
///
/// # Examples
///
/// ```no_run
/// # fn main() -> anyhow::Result<()> {
/// lorag::tray::open_browser("http://localhost:3000")?;
/// # Ok(())
/// # }
/// ```
pub fn open_browser(url: &str) -> Result<()> {
    open_browser_impl(url)
        .with_context(|| format!("failed to open browser at {url} (please open manually)"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_browser_impl(url: &str) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
}

#[cfg(target_os = "macos")]
fn open_browser_impl(url: &str) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("open").arg(url).status()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_browser_impl(url: &str) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("xdg-open").arg(url).status()
}

/// 加载嵌入的图标 PNG → `tray_icon::Icon`。
///
/// 编译期把 `assets/icon.png` 嵌进二进制（`include_bytes!`）；运行时用 `image` crate
/// 解码一次得到 RGBA bytes，再喂给 `Icon::from_rgba`。
fn load_icon() -> Result<Icon> {
    let png_bytes = include_bytes!("../assets/icon.png");
    let img = image::load_from_memory(png_bytes).context("failed to decode embedded icon.png")?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), w, h).context("failed to build tray Icon from RGBA bytes")
}

/// 进入托盘事件循环，**阻塞当前线程**直到用户选 Quit。
///
/// 行为：
/// 1. 从嵌入的 `assets/icon.png` 加载图标
/// 2. 构建菜单（`Open Web UI` / separator / `Quit`）+ 托盘图标
/// 3. 启动后台 thread 把 `MenuEvent::receiver()` 的事件转发到内部 mpsc channel
///    （不能直接阻塞 `MenuEvent::receiver().recv()`，因为 Windows 上必须先 pump
///    message queue 才能触发 menu 事件，会造成死锁）
/// 4. 主循环：pump Win32 messages → 50ms `recv_timeout` 轮询 menu channel → 分发命令
/// 5. `Quit` → `shutdown_tx.send(())` → 返回 `Ok(())`，进程随 axum 优雅关闭退出
///
/// **必须**在创建托盘图标的同一个线程上调用（tray-icon 的限制）。
/// 在 Windows / Linux 可以是任何 thread；macOS 必须是 main thread（本任务不验证 macOS）。
///
/// # Arguments
///
/// * `port` - Web UI 监听端口（拼 `http://localhost:{port}` 给 `Open Web UI`）
/// * `shutdown_tx` - 通知 axum 优雅关闭的 oneshot 信号 sender
///
/// # Examples
///
/// ```no_run
/// # fn main() -> anyhow::Result<()> {
/// let (tx, _rx) = tokio::sync::oneshot::channel();
/// lorag::tray::run_tray_loop(3000, tx)?;
/// # Ok(())
/// # }
/// ```
pub fn run_tray_loop(port: u16, shutdown_tx: oneshot::Sender<()>) -> Result<()> {
    let icon = load_icon()?;
    let menu = Menu::new();
    let open_item = MenuItem::with_id("open", "Open Web UI", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);
    menu.append(&open_item)
        .context("failed to append Open Web UI menu item")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("failed to append separator")?;
    menu.append(&quit_item)
        .context("failed to append Quit menu item")?;

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("lorag")
        .with_icon(icon)
        .build()
        .context("failed to build tray icon")?;

    // 后台 thread：把 tray-icon 内部 channel 的 MenuEvent 转发成我们自己的 TrayCommand。
    // 不能直接阻塞 MenuEvent::receiver().recv()，因为 Windows 上必须先 pump message queue
    // 才能触发 menu 事件；阻塞 recv 会让主循环没机会 pump，造成死锁。
    let (menu_tx, menu_rx) = std::sync::mpsc::channel::<TrayCommand>();
    std::thread::spawn(move || {
        let menu_events = MenuEvent::receiver();
        while let Ok(event) = menu_events.recv() {
            if let Some(cmd) = menu_id_to_command(event.id.0.as_str())
                && menu_tx.send(cmd).is_err()
            {
                break;
            }
        }
    });

    println!("lorag tray: icon ready, right-click for menu");

    // 主事件循环：pump Win32 messages + 50ms recv_timeout 轮询 + 分发命令。
    // 50ms 是 responsiveness（用户点完菜单多久看到反应）跟 CPU 占用之间的合理折中。
    loop {
        pump_windows_messages();

        match menu_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(TrayCommand::OpenBrowser) => {
                let url = format!("http://localhost:{port}");
                if let Err(e) = open_browser(&url) {
                    eprintln!("{e:#}");
                }
            }
            Ok(TrayCommand::Quit) => {
                println!("lorag tray: quitting...");
                let _ = shutdown_tx.send(());
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                anyhow::bail!("menu event channel disconnected unexpectedly")
            }
        }
    }
}

/// Pump Win32 message queue（非阻塞 `PeekMessageW` 循环）。
///
/// 只在 Windows 上有实际代码；其他平台是 no-op。
/// **必须**在创建 tray icon 的线程上调用，否则 menu 事件永远不会 fire。
///
/// `PeekMessageW` 返回非零表示拿到了消息，0 表示队列空；`PM_REMOVE` 表示拿到后从队列删除。
/// `TranslateMessage` 把键盘消息转成字符消息；`DispatchMessageW` 调用注册的 window proc。
/// tray-icon 的 hidden window proc 在这里被触发，进而 push `MenuEvent` 到它的内部 channel。
#[cfg(target_os = "windows")]
fn pump_windows_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn pump_windows_messages() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_id_open_maps_to_open_browser() {
        assert_eq!(menu_id_to_command("open"), Some(TrayCommand::OpenBrowser));
    }

    #[test]
    fn menu_id_quit_maps_to_quit() {
        assert_eq!(menu_id_to_command("quit"), Some(TrayCommand::Quit));
    }

    #[test]
    fn menu_id_unknown_returns_none() {
        assert_eq!(menu_id_to_command("bogus"), None);
        assert_eq!(menu_id_to_command(""), None);
        // 大小写敏感——菜单 id 必须精确匹配
        assert_eq!(menu_id_to_command("OPEN"), None);
        assert_eq!(menu_id_to_command("Quit"), None);
    }

    #[test]
    fn open_browser_invalid_url_does_not_panic() {
        // 传一个明显不是合法 URL 的字符串；只验证返回 Result，不验证是否成功打开浏览器
        // （CI / headless 环境打开浏览器会失败，但**不能 panic**）。
        let result = open_browser("not a real url at all");
        // 不 assert Ok / Err——Windows 的 `start` 命令对无效字符串也可能返回 success，
        // 测试的核心是"不 panic"，调用本身返回什么取决于平台。
        let _ = result;
    }
}
