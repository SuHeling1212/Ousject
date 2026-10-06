# Ousject Object Management System

**Object Management System（OMS）是 Ousject 中统一管理全部 Object 的系统服务。**

OMS 负责回答四个基础问题：

```text
这个 Object 是谁？
这个 Object 属于谁？
这个 Object 当前在哪里？
当前调用者可以对它做什么？
```

OMS 同时负责 Object 的创建、定位、访问、修改、迁移、持久化、恢复和销毁。

本文档描述 OMS 的逻辑模型与实现边界。具体磁盘格式、Token 编码和设备协议由独立规范定义。

Object、Value、Type、Process、Network、Device 和传统 File 的统一语义由 [UNIFIED-OBJECT-MODEL.md](./UNIFIED-OBJECT-MODEL.md) 定义。OMS 负责实现该模型的身份、关系、权限、事务、持久化和恢复部分。

---

## 1. 设计目标

OMS 必须满足：

1. 系统中的 Process、Variable、Program、Network、Device、Collection 和 Data 都使用统一管理模型。
2. Object 身份不依赖 RAM 地址、磁盘位置、设备编号或网络地址。
3. Object 移动、换页或恢复时，调用者使用的身份不变。
4. Object 的状态、隶属关系、能力、权限和生命周期可以统一查询。
5. 持久 Object 的修改只有在原子提交成功后才可见。
6. Process 执行状态可以和它产生的 Object 修改共同提交。
7. 单个热点 Object 不应阻塞整个系统。
8. 查找常驻 Object 不应产生磁盘 I/O。
9. 不使用一个必须全局加锁的巨大对象表。
10. 系统突然断电后能够恢复到最后一个完整提交状态。

OMS 的目标不是让所有 Object 永远驻留在 RAM，也不是让所有操作都经过一个中央线程。

> **OMS 提供统一语义，但采用分布式、分片化的执行结构。**

---

## 2. Object 的统一描述

每个 Object 在逻辑上具有：

```text
Object
├── ObjectId
├── TypeId
├── ParentId / Belonging
├── Value / State
├── CapabilitySet
├── AccessPolicy
├── Lifecycle
├── Version
├── ProviderId
└── Residency
```

其中：

- `ObjectId`：对象的永久逻辑身份。
- `TypeId`：对象类型及其状态布局版本。
- `ParentId`：对象的直接隶属对象；根对象没有 Parent。
- `Value / State`：由该类型定义的正式内容；小值可以内联，大值可以通过不可变 Segment 表示。
- `CapabilitySet`：对象能够执行的能力。
- `AccessPolicy`：哪些主体可调用哪些能力。
- `Lifecycle`：创建、活动、暂停、迁移、终止、回收等状态。
- `Version`：每次成功提交后递增的逻辑版本。
- `ProviderId`：可选；标识实现 Process、Network 或 Device 等领域能力的 Provider Object。
- `Residency`：当前副本所在位置，仅供 OMS 内部使用。

`Residency` 不是 Object 身份的一部分。

---

## 3. ObjectId

每个 Object 创建时获得不可变的 `ObjectId`。

建议使用 128 位标识：

```text
ObjectId = Node/Domain Prefix + Time/Sequence + Randomness
```

要求：

- 无需中央锁即可并行生成。
- 在系统可接受的生命周期内不重复。
- 不编码 RAM 地址或磁盘块位置。
- 不因迁移、压缩、重启或父对象变化而改变。
- 已回收的 ObjectId 不重新分配给新 Object。

源码中的变量名不是 ObjectId。名字由所属 Object 的 Name Map 解析：

```text
Process Object
└── Name Map
    └── "counter" → ObjectId
```

Praxis 的 `link` 同样保存稳定的 Object 关系，而不是保存地址。

---

## 4. OMS 的逻辑组成

OMS 由以下逻辑组件组成：

```text
Object Management System
├── Object Directory
├── Object Shards
├── Residency Manager
├── Transaction Manager
├── Capability & Policy Engine
├── Lifecycle Manager
├── Persistence Engine
├── Effect Manager
└── Recovery Manager
```

这些组件是职责边界，不要求对应独立 Process。

### 4.1 Object Directory

Object Directory 将 `ObjectId` 解析到负责它的 Object Shard。

Directory 只保存轻量路由信息，不保存完整 Object State：

```text
ObjectId Prefix / Hash Range
    → ShardId
    → Epoch
    → Route
```

### 4.2 Object Shard

Object Shard 管理一部分 Object 的元数据、版本和提交顺序。

默认分片方式：

```text
ShardId = stable_hash(ObjectId) mod ShardCount
```

实现可以使用一致性哈希或范围分片，以支持动态扩缩容。

Shard 内部可以进一步按 CPU Core 建立执行队列，避免共享锁争用。

### 4.3 Residency Manager

Residency Manager 管理 Object 的物理驻留：

```text
Not Resident
Metadata Resident
Partially Resident
Fully Resident
Pinned
Device-backed
Remote-backed
```

它负责加载、驱逐、预取、压缩和迁移，但不能改变 ObjectId 和正式版本。

### 4.4 Transaction Manager

Transaction Manager 负责读集合、写集合、冲突检测、提交、发布和失败回滚。

### 4.5 Capability & Policy Engine

它验证：

```text
Object 是否具有该能力
调用者是否被授权调用该能力
授权是否仍在有效期内
能力调用是否满足当前生命周期状态
```

### 4.6 Lifecycle Manager

它管理 Object 创建、父子关系变更、暂停、终止和安全回收。

### 4.7 Effect Manager

它管理无法由本地事务回滚的外部副作用，例如网络发送和设备动作。

---

## 5. Object Directory 与查找路径

标准查找流程：

```text
ObjectId
   ↓
Per-Core Route Cache
   ↓ miss
Shard Directory
   ↓
Object Shard
   ↓
Per-Shard Object Index
   ↓
Resident Object / Persistent Locator
```

### 5.1 快速路径

常驻 Object 的目标快速路径为：

```text
Object Handle
→ 校验 Generation / Version
→ 直接访问只读快照或提交入口
```

快速路径不进行磁盘访问，不获取全局锁。

### 5.2 Object Handle

Process 可以缓存短期 `ObjectHandle`：

```text
ObjectHandle
├── ObjectId
├── ShardId
├── DirectoryEpoch
├── Generation
└── AccessToken / Capability Context
```

Handle 不是 Pointer，也不能直接成为持久格式。

当分片迁移或 Object 被驱逐时，旧 Handle 可失效；系统通过 ObjectId 重新解析。

### 5.3 缓存失效

Directory 使用 `Epoch` 标识路由代次。Shard 使用 `Generation` 标识运行时实例代次。

```text
Handle.Epoch != Directory.Epoch
    → 重新解析路由

Handle.Generation != Shard.Generation
    → 丢弃本地 Handle
```

因此迁移不需要扫描并修改所有调用者。

---

## 6. Object 元数据与状态分离

为降低访问成本，OMS 将小型元数据与大型状态分离。

### 6.1 Object Header

常驻 Header 建议包含：

```text
ObjectId
TypeId
ParentId
Version
LifecycleState
StateRoot
CapabilitySetId
AccessPolicyId
Flags
```

### 6.2 State Segments

大型 Object State 划分为不可变 Segment：

```text
StateRoot
├── Segment A
├── Segment B
└── Segment C
```

修改采用 Copy-on-Write：只生成发生变化的 Segment 和新的 StateRoot。

优点：

- 读者可以无锁读取旧版本。
- 小修改不复制整个大型 Object。
- 快照和事务回滚成本低。
- 未变化 Segment 可以安全共享和去重。

---

## 7. 读取模型

OMS 默认提供版本化快照读取。

读取者取得：

```text
ReadView
├── ObjectId
├── Version
└── Immutable StateRoot
```

已发布版本不可原地修改，因此读者不需要持有长期排他锁。

普通读取流程：

1. 解析 ObjectId。
2. 检查访问权限。
3. 取得当前已发布的 `StateRoot + Version`。
4. 如有必要加载相关 Segment。
5. 返回稳定 ReadView。

系统可以支持：

- `read_committed`：读取调用时已提交的最新状态。
- `snapshot`：同一事务中的所有读取基于一致快照。

事务默认使用 Snapshot Isolation，并对写入对象执行版本冲突检测。

对需要严格串行化的能力，可以要求 Serializable 模式或对象级顺序执行。

---

## 8. 修改模型

任何正式修改都必须在 Transaction 中发生。单个字段修改由系统隐式包装为单对象 Transaction。

```text
Published State
      ↓ read
Candidate State
      ↓ validate
Durable Commit
      ↓ publish
New Published State
```

事务包含：

```text
Transaction
├── TransactionId
├── SnapshotVersion
├── ReadSet
├── WriteSet
├── Parent/Link Changes
├── Process State Update
├── Effect Intents
└── Commit Metadata
```

候选状态在提交前对普通读取者不可见。

---

## 9. 单 Shard 原子提交

当事务的所有写入 Object 均由同一个 Shard 管理时，采用单 Shard 提交。

### 9.1 提交流程

```text
1. Build
   生成候选 StateRoot 和元数据变更

2. Validate
   校验权限、生命周期、类型约束和 Expected Version

3. Reserve
   为 WriteSet 分配 Shard 内提交序号

4. Persist
   写入新 Segment、Transaction Record 和 Commit Record

5. Durability Barrier
   确认 Commit Record 已达到要求的持久级别

6. Publish
   原子替换 Object 的 Published StateRoot / Version

7. Notify
   使缓存失效并唤醒等待者
```

发布动作必须短小且不可阻塞。磁盘 I/O 在发布前完成。

### 9.2 Commit Point

事务的唯一 Commit Point 是持久 `Commit Record` 成功通过 Durability Barrier 的时刻。

在此之前失败：

```text
旧状态保持正式有效
候选状态可以被回收
```

在此之后崩溃：

```text
Recovery Manager 必须发布或重建新状态
```

不能存在“内存已发布但持久提交未知”的正式状态。

---

## 10. 跨 Shard 原子提交

跨 Shard Transaction 不能依靠一个全局锁。

OMS 使用带持久化决定记录的协调协议。基础实现可以采用优化后的 Two-Phase Commit；未来可替换为共识驱动的提交协议，但语义不变。

### 10.1 Prepare

协调者按稳定顺序联系参与 Shard：

```text
排序键 = ShardId
```

每个参与 Shard：

1. 校验 Expected Version、权限和约束。
2. 为写集合建立短期写入意图。
3. 持久写入 `Prepared(TransactionId, Digest)`。
4. 返回 Prepared。

稳定排序和短期意图降低死锁风险。超时只能中止尚未形成全局 Commit Decision 的事务。

### 10.2 Decide

全部参与者 Prepared 后，协调者持久写入唯一决定：

```text
CommitDecision(TransactionId, ParticipantSet, Digest)
```

该记录是跨 Shard 事务的 Commit Point。

如果任一参与者 Prepare 失败，协调者写入 Abort Decision。

### 10.3 Publish

参与 Shard 收到 Commit Decision 后：

1. 持久记录本地 Commit。
2. 原子发布新版本。
3. 释放写入意图。
4. 返回完成状态。

发布可以在不同 Shard 略有时间差。需要跨 Shard 一致快照的读者使用 TransactionId/Commit Timestamp 判断版本可见性，不观察半提交结果。

### 10.4 协调者故障

协调者本身不保存唯一的临时真相。参与者在恢复后通过持久 Decision Record 查询最终决定。

Prepared 状态不能因超时自行猜测 Commit 或 Abort。

系统可复制 Decision Record，避免单一存储故障长期阻塞 Prepared Transaction。

---

## 11. Process 与 Object 共同提交

执行 Praxis Token 时，事务可以同时包含：

```text
Object 修改
Name Map / link 修改
Process Token Position
Process Call State
Process Variable State
等待或调度状态
```

例如 `counter++` 的提交结果必须是：

```text
counter.Version = N + 1
Process.TokenPosition = next
```

两者同时生效或都不生效。

恢复后不能出现 Object 已经修改，但 Process 再次执行同一 Token 的状态。

对于长时间计算，Process 可在安全点生成 Checkpoint Transaction。安全点之间的临时寄存器和工作缓存不要求逐指令持久化。

---

## 12. 隶属关系与 link

### 12.1 Parent 关系

Parent 变更属于事务写入，必须同时维护：

```text
Child.ParentId
OldParent.ChildIndex
NewParent.ChildIndex
```

三者必须原子更新。

OMS 必须阻止非法循环：

```text
A belongs to B
B belongs to A
```

对于深层树，循环检测使用祖先摘要、层级编号或异步验证索引加速，但最终提交前必须得到确定结果。

### 12.2 link 关系

`link` 是持久的显式关系：

```text
Link
├── Source Object / Name
├── Target ObjectId
├── Link Kind
└── Policy
```

创建、修改和删除 link 都通过 Transaction 完成。

Object 迁移不会影响 link。目标被终止时，根据 Policy：

- 阻止终止；
- 将 link 标记为失效；
- 级联处理；
- 转移到替代 Object。

---

## 13. 生命周期与回收

推荐生命周期：

```text
Creating
→ Active
→ Suspended / Migrating
→ Terminating
→ Tombstoned
→ Payload Reclaimed after 7 days
→ Minimal metadata retained
```

### 13.1 创建

Object 只有在创建 Transaction 提交后才可被普通调用者发现。

创建事务可以同时写入：

- 初始 State；
- Parent 关系；
- CapabilitySet；
- AccessPolicy；
- 创建者 Process 的变量或 link。

### 13.2 终止

终止通过 `对象名.retire()` 产生 Tombstone，不提供额外的宿主命令。清理不是立刻发生：对象内容会保留 7 天，再由 OMS 后台服务自动回收。

清理后永久保留的最小 Tombstone 元数据：

```text
ObjectId
TypeId
Owner
FinalVersion
RetiredAt
Lifecycle = Tombstoned
```

### 13.3 七天后的自动回收

OMS 后台服务在系统运行期间等待最早的到期 Tombstone；没有待到期对象时不扫描对象表。对象库打开时会重建到期索引；系统关闭期间错过的清理在下次启动时补做。

到期后，系统原子地删除 Tombstone 的原始 State、Links 和多余授权，再保留以上元数据。ObjectId 仍被占用，不能再次创建。

Pending、结果不明或无法安全识别状态的 Effect 不可退役，也不会被回收。清理期间 OMS 暂时锁住全部 Shard，并以代际快照切换保证崩溃后只能恢复到完整的清理前或清理后状态。

时限按系统墙钟计算；宿主时钟大幅前调可能导致提前到期。生产系统需要可信硬件时钟。

---

## 14. 能力与权限

每次能力调用逻辑上执行：

```text
resolve(ObjectId)
→ object supports CapabilityId?
→ caller authorized?
→ lifecycle permits invocation?
→ execute
```

为提高性能，Policy Engine 可以签发短期 Capability Token：

```text
CapabilityToken
├── SubjectId
├── ObjectId / Scope
├── CapabilityId
├── PolicyVersion
├── Expiry
└── Integrity Tag
```

Token 只缓存授权结论，不能扩大权限。以下情况强制重新验证：

- PolicyVersion 改变；
- Token 过期；
- Object 生命周期变化；
- 调用跨越安全域；
- 能力被撤销。

安全敏感能力不允许仅依赖无限期缓存。

---

## 15. 外部副作用

外部副作用不能被普通状态事务假装回滚。

OMS 使用 Effect Intent：

```text
Transaction
├── Object State Changes
└── EffectIntent
    ├── EffectId
    ├── Target
    ├── Payload Digest
    ├── Idempotency Key
    └── Delivery Policy
```

执行顺序：

```text
持久提交 Object State + EffectIntent
                ↓
Effect Manager 执行外部动作
                ↓
持久记录 Effect Result
```

外部协议支持 Idempotency Key 时，Effect Manager 使用稳定 `EffectId` 重试。

不支持幂等的设备必须明确声明交付语义，例如：

- at-most-once；
- at-least-once；
- 需要人工确认的不确定状态。

OMS 不承诺无法由现实设备提供的 exactly-once 行为。

---

## 16. 持久格式与恢复

持久层至少保存：

```text
Object State Segments
Object Metadata Records
Transaction Records
Prepare / Decision / Commit Records
Effect Intents and Results
Checkpoints
```

记录包含长度、类型、TransactionId、校验和与格式版本。

### 16.1 Write-Ahead 原则

任何 Published State 指针变化之前，足以恢复该变化的记录必须先持久化。

### 16.2 Checkpoint

系统周期性生成一致 Checkpoint：

```text
Checkpoint
├── Last Included Commit
├── Shard Object Index Roots
├── Directory Epoch
└── Integrity Metadata
```

Checkpoint 采用后台 Copy-on-Write，不长时间暂停正常事务。

### 16.3 启动恢复

恢复步骤：

1. 加载最近有效 Checkpoint。
2. 校验记录完整性。
3. 重放 Checkpoint 后的 Commit Decision。
4. 丢弃没有 Commit Decision 的候选状态。
5. 恢复 Prepared Transaction 并查询最终决定。
6. 重建 Published StateRoot 和版本索引。
7. 恢复 Process 安全点。
8. 重新调度未完成 Effect Intent。

损坏记录不能被解释为成功提交。

---

## 17. 性能设计

### 17.1 无全局热点

以下操作不得要求系统级全局锁：

- ObjectId 分配；
- 普通 Object 查找；
- 单 Shard 读取和提交；
- 缓存命中时的能力校验；
- 状态 Segment 加载和驱逐。

全局行为通过 Epoch、分片协议或后台协调完成。

### 17.2 Per-Core 数据结构

每个 CPU Core 可以拥有：

- Route Cache；
- Handle Cache；
- Transaction Builder；
- 提交批次队列；
- 内存分配 Arena；
- 访问统计缓冲区。

统计数据批量汇总，避免每次访问写同一个计数器。

### 17.3 Group Commit

多个独立 Transaction 可以共享一次 Durability Barrier：

```text
Tx A ─┐
Tx B ─┼→ Append Records → One Flush → 独立发布
Tx C ─┘
```

Group Commit 降低存储同步成本，但每个事务仍保留独立成功或失败结果。

系统应设置最大批次等待时间，避免低负载时延迟过高。

### 17.4 分层缓存

```text
L1: Per-Core Handle / Route Cache
L2: Shard Metadata Cache
L3: Resident State Segment Cache
L4: Persistent Store
```

驱逐优先考虑：

- 访问频率和最近访问时间；
- 重新加载成本；
- Object 优先级；
- 未提交事务引用；
- Process 即将运行所需状态。

脏的候选状态不能被当作正式版本驱逐或发布。

### 17.5 大 Object

大 Object 必须按 Segment 操作，并支持范围读取、增量校验和增量持久化。

禁止因为修改一个字段而复制或锁定整个大型 Object。

### 17.6 热点 Object

对高竞争 Object 可采用：

- Shard 内顺序执行；
- 乐观并发控制和有限重试；
- 可交换操作合并，例如计数累加；
- 将无共享部分拆为子 Object；
- 对只读状态发布不可变副本。

OMS 不自动改变应用语义。只有被类型声明为可交换的操作才能合并或重排。

### 17.7 背压

当持久存储、Shard 队列或 Effect 队列饱和时，OMS 必须施加背压，而不是无限增长内存队列。

背压可以传播到 Process 调度器，使相关 Process 等待。

---

## 18. 并发控制与冲突

默认采用乐观并发控制：

```text
读取 Object Version N
生成 Candidate State
提交时要求 Current Version == N
```

不相等时返回 Conflict，调用者或 Runtime 决定是否重试。

适合悲观锁的场景：

- 冲突率长期很高；
- 能力执行成本很高，重试代价不可接受；
- 设备操作需要排他访问。

锁和写入意图必须：

- 有明确所有者 TransactionId；
- 有超时监测，但不能破坏已提交决定；
- 按稳定顺序获取；
- 不在等待磁盘 Flush 时持有全局结构锁。

---

## 19. Object 类型接口

每种 Object Type 向 OMS 注册：

```text
TypeDescriptor
├── TypeId
├── SchemaVersion
├── State Validator
├── Capability Descriptors
├── Serialization / Segment Codec
├── Migration Functions
├── Conflict Policy
├── Lifecycle Hooks
└── Effect Declarations
```

Type 实现不能绕过 Transaction 直接修改 Published State。

Schema Migration 生成新候选版本，并通过普通原子提交发布。迁移失败时旧版本保持有效。

---

## 20. 观测与审计

OMS 应提供以下只读观测能力：

```text
inspect(ObjectId)
list_children(ObjectId, cursor)
locate(ObjectId)
history(ObjectId, cursor)
transaction_status(TransactionId)
effect_status(EffectId)
```

列举接口必须分页，禁止一次复制整个系统 Object 列表。

关键指标包括：

- 查找缓存命中率；
- 读取与提交延迟分位数；
- Group Commit 批次大小；
- 事务冲突率与重试率；
- Prepared Transaction 数量和持续时间；
- Shard 队列深度；
- Resident State 大小和驱逐率；
- 恢复时间；
- Effect 重试和不确定结果数量。

审计记录本身采用追加和分段归档，不应成为所有普通读取的同步依赖。

---

## 21. 故障语义

OMS 对调用者返回明确结果：

```text
Committed
Aborted(reason)
Conflict(current_version)
Denied(capability)
NotFound
Terminated
TemporarilyUnavailable
OutcomeUnknown(transaction_id)
```

`OutcomeUnknown` 只表示调用者当前没有取得结果，不表示事务未提交。调用者必须使用 `transaction_status` 查询，不能盲目重复非幂等操作。

系统故障不能把未完成候选状态暴露为已提交状态。

---

## 22. 一致性约束

OMS 必须始终维持：

1. 一个 ObjectId 在同一逻辑时刻只对应一个正式 Object。
2. Published Version 必须来自已持久提交的 Transaction。
3. Object Version 单调递增。
4. Object 的 Parent 关系和 Parent 的 ChildIndex 一致。
5. 已提交 link 在迁移、重启和驱逐后保持语义不变。
6. 权限变更和受其保护的敏感状态可以在同一事务中提交。
7. Process 安全点不能领先于它依赖的 Object 提交。
8. 未提交 State 不得被普通读取者观察。
9. 外部 Effect 必须能够关联到持久 Effect Intent。
10. 已确认 Committed 的状态在突然断电后必须可恢复。

---

## 23. 建议的首个实现范围

第一阶段应实现最小但完整的正确性闭环：

1. 单机 ObjectId 与固定数量 Shard。
2. 内存 Object Directory 和持久 Shard Index。
3. 不可变 StateRoot 与 Copy-on-Write Segment。
4. 单 Shard 乐观事务。
5. Write-Ahead Log、Commit Record 与 Group Commit。
6. Checkpoint 和突然断电恢复测试。
7. Process Token Position 与 Object State 共同提交。
8. Parent、link、Capability 与 Policy 的事务化更新。
9. 基础 Effect Intent 和幂等重试。
10. 指标、事务状态查询和一致性检查工具。

第二阶段再加入：

- 动态 Shard 迁移；
- 跨 Shard 原子提交；
- 多设备复制；
- Remote-backed Object；
- 更复杂的分层缓存和预取；
- 在线 Schema Migration。

正确的恢复和提交语义必须先于分布式扩展。

---

## 24. 核心模型总结

OMS 的统一访问路径是：

```text
ObjectId
   ↓
Directory / Cache
   ↓
Object Shard
   ↓
Capability + Permission Check
   ↓
Immutable Published Version
```

OMS 的修改路径是：

```text
Read Published Version
   ↓
Build Candidate State
   ↓
Validate Version + Policy
   ↓
Persist Transaction
   ↓
Durable Commit Point
   ↓
Atomic Publish
   ↓
Notify / Reclaim Old Version
```

整个系统遵循：

> **统一身份、分片管理、无锁读取、事务修改、持久化先于可见性。**

OMS 统一管理所有 Object，但不会把整个系统压缩成一个中央瓶颈。统一的是 Object 语义、事务边界与恢复规则；具体执行通过 Shard、Per-Core Cache、不可变版本、Copy-on-Write 和 Group Commit 并行完成。

## Hosted Core 当前新增对象

当前实现另注册了 `core.swap_pool`、`core.timer`、`core.audit` 和 `core.audit_event`。它们都使用普通 OMS Object、Version、Link、Capability、事务和 WAL；没有地址映射或第二套共享状态系统。

- SwapPool membership 是 pool 上的 `member:<name>` Link。Attach/detach 不改变 member Object 的 Parent、ObjectId 或生命周期。对 member 的读取/修改仍先检查该 Object 的 Capability，并用其 ObjectVersion 做冲突检测。
- Timer 的状态和 deadline 持久化。到期状态、等待登记、Process WaitReason 和唤醒在一个 OMS Transaction 中修改；提交失败后 Timer 仍为 Armed，恢复可重试。
- Channel 消息、收发、等待登记和 Process 状态都是同一组普通 Object 事务。单消息最多 1 MiB，队列最多 1024 条/8 MiB。
- Process 执行片的 Process Object 与写入 Object 一起提交。Worker lease 的 owner/generation/deadline 持久保存，恢复会推进 generation，旧 Worker 后续写回将因版本/代次不一致而失败。
- Effect 意图先于外部调用持久化。`manual` 中断恢复为 `unknown`；`retry_idempotent` 使用相同 Effect ID 恢复。OMS 本地提交无法单独保证远端服务 exactly-once。
- Audit root 和事件对象只由内核写入，事件创建和被审计动作位于同一 OMS Transaction。事件详情不超过 4 KiB；当前没有普通 Subject 可用的全局 Audit 读取 API。

以上实现依赖当前 FileSnapshotBackend 的成功持久写入，不扩展为断电缓存、远端 exactly-once 或宿主硬件可靠性承诺。
