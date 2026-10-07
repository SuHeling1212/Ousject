# 当前状态

本文描述仓库当前的 `0.0.0` 实现，不是未来路线图。状态依据源码、测试和验收脚本整理。

## 定位

当前交付物是“可在宿主应用中运行的持久对象系统核心”：

- 可以编译和运行 Praxis 程序；
- 可以把 Object、Program、Process 和系统状态写入持久 Store；
- 可以测试崩溃恢复、权限、事务和宿主 Provider；
- 不能脱离 Linux/macOS 等宿主独立启动。

## 功能矩阵

| 区域 | 状态 | 主要证据 |
|---|---|---|
| Object/Value/Type | 已实现 | `oms-types`、`unified_objects` 测试 |
| 单/多 Shard 事务 | 已实现 | `m1_stability` 并发与跨 Shard 测试 |
| WAL、Checkpoint、恢复 | 已实现，格式预发布 | `m1_stability` 故障与损坏测试 |
| Parent、Link、Namespace | 已实现 | OMS 与 system e2e 测试 |
| Capability 与 Subject 权限 | 已实现 | OMS、security/network、audit 测试 |
| Tombstone 与延迟回收 | 已实现 | retention 与 stability 测试 |
| Praxis compiler | 已实现子集 | compiler 测试、`examples/` |
| OTF0 与 VM | 已实现，格式预发布 | `tf-format`、system e2e |
| 持久 Program/Process | 已实现 | recovery、atomic objects 测试 |
| 协作式 Scheduler | 已实现 | scheduler 与 recovery 测试 |
| Channel IPC | 已实现 | IPC recovery 测试 |
| SwapPool | 已实现 | swap pool 测试 |
| 持久 Timer | 已实现 | timer recovery 测试 |
| Durable Effect | 已实现 | durable effects 测试 |
| 用户、密码、Session | 已实现 | auth 与 shell 测试 |
| Module Registry | 已实现 | services 测试 |
| 本地 Package 生命周期 | 已实现核心流程 | package lifecycle 测试 |
| 宿主 Console/网络/设备 | 开发适配可用 | CLI hardware 与 system check |
| 裸机启动与真实驱动 | 未实现 | 不在当前宿主系统边界内 |

## Praxis 当前可验证能力

编译器与 VM 的共同测试覆盖：

- 变量、基本运算、条件和循环；
- Array、Map、索引、长度和更新；
- 函数、返回、Class、字段和方法；
- `try/catch`、`break`、`continue`；
- `import` 与 `include`；
- Object 创建、查找、查询、替换、Link 和授权；
- `transaction` 原子块；
- 子 Process、Channel、Timer、Console 输入和系统服务。

这不代表此前设计稿中的每种语法都已实现。完整语言参考将在后续文档批次中从 parser 和
端到端测试重新生成。

## 运行与安全边界

- 普通持久命令需要 `--session <token>`。
- `--local` 是明确的最高本机开发/恢复权限，不会被默认推断。
- `ProviderOnly` Type 不能由普通 Praxis 程序伪造。
- Session secret 只用于认证；密码通过摘要验证，系统验收会检查明文没有写入 Store。
- Audit 事件追加记录权限相关操作，并避免记录 Object Value 与 secret 内容。
- Store 同时只允许一个活动持有者。

当前实现禁止 Rust `unsafe`，但这不等于完成安全审计，也不构成对恶意宿主的隔离。

## 当前宿主依赖

宿主仍提供：

- Rust 标准库、线程、锁和内存分配；
- 文件系统与 `FileSnapshotBackend`；
- TCP、DNS 和终端接口；
- 显示、键盘和块存储的临时适配入口；
- Ousject CLI 进程本身。

Ousject Process 不是 Linux Process。它是由 VM 执行并通过 OMS 持久化的 Object。

## 明确未完成

- Bootloader、固件入口和 CPU 初始化；
- 页表、物理/虚拟内存管理与独立分配器；
- 中断、抢占式调度和多核内核启动；
- PCI、USB、ACPI 等总线枚举；
- 独立网络协议栈与真实硬件驱动；
- 稳定的持久格式、语言 ABI、Package 格式和 public API；
- 发行版级安装、升级、签名、公证和兼容策略。

## 验收

当前边界的完整验收命令是：

```bash
./scripts/check-m1
./scripts/check-system
```

它们要求格式检查、严格 Clippy、全部 Workspace 测试和 CLI 系统冒烟测试同时通过。单独
一份文档或演示成功不能替代这些验收。

## 文档状态

2026-10-07 开始按新的信息架构重写文档。当前已完成入口、快速开始、构建验证、架构、
对象模型、持久化和本状态页。Praxis、CLI、Object API、Package、Runtime internals、
性能与 Roadmap 仍在后续批次中重写。
