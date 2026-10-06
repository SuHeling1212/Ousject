# Ousject Praxis API 参考

本文记录当前代码中已经实现或已经声明的 Praxis Object API。Rust 内部接口和开发/恢复 CLI 不属于本文范围。

基本使用方式始终相同：

```praxis
对象 = object.create(...)
对象 = object.find(...)
对象列表 = object.query(...)
结果 = 对象.能力(...)
```

## 1. Object Registry

`object` 是系统预置的 Object Registry 对象。

| API | 返回 | 用途 |
|---|---|---|
| `object.create(type, value)` | 新对象 | 创建指定类型的对象 |
| `object.create(type, value, parent)` | 新对象 | 创建指定类型的子对象 |
| `object.find(id_or_name)` | 对象 | 按 ObjectId 或当前 Process 中的名字发现对象 |
| `object.query(type)` | ObjectId 数组 | 查询当前用户可见的指定类型对象 |
| `object.query(type, capability)` | ObjectId 数组 | 查询提供指定能力的对象 |

示例：

```praxis
note = object.create("core.text", "hello")
console = object.find("console")
display_ids = object.query("device.display", "present")
display = object.find(display_ids[0])
```

## 2. 所有对象的统一属性

| 属性 | 内容 |
|---|---|
| `obj.id` | 稳定 ObjectId |
| `obj.type` | Type 名称 |
| `obj.parent` | 父对象 ID；没有时为 `null` |
| `obj.status` | 对象生命周期；Process 返回运行状态 |
| `obj.owner` | 所有者 SubjectId |
| `obj.version` | 当前 Object 版本 |
| `obj.permissions` | 所有者和授权表；需要管理权限 |
| `obj.inspect` | ID、类型、父对象、版本和状态 |
| `obj.capabilities` | 对象公布的基础能力和领域能力 |
| `obj.value` | 对象的本质内容 |
| `obj.children` | 子对象 ID 数组 |
| `obj.links` | Link 名称到 ObjectId 的 Map |

Process 还提供 `variables`、`program`、`user`、`subject`、`position`、`result` 和 `error` 属性，并允许通过 `process.变量名` 读取变量。

## 3. 所有对象的统一能力

| API | 用途 |
|---|---|
| `obj.replace(value)` | 原子替换对象内容 |
| `obj.link(name, target)` | 建立命名 Link |
| `obj.unlink(name)` | 删除命名 Link |
| `obj.grant(subject, capability)` | 授予基础权限 |
| `obj.revoke(subject, capability)` | 撤销基础权限 |
| `obj.retire()` | 原子退役对象及其子对象，并清理名字和 Link |

退役对象的内容保留 7 天，之后由内核后台服务自动清理。ObjectId 和最小元数据永久保留。

可授权的基础权限：

| 权限 | 含义 |
|---|---|
| `view_value` | 查看对象内容 |
| `replace_value` | 替换对象内容 |
| `create_child` | 创建子对象 |
| `invoke` | 调用对象能力 |
| `link` | 管理 Link |
| `reparent` | 修改父对象 |
| `retire` | 退役对象 |
| `inspect` | 查看元数据 |
| `manage_policy` | 管理授权策略 |

## 4. Console

```praxis
console = object.find("console")
```

| API | 用途 |
|---|---|
| `console.print(value)` | 可靠输出；记录 Effect 并等待完成 |
| `console.println(value)` | 输出一行 |
| `console.render(frame)` | 临时画面输出，不记 Effect；只能用于可重画的终端界面 |
| `console.read_line()` | 读取一行文本 |
| `console.read_secret()` | 读取不回显的秘密文本 |
| `console.size()` | 返回 `{columns, rows}` |
| `console.is_interactive()` | 判断是否连接交互终端 |

```praxis
terminal = object.find("terminal")
session = object.find(terminal.open())
process = object.find(session.process())
session.close()
```

`terminal.open()` 返回当前用户可复用的 Terminal Session ID。Session 的提交继续在同一个 Process 中运行，直到显式关闭。

| Terminal Session API | 用途 |
|---|---|
| `session.process()` | 获取此会话长期使用的 Process |
| `session.submit(source)` | 编译并提交一段 Praxis，提交状态与对象变化一起保存 |
| `session.history()` | 查看最近提交 |
| `session.pending_input()` / `session.save_input(text)` | 保存和恢复多行输入 |
| `session.cancel()` | 中断当前提交，保留会话与此前变量 |
| `session.close()` | 关闭会话，结束其 Process 并卸载该会话的 Module |

## 5. Time

```praxis
time = object.find("time")
```

| API | 用途 |
|---|---|
| `time.now()` | 返回 Unix 毫秒时间 |
| `time.monotonic()` | 返回本次启动后的单调毫秒时间 |
| `time.sleep(milliseconds)` | 让当前 Process 挂起指定时长，到期后由调度器恢复 |

睡眠截止时间、内部持久 Timer 和 Process WaitReason 一起保存；等待时不占用执行线程，其他 Process 可以继续运行。Timer 用系统 wall clock 存 deadline；重启恢复时会唤醒已到期的 Process。

## 6. Math

```praxis
math = object.find("math")
```

| API | 用途 |
|---|---|
| `math.abs(x)` | 绝对值 |
| `math.min(a, b)` | 较小值 |
| `math.max(a, b)` | 较大值 |
| `math.clamp(x, min, max)` | 把数值限制在范围内 |
| `math.sqrt(x)` | 平方根 |
| `math.pow(base, exponent)` | 幂 |
| `math.floor(x)` | 向下取整 |
| `math.ceil(x)` | 向上取整 |
| `math.round(x)` | 四舍五入 |
| `math.trunc(x)` | 截断小数 |
| `math.sin(x)`、`cos(x)`、`tan(x)` | 三角函数 |
| `math.atan2(y, x)` | 二参数反正切 |
| `math.hypot(x, y)` | 斜边长度 |
| `math.log(x)` | 自然对数 |
| `math.log2(x)` | 以 2 为底的对数 |
| `math.log10(x)` | 以 10 为底的对数 |
| `math.exp(x)` | 指数函数 |
| `math.random()` | `[0, 1)` 随机浮点数 |
| `math.random_integer(min, max)` | 包含两端的随机整数 |
| `math.pi` | π |
| `math.e` | 自然常数 e |

## 7. Crypto

```praxis
crypto = object.find("crypto")
digest = crypto.sha256("hello")
```

`crypto.sha256(value)` 返回由 64 个十六进制字符组成的 SHA-256。Text 按 UTF-8 字节计算，Bytes 按原始字节计算，其他 Value 按规范 TF 编码计算。Package 内核仍自行计算和验证哈希，不信任调用者传入的摘要。

## 8. Text 与文本 Value

| API | 用途 |
|---|---|
| `text.slice(start, end)` | 按 Unicode 字符截取 |
| `text.find(needle)` | 查找文本位置 |
| `text.contains(needle)` | 判断是否包含文本 |
| `text.split(separator)` | 分割为数组 |
| `text.replace_all(from, to)` | 替换全部匹配项 |
| `text.trim()` | 删除两侧空白 |
| `text.lower()` | 转换为小写 |
| `text.upper()` | 转换为大写 |

## 9. Program

Program 由编译器产生，普通程序不能伪造 Provider-only Program。

| API | 用途 |
|---|---|
| `program.execute()` | 创建 Process |
| `program.execute(bindings)` | 使用变量对象绑定创建 Process |
| `program.execute(entry)` | 从指定入口创建并启动 Process |

## 10. Process

| API 或属性 | 用途 |
|---|---|
| `process.start()` | 开始运行 |
| `process.wait()` | 等待并返回最终状态 |
| `process.suspend()` | 暂停 |
| `process.resume()` | 恢复 |
| `process.terminate()` | 终止 |
| `process.bindings()` | 返回变量名到 ObjectId 的映射 |
| `process.variables` | 返回变量及其值 |
| `process.program` | Program ObjectId |
| `process.user`、`process.subject` | 运行身份 |
| `process.position` | 当前 Token 位置 |
| `process.result` | 正常结束结果 |
| `process.error` | 失败信息 |
| `process.变量名` | 读取指定变量 |

Process 持久状态包括 `Ready`、`Running`、`Waiting`、`Suspended`、`Halted`、`Terminated`、`Failed`。等待目标以统一 WaitReason 保存，用户通过 Process 管理 API 控制生命周期；Worker lease 字段是内核状态，不是 Praxis capability。

`terminate()` 只终止运行，不会自动退役 Process。

## 11. Channel

```praxis
channel = object.create("core.channel", [])
```

| API | 用途 |
|---|---|
| `channel.send(value)` | 发送消息 |
| `channel.receive()` | 接收一条消息；没有消息时返回 `null` |
| `channel.wait()` | 没有消息时挂起当前 Process |
| `#channel` | 获取当前消息数量 |

Channel 单条消息最多 1 MiB，队列最多 1024 条或 8 MiB 编码状态。send/wakeup 与 receive/dequeue 都和 Process 进度在同一个持久事务中提交。

## 11.1 SwapPool（交换池）

```praxis
pool = object.create("core.swap_pool", {})
shared = object.create("core.value", {count: 0})
pool.attach("state", shared.id)
member_id = pool.get("state")
same_object = object.find(member_id)
same_object.replace({count: 1})
pool.detach("state")
```

`attach(name, object_id)`、`detach(name)`、`get(name)`、`contains(name)`、`list()` 管理/查询 membership Link。Attach 不迁移 Parent、不改变 ObjectId；detach 不 retire member。member 自身的 capability 仍单独检查。上限为每池 4096 个成员、每 Subject 64 个池。完整并发及恢复语义见 [SWAPPOOL.md](./SWAPPOOL.md)。

## 12. Namespace

```praxis
space = object.create("core.namespace", {})
```

| API | 用途 |
|---|---|
| `space.bind(name, target)` | 给对象绑定名字 |
| `space.resolve(path)` | 按名字或路径解析对象 |
| `space.unbind(name)` | 删除名字 |

Namespace 是对象命名空间，不是文件系统。

## 13. Compiler

```praxis
compiler = object.find("compiler")
```

| API | 用途 |
|---|---|
| `compiler.compile(source)` | 编译包含唯一无参数 `main()` 的完整 Praxis 程序，返回 Program Object |
| `compiler.validate(source)` | 按完整程序入口规则检查源码 |
| `compiler.disassemble(program_id)` | 查看 Program Token |

`compiler.compile()` 还会把原始源码保存在 Program 的 `source` Link 下，供 Package Builder 验证并打包；源码 Text Object 是 Program 的子对象。

这里要分清两种东西：

- `ousject compile app.px app.tf` 产生的是一个 Program，只包含一段可执行代码。当前可用 `ousject run-tf app.tf` 直接运行；PX 内还没有把任意 `.tf` Bytes 导入为持久 Program Object 的 `compiler.import()`。
- `packages.export(package_id)` 产生的是一个完整 Package，里面可以同时包含 Program、Module、资源和依赖。它可以用 `packages.import(bytes)` 导入另一个 Ousject。

在 PX 内制作应用 Package 时不需要先生成 `.tf` 文件：直接调用 `program_id = compiler.compile(source)`，再把 `program_id` 交给 `packages.build({ entry: program_id, ... })`。

## 14. System

```praxis
system = object.find("system")
```

| API | 用途 |
|---|---|
| `system.status()` | 获取系统状态 |
| `system.health_check()` | 检查系统一致性 |
| `system.shutdown()` | 请求关机；仅 `local` |
| `system.restart()` | 请求重启；仅 `local` |

## 15. Object Store

```praxis
store = object.find("store")
```

| API | 用途 |
|---|---|
| `store.stats()` | 获取 Shard、对象数量，以及本次启动后的提交次数、WAL 变更字节、全量快照编码量和提交延迟估值（p50/p95/p99 纳秒）；不做健康一致性扫描 |
| `store.health_check()` | 检查对象库一致性 |
| `store.effects()` | 列出 Effect 对象；仅 `local` |

系统没有手动 checkpoint 或 `gc()` API。对象改变时由对象库原子提交；检查点由内核按需维护。退役对象的内容由内核自动清理，元数据保留。

## 16. Authentication、User 与 Session

```praxis
authentication = object.find("authentication")
users = object.find("users")
```

### Authentication

| API | 用途 |
|---|---|
| `authentication.local_initialized()` | 检查 `local` 是否已初始化 |
| `authentication.initialize_local(password)` | 初始化最高用户 |
| `authentication.login(name, password)` | 登录并切换当前 Process 身份 |
| `authentication.logout(token)` | 注销 Session |
| `authentication.current_user()` | 获取当前用户 |
| `authentication.change_password(name, password)` | 修改密码 |

### User Registry

| API | 用途 |
|---|---|
| `users.create_user(name, password)` | 创建用户；仅 `local` |
| `users.users()` | 列出用户；仅 `local` |
| `users.disable_user(name)` | 禁用用户；仅 `local` |

### Session

| API | 用途 |
|---|---|
| `session.revoke()` | 撤销 Session |

## 17. Package Registry

```praxis
modules = object.find("modules")
packages = object.find("packages")
greetings = modules.find("greetings", "0.1.0")
package = packages.build({
    namespace: "example",
    name: "greetings",
    version: "0.1.0",
    modules: { greetings: greetings }
})
installation = packages.install(package)
greetings_module = installation.module("greetings")
```

`modules.find(name, version)` 仅 `local` 可用，返回精确版本的 Module Object ID。`packages.build()` 的 `modules` Map 必须装 Module Object ID；构建时会校验源码 SHA，并把这些 Module 已锁定的依赖一并打入 Package。模块源码不再内嵌在 PX 字符串里。`kind` 可省略：有 `entry` 时自动认作 `application`，否则是 `library`。Application 的 `entry` 传 `compiler.compile(source)` 返回的 Program Object ID。

`dependencies` 可传已登记 Package 或当前用户的 Installation Object ID 数组。构建时会把每个依赖固定为精确坐标和 SHA-256；安装时会递归验证依赖闭包，并在一个原子事务中安装所有缺失依赖。已安装同一坐标但 SHA 不同会拒绝，卸载仍被其他安装包依赖的版本也会拒绝。包内 PX 可导入自己声明依赖中的模块，例如 `import "package/example/shared/1.0.0/common"`。

| API | 用途 |
|---|---|
| `packages.build(spec)` | 仅 `local` 可用；从 Module/Program Object 校验并构建不可变 Package；同一坐标不能替换为不同内容 |
| `packages.import(bytes)` | 导入并验证完整 Ousject Package；依赖必须已登记；返回 Package Object ID |
| `packages.export(package_id)` | 导出 Package 的完整规范化字节，可传给 `packages.import()` |
| `packages.search(query)` | 按坐标包含匹配搜索本机登记的包，最多返回 100 条；空字符串列出前 100 条 |
| `packages.find("namespace/name/version")` | 从本机登记表找到坐标对应 Package ID |
| `packages.info(package_id)` | 查看 Package Manifest 与校验结果 |
| `packages.verify(package_id)` | 校验 Manifest、源码哈希和编译 Program 编码 |
| `packages.install(package_id)` | 为当前用户原子安装 Package 及其精确依赖闭包；同坐标、同 SHA 重复安装会返回已有对象 |
| `packages.list()` | 列出当前用户的安装对象 |
| `packages.require("namespace/name/version")` | 找到当前用户已安装的精确版本 |
| `packages.require("namespace/name")` | 找到当前默认版本；`install` 和 `upgrade` 会更新默认版本 |
| `packages.restore(retired_installation_id)` | 在 7 天内容保留期内重建已卸载版本并恢复其 Data；返回新的 Installation ID |
| `packages.recover()` | 仅 `local`；检查全部 Package 和当前安装索引的一致性。OMS 安装本身是原子的，因此它只报告问题，不猜测或重放半次安装 |

Installation 提供：

| API | 用途 |
|---|---|
| `installation.info()` | 查看安装坐标、Package、有效性和声明能力；能力不再有单独的 `permissions()` API |
| `installation.verify()` | 检查安装记录指向的 Package 是否有效且哈希一致 |
| `installation.module(name)` | 获取包内已安装的 Praxis Module Object |
| `installation.resource(name)` | 读取 Package 中按 Value 快照保存的只读资源 |
| `installation.data()` | 取得此用户、此 Package 版本专有的可变 Data Object；首次取得时原子创建 |
| `installation.data_info()` | 返回 Data Object（如已创建）、已使用字节数、配额和剩余空间 |
| `installation.data_quota()` | 返回此安装当前的数据配额字节数 |
| `installation.set_data_quota(bytes)` | 仅 `local`；设置 0 到 8 MiB 的配额，不能低于当前使用量 |
| `installation.reset_data_quota()` | 仅 `local`；恢复默认 8 MiB 配额 |
| `installation.data_export()` | 返回数据的完整 Value 快照；尚未创建数据时返回 `null` |
| `installation.data_import(snapshot)` | 原子替换为给定 Value 快照，并检查配额 |
| `installation.data_clear()` | 原子清空私有数据 |
| `installation.run([arguments], [capabilities])` | 启动 Application；返回绑定确切 Package 的 Process Object ID |
| `installation.upgrade(package_id)` | 原子安装同一 Package 的新版本并切换默认版本；旧 Process 保持旧 SHA |
| `installation.rollback(installation_id_or_coordinate)` | 将默认版本切回同一 Package 已安装的旧版本，不覆盖任何数据 |
| `installation.uninstall()` | 退役当前用户的安装对象；内容按统一规则保留 7 天 |

`run()` 只适用于 `application`。第二个参数是本次运行获准的能力列表，例如 `console.println`；每项必须已经在包的 `capabilities` 声明中。内核既会发放对应 Object，也会在每次调用时检查具体方法：批准 `console.println` 不会顺带批准 `console.read_line`；只写 `console` 才代表该对象的全部方法。能力按本机发现的 Console、Time、Keyboard、Display、Sensor、Resolver、Math 或 Crypto Object 发放；请求的硬件对象没有发现时启动会失败。原始磁盘和共享网络连接目前不能授予 Package Application：它们需要按应用隔离的存储和网络 Provider，内核会拒绝这两类授权。每次启动创建独立的 Package Subject；Process、参数、资源、私有数据和外部 Object Capability 一起原子提交。卸载仍在运行或暂停状态的 Application 会失败。

Package Data 是普通 Value Object，可用统一接口读取和修改：

```praxis
data = object.find(installation.data())
data.replace({ theme: "dark" })
```

不同用户、不同包版本的安装会得到不同 Data Object；每个 Installation 默认最多 8 MiB，`local` 可以把单个包限制得更小。卸载时它随 Installation 一起退役，`packages.restore()` 会在七天保留期内恢复原内容。卸载根包后，内核会在同一事务中退役没有其他引用、未被加载且只是依赖安装的 Package；用户明确安装过的 Package 永远不会被自动删除。

构建、导入、安装、Application 启动、Export 调用、升级、回滚、卸载和恢复都会写入内部 `core.package_audit`。记录包含用户、动作、目标、时间和关键参数；记录不可修改或手动退役。失败事务不会留下成功审计记录；审计记录属于内核实现，不占用普通包 API。

Package 可在 Ousject 之间用 Bytes 搬运：

```praxis
bytes = packages.export(package_id)
copied_package_id = packages.import(bytes)
```

导入会重新计算 SHA、检查规范编码和精确依赖；不是把调用者提供的 SHA 当成可信证明。此格式是 Ousject 的规范 TF Manifest，不是 CilExec 的 SQLite `.db` 包。

### Market

```praxis
market = object.find("market")
market.configure("https://packages.example")
market.update()
matches = market.search("editor")
package = market.info("<64 位小写 SHA-256>")
download = object.find(market.download(package.sha256))
package_bytes = download.bytes()
installation = market.install(package.sha256)
```

| API | 用途 |
|---|---|
| `market.configure(origin)` | 设置当前用户的市场地址；每个用户各自保存 |
| `market.origin()` | 查看当前用户设置的地址 |
| `market.update()` | 下载并校验市场索引 |
| `market.search(text)` | 用空格分隔的 AND 前缀搜索，最多返回 100 项 |
| `market.info(sha256)` | 按完整 SHA 查看一项；没有则返回 `null` |
| `market.download(sha256)` | 下载并核对完整文件 SHA；返回 `core.package_download` Object ID |
| `download.bytes()` / `download.info()` | 读取下载字节或查看 SHA、大小、状态；仅下载所属用户可读 |
| `market.install(sha256)` | 下载 Ousject 格式 Package、导入并原子安装完整必需依赖 |
| `market.list()` | 列出当前用户已安装的 Package |

市场索引接口按 CilExec Market v1 读取（`apiVersion: "cilexec.market/v1"`）。包文件格式不是跨项目通用的：当前 CilExec 市场提供 SQLite `.db`，里面是 FCL；Ousject 使用 Praxis 与规范 TF Manifest。因此 CilExec `.db` 可以查看和下载，但 `market.install()` 会明确拒绝，不能直接运行。要安装 Ousject 包，市场必须返回 Ousject 导出的 Package 字节，并在索引中声明对应的坐标、SHA 和依赖。当前完整下载上限为 15 MiB，这是单个 Object Value 的编码限制；CilExec 服务器允许更大的文件，但 Ousject 目前不会接收大于此限的制品。

Application Process 可读取随启动一起保存的参数和资源，也可取得该用户专属的 Package Data：

```praxis
arguments = object.find("arguments")
resources = object.find("package_resources")
data = object.find("package_data")
```

Process 通过 `process.wait()` 等待结束。运行所需能力必须由调用者显式列出：

```praxis
editor = packages.require("example/editor/0.1.0")
process_id = editor.run({ document: "notes" }, ["console.println"])
process = object.find(process_id)
result = process.wait()
```

在 Terminal Session 中可按完整版本坐标导入包内模块；模块内部 `import "helper"` 只会解析到同一个 Package 的模块：

```praxis
import "package/example/greetings/0.1.0/greetings"
hello()
```

导入模块的顶层代码和函数会在当前用户的 Terminal Process 中运行，使用该用户本来拥有的权限。Application 则使用独立 Package Subject，只有 `run()` 第二个参数明确授予且 Manifest 声明的能力可以访问。`installation.verify()` 会递归校验安装索引、依赖、Package、包内模块对象及它们之间的引用。

Manifest 的 Export 可以直接作为 Installation 的方法调用。它会创建一个新的 Process，在调用者身份下运行目标函数，并返回 Process Object ID；用 `wait()` 等待后，再读取 `result`：

```praxis
process_id = library.greet("Ada")
process = object.find(process_id)
process.wait()
answer = process.result
```

Export Process 与调用者共享权限身份，但参数会复制成该 Process 自己的 Value Object；它会固定绑定到调用时的 Package 和 Program，后续升级不会替换正在运行的代码。

Package 规格使用 Text 字段 `namespace`、`name`、`version`，以及保存 Module Object ID 的 `modules` Map。可选 `resources` Map 会把 Value 快照打入包；可选 `exports` Map 使用 `{ module, function, arguments }` 描述公开函数，构建和验证时会检查目标函数存在。导出方法调用会返回绑定目标 Program 的 Process Object ID，过程结果通过 Process 的 `result` 读取。模块源码最多 1 MiB，全部源码最多 8 MiB，最多 256 个模块；最多声明 128 项能力；依赖闭包最多 256 个 Package、依赖深度最多 64；编译展开最多 64 MiB，Program 最多 32 MiB。Module 自身锁定的依赖会被打包进去；Package 依赖单独锁定坐标和 SHA，并在安装时原子安装。Library 使用调用者权限；Application 有独立 Package Subject，运行权限只从调用者明确批准的已声明能力发放。当前有 CilExec Market v1 索引和下载客户端，没有发布服务器；CilExec SQLite/FCL 包不兼容 Ousject Package，发布者签名也未实现。

## 18. Process 管理

调度器是内核内部机制，不提供单独的 Praxis `scheduler` 对象。当前用户可见的 Process 用统一查询发现；每个 Process 自身提供生命周期能力：

```praxis
process_ids = object.query("core.process")
process = object.find(process_ids[0])
process.suspend()
process.resume()
process.terminate()
```

已结束的 Process 及其结果保留七天，然后内核自动退役 Process 和它拥有的数据；它引用的 Program 不会被连带删除。退役内容之后按 Object Store 的七天策略清理，ObjectId 和元数据保留。

## 19. Type Registry

```praxis
types = object.find("types")
```

| API | 用途 |
|---|---|
| `types.types()` | 列出所有 Type |
| `types.descriptor(name)` | 查看 Type 描述 |
| `types.register(name, schema, creation, capabilities)` | 注册 Type；仅 `local` |

支持的 Schema：`any`、`text`、`bytes`、`collection`、`record`。

支持的创建策略：`public`、`provider_only`。

## 20. Provider Registry

```praxis
providers = object.find("providers")
```

| API | 用途 |
|---|---|
| `providers.providers()` | 列出已注册 Provider 类型 |
| `providers.devices()` | 列出已发现设备对象 |

两项都只允许最高用户 `local` 调用；普通程序不需要访问 Provider 内部清单。

## 21. Effect

外部操作会先建立持久 Effect 意图。

| API | 用途 |
|---|---|
| `effect.status()` | 返回 `pending`、`running`、`completed`、`failed` 或 `unknown` |
| `effect.result()` | 返回外部操作结果 |

Interrupted Effect 的恢复策略为 `manual` 或 `retry_idempotent`。Manual 操作结果不确定时不会自动重放；详情见 [EFFECTS.md](./EFFECTS.md)。

## 22. DNS Resolver

```praxis
resolver = object.find("resolver")
addresses = resolver.resolve("example.com")
```

| API | 用途 |
|---|---|
| `resolver.resolve(hostname)` | 返回 IP 地址数组 |

## 23. Network Endpoint

| API | 用途 |
|---|---|
| `endpoint.connect(host, port)` | 建立 TCP 连接 |
| `endpoint.listen(host, port)` | 监听 TCP 地址 |
| `endpoint.accept()` | 接受连接并返回新 Endpoint |
| `endpoint.send(text_or_bytes)` | 发送数据 |
| `endpoint.receive()` | 最多接收 65536 字节 |
| `endpoint.receive(maximum)` | 指定最大接收字节数 |
| `endpoint.close()` | 关闭 Endpoint |

有网络 Provider 时，Praxis 可以创建尚未连接的 Endpoint，再调用它的能力：

```praxis
endpoint = object.create("net.endpoint", { transport: "tcp" })
endpoint.connect("example.com", 443)
```

只有系统发现了网络 Provider 后才可用；Provider 校验创建配置。

## 24. Display

| API | 用途 |
|---|---|
| `display.present(text_or_bytes)` | 显示内容 |
| `display.configure(configuration)` | 修改显示配置 |

## 25. Keyboard

键盘事件按 Process 独占终端输入。调用 `capture()` 后读取；结束或失败时内核会自动释放占用。Console 的 `read_line/read_secret` 也使用同一输入源，二者不会同时抢读。

```praxis
keyboard_id = object.query("device.keyboard", "capture")[0]
keyboard = object.find(keyboard_id)
keyboard.capture()
event = keyboard.poll_event()  // 没有按键时返回 null
keyboard.release()
```

| API | 用途 |
|---|---|
| `keyboard.capture()` | 独占键盘输入 |
| `keyboard.release()` | 主动释放输入；Process 结束时也会自动释放 |
| `keyboard.poll_event()` | 立即读取一个事件，没有事件时返回 `null` |
| `keyboard.poll_events(maximum)` | 一次最多取出 1–256 个已排队事件；空队列返回空数组 |
| `keyboard.next_event()` | 等待并读取下一个事件 |

`poll_events()` 和 `console.render()` 是临时交互通道：它们不产生 Effect 记录，并随 VM 执行片保存进程状态。画面在重启后需要由程序重画；已经读出的硬件按键无法在崩溃后放回输入队列。

事件是记录，包含 `key`、`text`、`pressed`、`ctrl`、`alt`、`shift`。支持文本、方向键、Home/End、Insert/Delete、Page Up/Down、F1–F12 和修饰键。终端通常只提供按下事件，不提供可靠的物理 key-up 事件。

## 26. Block Storage

这是内核与硬件驱动之间的底层接口，不授予普通 Praxis 程序。Praxis 程序直接创建和更新对象；对象库负责对象的持久化，不需要程序自己读写磁盘块。

## 27. Sensor

类型表已经声明 `device.sensor`：

| API | 用途 |
|---|---|
| `sensor.sample()` | 采样 |
| `sensor.calibrate()` | 校准 |

当前启动流程没有发布实际 Sensor Provider，因此通常没有可调用的 Sensor 实例。

## 28. Class 实例

用户 Class 的公开方法会成为实例对象能力：

```praxis
class Counter {
    value = 0

    public func add(amount) {
        this.value = this.value + amount
        return this.value
    }
}

counter = object.create("Counter", {})
result = counter.add(2)
```

私有方法和私有字段只能在 Class 内部访问。

## 29. 当前主要限制

- 定时睡眠已改为非阻塞挂起；输入等待仍通过同一持久 Effect 轮询恢复。
- 结束的 Process 和结果保留 7 天，然后内核自动退役 Process 及其拥有的数据；Program 不会被连带删除。退役后再按统一的 7 天墓碑保留策略清理内容，元数据保留。
- Device API 只有在启动时真正发现并发布对应设备对象后才能使用。
- Network、Console、Display 和 Keyboard 等外部操作通过持久 Effect 记录意图，但远端系统不支持幂等时仍不能承诺跨整机崩溃 exactly-once。
- 退役内容清理没有手动命令，由系统后台自动执行。
