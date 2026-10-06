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

2026-10-06 优化后基准，在当前云端容器、相同参数下运行。基准程序位置：[benchmark.rs](./crates/oms-runtime/examples/benchmark.rs)。

OMS 内存 SnapshotBackend 基准（它故意测量完整快照兼容后端，不代表文件 WAL 后端）：

```text
commits=1000
state_bytes=4096
resident_objects=100
debug_commits_per_second=4825.12
debug_commit_p50_ns=176228
debug_commit_p95_ns=286966
debug_commit_p99_ns=449502
release_commits_per_second=33059.35
release_commit_p50_ns=26721
release_commit_p95_ns=40551
release_commit_p99_ns=102776
backend_writes=1000
backend_snapshot_bytes=418012000
write_amplification=102.05
```

此兼容后端每次都接收完整快照，所以 102 倍写放大是预期值，不代表持久化文件后端。

文件 WAL 基准（100 个常驻对象，每个 4096 bytes；先显式建立 Checkpoint，再做 1000 次同步更新）：

```text
debug_durable_commits_per_second=557.47
debug_durable_commit_p50_ns=1697784
debug_durable_commit_p95_ns=2178173
debug_durable_commit_p99_ns=3631997
release_durable_commits_per_second=794.69
release_durable_commit_p50_ns=1240459
release_durable_commit_p95_ns=1336014
release_durable_commit_p99_ns=1586554
checkpoint_bytes=267552
wal_bytes_after_1000_commits=29601
```

Debug 与 Release 的磁盘数据来自同一云端环境，并非不同物理硬盘对比；操作系统页缓存、虚拟磁盘和容器调度会影响 `sync_data` 时间。WAL 在超过 Checkpoint 的 25% 后会触发后台快照，所以表中 WAL 字节数是最后一次自动 Checkpoint 之后的剩余量，不是 1000 条记录的总写入量。同步切换阶段造成的锁等待会反映在 p99；需再用并发和大对象测量确认其上限。用 4096-byte 状态反复写相似数据时，WAL 的无损压缩效果明显，不能把该字节数直接外推到随机数据。

VM 合成热循环（同一 10000 次迭代、约 90007 Token）：

```text
debug_elapsed_ms=79
debug_tokens_per_second=1132036.28
release_iterations=100000
release_tokens=900007
release_elapsed_ms=94
release_tokens_per_second=9500779.91
```

优化前记录为 Debug 约 21,162 Token/s；新的 Debug 测量约 1.13M Token/s。Release 数据来自合成 VM 循环，不能代表包含编译、I/O、Provider 和终端渲染的真实命令速度。

运行命令：

```bash
./scripts/cargo-local run --quiet -p ousject-vm --example vm_benchmark -- 10000
./scripts/cargo-local run --release --quiet -p ousject-vm --example vm_benchmark -- 100000
./scripts/cargo-local run --release --quiet -p oms-runtime --example benchmark -- 1000 4096 100
```

包含后台 Checkpoint 25% 阈值的最终代码通过 `cargo fmt --all -- --check`、`cargo check --workspace` 和 `cargo test --workspace -- --test-threads=1`。OMS 41 项、VM 57 项和其余工作区测试全部通过；VM 端到端共 46 项，其中终端测试通过 `main()` 入口执行完整程序，并按 Object Store 中的 Terminal Session 对象验证状态。其余结果不能替代真实目标硬件、100 万对象、跨 Shard 并发和断电故障注入测试。
