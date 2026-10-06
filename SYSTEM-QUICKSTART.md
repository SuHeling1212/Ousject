# Ousject System MVP Quick Start

正式的最小启动路径先把 Praxis 用户程序安装成 Object，然后启动 `init`：

```bash
./scripts/ousject system-install system --local
./scripts/ousject boot
```

首次 `boot` 会依次运行 `local-setup.px`、`login.px` 和 `shell.px`；以后启动会跳过 `local` 设置。下面的 `run ... --local` 是开发/恢复方式，不是正式用户 Shell。

这是 Ousject 第一阶段的系统级最小运行版本。当前先由 Linux 启动应用并提供硬件适配接口；Ousject 的系统语义和 Process 执行不使用 Linux 进程模型。

当前宿主提供的底层能力包括：

- 启动 `ousject` 应用程序；
- 发现 Console、终端显示/键盘、DNS Resolver 和块存储适配端点；时间通过 `core.time` 服务提供，而不是伪装成 Device；
- 提供 TCP Socket 的 Network Provider；
- 通过 `FileSnapshotBackend` 提供文件 I/O 和持久化；
- 提供宿主时钟，以及 Rust 标准库所依赖的内存分配和同步机制。

Praxis、TF、Token VM、Process、Value Object、Scheduler、OMS、权限和原子提交全部由 Ousject 自己定义。持久层通过 `SnapshotBackend` 隔离。Console 必须由硬件适配器发现并注册；单有持久化对象不能执行打印能力。

---

## 1. 运行完整示例

完整 `.px` 程序必须包含一个无参数 `func main()`。入口文件顶层只放函数、Class 和源码导入声明；执行语句写进 `main()`。被导入源码不能声明 `main()`，其顶层初始化会先于入口 `main()` 执行。交互式 Shell 输入不受此入口规则限制。

```bash
./scripts/ousject run examples/hello.px --local
```

默认 Object Store：

```text
.ousject/objects.oms
```

示例输出：

```text
Ousject running
Ousject running
Ousject running
3
```

命令还会输出 Process ObjectId。Program、Process 和 Variable 状态已经写入 Object Store。

---

## 2. 编译和执行 TF

```bash
./scripts/ousject compile examples/hello.px hello.tf
./scripts/ousject tf-dump hello.tf
./scripts/ousject run-tf hello.tf --local
```

发布前所有格式保持版本 0：程序为 OTF0，Process 状态为 OPS0，Value 为 OVL0，Object Store 快照为 OMS0。系统只读取版本 0；其他 Magic、损坏、截断、未知 Token 和越界 Jump都会被拒绝。

---

## 3. 暂停与恢复

通过较小的 Token 预算让 Process 保持 Running：

```bash
./scripts/ousject run examples/hello.px --steps 5 --local
```

命令输出：

```text
process=<PROCESS_ID> status=Running steps=5
```

重新启动命令并继续：

```bash
./scripts/ousject resume <PROCESS_ID> --local
```

VM 从持久化的 Token Position、Stack 和 Variable Objects 继续，而不是重新执行程序。

---

## 4. 检查系统对象

```bash
./scripts/ousject list --local
./scripts/ousject inspect <OBJECT_ID> --local
./scripts/ousject check --local
```

`check` 验证 Parent/ChildIndex、生命周期和 Object Store 基本不变量。

使用其他 Object Store：

```bash
./scripts/ousject run program.px --state /path/to/objects.oms --local
```

临时禁用持久化：

```bash
./scripts/ousject run program.px --memory --local
```

---

## 5. 当前 Praxis 可执行子集

### 5.1 统一 Object API（CLI 和 Praxis）

列出已经注册的 Object Type：

```bash
./scripts/ousject types --local
```

选择类型并创建一个持久 Object：

```bash
object_id=$(./scripts/ousject object-create core.text "hello object" --local)
./scripts/ousject object-value "$object_id" --local
./scripts/ousject object-query core.text --local
```

创建无 File 的命名空间并绑定任意 Object：

```bash
namespace_id=$(./scripts/ousject object-create core.namespace '{}' --local)
./scripts/ousject object-bind "$namespace_id" README "$object_id" --local
./scripts/ousject object-resolve "$namespace_id" README --local
```

`README` 是 Namespace 中指向 Text Object 的命名 Link，不是传统 File。可以用同一种方式命名 Collection、Program、Process 或其他 Object。

Type Registry 会验证 Value Schema，并从 Type Descriptor 安装能力。硬件 Device 不能通过普通类型化创建入口伪造；`net.endpoint` 由允许用户创建的 Network Provider 初始化。`core.process` 只能由 VM 按受控入口配置创建。Console、Process、Program、Collection、Namespace、Channel、Network 和宿主设备能力均已接入。

Praxis 中也可直接操作对象：

```praxis
x = 46
note = object.create("core.value", 42)
aaa = object.find("console")
aaa.println(x + 1)
aaa.println(note + 1)
aaa.println(x.type)
aaa.println(note.type)
aaa.println("hello from console")
note.retire()
```

完整示例：`./scripts/ousject run examples/objects.px`。两种赋值都会让变量名绑定一个持久 Object，普通表达式自动读取其 Value。`x.id` 显式取得对象 ID；`b = x` 复制值并创建独立对象，不建立 Link。对象先通过 `object.create/find/query` 创建或发现并绑定名字，之后读取 `名字.属性` 或调用 `名字.能力(...)`。

`note.retire()` 会原子退役该 Object 及其子 Object，同时移除所有变量别名和显式 Link；ObjectId 的 Tombstone 保留且永不复用。事务失败时什么都不会改变。

### 5.2 Praxis 语法

字面量：

```praxis
123
3.14
"text"
"中文 UTF-8"
true
false
null
[1, 2, 3]
{ name: "Ada", age: 18 }
```

变量和算术：

```praxis
count = 1
count++
count--
value = (count + 2) * 3
remainder = value % 2
items[0] = 10
items[0]++
user["age"] = 19
length = #items
```

比较和布尔否定：

```praxis
count == 3
count != 3
count < 10
count <= 10
count > 0
count >= 0
not false
enabled and count > 0
enabled || count > 0
```

控制流：

```praxis
while count < 3 {
    count++
    if count == 1 {
        continue
    }
    if count == 2 {
        break
    }
}

console = object.find("console")
if count == 3 {
    console.println("ok")
} else {
    console.println("unexpected")
}
```

函数、Class 与统一创建：

```praxis
func twice(value) {
    return value * 2
}

class Counter {
    value = 0

    func add(amount) {
        this.value = this.value + amount
        return this.value
    }
}

counter = object.create("Counter", { value: twice(5) })
console.println(counter.add(2))
```

显式事务与错误处理：

```praxis
transaction {
    source.balance = source.balance - 10
    target.balance = target.balance + 10
}

try {
    value = 1 / 0
} catch (error) {
    console.println(error)
}
```

创建子 Process 并通过显式 Link 共享对象：

```praxis
func worker() {
    channel = object.find("channel")
    channel.value++
}

child = object.create("core.process", {
    entry: "worker",
    links: { channel: channel.id }
})
child.start()
child.wait()
```

Class、Function、Process、Program、Collection、Namespace、Channel、Network、事务、异常和模块语法已经执行。设备能力当前连接宿主适配器，正式裸机驱动属于下一阶段。完整状态见 `PRAXIS-IMPLEMENTATION-STATUS.md`。

### 5.3 用户与权限管理

正常路径在 Praxis Shell 或系统程序中管理用户，不再提供 Rust 版登录/用户命令：

```praxis
users = object.find("users")
alice = users.create_user("alice", "a strong password")
console.println(users.users())
```

登录由 `system/login.px` 完成。成功后当前 Process 的 Subject 和所需对象权限原子迁移到登录身份；Session Object 只保存 Token hash：

```praxis
authentication = object.find("authentication")
session = authentication.login("alice", "a strong password")
authentication.change_password("alice", "a new password")
authentication.logout(session["token"])
```

新 Subject 默认不能读取其他用户的 Object；`local` 或对象 owner 可以按能力授权和撤销。下面的命令只属于显式开发/恢复模式：

```bash
./scripts/ousject object-grant <OBJECT_ID> <SUBJECT_ID> view_value --local
./scripts/ousject object-revoke <OBJECT_ID> <SUBJECT_ID> view_value --local
```

Praxis 内的授权代码使用 `item.grant(...)` 和 `item.revoke(...)`；OMS 以 Process 持久 Subject 执行所有访问检查。

---

## 6. 系统执行模型

完整流程与宿主边界见 [PROCESS-RUNTIME.md](./PROCESS-RUNTIME.md)。

每个程序创建：

```text
Program Object ←─ "program" Link ─ Process Object
Console Object ←─ "console" Link ─┤
                                   └── Value Objects
```

Program 与 Process 不是父子关系；Process 用命名 Link 指向 Program 和本次启动发现的共享 Console。变量名绑定的 Value Object 是 Process 的子 Object。

VM 的 `Store` Token 将以下内容放在一个 OMS Transaction 中：

```text
Value Object 新状态
Process Token Position
Process Stack
Process Variable Name Map
```

候选快照首先交给 `SnapshotBackend`。只有 Backend 确认原子替换成功后，OMS 才发布新的内存版本。

读取者不会观察到候选状态或半完成 Parent/Value 更新。

---

## 7. 当前保证

- Praxis 到 TF 到 VM 的完整执行链。
- Program、Process、Console 和变量值都是 Object。
- 固定多 Shard Transaction 原子性。
- 持久化先于内存可见性。
- 原子文件替换和文件/目录同步。
- Object Store 重启恢复和损坏检测。
- Token Position 与 Value Object 修改共同提交。
- Expected Version 并发冲突检测。
- 固定步数执行与跨进程恢复。
- 多 Process 协作式 Scheduler 核心。
- Capability Grant/Revoke 和 Object 生命周期检查。
- Type Registry、统一 Value、按类型创建和索引查询。
- Namespace Object、原子命名 Link 和路径解析；内部不依赖核心 File 类型。
- 宿主持久层可替换为内核 Backend。

---

## 8. 当前限制

- 当前在宿主进程中运行，尚不能裸机启动；迁移需要 Boot、内存、中断、同步、网络栈和正式驱动。
- WAL 已校验且可恢复，但当前每条记录仍包含完整候选快照；增量 WAL、Group Commit 和 Segment COW 属于性能优化。
- 同一 Object Store 有单写者锁，不允许两个 Ousject 实例同时写。
- 外部 Effect 先持久化；无远端幂等协议时整机崩溃后的交付是 at-least-once，可能重复。
- Scheduler 是协作式；抢占、优先级、多核和定时器属于脱离宿主阶段。
- Network/Device 当前使用 Linux API 适配器，尚不是正式硬件驱动。
- 当前实现的是本文语法规范，不声称具有 Python/Java 的完整标准库、反射或调试器。

这些限制是后续阶段的实现范围，不会被描述为已经完成。

---

## 9. 完整验收

```bash
./scripts/check-system
```

验收会运行：

1. Rustfmt。
2. 严格 Clippy，Warning 视为 Error。
3. 全 Workspace 单元、并发、故障注入、恢复和端到端测试。
4. Praxis 示例编译。
5. TF 执行与持久 Object Store 检查。
