# Ousject Object Management System 实现计划

> 本文保留最初的分阶段设计。当前实际完成状态见 [STATUS.md](./STATUS.md)；后续无损性能工作以 [PERFORMANCE-PLAN.md](./PERFORMANCE-PLAN.md) 为准。

本文档将 [OBJECT-MANAGEMENT.md](./OBJECT-MANAGEMENT.md) 转换为可执行的工程计划。

实现语言：**Rust**。

首要顺序：

```text
正确性
→ 可恢复性
→ 可观测性
→ 性能
→ 跨 Shard 扩展
```

未经测试证明正确的持久化优化不得进入默认路径。

---

## 1. 第一版范围

第一版 OMS 是单机、固定分片、可持久恢复的最小实现。

必须包含：

- 稳定 `ObjectId`；
- Object Header 与不可变 State；
- 内存 Directory；
- 固定数量 Shard；
- Object 创建、读取、修改和 Tombstone；
- Parent 与 `link` 关系；
- Capability 与基础权限检查；
- 单 Shard 乐观事务；
- WAL、Commit Record、Group Commit；
- Checkpoint 与崩溃恢复；
- Process Token Position 与 Object State 共同提交；
- Effect Intent 的持久记录；
- 指标与一致性检查工具。

第一版暂不包含：

- 跨机器运行；
- 动态 Shard 扩缩容；
- 完整跨 Shard 2PC；
- Linux Driver 集成；
- Praxis Compiler；
- TF Token 解释器；
- Remote-backed Object；
- 在线 Schema Migration。

这些功能需要第一版存储和恢复语义稳定后再接入。

---

## 2. 建议的 Rust Workspace

```text
Ousject/
├── Cargo.toml
├── rust-toolchain.toml
├── crates/
│   ├── oms-types/          # ObjectId、版本、错误和公共类型
│   ├── oms-state/          # StateRoot、Segment、COW 状态
│   ├── oms-policy/         # Capability 与权限校验
│   ├── oms-directory/      # ObjectId → Shard 路由
│   ├── oms-shard/          # Object Index、读取和单 Shard 提交
│   ├── oms-transaction/    # 事务构建、ReadSet、WriteSet、冲突检测
│   ├── oms-storage/        # WAL、Record、Flush、Checkpoint
│   ├── oms-recovery/       # 重放、校验与恢复
│   ├── oms-effect/         # Effect Intent 与结果跟踪
│   ├── oms-runtime/        # 对外统一 OMS API
│   └── oms-tools/          # inspect、fsck、benchmark 工具
├── tests/
│   ├── conformance/        # 规范级行为测试
│   ├── crash/              # 故障注入和恢复测试
│   └── integration/        # 跨 crate 集成测试
├── benches/
└── docs/
```

初期可以减少 crate 数量，但模块边界应保留。建议第一轮先创建：

```text
oms-types
oms-storage
oms-shard
oms-runtime
oms-tools
```

等接口稳定后再拆出其他 crate，避免早期被工程结构拖慢。

---

## 3. 核心公共类型

第一批需要冻结语义、但不冻结二进制格式的类型：

```rust
pub struct ObjectId(u128);
pub struct TypeId(u128);
pub struct TransactionId(u128);
pub struct ShardId(u32);
pub struct ObjectVersion(u64);
pub struct DirectoryEpoch(u64);

pub enum LifecycleState {
    Creating,
    Active,
    Suspended,
    Migrating,
    Terminating,
    Tombstoned,
}
```

Object Header 的逻辑字段：

```rust
pub struct ObjectHeader {
    pub id: ObjectId,
    pub type_id: TypeId,
    pub parent_id: Option<ObjectId>,
    pub version: ObjectVersion,
    pub lifecycle: LifecycleState,
    pub state_root: StateRootId,
    pub capability_set: CapabilitySetId,
    pub access_policy: AccessPolicyId,
    pub flags: ObjectFlags,
}
```

错误必须可供调用者做确定处理：

```rust
pub enum OmsError {
    NotFound(ObjectId),
    Conflict { expected: ObjectVersion, actual: ObjectVersion },
    Denied { object: ObjectId, capability: CapabilityId },
    InvalidLifecycle(LifecycleState),
    ValidationFailed(ValidationError),
    TemporarilyUnavailable,
    OutcomeUnknown(TransactionId),
    Storage(StorageError),
    Corruption(CorruptionError),
}
```

严禁在公共 API 中暴露磁盘偏移或内存地址作为 Object 身份。

---

## 4. 对外 API 草案

第一版统一入口：

```rust
pub trait ObjectManager {
    fn create(&self, request: CreateObject) -> Result<CommitResult, OmsError>;
    fn read(&self, request: ReadObject) -> Result<ObjectView, OmsError>;
    fn begin(&self, options: TransactionOptions) -> Transaction;
    fn commit(&self, tx: Transaction) -> Result<CommitResult, OmsError>;
    fn transaction_status(&self, id: TransactionId)
        -> Result<TransactionStatus, OmsError>;
    fn inspect(&self, id: ObjectId) -> Result<ObjectMetadata, OmsError>;
}
```

事务 API 必须显式保留：

- Snapshot/Read Version；
- ReadSet；
- WriteSet；
- Parent 和 link 修改；
- Process Checkpoint；
- Effect Intents；
- 调用者安全上下文。

单字段更新同样通过内部 Transaction，不提供绕过提交路径的写接口。

---

## 5. 阶段计划

## 阶段 0：工程基线

目标：建立能持续验证的 Rust 工程。

任务：

1. 创建 Cargo workspace 和初始 crate。
2. 固定 Rust stable toolchain。
3. 开启严格 lint：`rustfmt`、`clippy`、文档检查。
4. 建立单元、集成和故障测试目录。
5. 定义统一错误处理策略。
6. 配置 CI 命令，但暂不绑定具体托管平台。
7. 建立 benchmark 基线入口。

完成标准：

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

全部通过。

预计产物：工程骨架和空实现，不包含持久化承诺。

---

## 阶段 1：核心类型与纯内存 OMS

目标：验证对象模型，不引入磁盘复杂度。

任务：

1. 实现 ObjectId/TransactionId 生成器。
2. 实现 ObjectHeader、ObjectVersion 和 LifecycleState。
3. 实现固定 Shard 路由。
4. 实现每 Shard 的 Object Index。
5. 实现不可变 ObjectView。
6. 实现 Create、Read、Update、Tombstone。
7. 实现 Expected Version 冲突检测。
8. 实现 Parent 与 ChildIndex 的原子内存更新。
9. 实现持久语义的 `link` 模型。
10. 实现基础 Capability/Policy 检查。

关键测试：

- ObjectId 不因状态更新而变化。
- 旧 ObjectView 在新版本发布后仍可安全读取。
- 并发更新同一版本时只允许一个成功。
- Parent 和 ChildIndex 不出现单边更新。
- Tombstoned Object 不能被普通能力修改。
- 权限失败不产生任何状态变化。

完成标准：

- 规范级单元测试通过；
- Thread Sanitizer 可用环境下无数据竞争；
- 所有公开写入都经过 Transaction。

---

## 阶段 2：State Segment 与 Copy-on-Write

目标：让大型 Object 的小修改不复制整个状态。

任务：

1. 定义 StateRootId 与 SegmentId。
2. 实现不可变 Segment Store。
3. 实现 StateRoot 到 Segment 的索引。
4. 实现 Copy-on-Write 更新。
5. 实现 Segment 校验和。
6. 实现 ReadView 的版本固定。
7. 实现基于 Epoch 的旧版本回收原型。

关键测试：

- 修改一个 Segment 不复制未变化 Segment。
- 旧 ReadView 使用的 Segment 不被提前回收。
- StateRoot 校验失败会返回 Corruption，而不是继续执行。
- 同内容去重不能改变 Object 的版本语义。

性能门槛：

- 常驻只读路径不产生存储 I/O；
- 读取路径不获取全局排他锁；
- 更新成本主要随变化 Segment 数量增长。

---

## 阶段 3：WAL 与单 Shard 原子提交

目标：实现“持久化先于可见性”。

任务：

1. 定义带版本和校验和的 WAL Record Envelope。
2. 定义 Begin、Data、Commit、Abort Record。
3. 实现只追加 WAL Writer。
4. 实现 Durability Barrier 抽象。
5. 实现单 Shard 提交状态机。
6. 只有 Commit Record 持久化后才发布 StateRoot。
7. 实现 Group Commit。
8. 实现 `transaction_status` 索引。
9. 实现日志截断前的安全条件。

提交状态机：

```text
Building
→ Validating
→ Persisting
→ Durable
→ Publishing
→ Committed
```

失败状态：

```text
Building / Validating / Persisting
→ Aborted
```

关键测试：

- Commit Record 持久化前，读取永远只看到旧状态。
- 持久化失败后内存和磁盘都保持旧正式版本。
- Commit Point 后进程崩溃，恢复必须得到新状态。
- 同一 Group Commit 内的事务保留独立结果。
- 重复提交同一 TransactionId 不产生第二次修改。

这是第一版最重要的正确性里程碑。

---

## 阶段 4：Checkpoint 与崩溃恢复

目标：任何故障注入点都恢复到确定状态。

任务：

1. 实现一致 Checkpoint 格式。
2. 实现后台 Checkpoint 创建。
3. 实现 WAL Replay。
4. 重建 Object Index、Published Version 和事务状态。
5. 丢弃无 Commit Record 的 Candidate State。
6. 恢复 Tombstone、Parent、link 与 Policy。
7. 实现校验和、截断记录和损坏记录处理。
8. 实现 `oms-fsck` 只读检查工具。

故障注入点至少包括：

- 写入 Record 前；
- Record 写入一半；
- Data Record 后、Commit Record 前；
- Commit Record 写入一半；
- Commit Record 持久化后、Publish 前；
- Publish 后、响应调用者前；
- Checkpoint 创建中途；
- WAL 截断前后。

完成标准：

- 每个注入点重复运行并恢复后，一致性检查全部通过；
- 已确认 Committed 的事务绝不丢失；
- 未到 Commit Point 的事务绝不作为正式状态出现；
- `OutcomeUnknown` 可通过 TransactionId 查到最终状态。

---

## 阶段 5：Process Checkpoint 共同提交

目标：防止系统恢复后重复执行已经生效的 Token。

任务：

1. 定义最小 ProcessCheckpoint：Program、TokenPosition、CallState Root。
2. 将 ProcessCheckpoint 放入 Transaction WriteSet。
3. Object 修改和下一个 TokenPosition 使用同一个 Commit Record。
4. 定义运行时安全点。
5. 实现恢复后从最近安全点继续。

关键测试：

```text
执行 counter++
在每个提交步骤注入崩溃
恢复并继续运行
最终 counter 只能增加一次
```

完成标准：对象状态和 Process 位置不存在半提交组合。

---

## 阶段 6：Effect Intent

目标：正确描述不可回滚的外部行为。

任务：

1. 定义 EffectId、Intent、Result 与 DeliveryPolicy。
2. 将 Intent 与 Object State 在同一事务中提交。
3. 实现提交后 Effect Worker。
4. 实现幂等键和指数退避重试。
5. 实现不确定结果状态。
6. 提供 Effect 查询和人工处理入口。

关键测试：

- 未提交 Intent 永远不执行。
- Commit 后 Worker 崩溃，恢复后 Intent 仍会被调度。
- 支持幂等的 Effect 重试不会重复产生结果。
- 非幂等设备不会被错误宣称为 exactly-once。

---

## 阶段 7：性能优化与背压

目标：在不改变正确性语义的前提下优化吞吐和延迟。

任务：

1. 增加 Per-Core Route/Handle Cache。
2. 将热点统计改为 Per-Core 汇总。
3. 调优 Group Commit 批次与最大等待时间。
4. 实现有界 Shard/Storage/Effect 队列。
5. 实现背压传播。
6. 实现 Resident Segment Cache 和驱逐。
7. 增加热点 Object 观测。
8. 对声明为可交换的操作实现安全合并。

基准场景：

- 常驻单 Object 读取；
- 随机 Object 读取；
- 独立 Object 并发更新；
- 单热点 Object 冲突更新；
- 小 Transaction 持久提交；
- Group Commit 吞吐；
- 大 Object 单 Segment 更新；
- 冷启动 WAL Replay；
- Checkpoint 期间的读写延迟。

不在本阶段预设绝对性能数字。第一次实现产生基线后，再根据目标硬件制定预算。

性能优化必须通过崩溃测试套件，不能只通过 benchmark。

---

## 阶段 8：跨 Shard 原子提交

目标：提供多 Shard Transaction 的原子性和一致可见性。

开始条件：

- 单 Shard WAL 和恢复测试长期稳定；
- TransactionId 状态查询可靠；
- Shard 本地 Prepared 状态可以持久恢复；
- 已有可观测和故障注入设施。

任务：

1. 实现参与 Shard 的 Prepare Record。
2. 实现稳定 ShardId 排序和写入意图。
3. 实现持久 Commit/Abort Decision Record。
4. 实现协调者故障恢复。
5. 实现 Prepared Transaction 查询和清理。
6. 实现跨 Shard 快照可见性判断。
7. 实现跨 Shard Parent/link 原子变更。

关键故障测试：

- 任一参与者 Prepare 前后崩溃；
- 协调者写 Decision 前后崩溃；
- 部分参与者 Publish 后崩溃；
- 重复、乱序和延迟消息；
- 恢复期间再次崩溃。

Prepared 参与者不得仅根据超时猜测最终决定。

---

## 6. 测试策略

### 6.1 单元测试

覆盖数据结构、状态机、编码、校验和、冲突判断和权限决策。

### 6.2 属性测试

适合验证：

- Record 编解码往返；
- 任意操作序列后 Parent/ChildIndex 一致；
- Object Version 单调；
- 未提交版本不可见；
- Tombstone 不会复活为其他 Object。

### 6.3 模型测试

建立简单、低性能但明显正确的参考模型。随机生成 Create、Read、Update、link、Parent 和 Crash 操作，对比真实实现结果。

### 6.4 并发测试

使用可控调度器探索关键交错：

- Read 与 Publish；
- 两个 Update 的 Validate 与 Commit；
- Tombstone 与能力调用；
- Policy 修改与受保护写入；
- Segment 回收与旧 ReadView。

### 6.5 崩溃测试

存储层的每个逻辑写入点都可注入：

```text
成功
返回错误
短写
内容损坏
进程立即终止
```

崩溃测试是提交功能的验收条件，不是后续补充。

### 6.6 性能测试

性能结果必须记录：

- 硬件与文件系统；
- Durability 模式；
- Object/Segment 大小；
- 并发数；
- p50、p95、p99 延迟；
- 吞吐；
- Flush 次数；
- 冲突和重试率。

---

## 7. 安全与 `unsafe` 规则

OMS 核心第一版默认禁止 `unsafe`。

只有满足以下条件才允许引入：

1. 有明确性能或硬件需求；
2. 安全边界位于独立小模块；
3. 写明 Safety Invariant；
4. 有 Miri、属性测试或等价验证；
5. Code Review 可以单独审查该边界。

WAL、事务状态机和权限检查不应依赖未审查的 `unsafe` 优化。

---

## 8. 持久格式版本策略

第一版记录必须从一开始包含：

```text
Magic
FormatVersion
RecordType
RecordLength
TransactionId
Payload
Checksum
```

规则：

- 未知必需 RecordType：拒绝以读写模式启动。
- 未知可选 RecordType：按格式规则安全跳过。
- 校验失败：停止自动重放并报告确切位置。
- 不使用 Rust 内存布局作为持久格式。
- 发布前统一使用版本 0，允许直接替换格式且不保留旧格式读取；正式发布后的格式升级必须有显式迁移路径。

---

## 9. 可观测性与开发工具

实现期间同步建设：

```text
oms inspect <object-id>
oms transaction <transaction-id>
oms children <object-id> --cursor ...
oms wal dump <path>
oms checkpoint verify <path>
oms fsck <store-path>
oms benchmark <scenario>
```

工具默认只读。任何修复模式必须显式指定目标并先生成备份或新存储副本。

日志中不输出完整敏感 Object State，只记录 ID、版本、事务、时延和错误分类。

---

## 10. 首个开发里程碑

第一个里程碑名称：**M1 — In-Memory Object Core**。

M1 只做阶段 0 和阶段 1，建议按以下顺序提交：

1. `chore: initialize Rust workspace`
2. `feat(types): add stable object and transaction identifiers`
3. `feat(core): add immutable object header and views`
4. `feat(directory): route objects to fixed shards`
5. `feat(transaction): add optimistic single-shard transactions`
6. `feat(relations): transact parent and link updates`
7. `feat(policy): enforce capabilities and access policy`
8. `test(conformance): verify object model invariants`

M1 验收演示：

```text
1. 创建 Process Object
2. 在 Process 下创建 Variable Object counter = 0
3. 建立 displayed link counter
4. 两个并发事务尝试从 Version 0 更新 counter
5. 一个提交成功，另一个返回 Conflict
6. 旧 ReadView 仍显示 0，新 ReadView 显示 1
7. 无权限调用返回 Denied，状态保持 1
8. Tombstone 后普通访问返回 Terminated
```

M1 明确不声称状态可在断电后恢复。持久性承诺从阶段 3 和阶段 4共同完成后才成立。

---

## 11. 开始实现前需要确认的决定

以下选择会影响代码结构，应在写工程骨架时确认：

1. 最低支持的 Rust 版本，以及使用 stable 还是固定版本。
2. 第一版目标平台：建议 `x86_64 Linux` 用户态原型。
3. 第一版存储后端：建议普通文件 + 明确 Flush 接口，后续再接块设备。
4. 异步模型：核心事务状态机建议同步接口起步，I/O 层保留异步替换边界。
5. 持久编码：需要在阶段 3 前选定并写入格式规范。
6. 许可证和仓库贡献规则。

若没有额外限制，建议采用：

```text
Rust stable（通过 rust-toolchain.toml 固定）
x86_64 Linux 用户态原型
本地文件存储
同步核心 + 可替换 I/O trait
小端、显式字段编码的自定义 WAL 格式
```

---

## 12. 当前下一步

获得实现确认后，立即执行 M1 的第一项：

1. 创建 Cargo workspace；
2. 创建 `oms-types`、`oms-shard`、`oms-runtime` 和 `oms-tools`；
3. 配置格式化、lint 和测试命令；
4. 实现 `ObjectId`、`TransactionId`、`ObjectVersion`；
5. 提交第一组类型不变量测试。

在 M1 完成前不开始 WAL；在 WAL 的崩溃测试完成前不宣称 OMS 具有持久原子性。
