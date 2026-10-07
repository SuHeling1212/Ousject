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
| `terminal` | `open()` |
| `modules` | Module API |
| `packages` | Package Registry API |
| `market` | Package Market API |
| `audit` | Audit 根 Object；通过通用 Object 属性读取 |
| `resolver` | `resolve(hostname)` |
| `programs` | 系统 Program Namespace，常用 `resolve(path)` |
| `console` | Console Provider API |

`types.register` 的 `schema` 可为 `any`、`text`、`bytes`、`collection`、`record`；
`creation` 可为 `public` 或 `provider_only`。

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
`replace_all(from,to)`、`trim()`、`lower()`、`upper()`。

`time.now()` 返回 Unix 毫秒，`time.monotonic()` 返回单调时钟纳秒，`time.sleep(ms)` 暂停当前
Process，并释放 Worker Lease。

## Terminal Session

`terminal.open()` 返回 `core.terminal_session`：

| API | 返回/作用 |
| --- | --- |
| `submit(source)` | 提交交互代码并返回 Process ID |
| `process()` | Session Process ID |
| `history()` | 已清理敏感内容的历史数组 |
| `pending_input()` | 尚未提交的输入 |
| `save_input(source)` | 保存尚未提交的输入 |
| `update_size(columns, rows)` | 更新终端尺寸 |
| `cancel()` | 中断当前提交 |
| `close()` | 关闭 Session 并结束持久上下文 |

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
| Display | `present(text_or_bytes)`、`configure(configuration)` |
| Keyboard | `capture()`、`release()`、`next_event()`、`poll_event()`、`poll_events(maximum)` |
| Block Storage | `load_block(index)`、`store_block(index, bytes)`；块大小 4096 bytes |
| Network Endpoint | `connect(host,port)`、`listen(host,port)`、`accept()`、`send(data)`、`receive([maximum])`、`close()` |
| Resolver | `resolve(hostname)` |

Keyboard 返回的事件是 Record，使用索引读取字段，例如 `event["key"]` 和 `event["text"]`；不要用
`event.key` 访问 Record 字段。

## 如何查看当前运行时实际能力

```px
types = object.find("types")
console = object.find("console")

console.println(types.types())
item = object.find("system")
console.println(item.inspect)
console.println(item.capabilities)
```

`types.types()` 是运行时 Type 清单，`.capabilities` 是某个具体 Object 当前可见的能力清单；
本文则补充参数、返回值和约束。API 事实源是 VM 的 Object 调用分派、Provider 实现和 OMS
类型描述符。
