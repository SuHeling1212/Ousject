# Ousject 0.0.0 状态

当前里程碑：**Hosted Ousject Core**。当前仓库已包含 Module Registry 收尾、持久协作调度与 Process 恢复、Channel、SwapPool、Effect、Timer、最小 Audit、资源限制，以及 Package SHA 和安装/运行隔离流程。

调度器统一运行终端与普通 Process。Process 状态、等待原因、工作租约代次和执行片中的 Object 修改持久保存。等待输入、IPC、Effect 或 Timer 的 Process 不占执行 Worker；重启恢复 Ready/Waiting 状态并使旧 Worker 代次失效。每个执行片最多 4096 Token 或 20ms。

Channel 保存消息并将 send/receive 与等待者或 Process 位置原子提交。SwapPool 以持久 Link 共享现有 Object 身份，不共享内存地址；成员仍受 Object 自身权限和版本冲突规则保护。Timer 到期可在重启时恢复。Effect 对不确定的手动策略操作记为 `unknown`，只对明确幂等的操作使用同一 Effect ID 重试。Audit 是内核追加式事件对象，不向普通 Subject 发布读取能力。

Module Library 在调用者 Process 和 Subject 中执行；Manifest 能力声明不会授予调用者没有的权限。独立 Service Module、Kernel Extension ABI 和真实硬件驱动仍不属于当前 Hosted Core。Package 内容身份由 coordinate 与 SHA-256 锁定，高权限运行需 `local` 显式批准清单声明的能力。

本轮所述系统仍由 Linux/Rust 应用启动。它不包含 Bootloader、页表、物理内存管理、中断、裸机多核启动、网络栈或正式硬件驱动。详见 [PRE-HOST-ACCEPTANCE.md](./PRE-HOST-ACCEPTANCE.md)。

执行状态和恢复规则见 [PROCESS-RUNTIME.md](./PROCESS-RUNTIME.md)、[SCHEDULER.md](./SCHEDULER.md)、[IPC.md](./IPC.md)、[SWAPPOOL.md](./SWAPPOOL.md)、[EFFECTS.md](./EFFECTS.md) 和 [TIMERS.md](./TIMERS.md)。Package 计划仍记录当前功能及未实现的发布边界，见 [PACKAGE-MANAGER-COMPLETE-PLAN.md](./PACKAGE-MANAGER-COMPLETE-PLAN.md)。

最终验收命令：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/check-system
```

格式版本仍为预发布版本 0。
