//! GPU 不可用时的原生降级对话框（M12 G1）。
//!
//! GPUI 的 blade 渲染器初始化失败时**绝不能**用 gpui 自己的弹窗——它起不来。
//! 本模块用各平台最原生、零依赖的方式弹一个模态框，告诉用户：
//! 你的机器不支持图形加速，请回退到 CLI（`lorag --help`）。
//!
//! ## 平台实现
//!
//! - **Windows**：`user32!MessageBoxW`（已在 Cargo.toml 里有 windows-sys 0.59，
//!   跟 M11 tray 用同一组 Win32 features，零额外编译成本）。
//! - **macOS**：`osascript -e 'display dialog ...'`，零依赖 sub-process。
//! - **Linux / 其他 Unix**：先试 `notify-send` 弹桌面通知，失败则只写 stderr。
//!
//! 所有平台在弹之前都会先把完整错误打到 stderr（日志），用户能把详细原因给开发者排障。

use std::io::Write;

use super::gpu_probe::GpuProbeError;

/// 对话框标题（UTF-8，跨平台统一使用）。
const DIALOG_TITLE: &str = "lorag — 需要图形加速";

/// 对话框正文。
///
/// 严格遵循 AGENTS.md §4.4 三段模板：[动作] + [对象] + [原因/建议]。
/// 这里给用户的建议是"请使用命令行版本"。
const DIALOG_BODY: &str = "lorag 桌面版需要 GPU 加速（DirectX 11/12 / Metal / OpenGL）。\n\n\
    你的机器不支持图形加速或驱动有问题。\n\n\
    请使用命令行版本：`lorag --help`";

/// 显示"GPU 不可用"的原生对话框。
///
/// 调用约定：
/// 1. 先把完整错误（含 `{e:#}` debug 格式）打到 stderr
/// 2. 然后尝试弹系统原生模态框（阻塞直到用户点"确定"）
/// 3. 对话框失败（比如无桌面环境）也**不**返回 Err——降级成只写 stderr 即可
///
/// 入参是 probe 出来的错误引用，对话框里展示给用户的是预写好的中文友好文案，
/// 不会暴露技术栈细节（blade / winit / HRESULT），那些只走 stderr。
///
/// # Examples
///
/// ```no_run
/// # fn main() {
/// let err = lorag::gui::gpu_probe::GpuProbeError::InitFailed("no D3D".into());
/// let _ = lorag::gui::fallback_dialog::show_unavailable_gpu_dialog(&err);
/// # }
/// ```
pub fn show_unavailable_gpu_dialog(error: &GpuProbeError) -> std::io::Result<()> {
    // 先写详细错误到 stderr（保留技术栈细节给开发者排障）。
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "lorag GUI: GPU initialization failed — {error:#}")?;
    writeln!(stderr, "lorag GUI: showing native fallback dialog")?;
    drop(stderr);

    show_native_dialog(DIALOG_TITLE, DIALOG_BODY)
}

/// 平台分发：Windows / macOS / Linux。
///
/// 每个平台实现都尽量"容错不 panic"——即使对话框本身失败也只返回 Err，
/// 让调用方继续走 `process::exit(1)`。
#[cfg(target_os = "windows")]
fn show_native_dialog(title: &str, body: &str) -> std::io::Result<()> {
    show_windows_message_box(title, body)
}

#[cfg(target_os = "macos")]
fn show_native_dialog(title: &str, body: &str) -> std::io::Result<()> {
    show_macos_dialog(title, body)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn show_native_dialog(title: &str, body: &str) -> std::io::Result<()> {
    show_linux_dialog(title, body)
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(unix, not(target_os = "macos"))
)))]
fn show_native_dialog(_title: &str, _body: &str) -> std::io::Result<()> {
    // 不认识的平台（wasm / 嵌入式等）—— 只写 stderr，不尝试弹框。
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// Windows: MessageBoxW via windows-sys
// ──────────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn show_windows_message_box(title: &str, body: &str) -> std::io::Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
    };

    // MessageBoxW 需要 UTF-16 字符串，末尾要额外 NUL。
    let title_utf16: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let body_utf16: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: 这是调用 Windows 平台原生 API 的必需 `unsafe` 块（AGENTS.md §2.1
    // 明确豁免"平台 API 必需"场景；与 src/tray.rs:197-217 的 Win32 pump 同例）。
    //
    // 这里满足全部安全前提：
    // 1. `title_utf16` / `body_utf16` 在调用期间全程存活（Vec 没被 drop）
    // 2. 都是以 NUL 结尾的合法 UTF-16 字符串（`encode_utf16().chain(once(0))`）
    // 3. `hWnd = std::ptr::null_mut()` 表示无主窗口（消息框变成 top-level 模态框），
    //    这正是我们要的——probe 阶段还没有任何窗口
    // 4. uType 组合：MB_OK（一个确定按钮）| MB_ICONERROR（红色叉号图标）|
    //    MB_SETFOREGROUND | MB_TOPMOST（即使从后台子进程 / RDP 里拉起来也能看见）
    // 5. 不依赖任何未初始化数据；返回值是按钮 id，我们不关心（反正用户点啥都退出）
    unsafe {
        let _clicked = MessageBoxW(
            std::ptr::null_mut(),
            body_utf16.as_ptr(),
            title_utf16.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// macOS: osascript
// ──────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn show_macos_dialog(title: &str, body: &str) -> std::io::Result<()> {
    // osascript 的 `display dialog` 参数是 AppleScript 字符串字面量，
    // 需要把 `\` 和 `"` 转义掉（body 里我们不用双引号，但做防御性转义）。
    let escaped_body = body.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display dialog \"{escaped_body}\" with title \"{escaped_title}\" buttons {{\"确定\"}} \
         default button 1 with icon stop"
    );

    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("osascript exited with {s}"),
        )),
        Err(e) => Err(e),
    }
}

// ──────────────────────────────────────────────────────────────────────
// Linux: notify-send (zenity fallback)
// ──────────────────────────────────────────────────────────────────────

#[cfg(all(unix, not(target_os = "macos")))]
fn show_linux_dialog(title: &str, body: &str) -> std::io::Result<()> {
    // 优先 notify-send（大部分桌面环境都带）。它是非阻塞的 desktop notification，
    // 没有模态框那么强提示，但能覆盖绝大多数用户。
    if let Ok(status) = std::process::Command::new("notify-send")
        .arg("--urgency=critical")
        .arg(title)
        .arg(body)
        .status()
    {
        if status.success() {
            return Ok(());
        }
    }

    // 回退：zenity（GNOME 默认带）弹真正的模态 error 对话框。
    if let Ok(status) = std::process::Command::new("zenity")
        .arg("--error")
        .arg("--title")
        .arg(title)
        .arg("--text")
        .arg(body)
        .status()
    {
        if status.success() {
            return Ok(());
        }
    }

    // 两个都没装（headless / 精简容器 / TTY）—— 让调用方继续 process::exit。
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "neither notify-send nor zenity is available; wrote error to stderr instead",
    ))
}
