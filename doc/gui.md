# 桌面 GUI / Desktop GUI (M12)

> `lorag-gui` 是 M12 GPUI 桌面启动器 —— **不是聊天客户端**。聊天永远走浏览器 Web UI。
> 本页讲给终端用户的使用说明 + 给开发者的编译细节。

---

## 给终端用户（双击即用）

### 安装

1. 从 [Releases 页面](https://codeberg.org/natane/lorag/releases) 下载 Windows MSI 安装包
2. 双击安装（默认装到 `%LOCALAPPDATA%\lorag\`，不需要管理员权限）
3. 双击桌面 `lorag-gui` 图标启动

### 7 张页面

| 页面 | 干啥 |
|---|---|
| **服务** | 启动 / 停止本地服务；点"打开聊天"自动开浏览器 |
| **模型** | 下载 / 刷新 LLM、Embedding、Rerank 状态（Rerank 留空优雅降级） |
| **文档** | 原生对话框选文件 / 文件夹摄入；显示已摄入源列表 |
| **健康检查** | 11 项环境自检，PASS / WARN / FAIL 一目了然 |
| **日志** | 实时滚动日志，可过滤 / 导出 / 打开日志文件夹 |
| **设置** | 图形化编辑 `.env`，原子保存（`.tmp` → rename），重启生效 |
| **关于** | 版本、技术栈、快捷入口 |

### 托盘行为

- **关窗口不退出** —— 常驻系统托盘
- **双击托盘图标** = 显示窗口
- **右键托盘菜单**：显示窗口 / 打开聊天 / 退出
- **Quit 走 graceful shutdown**：托盘 Quit → 关 axum 服务（5 秒超时强退）→ `cx.quit()`

### 主题

- **Tokyo Night**（深色）
- **Ayu**（浅色）
- 主题切换基于 GPUI 内置主题（`themes/tokyonight.json` + `themes/ayu.json` 是跟踪的）

### 没显卡怎么办

启动时会用 `windows-sys MessageBoxW` 弹友好原生对话框告诉你 GPU 探测失败，让你退回 CLI 用 —— **不 panic、不闪退**。绝大多数 2015 年后的机器都支持 DirectX 11/12 / Metal / Vulkan，没问题。

### 日志位置

仅 GUI 模式：`%APPDATA%\lorag\logs\lorag.log.YYYY-MM-DD`，daily 滚动，保留 7 天。

CLI 模式仍走 stderr。

---

## 给开发者

### 编译

```bash
cargo build --features cuda --features gui
cargo run --features cuda --features gui --bin lorag-gui
```

**两个 feature 都要开**：
- `--features cuda`（或 `--features metal`）—— 保住 GPU 加速
- `--features gui` —— 拉 GPUI 依赖

**feature flag 隔离**：`gui` 是 `optional = true` behind `gui` feature。默认 `cargo build`（无 feature）**不**会拉 GPUI 依赖 —— CLI 日常迭代保持快。

### Cargo 依赖（全部 `optional = true` behind `gui`）

| 依赖 | 用途 | 备注 |
|---|---|---|
| `gpui` | GPUI 框架 | `{ git = "https://github.com/zed-industries/zed" }` |
| `gpui_platform` | 平台运行时（winit/blade 渲染） | 同 git URL，`features = ["font-kit", "runtime_shaders"]` |
| `gpui-component` | UI 组件库（Button / Sidebar / DataTable / Input / Dialog ...） | `{ git = "https://github.com/longbridge/gpui-component", rev = "57a9903f48160845aabc8b92a1e2f5348c80d439" }` |
| `gpui-component-assets` | 内置图标 / 字体资源 | 跟 `gpui-component` 同源同 rev |
| `rfd` 0.15 | 跨平台原生文件 / 文件夹选择器 | 同步 `FileDialog` 必须 `spawn_blocking` |
| `tracing-appender` 0.2 | GUI 磁盘滚动日志 | 非 GUI 模式保持 stderr only |

⚠️ **不要追 `gpui` / `gpui-component` main 分支** —— pin 到 Cargo.toml 里的具体 commit（`gpui-component` rev `57a9903f`）。追 main 分支会随机 break。

### 架构约束

- aha candle 推理、`std::fs`、`rfd::FileDialog`（原生 modal loop 阻塞）、`std::process::Command` 一律放 tokio `spawn_blocking`（tokio runtime 在 GUI 启动时建一次，整个进程复用），**绝不能上 GPUI UI thread**
- tokio runtime + GPUI smol executor 共存；同步阻塞经 `cx.spawn` + `cx.update` 推回 UI thread
- 独立 OS thread 跑 tray-icon 0.19 事件循环 + Win32 message pump（避免跟 GPUI smol executor 抢线程），`std::sync::mpsc` → `tokio::spawn_blocking` → `AsyncApp` 桥接（`AsyncApp: !Send`）
- 配置单一来源：设置页改完写回 `.env`（`AppConfig::save_to_dotenv()` 原子写 `.tmp`→rename），不引入 GUI 专属配置文件
- 关闭窗口：`on_window_should_close` 返回 false + `window.minimize_window()` 最小化到托盘

### 不做（已知 TODO）

- **G13 开机自启**：实装在 [AGENTS.md §6](../AGENTS.md) 提到的 `gui::autostart` 模块里还没接进设置页
- **G14 macOS / Linux 打包**：Windows MSI 已验证；macOS / Linux 待跟进
- **聊天嵌入 GPUI**：明确不做 —— 聊天永远走浏览器（点"打开聊天"按钮 → `localhost:port`）

---

## MSI 打包

完整步骤见 [doc/install.md §Windows MSI 打包](install.md#windows-msi-打包)。

简版：

```bash
cargo build --release --features cuda --features gui
cargo install cargo-wix --locked    # 需要 WiX Toolset v3.14+ 在 PATH
cargo wix
# 产物：target\wix\lorag-0.1.0-x86_64.msi
```

---

## 更多信息

- 编译 / CUDA 陷阱 → [doc/install.md](install.md)
- 数据流 / 模块边界 → [doc/architecture.md](architecture.md)
- 命令清单 → [doc/usage.md](usage.md)
- Rust API 级 GUI 模块设计 → [PLAN.md §4.9](../PLAN.md)
- 开发循环 / 排错 → [doc/development.md](development.md)