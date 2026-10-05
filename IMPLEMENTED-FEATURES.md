# Ousject 0.0.0 已实现功能

本文列出已经进入 VM 或 Provider 执行路径的功能；自动化测试的具体覆盖范围以测试代码为准。退役对象的 7 天自动清理和元数据保留规则见 [GARBAGE-COLLECTION.md](./GARBAGE-COLLECTION.md)。宿主边界见 [PRE-HOST-ACCEPTANCE.md](./PRE-HOST-ACCEPTANCE.md)，性能工作见 [PERFORMANCE-PLAN.md](./PERFORMANCE-PLAN.md)。

## 执行链与 Praxis

```text
Praxis → Compiler → OTF0 → Program Object → Process Object / VM → OMS → WAL + OMS0
```

- 标量、UTF-8 Text、Array、Map、Record、Error。
- 算术、比较、短路布尔、长度、索引读写与后缀增减。
- 可发现的 `core.math` Object：`abs/min/max/clamp`、`sqrt/pow`、`floor/ceil/round/trunc`、三角函数、`atan2`、`hypot`、对数与指数、`random/random_integer`，以及 `pi/e`。
- `if/else if/else`、`while`、`break/continue`。
- `func/return`、持久调用帧、`try/catch`。
- Class 字段/方法、`this`、public/private、单继承、`super.method`、按参数数量重载。
- `object.create("Class", {...})` 统一创建；没有 `new/init` 第二套对象机制。
- `import/include`、显式 `link`、显式多 Object `transaction {}`。
- 普通赋值复制 Value；只有 `link` 显式共享 Object 身份。

数学能力通过 Object 发现和调用：

```praxis
math = object.find("math")
radius = math.sqrt(25)
angle = math.atan2(1, 0)
console = object.find("console")
console.println(math.pi)
```

数学函数只接受有限的整数或浮点数；非法定义域和非有限结果会返回错误。浮点型的舍入函数返回浮点数，整数输入保持整数；`round` 的中点按远离零的方向舍入。

## 常用 API

这些是日常程序最常直接调用的 API：

| 对象 | 能力 | 用途 |
|---|---|---|
| `console` | `print`、`println`、`read_line`、`read_secret`、`size`、`is_interactive` | 输出、读取输入、获取终端尺寸和交互状态 |
| `time` | `now`、`monotonic`、`sleep` | Unix 毫秒时间、单调运行时间、按毫秒等待 |
| `math` | 数学函数、`random`、`random_integer` | 数值计算和随机数；整数随机范围含两端 |
| Text Value | `slice`、`find`、`contains`、`split`、`replace_all`、`trim`、`lower`、`upper` | 直接处理文本；`slice` 的位置按 Unicode 字符计 |
| `resolver` | `resolve` | 使用宿主 DNS 配置把主机名解析为 IP 地址数组 |

```praxis
console = object.find("console")
time = object.find("time")
math = object.find("math")
resolver = object.find("resolver")

console.println("hello")
terminal = console.size() // { columns: ..., rows: ... }
started_ms = time.monotonic()
random_number = math.random()
random_dice = math.random_integer(1, 6)
clean_name = "  Praxis  ".trim().lower()
addresses = resolver.resolve("example.com")
```

`time.sleep(milliseconds)` 会持久保存唤醒时间并挂起当前 Process，不会阻塞 VM 工作线程。`console.size()` 在没有可查询的终端尺寸时返回错误；`is_interactive()` 可用于判断当前终端是否支持交互输入/输出。

## 统一 Object 系统

基础 API：

```text
object.create  object.find    object.query
name.id        name.type      name.parent
name.status    name.inspect   name.capabilities
name.owner     name.version   name.permissions
name.value     name.replace   name.children
name.links     name.link      name.unlink
name.grant     name.revoke    name.retire
```

`object` 只负责创建、发现和查询。Process 可通过 `process.value` 查看完整的可读运行状态，通过 `process.variables` 查看变量值，并用 `process.x` 直接读取变量 `x`。系统程序需要传递原始变量绑定时使用 `process.bindings()`。

- 内置与持久动态 Type Descriptor；Schema、创建策略、基础权限和领域能力校验。
- `core.namespace` 的 `bind/resolve/unbind`，命名任意 Object，不需要核心 File 类型。
- `core.collection` 可保存 Array 或 Map，但不提供集合专属 API；索引读写、长度和整体替换与普通变量一致。
- `core.program.execute([entry])` 创建 Process。
- `core.process` 的 `start/wait/suspend/resume/terminate/bindings`，以及可读运行属性、变量、`result` 和 `error`。`wait()` 返回最终状态；子 Process 失败时得到 `"failed"`，错误内容可从 `process.error` 读取。
- `core.channel` 的 `send/receive/wait`；消息长度用 `#channel`；消息、挂起和唤醒原子提交。
- `retire` 原子清理对象树、变量别名和显式 Link，并留下不可复用 Tombstone。

## Provider 与设备

- 通用 `ObjectProvider` 注册与能力分发。
- `core.effect` 先持久记录请求，再执行 Provider，再原子记录结果和推进 Process。
- Console `print/println/read_line/read_secret/size/is_interactive`；`print` 不添加换行，适合提示符；无输入时仅挂起调用 Process，并以同一持久 Effect 轮询恢复。
- `core.time` 的 `now/monotonic/sleep`；sleep 记录持久化唤醒时间并挂起 Process，不阻塞 VM 工作线程。
- Display `present/configure`；Keyboard `capture/release/next_event/poll_event` 返回结构化文本、方向键、功能键和修饰键事件。Console 按行/秘密输入和 Keyboard 共用单一、互斥的终端输入源。
- Block Storage `load_block/store_block` 仅保留为内核/驱动层接口，不再授予普通 Praxis 用户。
- 结束的 Process 及其结果保留七天后由内核自动退役；随后对象内容按墓碑的七天策略清理，Program 不会因 Process 结束而被删除。
- 物理设备由启动时发现并发布；普通 Praxis 不能伪造 Provider-only Object。

Console/Display/Network 等外部世界无法仅靠本地事务保证跨整机崩溃 exactly-once。Effect 意图不会丢；同一启动内重试复用 EffectId。重启后无远端幂等协议的操作采用 at-least-once，可能重复。

## Process、IPC、用户与权限

- Process 状态持久保存 Subject、Token 位置、栈、变量绑定、调用帧、异常帧和状态。
- Ousject VM 与协作式 Scheduler 不创建 Linux 子进程。
- 子 Process 继承 Subject；调度时始终恢复该 Subject，不以系统身份偷跑。
- 显式 Link 和 Channel IPC；跨 Subject 唤醒由内核验证等待登记后原子完成。
- `core.user` 使用随机 salt 与 PBKDF2-HMAC-SHA256 密码验证信息。
- `core.session` 只保存 Token 的 SHA-256 hash、Subject 和过期时间；明文 Token 只在登录时显示一次。
- 最高身份固定为真实的 `local` User（Subject `...0001`），只能初始化一次。
- Praxis 可通过 Authentication/User Registry Object 初始化 `local`、创建/禁用用户、登录、改密码和退出；登录会原子迁移当前 Process 身份与所需访问权。
- 用户、Session、权限迁移和当前调用指令的 Process 状态使用同一次 OMS 提交；持久化失败不会留下半次登录。
- `console.read_secret()` 在交互终端关闭回显，且只把启动期不透明句柄写入 Process/Effect/WAL。
- Rust 开发/恢复工具没有隐式最高权限：正常操作需要 `--session`，显式 `--local` 才使用恢复权限。
- owner、Grant/Revoke 和九种基础 Capability 均由 OMS 强制执行。

## 原子性与恢复

- 固定 Shard 路由；跨 Shard 事务按稳定顺序持锁、验证完整候选、持久化后一次发布。
- Process 下一位置与 Object 创建/修改、Link、权限和生命周期变更共同提交。
- 文件 WAL 使用带长度、基线校验和与记录校验和的增量变更；`sync_all` 是提交持久点，损坏或不匹配的基线不会静默恢复。
- 每 64 次提交批量生成原子 Checkpoint，减少每次提交重复写快照和同步目录；Checkpoint 失败时仍从 WAL 恢复。
- `commit_batch` 让多个事务共享一次持久写入/flush；批内任一失败则全部不发布。
- WAL 增量可逐记录做可逆 RLE 压缩；外层长度与校验和覆盖压缩数据，翻转字节会拒绝恢复。
- 退役时间随 OMS 状态原子持久化；后台内核服务在满 7 天后回收 Tombstone 内容，永久保留 ObjectId、TypeId、所有者、最终版本、退役状态和退役时间。没有新增手动清理命令。
- 到期清理以完整快照和校验代际清单原子切换；崩溃恢复只能看到清理前或清理后的完整状态。旧格式 Tombstone 首次打开时从升级时开始计算 7 天，并安全持久化迁移时间。
- 普通事务只对参与 Shard 获取写锁，其他 Shard 保持一致读锁；退役/重挂因涉及旧 Parent 仍保守写锁全部。
- VM 按 Program Object 版本缓存已验证/解码的 TF，版本变化自动失效。
- 同一 Store 单写者锁；干净退出释放，Linux 宿主可识别并回收死进程遗留锁。
- 恢复时验证 Parent/Child、Type 索引、动态 Type Descriptor 和格式完整性。
- Tombstone 内容满 7 天后自动回收，最小元数据和 ObjectId 永久保留；未决 Effect 不会被回收。

## 内核对象与 Praxis 用户空间

- Process 可发现 `system/authentication/users/scheduler/compiler/types/providers/store` 服务对象。
- Praxis 可以查询系统/Store 健康、管理用户与 Process、编译并运行源码、查看/注册 Type、查看 Provider/设备/Effect，并触发安全 Checkpoint。
- `system/*.px` 包含 `init`、首次 `local` 设置、登录、Shell 和管理程序。
- `system-install` 把这些源码编译成 Program Object，并原子安装到 `system` Namespace；`boot` 之后只恢复 `init`，启动控制流由 Praxis 完成。

## 开发/恢复 CLI

```text
system-install, boot
compile, run, run-tf, resume, schedule, tf-dump
inspect, list, check, types, type-register
object-create, object-value, object-query
object-bind, object-unbind, object-resolve
object-grant, object-revoke
```

默认 Store 是 `.ousject/objects.oms`；`--memory` 明确关闭 Object Store 持久化。该 CLI 是宿主引导和恢复工具，不是正式系统 Shell。全部格式仍为预发布版本 0。

## 验证

```bash
./scripts/check-system
```
