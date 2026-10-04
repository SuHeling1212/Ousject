# Ousject 0.0.0 状态

当前里程碑：**Hosted pre-kernel system core**。

已完成并通过测试：独立 Praxis 编译器、OTF0 VM、持久 Process、协作调度、统一 Object/Type/Provider、用户会话与权限、Namespace/Channel IPC、跨 Shard 原子提交、WAL/Checkpoint 恢复、Effect Intent，以及 Console/TCP/Display/Keyboard/Clock/Block Storage 宿主适配器。

这不是 Linux 发行版。Ousject Process、权限、事务和对象模型由本项目实现；Linux 当前只启动这个应用并提供可替换的硬件/文件 API。真正脱离宿主仍需要 Boot、内存/中断/多核、网络栈和正式驱动，详见 [PRE-HOST-ACCEPTANCE.md](./PRE-HOST-ACCEPTANCE.md)。

正确性保证和已知外部交付边界见 [IMPLEMENTED-FEATURES.md](./IMPLEMENTED-FEATURES.md)。下一步优化计划见 [PERFORMANCE-PLAN.md](./PERFORMANCE-PLAN.md)。

本地验证：

```bash
./scripts/check-system
```

项目工具链位于 `.tools/rust`，可用 `scripts/cargo-local`。版本号和 OTF/OPS/OVL/OMS/WAL Magic 在正式发布前保持 0。
