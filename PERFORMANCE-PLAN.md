# Ousject 性能优化计划（不牺牲已提交信息）

## 不可破坏的提交契约

性能优化只能改变实现，不能改变以下语义：

1. 只有 WAL 完整记录写入、校验并 `sync_all` 成功后，提交才可返回成功。
2. 内存新版本只能在持久化成功后发布；失败时正式状态保持原样。
3. Checkpoint 必须先写临时文件、同步文件、原子替换并同步目录，之后才可丢弃已覆盖 WAL。
4. 恢复只接受 Magic、长度和校验和全部正确的完整记录；尾部半条记录直接忽略。
5. Process 执行位置、变量/Object 修改、Link、权限和生命周期变更必须仍在同一事务中。
6. 外部操作必须先持久化 `core.effect` 意图，再调用 Provider；重试必须复用同一个 EffectId。
7. ObjectId 永不复用。当前 Tombstone 保留原状态，不做有损物理回收。

这里的“不丢失”指：**已经向调用者确认成功的 Ousject 提交，在满足存储设备兑现 flush、介质未同时全部损坏的前提下，重启后仍可恢复。** 单盘物理损毁、设备谎报 flush、终端/网络远端不支持幂等，无法由软件声称绝对消除；正式系统要用校验、镜像/复制和远端幂等协议覆盖这些故障。

## 优化顺序

当前首轮可重复基线已记录在 [PERFORMANCE-BASELINE.md](./PERFORMANCE-BASELINE.md)：100 个 4 KiB Object 中只更新一个时，完整快照 Backend 的写放大约为 101.86 倍。

### P0：先建立基线

- 固定对象数量、Value 大小、事务宽度、Shard 数量和并发数的 benchmark。
- 测量提交延迟 p50/p95/p99、fsync 次数、写放大、恢复时间、锁等待和 VM Token/s。
- 每个优化都必须同时跑故障注入与恢复一致性测试；只快但破坏契约的改动不得合入。

### P1：增量 WAL（已完成第一阶段）

- 将当前“每次 WAL 保存完整快照”改为带 TransactionId、前序 LSN、长度和校验和的增量事务记录。
- 一个事务的所有 Shard 修改先写 Prepare 数据，最后写唯一 Commit Decision；没有 Commit Decision 的记录恢复时不可见。
- WAL 只追加，不原地覆盖。

收益：把每次提交的写放大从整个 Store 降为本次变更量。

### P2：Group Commit（显式批量接口已完成）

- 多个互不冲突事务共享一次设备 flush。
- 每个事务仍有独立边界、校验和与结果；flush 失败时这一组都不得发布。
- 提供最大等待时间和最大批量，避免低负载延迟失控。

收益：高并发时显著减少 fsync 次数。

### P3：不可变 Segment / Copy-on-Write（对象级已具备，大 Value 子分段待后续）

- 大 Value 拆为带校验和的不可变 Segment；State Root 只引用 SegmentId。
- 小修改只写新 Segment 和新 Root。
- 引用计数只是缓存，恢复时必须能从 Root 重建，不能因计数损坏误删数据。

收益：降低大型 Object 小修改的复制与写放大。

### P4：Checkpoint 与无损压缩（已完成第一阶段）

- 生成带世代号和全局校验的 Checkpoint；恢复可从最近有效世代加后续 WAL 开始。
- 压缩必须可逆且逐块校验。
- 只有新 Checkpoint 完整验证且目录同步成功后，才可删除被覆盖 WAL。
- Tombstone、Effect 和审计所需记录默认保留；任何保留期限策略必须由用户明确开启。

### P5：缩小锁范围与并行 Shard（参与 Shard 写锁已完成）

- 从当前稳定顺序的全 Shard 写锁，演进为确定的参与 Shard 锁集。
- 跨 Shard 事务使用持久 Commit Decision；协调器崩溃后可由恢复器完成，而不是猜测回滚。
- 保持全局锁顺序，禁止以超时掩盖已形成 Commit Decision 的事务。

### P6：读取和 VM 热路径（Program 解码缓存已完成）

- 版本固定的只读快照、Type/Namespace 索引缓存和解码缓存。
- TF 验证结果缓存、基本块预解码、减少每 Token 的 Process 全量编码。
- 缓存永远可丢弃重建，不成为唯一正式数据。

## 每阶段准入测试

- 任意写入点断电/截断后，只能恢复到旧提交或新提交，不能出现混合状态。
- 随机翻转 WAL、Checkpoint 和 Segment 字节时必须报损坏，不能静默接受。
- 并发冲突只允许一个预期版本获胜。
- 跨 Shard Parent、Link、权限、Process 位置和 Effect 始终一起出现或一起不出现。
- 重放同一事务/Effect 不产生第二份 Object 修改；外部远端若不支持幂等，文档明确可能重复。
- 优化前后对同一提交序列产生相同的可观察 Object 状态。
