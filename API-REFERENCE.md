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
| `console.print(value)` | 输出内容，不自动换行；适合提示符和实时界面 |
| `console.println(value)` | 输出一行 |
| `console.read_line()` | 读取一行文本 |
| `console.read_secret()` | 读取不回显的秘密文本 |
| `console.size()` | 返回 `{columns, rows}` |
| `console.is_interactive()` | 判断是否连接交互终端 |

## 5. Time

```praxis
time = object.find("time")
```

| API | 用途 |
|---|---|
| `time.now()` | 返回 Unix 毫秒时间 |
| `time.monotonic()` | 返回本次启动后的单调毫秒时间 |
| `time.sleep(milliseconds)` | 让当前 Process 挂起指定时长，到期后由调度器恢复 |

睡眠截止时间和 Process 状态一起原子保存；等待时不占用执行线程，其他 Process 可以继续运行。

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

## 7. Text 与文本 Value

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

## 8. Program

Program 由编译器产生，普通程序不能伪造 Provider-only Program。

| API | 用途 |
|---|---|
| `program.execute()` | 创建 Process |
| `program.execute(bindings)` | 使用变量对象绑定创建 Process |
| `program.execute(entry)` | 从指定入口创建并启动 Process |

## 9. Process

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

`terminate()` 只终止运行，不会自动退役 Process。

## 10. Channel

```praxis
channel = object.create("core.channel", [])
```

| API | 用途 |
|---|---|
| `channel.send(value)` | 发送消息 |
| `channel.receive()` | 接收一条消息；没有消息时返回 `null` |
| `channel.wait()` | 没有消息时挂起当前 Process |
| `#channel` | 获取当前消息数量 |

## 11. Namespace

```praxis
space = object.create("core.namespace", {})
```

| API | 用途 |
|---|---|
| `space.bind(name, target)` | 给对象绑定名字 |
| `space.resolve(path)` | 按名字或路径解析对象 |
| `space.unbind(name)` | 删除名字 |

Namespace 是对象命名空间，不是文件系统。

## 12. Compiler

```praxis
compiler = object.find("compiler")
```

| API | 用途 |
|---|---|
| `compiler.compile(source)` | 编译 Praxis，返回 Program Object |
| `compiler.validate(source)` | 检查源码能否编译 |
| `compiler.disassemble(program_id)` | 查看 Program Token |

## 13. System

```praxis
system = object.find("system")
```

| API | 用途 |
|---|---|
| `system.status()` | 获取系统状态 |
| `system.health_check()` | 检查系统一致性 |
| `system.shutdown()` | 请求关机；仅 `local` |
| `system.restart()` | 请求重启；仅 `local` |

## 14. Object Store

```praxis
store = object.find("store")
```

| API | 用途 |
|---|---|
| `store.stats()` | 获取 Shard、对象、Active 和 Tombstone 数量 |
| `store.health_check()` | 检查对象库一致性 |
| `store.effects()` | 列出 Effect 对象；仅 `local` |

系统没有手动 checkpoint 或 `gc()` API。对象改变时由对象库原子提交；检查点由内核按需维护。退役对象的内容由内核自动清理，元数据保留。

## 15. Authentication、User 与 Session

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

## 16. Process 管理

调度器是内核内部机制，不提供单独的 Praxis `scheduler` 对象。当前用户可见的 Process 用统一查询发现；每个 Process 自身提供生命周期能力：

```praxis
process_ids = object.query("core.process")
process = object.find(process_ids[0])
process.suspend()
process.resume()
process.terminate()
```

已结束的 Process 及其结果保留七天，然后内核自动退役 Process 和它拥有的数据；它引用的 Program 不会被连带删除。退役内容之后按 Object Store 的七天策略清理，ObjectId 和元数据保留。

## 17. Type Registry

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

## 18. Provider Registry

```praxis
providers = object.find("providers")
```

| API | 用途 |
|---|---|
| `providers.providers()` | 列出已注册 Provider 类型 |
| `providers.devices()` | 列出已发现设备对象 |

两项都只允许最高用户 `local` 调用；普通程序不需要访问 Provider 内部清单。

## 19. Effect

外部操作会先建立持久 Effect 意图。

| API | 用途 |
|---|---|
| `effect.status()` | 返回 `pending`、`completed` 或 `failed` |
| `effect.result()` | 返回外部操作结果 |

## 20. DNS Resolver

```praxis
resolver = object.find("resolver")
addresses = resolver.resolve("example.com")
```

| API | 用途 |
|---|---|
| `resolver.resolve(hostname)` | 返回 IP 地址数组 |

## 21. Network Endpoint

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

## 22. Display

| API | 用途 |
|---|---|
| `display.present(text_or_bytes)` | 显示内容 |
| `display.configure(configuration)` | 修改显示配置 |

## 23. Keyboard

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
| `keyboard.next_event()` | 等待并读取下一个事件 |

事件是记录，包含 `key`、`text`、`pressed`、`ctrl`、`alt`、`shift`。支持文本、方向键、Home/End、Insert/Delete、Page Up/Down、F1–F12 和修饰键。终端通常只提供按下事件，不提供可靠的物理 key-up 事件。

## 24. Block Storage

这是内核与硬件驱动之间的底层接口，不授予普通 Praxis 程序。Praxis 程序直接创建和更新对象；对象库负责对象的持久化，不需要程序自己读写磁盘块。

## 25. Sensor

类型表已经声明 `device.sensor`：

| API | 用途 |
|---|---|
| `sensor.sample()` | 采样 |
| `sensor.calibrate()` | 校准 |

当前启动流程没有发布实际 Sensor Provider，因此通常没有可调用的 Sensor 实例。

## 26. Class 实例

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

## 27. 当前主要限制

- 定时睡眠已改为非阻塞挂起；输入等待仍通过同一持久 Effect 轮询恢复。
- 结束的 Process 和结果保留 7 天，然后内核自动退役 Process 及其拥有的数据；Program 不会被连带删除。退役后再按统一的 7 天墓碑保留策略清理内容，元数据保留。
- Device API 只有在启动时真正发现并发布对应设备对象后才能使用。
- Network、Console、Display 和 Keyboard 等外部操作通过持久 Effect 记录意图，但远端系统不支持幂等时仍不能承诺跨整机崩溃 exactly-once。
- 退役内容清理没有手动命令，由系统后台自动执行。
