# 构建与测试

## 工具链

Workspace 使用：

- Rust `1.85.0`
- Edition `2024`
- Cargo resolver `2`
- `unsafe_code = "forbid"`
- Clippy `all` 与 `pedantic` 设为 warning，验收时提升为 error

确认环境：

```bash
./scripts/cargo-local rustc --version
./scripts/cargo-local clippy --version
./scripts/cargo-local fmt --version
```

`scripts/cargo-local` 优先选择仓库私有工具链 `.tools/rust/usr`，没有时使用 `PATH`。

## 构建

一键编译 Release 版 CLI：

```bash
./scripts/build
```

产物为 `target/release/ousject`。脚本使用仓库固定工具链入口，并以 `--locked` 构建，确保
依赖版本与 `Cargo.lock` 一致。

构建整个 Workspace（开发配置）：

```bash
./scripts/cargo-local build --workspace
```

只构建宿主 CLI：

```bash
./scripts/cargo-local build -p ousject-cli
```

Release 构建：

```bash
./scripts/cargo-local build --release -p ousject-cli
```

生成的二进制名为 `ousject`。

## 验收入口

OMS 基线：

```bash
./scripts/check-m1
```

它依次运行格式检查、严格 Clippy、Workspace 测试和 `oms-tools demo`。

完整宿主系统：

```bash
./scripts/check-system
```

除 Workspace 静态检查与测试外，它还会在临时 Store 上验证：

- Praxis 编译与 TF 执行；
- Object 创建、读取、查询和 Namespace 解析；
- 控制流、集合、Class 与 Terminal 输入；
- 系统安装、首次初始化、登录和退出；
- 密码没有以明文写入测试 Store。

脚本使用临时目录并在退出时清理，不应在仓库留下运行状态。

## 分层测试

运行全部测试：

```bash
./scripts/cargo-local test --workspace
```

按 crate 运行：

```bash
./scripts/cargo-local test -p oms-runtime
./scripts/cargo-local test -p praxis-compiler
./scripts/cargo-local test -p ousject-vm
./scripts/cargo-local test -p ousject-cli
```

主要测试区域：

| 区域 | 主要验证内容 |
|---|---|
| `oms-runtime/tests/m1_stability.rs` | 并发、原子提交、权限、WAL、Checkpoint、恢复 |
| `oms-runtime/tests/unified_objects.rs` | Type、Value、Namespace 与统一创建规则 |
| `ousject-vm/tests/system_e2e/` | Praxis、Process、IPC、Effect、Timer、Package、认证 |
| `praxis-compiler/src/tests.rs` | 语法、Module 展开和编译错误 |
| `ousject-cli/src/cli/tests.rs` | CLI 与终端输入行为 |

## 格式与静态检查

```bash
./scripts/cargo-local fmt --all -- --check
./scripts/cargo-local clippy --workspace --all-targets -- -D warnings
```

修改 Rust 代码后，应先运行这两项，再运行相关 crate 测试，最后运行 `check-system`。

## 生成 Rust API 文档

```bash
./scripts/cargo-local doc --workspace --no-deps
```

HTML 输出位于 `target/doc/`。Rust public API 以生成的 rustdoc 为准；手写文档只解释跨 crate
关系、使用流程和稳定性边界。

## Apple Silicon 交叉构建

`scripts/build-macos-arm64` 仅用于在 Linux x86_64 环境交叉构建
`aarch64-apple-darwin` 二进制。脚本会下载固定 Rust 工具链、Zig 和 `cargo-zigbuild`，因此
需要网络访问以及 `curl`、`tar`、`file`。

```bash
./scripts/build-macos-arm64
```

成功后仓库根目录会出现：

```text
ousject-macos-arm64
```

脚本会用 `file` 校验产物确实是 `Mach-O 64-bit arm64`。它不是 macOS 原生构建的通用
说明，也不负责签名、公证或打包。

## Native UEFI 镜像（x86_64）

独立 EFI 镜像 crate 使用 Rust `x86_64-unknown-uefi` target 和 QEMU/OVMF。先为当前
工具链安装该 Rust target，再构建并启动：

```bash
./scripts/build-native
./scripts/run-native
./scripts/check-native-boot
```

`run-native` 与 `check-native-boot` 需要 QEMU 和 x86_64 OVMF code/variables 镜像。可以用环境变量
`QEMU`、`OVMF_CODE`、`OVMF_VARS_TEMPLATE` 或 `QEMU_FIRMWARE_DIR` 指定位置。当前 EFI 镜像通过
固件 Serial I/O 报告入口和退出 Boot Services，然后由原生 COM1 输出交接标记。镜像安装
自有 GDT/TSS/IDT，切换到自有 ring-0 栈，并通过 100 Hz PIT/8259 IRQ0 提供早期单调时钟。
可选 invariant-TSC 时钟只有在 CPUID 提供频率时才启用；当前 QEMU 配置不提供该信息。
镜像还会替换 CR3 为自有 4 GiB identity page tables（2 MiB pages；仅为单地址空间早期映射）。
异常仍为 fatal，系统随后停机。`check-native-boot` 会校验启动、CR3/栈切换、PIT IRQ 和时钟状态。可用 `QEMU_MEMORY` 调整 QEMU RAM（早期页表目前仅覆盖 4 GiB）。默认启用 `fault-smoke`
时还会注入 `#UD` 并校验异常向量、错误码槽和 RIP。设置 `NATIVE_FAULT_SMOKE=0` 可检查正常
路径；设置 `NATIVE_DOUBLE_FAULT_SMOKE=1` 可验证 #DF IST 栈；使用
`NATIVE_FAULT_SMOKE=0 NATIVE_PANIC_SMOKE=1 ./scripts/check-native-boot` 可单独检查 Rust
panic 诊断。可通过 `QEMU_CPU` 选择 QEMU CPU 模型。当前尚未启动 OMS、VM 或 Praxis。详见
[UEFI Stage A1 研究记录](../project/native-uefi-notes.md)。
