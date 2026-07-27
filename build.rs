//! Build script: embed the SolidJS frontend (`web/dist/`) into the binary.
//!
//! Strategy:
//! 1. If `web/dist/` exists → embed it (user already ran `bun run build`)
//! 2. If only `web/` exists + bun is available → run `bun run build` automatically
//! 3. If neither exists → create a minimal placeholder so rust-embed won't fail
//!
//! Result: `lorag serve` always has something to show, even without the full frontend.

use std::path::Path;

fn main() {
    let dist = Path::new("web/dist");
    if dist.join("index.html").exists() {
        // Already built — nothing to do
        return;
    }

    // Try building
    let web = Path::new("web");
    if web.join("package.json").exists() {
        let status = std::process::Command::new("bun")
            .args(["run", "build"])
            .current_dir(web)
            .status();

        match status {
            Ok(s) if s.success() => return,
            Ok(s) => {
                println!(
                    "cargo:warning=frontend build failed (exit {}), using placeholder",
                    s.code().unwrap_or(-1)
                );
            }
            Err(e) => {
                println!("cargo:warning=bun not available ({}), using placeholder", e);
            }
        }
    }

    // ── Fallback: minimal placeholder ──
    std::fs::create_dir_all(dist).ok();
    let html = r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>lorag — Web UI (placeholder)</title>
  <style>
    body { font-family: system-ui, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; background: #1d232a; color: #a6adbb; }
    .box { text-align: center; max-width: 480px; padding: 2rem; }
    h1 { color: #7480ff; }
    code { background: #2a323c; padding: 0.2em 0.5em; border-radius: 4px; }
    a { color: #7480ff; }
  </style>
</head>
<body>
  <div class="box">
    <h1>lorag</h1>
    <p>Web UI 未构建。运行以下命令生成前端：</p>
    <p><code>cd web && bun install && bun run build</code></p>
    <p>然后重新 <code>cargo build</code>。</p>
    <p><small>或者直接在终端使用 <code>lorag chat</code> / <code>lorag query</code>。</small></p>
    <p><a href="/api/status">/api/status →</a></p>
  </div>
</body>
</html>"#;
    std::fs::write(dist.join("index.html"), html).ok();
    println!("cargo:warning=web/dist/ placeholder created; build frontend for full UI");
}
