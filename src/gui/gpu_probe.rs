//! GPU 启动探测（M12 G1）。
//!
//! 在真正拉起 GUI 主窗口**之前**先尝试用 GPUI 的 blade 渲染器起一个最小窗口，
//! 验证当前机器是否支持图形加速（DirectX 11/12 / Metal / OpenGL）。
//!
//! ## 为什么要探测
//!
//! GPUI 默认走 GPU 合成（blade renderer）。在以下环境会直接 panic / 卡死：
//! - 虚拟机没开 GPU passthrough（Hyper-V 默认、VMware 默认）
//! - RDP 会话且没启用 GPU 重定向
//! - 老掉牙的集成显卡 / 驱动不完整
//! - 无显示器的 headless 服务器
//!
//! 一旦进入 `gpui_platform::application().run(...)` 主循环，blade 初始化失败会直接
//! panic 或者打印一堆栈信息闪退，小白完全看不懂。这个模块把故障收敛到
//! [`GpuProbeError`]，让上层 `gui_main` 可以弹原生对话框友好退出。
//!
//! ## 策略
//!
//! 单独开一条线程跑一次 "hello world" 版的 `application().run()`：
//! 1. 开一个 1×1 px 的隐藏窗口（`show: false`，避免闪一下）
//! 2. 窗口创建成功（blade renderer 已经初始化 + 交换链就绪）就 `cx.quit()`
//! 3. 主线程用 `mpsc::recv_timeout` 等最多 10 秒：
//!    - 收到 `Ok` → GPU 可用
//!    - 收到 `Err` → blade 初始化失败，回传 String
//!    - 超时 → 视为卡死（比如 RDP 下 blade 线程挂死），放弃线程

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use thiserror::Error;

/// GPU 探测失败原因。
///
/// 目前只区分一类错误（初始化失败），但保留枚举扩展性——未来若加入
/// "驱动版本过旧" / "缺 DX12" 等细分可加变体。
#[derive(Debug, Error)]
pub enum GpuProbeError {
    /// GPUI / blade 渲染器初始化失败，附带人类可读原因。
    #[error("failed to initialize GPUI renderer: {0}")]
    InitFailed(String),
}

/// 探测当前机器能否启动 GPUI 的 GPU 渲染器。
///
/// - `Ok(())` 表示可以安全进入 GUI 主循环；
/// - `Err(GpuProbeError::InitFailed(reason))` 表示图形加速不可用，调用方应弹
///   原生对话框提示用户改用 CLI，然后 `process::exit(1)`。
///
/// **不要**在这个函数成功后假设 GPU 永远稳定——probe 只是确认"能起步"，
/// 运行期 GPU lost 等极端场景仍可能出现，但那是后续问题。
///
/// # Examples
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// match lorag::gui::gpu_probe::probe_gpu() {
///     Ok(()) => println!("GPU OK"),
///     Err(e) => {
///         eprintln!("{e:#}");
///         std::process::exit(1);
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub fn probe_gpu() -> Result<(), GpuProbeError> {
    let (tx, rx) = mpsc::channel::<Result<(), String>>();

    let handle = thread::Builder::new()
        .name("lorag-gpu-probe".to_string())
        .spawn(move || {
            // catch_unwind 兜住 blade / winit panic（blade 在没有 GPU 时常见 panic）。
            let result = std::panic::catch_unwind(run_probe_app);
            let payload = match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(panic) => Err(panic_payload_to_string(&panic)),
            };
            // 线程已经要退出了，Receiver 是否还活着无所谓。
            let _ = tx.send(payload);
        })
        .map_err(|e| GpuProbeError::InitFailed(format!("failed to spawn probe thread: {e}")))?;

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {
            // probe 线程应该已经自己 return；join 一下回收资源。
            let _ = handle.join();
            Ok(())
        }
        Ok(Err(reason)) => {
            let _ = handle.join();
            Err(GpuProbeError::InitFailed(reason))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // 10s 还没回来：blade 很可能卡在驱动调用上（RDP / 某些虚拟机）。
            // 线程不能再 join（会一直阻塞），直接 detach 让进程退出时 OS 回收。
            // 这里只是 probe 阶段，detach 泄漏一条挂死线程可以接受（整个进程马上 exit）。
            std::mem::forget(handle);
            Err(GpuProbeError::InitFailed(
                "GPU probe timed out after 10 seconds (driver or renderer hung; common under \
                 RDP without GPU redirection, or VMs without GPU passthrough)"
                    .to_string(),
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            Err(GpuProbeError::InitFailed(
                "GPU probe thread disconnected before reporting a result".to_string(),
            ))
        }
    }
}

/// 真正的 GPUI probe 主体：起最小隐藏窗口 → 立即 quit。
///
/// 返回 `Ok(())` 表示 open_window 没报错（blade / winit / 交换链都起来了）。
/// 任何 `Err` 都当成 GPU 不可用。
fn run_probe_app() -> anyhow::Result<()> {
    use gpui::{AppContext as _, WindowBounds, WindowOptions, px, size};

    gpui_platform::application().run(|cx| {
        // 不显示窗口（show: false）+ 最小 1×1 px，避免 probe 在用户面前闪一下。
        let bounds = WindowBounds::centered(size(px(1.0), px(1.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(bounds),
            show: false,
            ..WindowOptions::default()
        };

        // open_window 内部会调 platform.open_window → blade 创建 GPU 上下文 + 交换链；
        // 这一步失败就说明 GPU 不可用（无 D3D / Metal / GL 设备）。
        // 用 Entity 占位即可，render 一次都不需要就 quit。
        match cx.open_window(options, |_window, cx| cx.new(|_| ProbeWindow)) {
            Ok(_) => {
                cx.quit();
            }
            Err(e) => panic!("probe open_window failed: {e:#}"),
        }
    });

    Ok(())
}

/// 空 entity，仅作为 `open_window` 的 root view 占位——render 从来不会被调用，
/// 因为 open_window 成功后立刻 `cx.quit()`。
struct ProbeWindow;

impl gpui::Render for ProbeWindow {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}

/// 把 `catch_unwind` 的 panic payload 转成可读字符串。
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic (no string payload)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_display_friendly() {
        let err = GpuProbeError::InitFailed("no D3D device".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("failed to initialize GPUI renderer"));
        assert!(msg.contains("no D3D device"));
    }
}
