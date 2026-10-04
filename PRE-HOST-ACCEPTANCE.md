# 脱离宿主之前的完成边界

Ousject 0.0.0 当前交付的是“可在宿主应用中运行的系统核心”。它不是 Linux 发行版，Ousject Process 也不是 Linux Process。Linux/Rust 目前只承担启动、内存/同步原语和硬件 API 适配。

## 已完成的宿主内系统层

- Praxis → OTF0 独立编译器、反汇编和 Token VM。
- 持久 Program、Process、Value、Class、Collection、Namespace、Channel、User、Session、Type 和 Effect Object。
- Process 身份、权限、调用栈、异常栈、挂起/恢复、协作式多进程调度。
- 同用户与跨用户显式 Channel IPC；等待和唤醒与消息提交保持原子。
- 动态 Type Descriptor；启动时 Provider 注册；统一 `object.create/find/query` 与 `name.capability(...)`。
- Console、TCP Endpoint、Display、Keyboard、Clock Sensor 和 4096-byte Block Storage 宿主适配器。
- 密码验证、一次展示的 Session Token、登录/退出，以及非系统 Subject Process。
- 跨 Shard 原子事务、乐观冲突、权限、Parent/Child、Link、Tombstone 和一致性检查。
- 校验 WAL、原子 Checkpoint、尾部撕裂恢复、Store 独占与死进程锁恢复。
- 外部 Provider 操作的持久 Effect Intent 和同一启动内的幂等重试。

## 明确留到脱离宿主阶段的内容

- 固件/Bootloader 入口、CPU 初始化、中断、页表、物理/虚拟内存管理。
- 自己的线程/同步原语、正式抢占式调度器和多核启动。
- PCI/USB/ACPI 等总线枚举及真正的显示、键盘、网卡和块设备驱动。
- 用正式块设备 Backend 替换 `FileSnapshotBackend`。
- 网络协议栈、硬件时钟驱动，以及不依赖宿主标准库的运行时。

这些内容不是“换几个 API 就完成”；它们是下一阶段的内核与驱动工作。现有 Provider 和 SnapshotBackend 边界用于让 Object/Process/事务语义在迁移时保持不变。

## 验收命令

```bash
./scripts/check-system
```

只有格式、严格 Clippy、全部单元/并发/故障/恢复/端到端测试和 CLI 冒烟测试同时通过，才算当前边界可用。
