# Ousject 下一阶段实现与拆分计划

状态：Hosted Core 当前阶段已完成并通过验收。表中仍列出的未实现项是明确保留的后续边界。

## 实施进度

| 阶段 | 状态 | 记录 |
|---|---|---|
| 0. 可信基线 | 已验收 | `8dd09e1`；`scripts/check-system` 全通过；源码归档解压后 Git 历史为 2 个提交并离线构建成功 |
| 1.1 CLI 与硬件适配拆分 | 已完成 | 命令、选项、运行时、终端输入与各硬件 Provider 已拆分；严格 Clippy 和 6 项 CLI 测试通过 |
| 1.2 VM 拆分 | 已完成 | VM 已分出生命周期、指令/事务执行、调用/Provider 分派、对象操作、内建函数、状态编解码、回收与调度文件；VM 13 项单元测试、55 项系统端到端测试及严格 Clippy 通过 |
| 1.3 OMS 拆分 | 已完成 | 对象、事务、管理器、保留策略、持久化和 WAL 已拆分；严格 Clippy、10 项单元测试、23 项稳定性测试及 8 项统一对象测试通过 |
| 1.4 Praxis 编译器拆分 | 已完成 | 编译入口、模块展开、词法器、语句/声明/控制流/表达式解析器已拆分；Clippy 与 10 项编译器测试通过 |
| 1.5 测试拆分 | 已完成 | VM 端到端测试按职责拆为 14 组；当前 55 项系统测试通过 |
| 2. 持久终端会话 | 已完成 | 稳定 Session/Process、跨 Store 重启恢复、每用户隔离、原子提交、历史/多行缓冲/尺寸、表达式回显、函数/Class/import 保留、Ctrl+C 只中断当前提交、旧命令 Token 压缩；终端硬件输入租约重启后重新发现，不复活旧租约 |
| 3. 内核扩展模块 | Hosted Library 已完成 | Module/Registry 支持安装、启停、升级、回滚、卸载、精确依赖锁定、源码 SHA 验证、实例恢复和旧版本引用保护。普通 Library 在调用者 Process/Subject 中运行，Manifest capability 声明不授予额外权限。独立 Service Module、Kernel Extension Type 注册和 Native/Rust ABI 未实现。 |
| 4. 正式调度器与 Process 恢复 | 已完成（Hosted Core） | Ready/Running/Waiting/Suspended/Halted/Terminated/Failed 与统一 WaitReason 持久保存；round-robin，4096 Token/20ms 执行片，Worker lease owner/generation/deadline，Store recovery 重建 runnable queue 并隔离旧 generation。 |
| 5. Effect、IPC、Timer 与审计 | 已完成（最小范围） | Channel 原子发送/接收/等待；SwapPool 持久成员 Link；Effect `manual`/`retry_idempotent` 恢复策略；Timer 与 `time.sleep` 持久恢复；内部追加式 Audit。未加入 Topic、Subscription、Broadcast。 |
| 6. 资源限制与安全加固 | 已完成（Hosted Core 基线） | Value 深度/编码上限、Channel 消息与队列限制、Process/子 Process/Pool/Package 限额；Effect 和终端历史递归/敏感输入脱敏；Package Application 的 Subject 与按方法 capability 隔离。宿主网络安全边界见 Provider 实现。 |
| 7. 包、导出、升级与发布 | 核心流程已实现 | Package coordinate/SHA 不可变索引、精确依赖闭包、导入/安装复验、按用户 Installation/Data、capability 批准、Application/Export、升级/回滚/恢复/卸载和 Market 下载。Market 仅支持 Ousject Package 格式；开发者签名/发布者信任不在范围内。 |

包管理器从当前 MVP 到完整交付的具体顺序和验收矩阵见 [PACKAGE-MANAGER-COMPLETE-PLAN.md](PACKAGE-MANAGER-COMPLETE-PLAN.md)。

## 1. 总体目标

下一阶段要解决四个核心问题：

1. 将过大的源码文件按职责拆开，保持行为不变。
2. 让一个终端会话真正对应一个长期存在的 Praxis Process。
3. 建立以 Object 和 Capability 为基础的内核扩展模块系统。
4. 补齐调度、Effect 恢复、IPC、审计、资源限制和发布恢复能力。

内核继续只保留 Object Store、Process 执行、权限、原子事务、Effect 基础设施和模块加载器。终端、网络及硬件适配能力逐步迁移到模块。

本轮最终验收命令为 `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 和 `./scripts/check-system`，均通过。工作区共 144 项测试通过，其中 VM 系统端到端测试 55 项；验收脚本还通过编译、控制流/集合/对象/管道输入 CLI 示例，以及本地用户首次启动和复登。

## 2. 执行原则

- 拆分阶段不改变行为，不同时重写功能。
- 每次只移动一个职责，保持提交规模可以审查和回退。
- 所有持久状态变更必须经过 OMS 原子事务。
- 模块不能直接获得宿主全部权限，只能使用被授予的 Object Capability。
- 外部操作必须经过持久 Effect。
- 文档中的“已实现”必须有代码位置和验收场景对应。
- 发布归档必须来自干净 Git 提交，并在临时目录中解压验证。
- 项目版本在正式发布前继续保持 `0.0.0`。

## 3. 阶段 0：建立可信基线

### 工作

- 整理并提交当前工作区内已经完成的修改。
- 确认 `.git` 是完整仓库，而不是指向其他工作树的文本文件。
- 保存当前公开 API、持久格式和系统启动流程清单。
- 为每个计划项建立“未开始、实现中、已验收”状态。
- 生成唯一文件名的源码归档，排除 `target/`、`.tools/` 和 `.ousject/`。

### 完成标准

- Git 工作树干净。
- 当前功能全部通过系统验收。
- 解压归档后可以查看完整 Git 历史并从源码构建。

## 4. 阶段 1：源码拆分，不改变行为

### 4.1 `ousject-vm`

将当前约 5300 行的 `crates/ousject-vm/src/lib.rs` 拆成：

```text
crates/ousject-vm/src/
├── lib.rs
├── machine.rs
├── process/
│   ├── mod.rs
│   ├── state.rs
│   ├── codec.rs
│   ├── lifecycle.rs
│   └── reaper.rs
├── executor/
│   ├── mod.rs
│   ├── instruction.rs
│   ├── call.rs
│   ├── function.rs
│   ├── class.rs
│   ├── collection.rs
│   └── transaction.rs
├── builtins/
│   ├── mod.rs
│   ├── object.rs
│   ├── compiler.rs
│   ├── authentication.rs
│   ├── process.rs
│   ├── math.rs
│   ├── text.rs
│   └── time.rs
├── provider_dispatch.rs
├── effect.rs
├── scheduler.rs
└── error.rs
```

### 4.2 `oms-runtime`

将当前约 4200 行的 `crates/oms-runtime/src/lib.rs` 拆成：

```text
crates/oms-runtime/src/
├── lib.rs
├── manager.rs
├── object.rs
├── transaction.rs
├── permissions.rs
├── type_registry.rs
├── indexes.rs
├── retention.rs
├── codec.rs
└── persistence/
    ├── mod.rs
    ├── wal.rs
    ├── snapshot.rs
    ├── manifest.rs
    ├── generation.rs
    └── lock.rs
```

### 4.3 `ousject-cli`

将当前约 2400 行的 `crates/ousject-cli/src/main.rs` 拆成：

```text
crates/ousject-cli/src/
├── main.rs
├── options.rs
├── runtime.rs
├── commands/
│   ├── mod.rs
│   ├── boot.rs
│   ├── compile.rs
│   ├── run.rs
│   ├── object.rs
│   └── system.rs
├── terminal/
│   ├── mod.rs
│   ├── console.rs
│   ├── input.rs
│   └── key_parser.rs
└── hardware/
    ├── mod.rs
    ├── display.rs
    ├── keyboard.rs
    ├── network.rs
    ├── resolver.rs
    └── block_storage.rs
```

### 4.4 `praxis-compiler`

```text
crates/praxis-compiler/src/
├── lib.rs
├── lexer.rs
├── parser.rs
├── expression.rs
├── statement.rs
├── declarations.rs
├── module_loader.rs
└── error.rs
```

### 4.5 测试拆分

将单个大型端到端测试文件按领域拆成 Object、Process、Class、Authentication、Console、Effect、Network、Recovery 和 Shell 测试文件。

### 完成标准

- 所有公开 API 和持久编码保持不变。
- 拆分前后的系统验收结果一致。
- 普通实现文件原则上不超过 1000 行；确有理由的集中分发表不得超过 1500 行。

## 5. 阶段 2：真正的持久终端会话

### 工作

- 新增 `core.terminal_session` Object。
- 一个 Terminal Session 固定关联一个 Praxis Process ID。
- 每次提交新源码时复用同一 Process，不再创建命令子进程。
- Process 保存变量、函数、Class、import、当前用户和终端输入模式。
- 支持直接输入表达式并显示结果，例如输入 `a` 输出 `123`。
- 函数和 Class 定义跨轮保留，允许后续重新定义并采用最新定义。
- 提示符使用 `console.print()`，保持 `local> ` 与输入在同一行。
- 保存多行输入、命令历史和终端尺寸。
- 支持断线、重新登录和系统重启后恢复同一会话。
- Ctrl+C 只中断当前提交，不销毁整个会话 Process。

### 完成标准

以下输入必须始终使用同一个 Process ID：

```praxis
a = 1
a++
a

func twice(value) {
    return value * 2
}

twice(a)
```

退出并重新进入终端后，`a` 和 `twice` 仍然可用。

## 6. 阶段 3：内核扩展模块系统

### 6.1 核心对象

- `core.module`：模块身份、版本、ABI、代码、依赖、声明能力、状态和哈希。
- `core.module_registry`：模块安装、查询、启用、停用、升级和卸载。
- `core.module_instance`：一次已加载模块及其运行状态。

### 6.2 模块种类

| 种类 | 用途 | 权限模型 |
|---|---|---|
| Praxis Module | 普通系统服务和可复用代码 | 普通 Object Capability |
| Kernel Extension | 注册新 Type、服务对象和内核能力 | 仅 `local` 安装，声明式授权 |
| Driver Module | 发现硬件并发布 Provider-owned Object | 只获得指定设备与 Effect 能力 |

第一阶段不加载宿主动态库。模块使用版本化 TF/Praxis 制品，避免重新绑定到 Linux、Rust ABI 或某个发行版。

### 6.3 生命周期

```text
created → installed → enabled → active
                     ↘ failed
active → draining → disabled → retired
```

### 6.4 原子性与恢复

- 安装时一次提交 Module、Program 与依赖版本绑定；Kernel Extension 的 Type、服务对象和权限事务仍待实现。
- 任一步失败时整个安装不生效。
- 升级先验证新版本，再原子切换活动版本。
- 卸载先禁止新调用，再等待正在执行的 Effect 完成或进入可恢复状态。
- 崩溃后根据持久模块状态恢复，不依赖内存注册表猜测。

### 6.5 第一批模块

1. Terminal：Console、Display、Keyboard 和 Terminal Session。
2. Network：Resolver、Endpoint，后续加入 HTTP Client 和目标策略。
3. Storage Driver：块设备适配，只向内核公布。
4. Diagnostics：健康检查、指标和审计查询。

### 当前边界

- Library Module 按调用者 Process/Subject 执行；Manifest 声明只作元数据，不用于提权。调用者本身没有对应 Object capability 时，Module 代码也不能调用它。
- 安装失败不会留下半成品；依赖 Object ID、源码 SHA 与 Module Instance 均可在 Store 重启后恢复。
- 独立 Service Module、Kernel Extension 的动态 Type 注册与 Native/Rust ABI 仍未实现。

## 7. 阶段 4：正式调度器与 Process 恢复

Hosted Core 已完成统一的持久 Process 状态、统一 `WaitReason`、round-robin runnable queue、4096 Token/20ms bounded slice 和 Worker lease fencing。等待的 Process 不占 Worker；睡眠时调度器等待最近 deadline。恢复扫描持久 Process/Timer/Effect 状态，清理旧 lease 并只重新排入 Ready Process。持久化队列由 Process Object 状态重建，而不是另存易分叉的队列副本。

Hosted 实现仍为协作式单机调度。真正抢占、OS 级多核和独立硬件 Worker 留在脱离宿主阶段。

## 8. 阶段 5：Effect、IPC、Timer 与审计

Hosted Core 已实现 `pending/running/completed/failed/unknown` Effect 状态，以及 `manual` 和 `retry_idempotent` 恢复策略。没有远端幂等保证时，恢复把不确定结果保留为 `unknown`，不自动重放。

IPC 仅包括持久 Channel 与 SwapPool。Channel 限制单消息 1 MiB、队列 1024 项/8 MiB，send/receive 与 Process 位置或 waiter wakeup 原子提交。SwapPool 只记录共享 Object 的名字到 ObjectId 的持久 Link，不分配共享地址。Topic/Subscription/Broadcast 不属于本轮范围。

`core.timer`、`time.sleep()`、取消、等待和重启后到期唤醒均已接通。最小 Audit 记录权限、Module/Package 生命周期、Process 和 Effect 生命周期事件；Audit details 上限 4 KiB，不记录 Value 或 Provider payload，普通 Subject 没有 Audit root 访问权限。

## 9. 阶段 6：资源限制与安全加固

当前边界实施 Value 最大 16 MiB/64 层/单容器一百万项，Channel 单消息与队列容量，子 Process 与活跃 Subject Process 限额，SwapPool 成员/数量上限，以及 Module/Package 体积、依赖深度和数量限制。Effect secret handle 递归脱敏，终端历史对敏感输入模式保存通用标记，Audit 只保存白名单事件和小型非内容详情。

登录限速、统一 Session 到期/并发策略和更细粒度的网络域名策略仍属于后续安全工作，不作为 Hosted Core 当前验收已完成项。

## 10. 阶段 7：Package 核心、导出、升级与发布

本地 Package 的不可变 coordinate/SHA、精确依赖闭包、按用户安装、私有 Data、资源、Application Subject、方法级 capability、Export、升级/回滚、卸载和保留期内恢复已实现。Market 客户端支持索引、搜索、SHA 校验下载和 Ousject Package 闭包安装；CilExec SQLite/FCL 文件不能直接安装。细节与明确未实现项见 [PACKAGE-MANAGER-COMPLETE-PLAN.md](PACKAGE-MANAGER-COMPLETE-PLAN.md)。

整库 Object 快照导出/导入、正式向前格式迁移流程、远端 Ousject Package 发布端和二进制发布流水线仍是后续工作。签名、Publisher trust、PKI、SBOM、Kernel Extension 发布体系不属于 Hosted Core。

## 11. 不加入或暂缓的内容

- 不把 PostgreSQL、Docker 或 systemd 变成内核依赖。
- 不重新建立“万物皆文件”的传统 VFS。
- 不允许模块直接执行任意宿主命令。
- 不在模块系统第一阶段支持不稳定的 Rust 动态库 ABI。
- 不建设 Publisher trust、PKI 或数字签名体系。
- 不在持久 Terminal Session 完成前制作 nano 类编辑器。

## 12. 推荐实施顺序

```text
可信基线（已完成）
  → 拆分 CLI 和硬件适配器
  → 拆分 VM Process/Codec/Reaper
  → 拆分 VM Executor/Builtins
  → 拆分 OMS Persistence/Transaction
  → 拆分 Compiler
  → 持久 Terminal Session
  → core.module 与 Module Registry
  → Terminal/Network/Storage 模块化
  → 正式 Hosted Scheduler（已实现）
  → Effect 恢复、Channel/SwapPool、Timer、Audit（已实现）
  → Hosted Core 资源限制（已实现）
  → Package 安装/运行/恢复（已实现；发布边界保留）
  → Praxis nano 编辑器
```

每个箭头都是独立验收点。前一阶段未通过时，不把后一阶段标记为完成。
