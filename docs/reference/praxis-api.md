# Praxis API 总表

本文列出当前 Praxis 程序能够调用的 Registry、Object 通用 API、内核服务和已实现的对象
能力。调用形式统一为 `receiver.method(arguments...)`。返回的 Object ID 是 Text，但赋给变量
后编译器会保留其 Object 身份，因而可以继续用点号访问。

权限错误、参数数量错误或参数类型错误会产生可捕获的 Error。标为“local”的能力只允许保留
的 `local` 系统身份调用。

## Object Registry

| API | 返回 | 说明 |
| --- | --- | --- |
| `object.create(type, value)` | Object | 创建 Object |
| `object.create(type, value, parent)` | Object | 在指定父 Object 下创建 |
| `object.find(name_or_id)` | Object | 从当前 Process 绑定或 Object ID 查找 |
| `object.query(type)` | Array<Object ID> | 查询可见 Object |
| `object.query(type, capability)` | Array<Object ID> | 按领域能力过滤；第二参数可为 `null` |

不存在 `objects.*` 或通用的 `object.call()`。

## 所有 Object 的通用属性与方法

| 成员 | 返回/作用 |
| --- | --- |
| `.id` | Object ID |
| `.type` | Type 名或 Praxis Class 名 |
| `.parent` | 父 Object ID 或 `null` |
| `.status` | 生命周期；Process 返回执行状态 |
| `.version` | Object 版本文本 |
| `.owner` | 所有者 Subject ID |
| `.permissions` | owner 与 grants；需要管理策略权限 |
| `.inspect` | `{id, type, parent, version, status}` |
| `.capabilities` | 当前可见能力名数组 |
| `.value` | Object 的业务值 |
| `.children` | 子 Object ID 数组 |
| `.links` | Link 名到 Object ID 的 Map |
| `.replace(value)` | 替换业务值 |
| `.link(name, target)` | 创建或更新 Link |
| `.unlink(name)` | 删除 Link |
| `.grant(subject, capability)` | 授权 |
| `.revoke(subject, capability)` | 撤销授权 |
| `.retire()` | 将 Object 及可级联清理的子对象转为 Tombstoned |

Process 另外公开 `.result`、`.error`、`.variables`、`.program`、`.user`/`.subject` 和
`.position`。Record/Class 实例的业务字段优先于通用属性；私有字段受 Class 可见性检查。

## 启动时可发现的服务

使用 `service = object.find("名称")` 获取服务。

| 名称 | API |
| --- | --- |
| `system` | `status()`、`health_check()`、`shutdown()` local、`restart()` local |
| `authentication` | `local_initialized()`、`initialize_local(password)` local、`login(name, password)`、`current_user()`、`logout(token)`、`change_password(name, password)` |
| `users` | `create_user(name, password)` local、`users()` local、`disable_user(name)` local |
| `compiler` | `compile(source)`、`validate(source)`、`disassemble(program_id)` |
| `types` | `types()`、`descriptor(name)`、`register(name, schema, creation, capabilities)` local |
| `providers` | `providers()` local、`devices()` local |
| `store` | `stats()`、`health_check()`、`effects()` local |
| `math` | 数学 API，另有只读字段 `.pi`、`.e` |
| `crypto` | `sha256(value)` |
| `time` | `now()`、`monotonic()`、`sleep(milliseconds)` |
| `terminal` | Terminal 字节流与 Shell API：`create()`、`configure(options)`、`resize(columns, rows)`、`foreground()`、`wait_event()`、`input(maximum)`、`output(bytes)`、`snapshot()`、`shell()` |
| `modules` | Module API |
| `packages` | Package Registry API |
| `market` | Package Market API |
| `audit` | Audit 根 Object；通过通用 Object 属性读取 |
| `resolver` | `resolve(hostname)` |
| `programs` | 系统 Program Namespace，常用 `resolve(path)` |
| `console` | Console Provider API |

`types.register` 的 `schema` 可为 `any`、`text`、`bytes`、`collection`、`record`；
`creation` 可为 `public` 或 `provider_only`。它登记 Praxis Object 的数据描述与 capability
名称，不会安装 Rust Provider 或 native Type 实现。Provider 只能在可信 VM 启动阶段通过 Rust
API 注册；第一个 runnable Process 开始执行时 Registry 自动 seal。

## Console

| API | 返回 | 说明 |
| --- | --- | --- |
| `console.print(value)` | `null` | 不换行输出 |
| `console.println(value)` | `null` | 换行输出 |
| `console.render(text_or_bytes)` | `null` | 将帧内容直接写入终端，不添加换行 |
| `console.read_line()` | Text | 读取一行 |
| `console.read_secret()` | secret Text | 隐藏回显读取；敏感明文不进入 Process 状态 |
| `console.size()` | Record | `{columns, rows}` |
| `console.is_interactive()` | Bool | 是否连接交互终端 |

`render` 是即时 Provider 调用，适合高频刷新终端画面；传入的 Text 可以包含 `\\e` 生成的
ANSI 控制字符。

## 数学、Text 与时间

`math` 支持：

| API | 说明 |
| --- | --- |
| `random()` | `[0, 1)` 的 Float |
| `random_integer(min, max)` | 闭区间随机 Integer |
| `abs(x)`、`min(a,b)`、`max(a,b)`、`clamp(x,min,max)` | 数值操作 |
| `sqrt(x)`、`pow(base, exponent)` | 幂与平方根 |
| `floor(x)`、`ceil(x)`、`round(x)`、`trunc(x)` | 舍入 |
| `sin(x)`、`cos(x)`、`tan(x)`、`atan2(y,x)`、`hypot(x,y)` | 三角与距离 |
| `log(x)`、`log2(x)`、`log10(x)`、`exp(x)` | 对数与指数 |

`core.text` Object 支持 `slice(start,length)`、`find(text)`、`contains(text)`、`split(delimiter)`、
`replace_all(from,to)`、`trim()`、`lower()`、`upper()` 和 `utf8_bytes()`。最后一个方法明确将
Text 编码为 UTF-8 `Bytes`，可作为 Terminal 或 Device 的二进制输出参数。`Bytes` 支持 `#bytes`
和 `bytes[index]`；索引结果为 `0..255` 的 Integer，不做 UTF-8 校验或文本转换。

`time.now()` 返回 Unix 毫秒，`time.monotonic()` 返回单调时钟纳秒，`time.sleep(ms)` 暂停当前
Process，并释放 Worker Lease。

## Terminal

`terminal = object.find("terminal")` 返回根 Terminal Object。Terminal 与 Display 是不同概念：
Terminal 传输任意 Bytes 并维护字符屏幕状态；物理 Display Provider（例如 framebuffer）负责
像素输出。当前 CLI 将活动 Terminal 的字符画面渲染到宿主 tty，不会把 stdout 发布成
`device.display`。

| API | 返回/作用 |
| --- | --- |
| `create()` / `create(options)` | 创建子 Terminal Object，返回其 ID；父子关系持久化，Screen 独立保存在运行期内存 |
| `configure({input_mode: "raw"|"canonical", echo: boolean})` | 配置输入方式；配置写入 Terminal Object |
| `resize(columns, rows)` | 调整屏幕尺寸；宽度 1–512，高度 1–256 |
| `foreground()` | 将调用它的 Process 设为该 Terminal 的前台；进程结束后恢复父 Terminal |
| `wait_event()` | 等待并返回 Terminal 事件 Record；目前包含 `{kind: "resize", columns, rows}`，经 Scheduler 等待，不要求程序轮询尺寸 |
| `input(maximum)` | 活动 Terminal 读取最多 `maximum` 个 `Bytes`；当前无可用输入时返回空 Bytes；范围 1–65536 |
| `output(bytes)` | 向 Terminal 写入原始 Bytes，返回写入长度；只更新屏幕/渲染，不逐帧创建 Effect |
| `snapshot()` | 返回完整行、光标、尺寸、alternate-screen 状态和增量 `dirty_rows` |
| `shell()` | 打开或复用当前用户的持久 Shell 子 Terminal，返回 Terminal Object ID |

输入由一个独占 Process 租约保护，和旧 Console/Keyboard 输入共享宿主输入。Raw 模式保留原始
字节；Canonical 模式按行返回 UTF-8 字节并追加 LF，可配置软件回显。Child Terminal 可以嵌套并
拥有独立 Screen、模式和输入路由；进程结束、失败或 Terminal 被退役时，输入所有权恢复到父
Terminal。Terminal 层级、配置、尺寸和前台 Process ID 持久化；Cell、解析器临时状态与渲染缓存
不持久化。重启时从 Object 父子关系和前台 Process 字段恢复路由。

宿主 tty 本身提供 Terminal 协议字节：方向键按当前 normal/application cursor mode 返回 CSI/SS3
序列；Bracketed Paste 和启用的 SGR Mouse 数据连同协议标记原样进入 `input()`。Terminal 不依赖
`device.keyboard`，也不把这些输入强制解码成 Text。当前 Linux tty backend 复用宿主终端模拟器
生成的序列，没有独立的 USB/HID 键码到 Terminal 序列转换器；更换为事件型宿主后端时需要由该
backend 实现相同的字节协议。

屏幕解析包括 UTF-8 grapheme（CJK、组合符、emoji 宽度）、CSI 光标移动/定位、擦除、插入/删除
字符与行、滚动区域、常见 SGR 属性/基本色/256 色/truecolor、OSC 标题，以及 `?47`、`?1047`、
`?1048`、`?1049`、应用光标、Bracketed Paste、SGR Mouse 等模式。宿主 Renderer 按脏行输出样式
化字符并同步这些模式。它仍不是完整 xterm/VT100 实现；未支持的控制序列会被忽略。

可运行示例：

```bash
cargo run -p ousject-cli -- run examples/terminal-object.px --memory --local
```

Terminal 屏幕缓冲区仅在本次运行期保存在内存中；尺寸和输入配置是持久 Object 状态。重启后
不会重放过去的输出字节来重建屏幕。

## Terminal Shell API

推荐 Shell 流程由 Terminal 自己持有状态：

```px
terminal = object.find("terminal")
shell_id = terminal.shell()
shell = object.find(shell_id)
process = object.find(shell.process())
shell.submit("answer = 42")
process.wait()
```

Shell 子 Terminal 通过以下 VM 原生方法管理交互 Process：

| API | 返回/作用 |
| --- | --- |
| `submit(source)` | 提交交互代码并返回 Process ID |
| `process()` | 当前 Terminal 的持久交互 Process ID |
| `history()` | 已清理敏感内容的历史数组 |
| `pending_input()` | 尚未提交的输入 |
| `save_input(source)` | 保存尚未提交的输入 |
| `update_size(columns, rows)` | 更新终端尺寸 |
| `cancel()` | 中断当前提交 |
| `close()` | 关闭 Terminal 并结束持久上下文 |

系统 Shell 与交互式程序统一使用 `terminal.shell()` 创建或重新连接当前用户的子 Terminal；
Shell 状态、历史和 Process 都由这个 `core.terminal` Object 持有。

## Process 与 Program

| 类型 | API | 返回/作用 |
| --- | --- | --- |
| `core.program` | `execute()` | 创建 Process，返回 ID |
| `core.program` | `execute(bindings)` | 使用 Object 绑定 Record/Map 创建 Process |
| `core.program` | `execute(entry)` | 以指定入口创建并启动 Process |
| `core.process` | `start()`、`resume()` | 置为 Ready |
| `core.process` | `suspend()` | 挂起 |
| `core.process` | `terminate()` | 终止 |
| `core.process` | `wait()` | 等待结束，返回 `halted`、`terminated` 或 `failed` |
| `core.process` | `bindings()` | 变量名到持久 Value Object ID 的 Record |

Process 等待不会同步占用父 Process 的 Worker Lease。失败后通过 `.error` 取得 Error 值。

## Namespace、Channel、Timer 与 SwapPool

| 类型 | API |
| --- | --- |
| `core.namespace` | `resolve(path)`、`bind(name,target)`、`unbind(name)` |
| `core.channel` | `send(value)`、`receive()`、`wait()` |
| `core.timer` | `arm(milliseconds)`、`wait()`、`cancel()`、`status()` |
| `core.swap_pool` | `attach(name,object_id)`、`detach(name)`、`get(name)`、`contains(name)`、`list()` |

Channel 最多保存 1024 条消息，单条不超过 1 MiB，队列编码后不超过 8 MiB。Timer 状态为
`armed`、`fired` 或 `cancelled`。

## Effect 与 Session

| 类型 | API |
| --- | --- |
| `core.effect` | `status()`、`result()`、`retry()` local、`resolve(result)` local |
| `core.session` | `revoke()` |

`retry` 和 `resolve` 只适用于恢复后状态为 `unknown` 的 Effect。

## Authentication 返回结构

- `initialize_local`、`current_user`、`users`、`create_user` 返回/包含
  `{name, object, subject, enabled}`。
- `login` 返回 `{token, subject, expires_at}`，同时把当前 Process 切换到该用户 Subject。
- 密码不得为空；创建或修改密码时会返回 `password cannot be empty`。用户名非法、用户禁用、
  凭据错误等也会返回可捕获 Error。
- User 持久记录只保存 Argon2id PHC 密码验证串，不保存明文密码；Session 只保存 bearer token
  的 SHA-256 摘要。凭据格式不提供旧 PBKDF2 记录兼容或自动迁移。

## Module Registry

| API | 返回/作用 |
| --- | --- |
| `install(name, version, source, capabilities)` | Module ID |
| `install(name, version, source, capabilities, dependencies)` | Module ID |
| `modules()` | 已安装 Module |
| `find(name, version)` | Module ID |
| `instances(module_id)` | 实例列表 |
| `enable(module_id)`、`disable(module_id)` | 改变启用状态 |
| `upgrade(module_id, version, source, capabilities[, dependencies])` | 新 Module ID |
| `rollback(current_id, target_id)` | 回滚版本 |
| `uninstall(module_id)` | 卸载 |

`dependencies` 是依赖描述数组；实际结构由安装校验规则检查。Praxis 的 `import`/`include`
通过当前 Subject 可见的 Module 加载源码。

## Package Registry

| API | 返回/作用 |
| --- | --- |
| `build(specification)` | 构建 Package Artifact，返回 ID |
| `import(bytes)`、`export(artifact_id)` | 导入/导出 Artifact |
| `search(query)`、`find(coordinate)` | 搜索或按坐标查找 |
| `info(artifact_id)`、`verify(artifact_id)` | 元数据与完整性校验 |
| `install(artifact_id)` | Package Installation ID |
| `list()` | 当前用户的安装列表 |
| `require(coordinate)` | 查找当前用户满足坐标的 Installation |
| `recover()` local | 安装恢复报告 |
| `restore(retired_installation_id)` | 恢复已卸载安装 |

Package Installation Object 支持：

| API | 返回/作用 |
| --- | --- |
| `info()`、`verify()` | 安装信息与校验 |
| `module(name)`、`resource(name)` | 读取模块 ID或资源值 |
| `run()`、`run(arguments)`、`run(arguments, capabilities)` | 运行应用并返回 Process ID |
| `upgrade(artifact_id)`、`rollback(target_id_or_coordinate)` | 版本切换 |
| `data()`、`data_info()`、`data_quota()` | 包私有数据与配额 |
| `set_data_quota(bytes)`、`reset_data_quota()` | 配额管理 |
| `data_export()`、`data_import(snapshot)`、`data_clear()` | 数据迁移/清理 |
| `uninstall()` | 卸载 |
| `installation.export_name(args...)` | 调用 Manifest 声明的动态导出，返回 Process ID |

Package Instance Object 支持 `process()` 和 `status()`。

## Package Market 与 Download

| API | 返回/作用 |
| --- | --- |
| `market.configure(origin)` | 设置当前用户 Market 地址 |
| `market.origin()` | 当前地址 |
| `market.update()` | 更新索引 |
| `market.search(query)`、`market.info(sha256)` | 搜索与查看条目 |
| `market.download(sha256)` | 创建或续传 Download Object |
| `market.install(sha256)` | 下载、验证并安装 |
| `market.list()` | 当前用户通过 Market 安装的包 |
| `download.bytes()` | 完成后取得 Package Bytes |

## 宿主设备 Provider

设备由 `providers.devices()` 枚举，再通过 `object.find(id)` 使用；能否调用取决于授权和宿主
是否发布了对应设备。

| 设备 | API |
| --- | --- |
| Display | 当前没有物理 Display Provider，因此不会发布可调用的 Display 方法，也不会把 tty 当作像素设备 |
| Keyboard | `capture()`、`release()`、`next_event()`、`poll_event()`、`poll_events(maximum)` |
| Block Storage | Binary `input(maximum)` / `output(bytes)` 顺序访问，以及 `load_block(index)` / `store_block(index, bytes)`；块大小 4096 bytes |
| Network Endpoint | Binary `input(maximum)` / `output(bytes)`，以及 `connect(host,port)`、`listen(host,port)`、`accept()`、`send(data)`、`receive([maximum])`、`close()` |
| Resolver | `resolve(hostname)` |

Keyboard 返回的事件是 Record，使用索引读取字段，例如 `event["key"]` 和 `event["text"]`；不要用
`event.key` 访问 Record 字段。

Keyboard Provider 只取得宿主 OS 提供的高层按键事件，不伪造 USB/HID 原始字节，因此不公布
Binary `input/output`。Display 与 Sensor 当前没有宿主 Provider，也不宣称可执行其领域方法。
`.capabilities()` 对 Provider-backed 类型只列出本次运行实际注册并支持的方法；Device 的高层
语义方法和 Binary API 共存。Binary I/O 接受/返回真正的 `Value::Bytes`：嵌入 NUL、invalid UTF-8
及 `0x00..0xFF` 不会被转成 Text 或做换行/JSON 处理。TCP `input(maximum)` 是一次 stream read，
可能短于 maximum；Block Storage `input/output` 使用持久化顺序 offset，块 API 仍可按 4096-byte
块访问。

Effect 语义按 Provider 与方法区分：Terminal 的 `input`/`output`/`snapshot` 为 ephemeral；TCP
Network、Block Storage 的真实收发/读写走 durable Effect，当前 host adapter 使用默认手动恢复
策略；Keyboard `poll_events` 为 ephemeral，其余捕获/等待方法仍经过 durable Effect。相同方法名
不隐含相同的恢复策略。

## User-space Driver 基础路径

最小驱动可以是一个 Praxis Program/Package，以受限 capability 启动为独立 Process。它只对获得
授权的 Device Object 调用 `input(maximum)` / `output(bytes)`，然后用普通 Object 保存或发布语义
状态，例如用 Namespace Link 暴露一个最近结果 Object。应用读取语义 Object，不必直接处理 Bytes。
Driver Process 失败由 VM 标记为 Failed；已有 Device Object、Capability 和 sealed Provider 集
保持不变。其他用户空间代码可以按策略重新启动 Program，但当前没有内建 Driver Manager 或自动
重启服务。Package/Market/`import` 只装载或编译 Praxis 与资源，`providers` 服务没有注册 API，
不能加载 native library、注册 Provider 或修补 VM/OMS。

## 如何查看当前运行时实际能力

```px
types = object.find("types")
console = object.find("console")

console.println(types.types())
item = object.find("system")
console.println(item.inspect)
console.println(item.capabilities)
```

`types.types()` 与 `types.descriptor(name)` 返回运行时 Type 清单/描述符；其中 Provider-backed
Type 的 `capabilities` 会按本次 VM 实际注册的 Provider 能力裁剪，Terminal 还包含 VM 实现的
Terminal-specific intrinsic。这个字段表示当前 Provider 层能执行哪些领域方法，不代表某个用户
已经获得调用权。`.capabilities` 读取具体 Object 的授权与当前实现能力，才是该对象可调用能力的
合并结果。本文补充参数、返回值和约束；API 事实源是 VM 的 Object 调用分派、Provider 实现和
OMS 类型描述符。
