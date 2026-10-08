# 当前状态

本文描述仓库当前的 `0.0.0` 实现，不是未来路线图。状态依据源码、测试和验收脚本整理。

## 定位

当前交付物是“可在宿主应用中运行的持久对象系统核心”：

- 可以编译和运行 Praxis 程序；
- 可以把 Object、Program、Process 和系统状态写入持久 Store；
- 可以测试崩溃恢复、权限、事务和宿主 Provider；
- Native UEFI 镜像已在 QEMU 退出 Boot Services、接管早期 CPU 状态、启用自有 heap，并运行共享 OMS 的真实 Object/Transaction smoke；
- 仍不能运行完整 VM/Scheduler/Praxis 用户空间，也没有恢复持久世界的 Native 后端。

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
| Terminal 兼容 API | 已实现 | CLI Terminal Provider 与 durable effect/输入测试 |
| Terminal 字节流、VT 屏幕子集与 Child Terminal 路由 | 已实现宿主版子集 | `HostTerminalProvider`、`TerminalScreen`、CLI 路由测试与 Terminal Shell e2e |
| Physical Display Provider | 未实现 | tty 不再作为 `device.display` 发布；无 framebuffer driver |
| Terminal Binary `input/output(Bytes)` | 已实现 | Host Terminal raw/canonical input 与 screen output；ephemeral |
| Network Binary `input/output(Bytes)` | 已实现 | TCP stream；网络收发通过 durable Effect |
| Block Storage Binary `input/output(Bytes)` | 已实现 | 宿主文件顺序读写；读写通过 durable Effect，并保留 block API |
| Keyboard Provider | 已实现高层事件 API | 宿主只提供按键事件，不伪造 HID/USB Bytes；`capture`、`next_event`、`poll_event(s)` |
| Device 能力清单 | 已实现动态交集 | 运行时 `.capabilities` 取 Object 授权、Type 描述符和实际 Provider 能力的交集 |
| User-space Driver 基础路径 | 可用现有机制表达，非独立框架 | Process、Capability、Package、普通 Object 与设备 Provider；尚无通用 Driver Manager/自动重启策略 |
| Native Provider Registry | 已实现启动期封闭 | `VirtualMachine::register_provider` 只在第一次执行用户 Process 前可用；之后 Registry seal |
| Native platform contracts | 有限的 `no_std` 接口 | `ousject-platform` 定义 BootInfo、内存区域、单调时钟、熵、Terminal/Block transport 和 machine control；Native Image 已使用内存区域、熵与时钟路径 |
| 共享 Object/OTF/Praxis 编译层 | `no_std + alloc` 可构建 | `oms-types`、`tf-format`、`praxis-compiler` 可关闭 `std`；ID 生成要求 Native 启动先由平台熵播种；OMS Runtime 也支持 `no_std + alloc`。VM 仍是 Host-bound |
| Native 物理内存与 heap | Bootstrap 阶段已实现并经 QEMU 验证 | 从 UEFI Conventional 区域中预留 8 MiB 连续 heap（低于现有 4 GiB identity map）；单调分配、OOM 诊断，无释放/回收或通用 frame manager |
| Native OMS | 共享核心已在 QEMU 实际运行 | `oms-runtime --no-default-features` 使用同一事务验证与发布逻辑；smoke 验证初始化、Object 创建/读取/更新、Parent/Link、Capability 拒绝和冲突事务原子失败。Native 暂无文件持久化、后台线程或真实墙钟 |
| Native UEFI 启动 | 早期 kernel 与 OMS bootstrap 已实现 | `ousject-native-image`：UEFI handoff、COM1、bootstrap frame selector、自有 4 GiB identity page tables、GDT/TSS/IDT 与栈、fatal exception/panic diagnostics、100 Hz PIT/8259 单调时钟、heap 与 OMS smoke；QEMU 验证；VM/Praxis execution 尚未接入 |
| 裸机完整系统与真实驱动 | 未实现 | 缺少动态页表、4 GiB 以上映射、frame 回收、APIC timer 与 Scheduler 等待接线、Native VM/Praxis execution、Native Terminal Provider、持久 block backend 与 VirtIO 驱动 |

## Praxis 当前可验证能力

编译器与 VM 的共同测试覆盖：

- 变量、基本运算、条件和循环；
- Array、Map、索引、长度和更新；
- 函数、返回、Class、字段和方法；
- `try/catch`、`break`、`continue`；
- `import` 与 `include`；
- Object 创建、查找、查询、替换、Link 和授权；
- `transaction` 原子块；
- 子 Process、Channel、Timer、Terminal 输入和系统服务。
- Terminal 字节输入/输出、VT 子集屏幕状态与 alternate-screen 快照。

语法子集和运行时 API 以 [Praxis 语法参考](../reference/praxis-syntax.md)、
[Praxis API 总表](../reference/praxis-api.md)、parser/VM 实现及端到端测试为准；文档不会把
设计草案当作已实现能力。

## 运行与安全边界

- 普通持久命令需要 `--session <token>`。
- `--local` 是明确的最高本机开发/恢复权限，不会被默认推断。
- `ProviderOnly` Type 不能由普通 Praxis 程序伪造。
- Session secret 只用于认证；密码通过摘要验证，系统验收会检查明文没有写入 Store。
- Audit 事件追加记录权限相关操作，并避免记录 Object Value 与 secret 内容。
- Hosted Store 同时只允许一个活动持有者。Native 当前使用纯内存 OMS，不打开 Host Store。

Hosted/Core 代码仍受 workspace `unsafe_code = "forbid"` 约束。Native Image 在 UEFI、页表、CPU 和 MMIO 边界允许局部 `unsafe`，并须逐处维护硬件不变量；这不等于完成安全审计，也不构成对恶意宿主的隔离。

## 当前宿主依赖

宿主仍提供：

- Rust 标准库、线程、锁和内存分配；
- Hosted OMS 的文件系统、`FileSnapshotBackend` 与后台工作线程；
- TCP、DNS 和终端接口；
- 键盘与块存储宿主适配；当前没有物理 Display driver；
- Ousject CLI 进程本身。

Ousject Process 不是 Linux Process。它是由 VM 执行并通过 OMS 持久化的 Object。

## 明确未完成或有意保留的边界

- 一般物理内存管理仍未完成：当前 PhysicalFrameAllocator 仅从 UEFI memory map 选区并预留连续范围；Native heap 是固定 8 MiB monotonic bootstrap heap，没有 free/reclaim；
- 动态页表、4 GiB 以上映射、细粒度内存权限、完整物理/虚拟内存管理和 frame 回收（早期 4 GiB identity map 已有）；
- APIC timer、Scheduler 等待/唤醒接线和多核时钟同步（早期 100 Hz PIT/8259 单调时钟已可在 QEMU 使用）；
- 中断、抢占式调度和多核内核启动；
- PCI、USB、ACPI 等总线枚举；
- 独立网络协议栈与真实硬件驱动；
- 完整 VT100/xterm 兼容（仅实现常见 TUI 序列子集；屏幕缓冲区不持久化）；
- 宿主 Keyboard 不提供 Binary `input/output`：OS 仅暴露高层键事件，故不声称有 HID/USB 字节流；
- 物理 Display Provider、像素渲染后端与真实硬件驱动；
- 通用 User-space Driver Manager、健康监控和自动重启策略；驱动可用普通 Process 与 Package
  表达，但目前没有专门生命周期框架；
- Native VM、Scheduler、Praxis execution、Terminal Provider 和 system init/login/shell；
- 稳定的持久格式、语言 ABI、Package 格式和 public API；
- 发行版级安装、升级、签名、公证和兼容策略。

Provider、Type 描述符和内建执行机制在用户空间执行开始后不可扩展。`types.register` 只登记
Praxis 可用的普通 Type 描述信息，不会注册 Rust Provider 或 native Type 实现。Package、Market
和 Praxis `import` 只处理 Praxis/数据资源，不加载 Rust 动态库，也不能修改 VM、OMS 或 Provider
Registry。用户空间 Driver 可以持有获授权的 Device Object、运行在独立 Process 中，并发布普通
Object 作为语义状态/服务入口；Driver 失败不会改变 Registry，但由上层策略决定是否重启。

## 验收

当前边界的完整验收命令是：

```bash
./scripts/check-m1
./scripts/check-system
```

它们要求格式检查、严格 Clippy、全部 Workspace 测试和 CLI 系统冒烟测试同时通过。单独
一份文档或演示成功不能替代这些验收。

## 相关文档

- [快速开始](../getting-started/quickstart.md)
- [系统架构](../concepts/architecture.md)
- [Praxis 语法](../reference/praxis-syntax.md)
- [Praxis API](../reference/praxis-api.md)
