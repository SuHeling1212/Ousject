# Ousject System MVP

第一阶段交付在宿主进程中运行的 Ousject 系统核心，不是 Linux 发行版，也不把 Linux 语义作为 Ousject 设计的一部分。

```text
Praxis Source
    ↓
Praxis Compiler
    ↓
TF / Token Stream
    ↓
Ousject VM + Scheduler
    ↓
Process Object + Variable Objects
    ↓
OMS Atomic Commit
    ↓
Persistent Object Store
```

当前宿主提供进程启动、控制台、文件 I/O、时间/进程 ID，以及 Rust 标准库使用的内存分配与同步。未来独立运行需要 Boot、内存管理、运行时与 Linux Driver 适配来承接这些依赖；以上核心系统语义保持不变，但不能仅替换设备驱动就让当前二进制裸机运行。

## MVP 验收

系统必须能够：

1. 将 Praxis 源码编译为 TF。
2. 保存和重新加载 TF。
3. 创建 Program Object 和 Process Object。
4. 通过 Token VM 执行算术、变量、条件、循环和输出。
5. 将 Variable 保存为属于 Process 的独立 Object。
6. 将 Token Position、VM Stack 和变量修改原子提交。
7. 在每次正式发布前持久保存 Object 状态。
8. 进程重启后恢复 Object、Process 和执行位置。
9. 提供 `compile`、`run`、`resume`、`inspect` 和交互 Shell。
10. 通过格式、静态检查、单元、恢复和端到端测试。

当前阶段支持单机固定多 Shard、跨 Shard 原子提交和协作式调度。外部操作先写持久 Effect Intent；无远端幂等协议时跨整机崩溃采用 at-least-once，可能重复但请求不会静默丢失。裸机启动、内存/中断/多核与正式硬件驱动属于下一阶段。
