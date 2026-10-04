# OMS M1 Quick Start

> 这是早期纯内存 M1 的历史说明，不代表当前系统限制。当前版本已经加入持久 WAL/Checkpoint、跨 Shard 原子提交、Process/Effect 和 Provider；请从 [SYSTEM-QUICKSTART.md](./SYSTEM-QUICKSTART.md) 开始使用。

M1 是 Ousject Object Management System 的第一阶段最小运行版本。

它提供单进程、纯内存、固定分片的 Object 管理核心。它适合验证 Object 模型、事务语义、能力权限和上层 Runtime 集成，不提供断电持久性。

---

## 1. 运行演示

在项目根目录执行：

```bash
./scripts/oms demo
```

演示会自动完成：

1. 创建 Process Object。
2. 创建属于 Process 的 counter Object。
3. 建立 `displayed link counter`。
4. 提交两个基于相同版本的更新。
5. 确认仅第一个更新成功，第二个返回 Conflict。
6. 向另一个 Subject 授予 Read 能力。
7. 使用新 Subject 读取 counter。
8. 运行全 Shard 健康检查。

演示成功时进程返回 `0`。

---

## 2. 交互式 Shell

```bash
./scripts/oms shell
```

Shell 默认使用一个 Shard，以便所有关系操作都能在单 Shard Transaction 中完成。

常用命令：

```text
create <state>
create-child <parent-id> <state>
read <object-id>
inspect <object-id>
update <object-id> <state>
link <source-id> <name> <target-id>
unlink <source-id> <name>
reparent <child-id> <parent-id|none>
tombstone <object-id>
list
stats
health
```

权限命令：

```text
new-subject
whoami
as <subject-id>
grant <object-id> <subject-id> <capability>
revoke <object-id> <subject-id> <capability>
```

Capability 名称：

```text
read
write
link
reparent
tombstone
inspect
manage-policy
```

Shell 中的 Object State 以 UTF-8 文本输入和显示。底层 Runtime 使用任意字节数组。

---

## 3. 多 Shard 模式

可以指定固定 Shard 数量：

```bash
./scripts/oms shell 4
```

M1 支持多个独立 Shard 并行存在，但一个 Transaction 只能修改一个 Shard。

CLI 创建子对象时会为它选择与 Parent 相同的 Shard。跨 Shard `link`、reparent 或联合更新会明确返回：

```text
CrossShardTransaction
```

跨 Shard 原子提交属于后续阶段。

---

## 4. Rust API 示例

```rust
use oms_runtime::{AccessContext, CreateObject, InMemoryObjectManager};
use oms_types::{SubjectId, TypeId};

let manager = InMemoryObjectManager::new(1)?;
let context = AccessContext::new(SubjectId::new());

let request = CreateObject::new(TypeId::new(), b"hello");
let object_id = request.id;

let mut create = manager.begin(context);
create.create(request);
manager.commit(create)?;

let old = manager.read(context, object_id)?;

let mut update = manager.begin(context);
update
    .expect(object_id, old.header().version)
    .update_state(object_id, b"world");
manager.commit(update)?;

assert_eq!(old.state(), b"hello");
assert_eq!(manager.read(context, object_id)?.state(), b"world");
# Ok::<(), oms_types::OmsError>(())
```

旧 `ObjectView` 使用不可变共享状态，因此新版本发布后仍保持原值。

---

## 5. 事务使用规则

修改已有 Object 前必须登记期望版本：

```rust
transaction.expect(object_id, observed_version);
```

如果正式版本已经变化，Commit 返回：

```text
Conflict {
    object,
    expected,
    actual,
}
```

Parent/ChildIndex 同时变化时，所有被修改的现有 Object 都必须登记版本。

候选状态只有在以下检查全部成功后才一次性发布：

- Expected Version；
- Object 生命周期；
- Capability 与 Access Policy；
- Parent/ChildIndex 一致性；
- Parent 循环；
- 单 Shard 边界。

任何一步失败，整个 Transaction 对正式状态不产生修改。

---

## 6. M1 提供的保证

在单个运行进程内：

- ObjectId 在 Object 生命周期中保持稳定。
- 已发布 Object Version 单调递增。
- 读取者只观察完整的旧版本或完整的新版本。
- 同一 Shard Transaction 原子发布。
- 失败 Transaction 不发布部分修改。
- 并发写入使用 Expected Version 检测冲突。
- Parent 与 ChildIndex 原子更新并检查循环。
- `link` 更新通过 Transaction 发布。
- Capability Grant/Revoke 与 Object State 使用相同提交模型。
- Tombstone Object 不再允许普通读取或修改，但可以被授权主体检查。
- `health_check` 可以验证当前关系不变量。

---

## 7. M1 明确不保证

M1 不提供：

- 进程退出后的状态保存；
- WAL 或 Commit Record；
- 断电恢复；
- Process Token Position 共同提交；
- 跨 Shard 原子提交；
- 动态 Shard 迁移；
- 外部 Effect 的可靠调度；
- Tombstone 的物理空间回收；
- 输入 link 在目标 Tombstone 时的自动清理策略。

因此，M1 的“原子提交”是进程内的原子发布，不是持久原子提交。

---

## 8. 验收

运行完整第一阶段验收：

```bash
./scripts/check-m1
```

它依次执行：

1. Rustfmt 格式检查。
2. 全 Workspace Clippy，并将 Warning 视为 Error。
3. 全部单元、集成和文档测试。
4. 可执行 Demo。

全部成功且返回码为 `0`，表示当前 M1 构建通过验收。
