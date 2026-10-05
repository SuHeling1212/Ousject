# Ousject 0.0.0 状态

当前里程碑：**Hosted pre-kernel system core**。

当前实现继续使用 OTF0/版本号 0：持久 Process、协作调度、统一 Object/Type/Provider、用户会话与权限、Namespace/Channel IPC、跨 Shard 原子提交、WAL/Checkpoint 恢复和 Effect Intent。Console 行读取与键盘结构化事件共享独占终端输入源；定时 sleep 以非阻塞 Process 挂起实现；结束 Process 的结果保留七天后自动退役。已退役 Object 的内容再经七天自动清理，最小元数据保留；没有手动 GC/checkpoint Praxis 命令。

这不是 Linux 发行版。Ousject Process、权限、事务和对象模型由本项目实现；Linux 当前只启动这个应用并提供可替换的硬件/文件 API。真正脱离宿主仍需要 Boot、内存/中断/多核、网络栈和正式驱动，详见 [PRE-HOST-ACCEPTANCE.md](./PRE-HOST-ACCEPTANCE.md)。

正确性保证和已知外部交付边界见 [IMPLEMENTED-FEATURES.md](./IMPLEMENTED-FEATURES.md)。退役对象保留与回收规则见 [GARBAGE-COLLECTION.md](./GARBAGE-COLLECTION.md)。下一步优化计划见 [PERFORMANCE-PLAN.md](./PERFORMANCE-PLAN.md)。

本地验证：

```bash
./scripts/check-system
```

项目工具链位于 `.tools/rust`，可用 `scripts/cargo-local`。版本号和 OTF/OPS/OVL/OMS/WAL Magic 在正式发布前保持 0。
