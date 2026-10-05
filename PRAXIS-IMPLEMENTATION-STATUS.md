# Praxis 实现状态（0.0.0 / OTF0）

本表只把“已进入 VM 执行路径”的语法/能力标为已实现；自动化测试覆盖范围以测试代码为准。

| 类别 | 已实现语法/能力 | 语义 |
|---|---|---|
| Value | Integer、Float、String、Bool、Null、Array、Map | UTF-8；Value 可嵌套并持久编码 |
| 运算 | `+ - * / %`、全部比较、`and/or/not`、`&&/\|\|/!`、`#` | 布尔短路；除零/溢出产生 Error |
| 修改 | `x = ...`、`x++/--`、字段与索引读写/增减 | 普通赋值复制 Value，不建立隐藏引用 |
| 控制流 | `if/else if/else`、`while`、`break/continue` | 支持嵌套 |
| 函数 | `func`、参数、局部变量、`return` | 调用帧随 Process 持久化 |
| Class | 字段、方法、`this`、public/private、`extends`、`super.method`、重载 | 单继承；按参数数量重载 |
| 创建 | `object.create("Type", value)` | Class 与内置 Type 统一入口；没有 `new/init` |
| 错误 | `try/catch` | 语言、类型、权限和对象错误可捕获；存储故障不被吞掉 |
| 原子块 | `transaction { ... }` | 块内支持赋值、字段/索引修改和 Link；外部 Effect 禁止放入块内 |
| 关系 | `b link a` | 两个名字显式绑定同一 Object；与 `b = a` 不同 |
| 模块 | `import`、`include` | import 一次、include 每次，检测循环 |
| 注释/分隔 | `//`、换行、`;` | `#` 是长度运算符 |

## 统一 Object API

```text
object.create  object.find    object.query
name.id        name.type      name.parent
name.status    name.inspect   name.capabilities
name.owner     name.version   name.permissions
name.value     name.replace   name.children
name.links     name.link      name.unlink
name.grant     name.revoke    name.retire
```

`object` 只负责创建、发现和查询。获得对象后读取 `name.property` 或调用 `name.capability(...)`。旧 `object.value(name)`、`objects.*`、`io.println`、`new` 和隐式打印均被拒绝。

| Object | 已实现领域能力 |
|---|---|
| `core.console` | `print`（不换行）、`println`、`read_line`、`read_secret`、`size`、`is_interactive` |
| `core.time` | `now`（Unix 毫秒）、`monotonic`（启动后毫秒）、`sleep`（持久化定时挂起，不阻塞 VM 工作线程） |
| `core.math` | 数学函数、`random`、`random_integer(min, max)`；另有 `pi/e` |
| `core.value` / `core.text` | `slice`、`find`、`contains`、`split`、`replace_all`、`trim`、`lower`、`upper` |
| `net.resolver` | `resolve(hostname)`，返回系统 DNS 解析出的地址数组 |
| 用户 Class | 源码定义的 public 方法 |
| `core.program` | `execute()`、`execute(entry)` |
| `core.process` | `start`、`wait`、`suspend`、`resume`、`terminate`、`bindings`；并公开 `result`、`error`、运行状态和变量 |
| `core.collection` | 没有集合专属能力；Array/Map 用索引读写、`#` 取长度，整体更新用通用赋值/`replace` |
| `core.namespace` | `bind`、`resolve`、`unbind` |
| `core.channel` | `send`、`receive`、`wait`；长度使用 `#channel` |
| `net.endpoint` | `connect`、`listen`、`accept`、`send`、`receive`、`close` |
| `device.display` | `present`、`configure` |
| `device.keyboard` | `capture`、`release`、`next_event`、`poll_event`；结构化按键事件，与 Console 共用独占输入源 |
| `device.block_storage` | 内核/驱动专用，不授予普通 Praxis Process |

内核调度器不再作为 Praxis 的 `scheduler` 对象暴露；程序通过 `object.query("core.process")` 发现 Process，并调用 Process 自身的 `suspend/resume/terminate`。已结束 Process 保留七天后自动退役。

`core.collection` 的值可以是 Array 或 Map。它们与变量使用同一套值操作：`items[i]` 读取、`items[i] = value` 写入、`#items` 取长度；数组改变长度时用通用 `items.replace([...])` 整体替换。没有 `get/set/length/insert/remove` 集合专属 API。

## Process 与通信

- `object.find("process")` 和 `object.find("program")` 发现当前对象。
- `object.create("core.process", { entry, links })` 创建同一 Program 的子 Process。
- `program.execute([entry])` 创建新的 Process Object。
- Link 传递共享 Object；Channel 提供队列、持久等待和原子唤醒。
- Process 以持久 Subject 执行；跨用户访问仍需 Object Grant。

## 系统保证

- 每次正式状态变化和下一 Token 位置一起提交。
- 跨 Shard 事务原子发布。
- Provider 调用前先保存 Effect Intent；完成状态、Provider Object 和 Process 推进共同提交。
- WAL/Checkpoint 恢复不会把半条记录当成提交。

Praxis 文档中定义的当前基础语法均已进入执行路径。尚未声称它拥有 Python/Java 的全部标准库、反射、泛型、异步语法或调试器；这些不在当前语法规范中。
