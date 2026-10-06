# 脱离宿主之前的完成边界

Ousject 0.0.0 当前交付的是“可在宿主应用中运行的系统核心”。它不是 Linux 发行版，Ousject Process 也不是 Linux Process。Linux/Rust 目前只承担启动、内存/同步原语和硬件 API 适配。

## 已完成的宿主内系统层

- Praxis → OTF0 独立编译器、反汇编和 Token VM。
- 持久 Program、Process、Value、Class、Collection、Namespace、Channel、SwapPool、Timer、Audit、User、Session、Type 和 Effect Object。
- Process 身份、权限、调用栈、异常栈、明确生命周期状态、统一 WaitReason、round-robin 执行片、Worker lease/generation 和恢复。
- Channel 与 SwapPool IPC；Channel 消息/消费/等待唤醒及 SwapPool membership 都经 OMS 原子持久事务。
- Effect `manual` / `retry_idempotent` 恢复策略、持久 Timer 与可恢复 `time.sleep()`、内部追加式 Audit。
- Value、Channel、Process、SwapPool、Module 和 Package 的 Hosted Core 资源限制及 secret 内容脱敏。
- 动态 Type Descriptor；启动时 Provider 注册；统一 `object.create/find/query` 与 `name.capability(...)`。
- Console、TCP Endpoint、Display、Keyboard、Clock Sensor 和 4096-byte Block Storage 宿主适配器。
- 密码验证、一次展示的 Session Token、登录/退出，以及非系统 Subject Process。
- 跨 Shard 原子事务、乐观冲突、权限、Parent/Child、Link、Tombstone 和一致性检查；退役内容 7 天后后台自动清理并永久保留元数据。
- 校验 WAL、原子 Checkpoint、尾部撕裂恢复、Store 独占与死进程锁恢复。
- 外部 Provider 操作的持久 Effect Intent；结果不确定时按恢复策略重试相同 Effect ID 或保留 `unknown`。

SwapPool 分享 Object ID 和通过 OMS 提交的 Object state，不暴露 RAM 地址。普通 Library Module 在调用者 Process/Subject 中运行，Manifest capabilities 不会授予额外权限。高权限 Package 仍由 `local` 检查 SHA、能力清单并显式批准后运行。

## 明确留到脱离宿主阶段的内容

- 固件/Bootloader 入口、CPU 初始化、中断、页表、物理/虚拟内存管理。
- 自己的线程/同步原语、正式抢占式调度器和多核启动。
- PCI/USB/ACPI 等总线枚举及真正的显示、键盘、网卡和块设备驱动。
- 用正式块设备 Backend 替换 `FileSnapshotBackend`。
- 网络协议栈、硬件时钟驱动，以及不依赖宿主标准库的运行时。

这些内容不是“换几个 API 就完成”；它们是下一阶段的内核与驱动工作。现有 Provider 和 SnapshotBackend 边界用于让 Object/Process/事务语义在迁移时保持不变。

## 验收命令

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/check-system
```

只有格式、严格 Clippy、全部单元/并发/故障/恢复/端到端测试和 CLI 冒烟测试同时通过，才算当前边界可用。
