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
- 控制流、集合、Class 与 Console 输入；
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
