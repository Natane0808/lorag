//! G13: cross-platform "launch at user login" toggle.
//!
//! Thin wrapper around the [`auto-launch`](::auto_launch) crate that wires up
//! the current executable path for the `lorag-gui` binary.
//!
//! Per-platform behavior (all handled by `auto-launch` 0.5):
//! - **Windows**: writes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\{app_name}`
//!   pointing at the running `lorag-gui.exe`. User-scoped — no admin required.
//! - **macOS**: drops `~/Library/LaunchAgents/com.{app_name}.plist` pointing at the
//!   current binary.
//! - **Linux**: writes `~/.config/autostart/{app_name}.desktop` (XDG Autostart).
//!
//! We intentionally do **not** pass `--minimized` or any other args: the GUI bin
//! currently has no such flag and would reject unknown args.

use std::env::current_exe;

use anyhow::{Context, Result};
use auto_launch::AutoLaunchBuilder;

/// Canonical app name used for the registry value / plist basename / desktop
/// filename. Matches the system-tray display name used elsewhere in the GUI.
pub const APP_NAME: &str = "lorag";

/// Build the [`auto_launch::AutoLaunch`] handle pointing at the running
/// `lorag-gui` executable. Errors if `std::env::current_exe()` fails (extremely
/// rare — e.g. the binary was deleted mid-run).
fn build() -> Result<auto_launch::AutoLaunch> {
    let exe = current_exe().context("failed to resolve current exe path for autostart")?;
    let exe_str = exe.to_string_lossy().to_string();
    let auto = AutoLaunchBuilder::new()
        .set_app_name(APP_NAME)
        .set_app_path(&exe_str)
        .set_use_launch_agent(true)
        .build()
        .context("failed to build AutoLaunch instance")?;
    Ok(auto)
}

/// Return whether autostart is currently enabled for `lorag-gui`.
pub fn is_enabled() -> Result<bool> {
    let auto = build()?;
    auto.is_enabled()
        .context("failed to query autostart status from the OS")
}

/// Enable autostart (idempotent: enabling when already on is a no-op).
pub fn enable() -> Result<()> {
    let auto = build()?;
    if auto
        .is_enabled()
        .context("failed to query autostart status before enable")?
    {
        return Ok(());
    }
    auto.enable().context(
        "failed to enable autostart for lorag-gui (check OS permissions; Windows writes to HKCU which should be user-writable)",
    )
}

/// Disable autostart (idempotent: disabling when already off is a no-op).
pub fn disable() -> Result<()> {
    let auto = build()?;
    if !auto
        .is_enabled()
        .context("failed to query autostart status before disable")?
    {
        return Ok(());
    }
    auto.disable().context(
        "failed to disable autostart for lorag-gui (check OS permissions on the registry / plist / desktop file)",
    )
}
