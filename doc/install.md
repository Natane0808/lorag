# lorag 安装编译指南

本文档涵盖从源码编译、GPU 加速配置和 Windows MSI 打包的完整步骤。基础配置（`.env` 文件设置）请参考 [doc/configuration.md](./configuration.md)。

---

## 1. 前置依赖

- Rust 1.85+（支持 Rust 2024  edition）
- Git（克隆仓库）
- 平台支持：Windows 10+ / Linux 64-bit / macOS 14+（支持 Apple Silicon 和 Intel）
- 其他编译依赖随功能开启，见下文各章节。

---

## 2. 快速 CPU 版编译

```bash
git clone https://codeberg.org/natane/lorag.git
cd lorag
cp .env.example .env
# 按照 doc/configuration.md 编辑 .env 配置文件
cargo build
cargo test --lib
```

编译完成后，可执行文件在 `target/debug/lorag`。

---

## 3. Cargo 功能特性表

| Feature | 用途 | 前置依赖 |
|---------|------|----------|
| `cuda` | NVIDIA GPU 加速（推荐 RTX 3060 及以上显卡 | CUDA Toolkit 12.x + `nvcc` + MSVC（Windows）/ GCC（Linux） |
| `flash-attn` | 配合 `cuda` 加速 attention 计算 | 需要先启用 `cuda` 功能 |
| `metal` | macOS Apple Silicon GPU 加速 | Xcode Command Line Tools |
| `gui` | M12 GPUI 桌面启动器（生成 `lorag-gui` 可执行文件） | 支持 DirectX 11/12 / Metal / Vulkan 的显卡 |

---

## 4. NVIDIA CUDA 加速配置

确保已安装 CUDA Toolkit 12.x，且 `nvcc` 在系统 `PATH` 中。Windows 下需要先安装 MSVC 工具链（Visual Studio 带 C++ 组件）。

启用 CUDA 编译：
```bash
cargo build --features cuda
```

首次编译需要下载和编译 CUDA 内核，全过程需要 5-10 分钟，取决于网络和磁盘速度。编译完成后，运行时只需要 NVIDIA 驱动，不需要保留 CUDA Toolkit。

---

## ⚠️ CUDA 编译陷阱

`cargo build`（不带任何功能标志）会直接覆盖已经编译好的 CUDA 二进制文件！

修改代码后，**必须始终使用**：
```bash
cargo build --features cuda
```
否则会生成 CPU 版二进制，虽然能运行，但 4B 模型单查询时间会从 1-3 秒退化到 15-30 秒。详见 [PLAN.md](../PLAN.md) §5.2。

---

## 5. macOS Apple Silicon 加速

启用 Metal GPU 加速：
```bash
cargo build --features metal
```

不需要额外的 CUDA 依赖，只需要 Xcode CLI 工具（`xcode-select --install` 可安装）。

---

## 6. M12 GPUI 桌面 GUI 编译

编译带桌面启动器需要同时启用 `gui` 功能：
```bash
# 带 CUDA 加速：
cargo build --features cuda --features gui

# CPU-only 版本：
cargo build --features gui
```

依赖系统有支持 DirectX 11/12（Windows）/ Metal（macOS）/ Vulkan（Linux）的显卡。更多 GUI 开发细节见 [doc/gui.md](./gui.md)。

---

## 7. Windows MSI 打包

打包步骤：

1. 编译带所有功能的 release 版本：
```bash
cargo build --release --features cuda --features gui
```

2. 安装 `cargo-wix`：
```bash
cargo install cargo-wix --locked
```
需要先安装 [WiX Toolset v3.14+，并确保 `candle.exe` 和 `light.exe` 在系统 `PATH` 中。

3. 生成 MSI 安装包：
```bash
cargo wix
```

输出安装包路径：`target\wix\lorag-0.1.0-x86_64.msi`。默认安装到 `%LOCALAPPDATA%\lorag\`，属于用户级安装，不需要管理员权限。稳定 GUID 已经预配置在 [wix/main.wxs](../wix/main.wxs) 中，不要重新生成。

---

## 8. 常见问题

1. **Windows 下 `nul` 设备错误**：确保使用 PowerShell 5.1+，并且项目路径不包含非 ASCII 字符。
2. **Windows 下链接失败，提示路径太长**：开启 Windows 长路径支持：`计算机配置 → 管理模板 → 系统 → 文件系统 → 启用 Win32 长路径。
3. **CUDA release 首次链接耗时 5-10 分钟**：属于正常现象，release 模式开启 `lto = thin`，冷链接需要优化大量依赖，增量链接后只需要约 30 秒。详见 [doc/development.md](development.md)。
4. **CUDA 编译时内存不足**：限制并行链接任务数：`cargo build --features cuda --jobs 4`。

---

## 9. 开发模式说明

项目默认开发 profile（`dev`）配置 `opt-level = 1`，这个配置在编译速度和运行速度之间做了平衡，对 0.6B 模型实测单查询只需要 4.5 秒，完整 debug 模式则需要 142 秒。详见 [doc/development.md](development.md)。

日常开发直接使用 `cargo build --features cuda` 即可，不需要每次都编译 `--release`，`--release` 只用于性能测试和最终打包。
