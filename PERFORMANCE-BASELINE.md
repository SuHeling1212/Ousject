# Ousject 性能基线

首次基线日期：2026-10-04。该数字用于比较代码改动，不是最终硬件性能承诺。

运行命令：

```bash
./scripts/cargo-local run --quiet -p oms-runtime --example benchmark -- 1000 4096 100
```

场景：Store 中常驻 100 个 4096-byte Object，连续 1000 次替换其中一个 Object 的状态；计量 Backend 统计 OMS 每次要求持久化的字节数，不包含真实磁盘 fsync 延迟。

```text
commits=1000
state_bytes=4096
resident_objects=100
elapsed_ms=716
commits_per_second=1395.17
commit_p50_ns=669022
commit_p95_ns=908645
commit_p99_ns=1122388
backend_writes=1000
logical_changed_bytes=4096000
backend_snapshot_bytes=417208000
write_amplification=101.86
object=（本次增量更新的 ObjectId）
checkpoint_bytes=267016
incremental_wal_bytes=154
```

结论：OMS 的通用 SnapshotBackend 边界仍收到完整候选状态，因此内存计量值仍为 101.86 倍；文件 Backend 已把相邻状态编码成带基线校验和的增量 WAL，并对记录做可逆 RLE 压缩。当前重复字节场景中，267016-byte Checkpoint 上的一次约 4 KiB 更新只新增 154-byte WAL。真实非重复数据的大小取决于可压缩性，但不会大于未压缩增量记录。

已实施：增量 WAL、逐记录无损压缩、每 64 次提交的原子 Checkpoint、显式安全 Checkpoint、批量事务共享一次持久写入的 Group Commit、仅对参与 Shard 获取写锁，以及按 Program Object 版本失效的 VM 解码缓存。所有优化仍遵循“WAL 同步成功后才发布内存状态”。

基准程序位置：[benchmark.rs](./crates/oms-runtime/examples/benchmark.rs)。后续每个性能阶段都应使用相同参数重跑，并另行增加真实块设备 fsync、并发、跨 Shard、恢复时间和 VM Token/s 基线。

VM 热路径基线（Debug 构建、10000 次循环）：

```text
iterations=10000
tokens=90007
elapsed_ms=4253
tokens_per_second=21162.51
```

运行命令：

```bash
./scripts/cargo-local run --quiet -p ousject-vm --example vm_benchmark -- 10000
```

这些数值来自当前容器和 Debug 配置，只用于同环境回归比较；不是正式硬件或 Release 构建性能承诺。
