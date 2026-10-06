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
- 完整可执行源码必须声明一个无参数 `main()`；入口顶层只允许声明，系统在导入源码的顶层初始化完成后自动调用 `main()`。交互提交不要求 `main()`。
- `import/include` 导入 Praxis 源码；被导入源码禁止声明 `main()`，`import` 只展开一次，`include` 每次展开；显式 `link`、显式多 Object `transaction {}`。
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

## 持久终端会话

Praxis Shell 通过 `terminal.open()` 找到当前用户的终端会话。会话对象固定连接一个 `core.process` 和一个 `core.program`；同一用户重新进入或系统重启后会复用这些对象，不会每行新建 Process。

| Terminal Session 能力 | 用途 |
|---|---|
| `process()` | 返回这个会话一直使用的 Process ID |
| `submit(source)` | 编译并提交一段 Praxis，变量存在同一个 Process 中 |
| `history()` | 最近 100 次提交 |
| `pending_input()` / `save_input(text)` | 恢复括号未闭合时的多行输入 |
| `update_size(columns, rows)` | 持久保存终端尺寸 |
| `cancel()` | 停止当前执行段、丢弃未完成输入，并保留会话与已提交变量 |
| `close()` | 关闭会话、终止其长期 Process、卸载该会话的 Module Instance，并退役会话 Program |

输入裸表达式会回显结果；函数和 Class 跨提交保留，后续同名定义会覆盖旧定义。每条已执行 Token 的对象变化仍按内核事务提交。交互 Program 会回收旧命令 Token，只留下当前代码和仍有效的最新函数/Class 定义，避免每输入一行就永久累积整段历史源码。

交互终端执行 `process.wait()` 时，Linux 终端适配器监听原始 Ctrl+C 键；它只中断当前终端提交，Process 仍可继续接收下一段代码。已经提交的 Token 不回滚。Shell 提示符下的 Ctrl+C 会清除尚未提交的多行输入。

交互式 `import "模块名"` 会从已启用的 Module Object 读取 Praxis 源码，并与当前提交一起编译。模块可在安装时锁定依赖 Module 的 Object ID；加载时按锁定 ID 取源码，所以依赖模块升级不会改变已有模块引用的版本。每个版本同时保存源码 SHA-256，导入时会重新计算并拒绝哈希不匹配的源码。每个 Terminal Session 对每个被导入模块及其依赖会持久创建一个 `core.module_instance` 记录；重复导入不会重复创建，重启后可查询恢复。模块声明的能力仍是登记信息，执行隔离和能力强制授权尚未完成。

## Praxis Module Registry

```praxis
modules = object.find("modules")
module_id = modules.install("greeter", "0.1.0", "func greeting() { return \"hello\" }", ["console.println"])
modules.enable(module_id)
installed = modules.modules()
```

`modules.install(name, version, source, capabilities)` 仅允许 `local`，也可追加第五个参数 `dependencies`，传入依赖 Module 的 Object ID 数组。安装会校验依赖、按锁定版本展开并编译源码，再用一次 OMS 原子事务创建 `core.module` 和其子 `core.program`；校验、编译或提交失败不会留下半个模块。模块名唯一、源码最多 1 MiB，安装后的代码可由 Terminal Session 的 `import` 使用。模块源码内的 `import` 只能引用其已声明的依赖；最外层终端可以导入任一已启用模块。

| Module Registry API | 用途 |
|---|---|
| `modules.install(name, version, source, capabilities[, dependencies])` | 安装并编译 Praxis 源码，可锁定依赖版本 |
| `modules.modules()` | 列出当前用户可见的 Module 版本 Object ID，包括 superseded 历史版本 |
| `modules.instances(module_id)` | 仅 `local` 可用；列出加载该 Module 的 Terminal Session 实例 Object ID |
| `modules.enable(module_id)` | 允许 Terminal Session 按名 import 此模块 |
| `modules.disable(module_id)` | 阻止之后的新 import；已经导入的定义留在会话中 |
| `modules.uninstall(module_id)` | 仅 `local` 可用；依赖版本或活动实例仍引用它时拒绝卸载 |
| `modules.upgrade(module_id, version, source, capabilities[, dependencies])` | 原子建立新版 Module 和 Program，返回新版 ID，并保留旧版内容；省略依赖参数时沿用旧版本锁定的依赖 |
| `modules.rollback(current_id, target_id)` | 原子切回同名的旧版本，不删除任一版本 |

`enable/disable`、安装、升级和回滚仅允许 `local`。升级保留旧版 Module 和 Program 内容，将其标记为 `superseded`，再以同一 OMS 事务创建新版；旧版 ID、版本和源码仍可检查。回滚会在单个事务中切换当前版本状态，两个版本的源码和 Program 都保留。依赖 Object ID 会随 Module 持久保存；当依赖升级时，依赖仍按原 ID 解析至保留的旧版。源码 SHA-256 在安装和升级时写入，并在每次模块导入时验证。真实持久 Store 关闭后重新打开，已启用 Module、其 Program 与锁定依赖都能恢复并执行。Module Instance 目前是会话加载关系记录，不是独立的 Process 或隔离运行时。`modules.uninstall(id)` 会拒绝卸载仍被任何安装版本依赖或仍被活动 Terminal Session 加载的 Module；依赖/实例对象 Links 会和 Module 安装/Terminal 导入同时原子提交，因此并发卸载会与导入冲突，不能卸载正在被装载的版本。通过检查后，在一个 OMS 事务中退役 Module 与其 Program，内容按系统 7 天保留规则处理。`session.close()` 会原子关闭会话并将活动 Module Instance 标为 unloaded，清除反向索引，因此关闭会话后不再永久阻止 Module 卸载。清理旧历史版本以及将声明能力限制到专用 Subject 还未实现。Module 源码导入后以调用者自己的 Process 身份运行；声明能力列表目前只做记录，不授予额外权限。模块不会获得宿主命令或 Rust 动态库权限。

## Local Package MVP

`packages.build(spec)` 仅允许 `local`，会从 Module Object 读取经过 SHA 校验的源码，并自动打入这些 Module 锁定的依赖；源码不需要写在 PX 字符串里。`modules.find(name, version)` 可发现精确版本的 Module。Application 入口接收 `compiler.compile(source)` 返回的 Program Object；编译器会把原始源码保存为 Program 的 source Link，Builder 会重新编译并比对 Program。`kind` 可按有没有入口自动判断。资源 Object 会被快照进 Package；声明的 Export 会检查目标模块和函数是否存在。

Package 会保存不可变 Manifest 和编译结果；`packages.verify(id)` 检查 Manifest 哈希、模块/入口源码哈希，并重新编译源码核对 Program。Registry Link 按内容哈希和精确坐标索引，已有坐标不能被其他内容覆盖。`packages.install(id)` 为当前 Subject 建立独立 Installation 索引和每个包内的 `core.package_module` Object；Package 本身全局只存一份。安装对象、模块、首次用户索引与对应 Links 在一个 OMS 事务中提交。`packages.list()`、`packages.require(coordinate)`、`packages.recover()`、`installation.info()`、`verify()`、`module(name)`、`data()` 和 `uninstall()` 管理安装生命周期。Terminal Session 可使用 `import "package/namespace/name/version/module"` 导入已安装 Module，包内短名称导入只解析同包 Module；导入代码在当前用户 Process 权限下执行。活动 Session 仍加载包模块时卸载会被拒绝，关闭 Session 后可卸载。`installation.data()` 原子创建此用户专有的 `core.package_data` Value Object，可用普通 `replace()` 修改；`data_info/export/import/clear` 提供显式备份和迁移，单包配额默认 8 MiB 且可由 `local` 收紧。不同用户和不同版本不会共享这份数据。退役安装会一并退役其 Module 与 Data Object，内容依照统一的 7 天策略保留；卸载根包时，不再被引用的隐式依赖会同一事务自动退役。

PX 可使用 `crypto.sha256(value)`：Text 按 UTF-8、Bytes 按原始字节、其他 Value 按规范 TF 编码计算。它是通用工具；Package 的哈希仍由内核自行计算。

当前 Package API 支持本地构建、校验、Bytes 导入/导出、按用户安装、私有 Data Object、只读资源、包内 Library Module 导入、精确 Package 依赖、Application Process 启动、Export 调用、版本升级/回滚和七天内恢复。依赖用坐标与 SHA-256 锁定；安装先核验完整闭包，再用单一事务创建所有缺失安装、模块索引和正反依赖 Links；卸载会拒绝仍有已安装依赖者或仍在运行的 Application。`installation.run(arguments, grants)` 会原子创建绑定固定 Package/SHA 的 Process、Package Instance、独立 Subject、参数、资源和 Data 入口；能力必须同时由 Manifest 声明并由调用者明确批准，内核会按具体方法逐次检查授权，例如 `console.println` 不包含 `console.read_line`。没有按应用隔离的 Provider 前，内核拒绝向 Package 授予原始磁盘和共享网络 Endpoint。Manifest Export 可作为 Installation 的方法调用，内核创建绑定该版本 Program 的 Process 并以调用者身份运行。`installation.upgrade(package_id)` 安装新版本并切换默认坐标；`rollback()` 只切换回已安装版本，不覆盖其数据。`packages.restore(retired_id)` 在 OMS 七天 payload 保留期内重建安装并恢复 Data，返回新的 Installation ID。Package 变更会在同一事务中建立不可变的内部 `core.package_audit`。新增的 Market API 可读取 CilExec Market v1 索引、搜索、按 SHA 续传下载，并对 Ousject Package 原子安装依赖闭包；下载最多 15 MiB。CilExec 当前市场发布的是 SQLite/FCL 包，和 Ousject TF/Praxis Package 不兼容，下载可用但安装会明确拒绝。构建和依赖闭包均有数量、深度、源码及编译展开上限，Package 成功校验缓存按 Package/Registry 版本失效并限制为 1024 项。

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

- Process 可发现 `system/authentication/users/scheduler/compiler/types/providers/store/modules` 服务对象。
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
