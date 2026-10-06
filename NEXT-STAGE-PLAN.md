# Ousject 下一阶段实现与拆分计划

状态：实施中。只有代码、文档和验收同时完成的项目才可以标记为已完成。

## 实施进度

| 阶段 | 状态 | 记录 |
|---|---|---|
| 0. 可信基线 | 已验收 | `8dd09e1`；`scripts/check-system` 全通过；源码归档解压后 Git 历史为 2 个提交并离线构建成功 |
| 1.1 CLI 与硬件适配拆分 | 已完成 | 命令、选项、运行时、终端输入与各硬件 Provider 已拆分；`cargo clippy -p ousject-cli --all-targets -- -D warnings` 与 5 项 CLI 测试通过 |
| 1.2 VM 拆分 | 已完成 | VM 已分出生命周期、指令/事务执行、调用/Provider 分派、对象操作、内建函数、状态编解码、回收与调度文件；VM 全部 11 项单元测试、36 项系统端到端测试及 Clippy 通过 |
| 1.3 OMS 拆分 | 已完成 | 对象、事务、管理器、保留策略、持久化和 WAL 已拆分；Clippy、9 项单元测试及 30 项 OMS 集成测试通过 |
| 1.4 Praxis 编译器拆分 | 已完成 | 编译入口、模块展开、词法器、语句/声明/控制流/表达式解析器已拆分；Clippy 与 10 项编译器测试通过 |
| 1.5 测试拆分 | 已完成 | VM 端到端测试按输入、服务、认证/Shell、数学、恢复、语言、Effect、原子对象和安全/网络拆为 9 组；当前 45 项系统测试通过 |
| 2. 持久终端会话 | 已完成 | 稳定 Session/Process、跨 Store 重启恢复、每用户隔离、原子提交、历史/多行缓冲/尺寸、表达式回显、函数/Class/import 保留、Ctrl+C 只中断当前提交、旧命令 Token 压缩；终端硬件输入租约重启后重新发现，不复活旧租约 |
| 3. 内核扩展模块 | 实施中 | 已加入 Module/Registry Object、`local` 原子安装/启停/升级/回滚、按名 import 和按 Object ID 锁定依赖版本；依赖升级后仍加载被锁定的旧版本；未声明的模块导入会失败且不会留下半安装对象；源码 SHA-256 在导入时验证；持久 Store 重启后的模块、依赖和 Module Instance 恢复已有端到端覆盖；`modules.uninstall` 已原子退役无引用版本，仍被依赖或活动实例引用时会拒绝；依赖/实例 Links 与对应的 Object 版本校验和安装、导入同事务提交；`session.close()` 会卸载会话的 Module Instance；声明能力强制授权仍未完成；Native/Rust ABI 明确不加载 |
| 4. 正式调度器与 Process 恢复 | 未开始 |  |
| 5. Effect、IPC、Timer 与审计 | 未开始 |  |
| 6. 资源限制与安全加固 | 未开始 |  |
| 7. 包、导出、升级与发布 | 实施中 | 本地 Package 已支持不可变 Package、精确坐标/SHA、按用户 Installation、私有 Data、只读资源、Module 导入和 Package 依赖闭包的原子安装/反向依赖保护；正在继续实现 Application 启动、Export 调用、权限隔离、恢复/升级与 Repository |

包管理器从当前 MVP 到完整交付的具体顺序和验收矩阵见 [PACKAGE-MANAGER-COMPLETE-PLAN.md](PACKAGE-MANAGER-COMPLETE-PLAN.md)。

## 1. 总体目标

下一阶段要解决四个核心问题：

1. 将过大的源码文件按职责拆开，保持行为不变。
2. 让一个终端会话真正对应一个长期存在的 Praxis Process。
3. 建立以 Object 和 Capability 为基础的内核扩展模块系统。
4. 补齐调度、Effect 恢复、IPC、审计、资源限制和发布恢复能力。

内核继续只保留 Object Store、Process 执行、权限、原子事务、Effect 基础设施和模块加载器。终端、网络及硬件适配能力逐步迁移到模块。

当前阶段验收命令为 `./scripts/check-system`。持久终端测试覆盖 Store 重启、用户隔离、Ctrl+C、同 Process 跨提交调用定义和表达式回显。模块安装、启停、升级保留旧版本、依赖精确锁定、未声明依赖拒绝、源码哈希校验和交互 import 已有端到端用例。完整 `scripts/check-system` 已通过：Clippy、45 项系统端到端测试、编译器/格式测试，以及启动、创建用户和复登验收全部成功。

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

### 完成标准

- 可以安装一个 Praxis Module；声明依赖按 Object ID 精确锁定，未声明的嵌套 import 会失败；导入会校验源码 SHA-256 并创建持久 Module Instance。服务 Object 发布和 Kernel Extension 的 Type 注册仍待实现。
- 模块无权调用未声明的能力。
- 模块安装失败不会留下半注册 Type 或服务。
- 模块升级和 Store 重启后仍使用正确依赖版本；已由 Store 关闭、重新打开后的端到端用例覆盖，且完整系统验收通过。

## 7. 阶段 4：正式调度器与 Process 恢复

### 工作

- 建立持久 Ready Queue、Sleeping Queue 和 Waiting Queue。
- 使用固定时间片公平调度多个 Process。
- 增加 Worker Lease、执行代次和过期提交防护。
- 通过状态变化通知唤醒工作线程，取消固定间隔轮询。
- Terminal Process、后台 Process 和驱动 Process 使用同一调度核心。
- 保留 Process 的暂停、恢复、终止和七天结果保留规则。

### 完成标准

- 一个等待输入或睡眠的 Process 不占用 Worker。
- 崩溃前已提交的执行位置在重启后继续运行。
- 旧 Worker 不能覆盖新 Worker 已提交的状态。
- 大量 Process 不会让单个 Process 永久饥饿。

## 8. 阶段 5：Effect、IPC、Timer 与审计

### Effect

- 状态扩展为 `pending/running/completed/failed/unknown`。
- 支持 `manual`、`retry_idempotent` 和 `query_remote` 恢复策略。
- 外部操作在结果不确定时不能被错误标记为失败或自动重复。

### IPC

- 保留简单 Channel。
- 增加直接消息、Topic、Subscription 和 Broadcast Object。
- 加入容量、顺序、公平性和发送者权限限制。

### Timer

- 在 `time.sleep()`之外加入独立的持久 Timer Object。
- 支持一次性定时器、取消和超时等待。

### Audit

- 用户、模块、权限、跨用户操作、Effect 处理和系统控制写入追加式审计对象。
- 普通用户只能查看自己的记录，`local` 可以查看全局记录。

## 9. 阶段 6：资源限制与安全加固

- Process 步数、内存对象数、消息容量和子对象数量限制。
- Module 安装大小、依赖深度和启动时间限制。
- 网络目标策略、DNS 固定、连接超时和收发上限。
- 登录失败限速、未知用户等时密码验证和密码策略。
- Session 到期、撤销和并发会话限制。
- Secret 内容禁止进入普通 Object、日志、Effect 参数和审计正文。

## 10. 阶段 7：包、导出、升级与发布

### Package

- 不可变 Package Object。
- 精确版本和内容哈希依赖。
- 每用户安装关系、能力声明、私有数据和执行来源。
- 本地包安装完成后再考虑远程市场。

### 导出与恢复

- 从一致性快照导出 Object、Type、Module、用户和权限。
- 导出文件包含端到端哈希和格式版本。
- 支持在新存储中验证并恢复。

### 格式升级

- 持久格式只前向升级。
- 每个升级步骤可重复执行并有恢复测试。
- 不把未知格式当作空系统启动。

### 发布

- 构建源码包和平台运行包。
- 生成 SHA-256、SBOM、变更说明和验收报告。
- 归档解压后验证 Git 历史、构建、首次启动、再次登录和恢复。

## 11. 不加入或暂缓的内容

- 不把 PostgreSQL、Docker 或 systemd 变成内核依赖。
- 不重新建立“万物皆文件”的传统 VFS。
- 不允许模块直接执行任意宿主命令。
- 不在模块系统第一阶段支持不稳定的 Rust 动态库 ABI。
- 不在本地包管理稳定前建设远程包市场。
- 不在持久 Terminal Session 完成前制作 nano 类编辑器。

## 12. 推荐实施顺序

```text
可信基线
  → 拆分 CLI 和硬件适配器
  → 拆分 VM Process/Codec/Reaper
  → 拆分 VM Executor/Builtins
  → 拆分 OMS Persistence/Transaction
  → 拆分 Compiler
  → 持久 Terminal Session
  → core.module 与 Module Registry
  → Terminal/Network/Storage 模块化
  → 正式 Scheduler
  → Effect 恢复、IPC、Timer、Audit
  → 资源限制与安全加固
  → Package、导出、升级和发布
  → Praxis nano 编辑器
```

每个箭头都是独立验收点。前一阶段未通过时，不把后一阶段标记为完成。
