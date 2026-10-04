# Ousject 统一对象模型

本文档定义 Ousject 中 Object、Value、Type、Capability、Process、Network、Device 和传统 File 的统一关系。

## 1. 结论

Ousject 只保留一个系统级实体：

```text
Object
```

Process、Program、Network Endpoint、Device、Collection 和普通数据都不是另一套基础设施。它们都是具有不同 Type、Value 和 Capability 的 Object。

传统 File 不作为 Ousject 的核心对象类型。文本、二进制内容和结构化数据直接作为 Object Value 持久化。需要兼容 POSIX 或外部工具时，由适配器临时提供 File View。

## 2. Object 和 Value 必须分开

Object 是可以被独立识别、授权、持久化、关联和管理的实体。Praxis 变量名绑定 Object：`x = 46` 创建或更新 `core.value`，而 `note = object.create("core.value", 42)` 显式创建同类对象并绑定 `note`。普通表达式读取其 Value；对象 API 使用其身份。`b = x` 复制 Value 到独立对象。

对象先通过 `object.create/find/query` 创建或发现，并把结果绑定到变量名；随后用 `变量名.能力(...)` 调用。运行时根据变量绑定的 Object、Type 和 Provider 检查权限并执行能力。控制台由硬件适配器发现并发布为共享 `core.console` Object：`aaa = object.find("console")` 发现并命名它，`aaa.println(text)` 调用打印能力。Network 和各宿主 Device 也使用同一条 Provider 路径。

Value 是 Object 当前保存的内容。

```text
Object
├── Identity
├── Type
├── Parent
├── Value
├── Capabilities
├── Policy
├── Lifecycle
├── Version
└── Provider
```

例如：

```text
Variable Object
└── Value = 42

Text Object
└── Value = "hello"

Process Object
└── Value = { token_position, stack, status }

Network Object
└── Value = { transport, local, remote, phase }
```

这一区分对性能非常重要。不是每个整数、字符串字符或 Array 元素都必须拥有全局 ObjectId。

- 需要独立身份、权限、生命周期或 Link 的内容，创建为 Object。
- 只属于某个 Object 内部状态的内容，保存为内联 Value。
- Array 和 Map 的元素默认是 Value；需要关联另一个 Object 时使用显式 Link。

因此，“万物皆对象”表示所有可独立管理的系统实体都服从 Object 模型，不表示每一个字节都要成为一个昂贵的独立对象。

## 3. 统一 Object 结构

每个 Object 使用同一种逻辑 Header：

| 字段 | 含义 |
|---|---|
| `id` | 永久且稳定的 ObjectId |
| `type_id` | 指向 Type Descriptor Object |
| `parent_id` | 隶属的父 Object，可为空 |
| `value_root` | 当前正式 Value 或分段状态根 |
| `capability_profile` | 该对象公布的能力集合 |
| `policy_id` | 调用权限和授权规则 |
| `lifecycle` | Creating、Active、Suspended、Terminating、Tombstoned 等 |
| `version` | 每次成功提交后递增 |
| `provider_id` | 实现特殊能力的 Provider Object，可为空 |
| `residency` | RAM、Storage、Remote、Device-backed 等内部位置 |

`residency` 不是对象身份，Praxis 程序不能依赖它。

## 4. Value 类型

第一版统一 Value 应支持：

```text
Null
Bool
Integer
Float
Text
Bytes
Array<Value>
Map<Text, Value>
Record<field, Value>
Error
```

Object 之间的关系不伪装成普通 Value 引用。关系使用显式结构：

```text
Parent
Child
Link(name, target_object_id)
```

这样不会重新引入隐藏 Pointer 或 Reference。

大型 Text、Bytes、Array 和 Map 使用不可变 Segment 与 Copy-on-Write，不要求每次修改复制整个 Value。

## 5. Type 也是 Object

类型不能只做成写死在内核里的枚举。每个可创建类型由一个 Type Descriptor Object 描述：

```text
Type Descriptor Object
├── type_id
├── name
├── schema_version
├── value_schema
├── default_capabilities
├── creation_policy
├── provider_id
└── migration_rules
```

常见 Type 可以包括：

```text
core.value
core.text
core.bytes
core.collection
core.program
core.process
net.endpoint
device.display
device.sensor
device.keyboard
device.block_storage
```

Type 只定义 Value 结构、默认能力和实现 Provider。ObjectId 仍然标识具体对象实例。

## 6. 创建对象 API

系统存在一个预绑定的 Object Registry Object：

```praxis
object
```

最简单的创建形式是：

```praxis
object_name = object.create(type_name, initial_value)
```

完整形式是：

```praxis
object = object.create(
    "core.text",
    "hello",
    {
        parent: this,
        persistence: "durable",
        links: {}
    }
)
```

调用者可以选择对象类型，但只能选择已经注册并且允许当前调用者创建的 Type。调用者不能通过伪造类型名获得设备、驱动或管理员能力。

底层创建流程必须是：

```text
解析 Type Descriptor
    ↓
检查 Create 权限
    ↓
验证 initial_value 是否符合 Schema
    ↓
由 Type/Provider 安装合法 Capability
    ↓
创建候选 Object
    ↓
持久化原子提交
    ↓
发布 ObjectId
```

能力不能由普通调用者随意写入：

```praxis
// 不允许：不能靠声明字符串伪造系统能力
object.create("fake", {}, { capabilities: ["terminate_any_process"] })
```

## 7. 三类创建方式

不同类型仍然通过同一个 Registry API 进入，但创建策略不同。

| 类型类别 | 创建方式 | 示例 |
|---|---|---|
| 普通数据对象 | 直接创建 | Text、Bytes、Collection、用户 Class |
| Provider 管理对象 | Registry 请求 Provider 创建 | Process、Network Endpoint、Timer |
| 已存在的物理对象 | 只能发现或由驱动发布 | Display、Keyboard、Sensor、Block Storage |

### 7.1 普通数据对象

```praxis
note = object.create("core.text", "hello")
settings = object.create("core.collection", {
    theme: "dark"
})
```

### 7.2 Process

```praxis
process = object.create("core.process", {
}, {
    links: {
        program: program
    }
})
```

Registry 会把创建请求交给 Process Provider。Provider 初始化 Token Position、Stack、状态和 Program Link。

### 7.3 Network

```praxis
connection = object.create("net.endpoint", {
    transport: "tcp"
})
```

Registry 会把创建请求交给 Network Provider。建立真实外部连接仍然是后续 Capability 调用和 Effect，不应隐藏在普通 Value 写入中。

### 7.4 Device

物理设备不是应用创建出来的。驱动发现硬件后发布 Device Object：

```praxis
display = object.query({
    type: "device.display",
    capability: "present"
})[0]
```

只有虚拟设备或受控模拟设备可以通过允许创建的 Device Type 产生。

## 8. 所有 Object 的统一 API

每个 Object 都通过同一个 `object.xxx(...)` 协议访问。变量名绑定 Object，在普通表达式中读出 Value；作为对象调用的目标时提供绑定身份。ID 也可以作为普通文本显式传递：

| API | 含义 |
|---|---|
| `object.id(target)` | 返回 ObjectId 文本 |
| `object.type(target)` | 返回类型名 |
| `object.parent(target)` | 返回父 ObjectId 文本或 `null` |
| `object.status(target)` | 返回生命周期和可用状态 |
| `object.inspect(target)` | 返回有权查看的描述 |
| `object.capabilities(target)` | 返回基础能力名称 |
| `object.value(target)` | 返回当前不可变 Value Snapshot |
| `object.replace(target, value)` | 原子替换 Value |
| `object.children(target)` | 返回可见子 ObjectId 文本数组 |
| `object.links(target)` | 返回命名 Link 到 ObjectId 文本的 Map |
| `object.link(source, name, target)` | 原子建立一个显式 Link |
| `object.unlink(source, name)` | 原子移除一个显式 Link |
| `name.capability(...)` | 对已经创建或发现并绑定到 `name` 的 Object 调用能力；按 Type 分发到内核能力、Class 方法或 Provider |

并不是每个 Object 都必须拥有相同的领域能力。统一的是基础协议和调用方式，不是行为本身。

```praxis
process.start()
connection.send(data)
display.present(frame)
sensor.sample()
```

Process、Network、Device 领域能力已经通过统一分发执行，例如：

```praxis
aaa = object.find("console")
aaa.println("hello")
```

## 9. Process、Network、Device 和数据如何统一

| 对象 | Value 保存什么 | Provider 做什么 | 典型领域能力 |
|---|---|---|---|
| 普通数据 | Text、Bytes、Array、Map、Record | 无或通用 Value Provider | `replace`、集合操作 |
| Program | TF、元数据、入口信息 | Program Provider | `execute`、`inspect` |
| Process | Token 位置、Stack、状态 | Scheduler/VM Provider | `start`、`wait`、`suspend`、`terminate` |
| Network Endpoint | 协议、地址、连接阶段、结果 | Network Provider | `connect`、`send`、`receive`、`close` |
| Device | 可持久配置、描述、最近状态 | Driver Provider | `present`、`sample`、`next_event`、`calibrate` |
| Collection | Array/Map Value 或分段索引 | Collection Provider | `get`、`set`、`insert`、`remove` |

这些对象使用相同的 ObjectId、权限、父子关系、Link、版本、事务、查询和恢复机制。

## 10. File 是否还存在

File 不作为 Ousject 核心本体存在。

传统 File 实际混合了多件不同的事：

```text
名字和路径
字节内容
持久化
访问权限
顺序读写游标
设备兼容接口
```

Ousject 将它们拆开：

| 传统 File 职责 | Ousject 对应模型 |
|---|---|
| 文件内容 | Object Value：Text 或 Bytes |
| 文件名 | Parent Object 的命名 Link |
| 目录 | Namespace/Collection Object |
| 权限 | Object Access Policy |
| 持久化 | OMS 默认持久提交 |
| 打开句柄 | 短期 Object Handle |
| 读写游标 | 调用者自己的 Cursor Object 或 Value |
| 特殊设备文件 | 直接使用 Device Object Capability |

例如传统的文本文件可以直接表示为：

```praxis
readme = object.create("core.text", "# Ousject")
object.link(project, "README", readme)
```

路径：

```text
/projects/ousject/README
```

只是 Namespace Object 逐级解析命名 Link，最终得到 ObjectId；它不证明底层存在传统文件。

为了兼容编译器、编辑器、Linux Driver 或 POSIX 工具，可以提供：

```text
File View Adapter
Object ↔ POSIX File View
```

File View 是边界兼容层，不是内核对象模型。

## 11. 原子提交和外部 Effect

普通 Value 修改继续遵守：

```text
Old Value
    ↓
Candidate Value
    ↓
Durable Atomic Commit
    ↓
Publish New Version
```

Process 的 Token Position、Stack 和它修改的对象可以共同提交。

Network 发送、显示画面和设备动作无法像普通 Value 一样回滚，因此统一使用 Effect Intent：

```text
提交 Effect Intent
    ↓
Effect Worker 调用 Provider
    ↓
提交 Effect Result
```

这样系统可以跟踪 Pending、Succeeded、Failed 和 Unknown，而不是假装外部世界可以回滚。

## 12. 性能规则

统一对象模型不能意味着所有操作都走昂贵的动态路径。

实现必须遵守：

1. 小 Value 内联存储，不为每个数值和集合元素分配 ObjectId。
2. Object Header 与大型 Value 分离，Header 可以常驻缓存。
3. 大型 Value 分段并使用 Copy-on-Write。
4. Type 和 Capability 在运行时使用数值 ID；字符串只用于源码和诊断。
5. 常用 Capability Dispatch 缓存在 Object Handle 中。
6. `object.query` 使用 Type、Capability、Parent 和 Name 索引，不扫描全部对象。
7. 只读操作取得不可变 Snapshot，不持有全局锁。
8. 写入使用版本检查和乐观并发控制。
9. Provider 不能阻塞 Object Shard 的提交线程；外部 I/O 通过 Effect Queue 执行。
10. Network Buffer、图像帧和大型 Bytes 可以使用分段、共享或零拷贝 Handle，但持久格式仍然只保存稳定身份和状态。

## 13. 推荐的 Praxis 表面语法

普通赋值仍然保持简单：

```praxis
count = 1
name = "Ousject"
```

编译器自动创建属于当前 Process 的 Variable Object，右侧内容成为它的 Value。

需要独立身份时显式创建：

```praxis
document = object.create("core.text", "hello")
```

需要共享关系时显式 Link：

```praxis
shown_document link document
```

需要系统行为时调用 Capability：

```praxis
process = object.create("core.process", {
}, {
    links: {
        program: program
    }
})
process.start()
```

## 14. 第一阶段实现顺序

1. ✅ 扩展 TF `Value`：Float、Bytes、Array、Map、Record、Error。
2. ✅ 引入 Type Descriptor Registry、内置类型 ID 和持久动态 Type Object。
3. ✅ 实现 `object.create/find/query`；OMS、CLI 和 Praxis 已接入。
4. ✅ 实现统一 Object 基础 API 和通用 Capability Provider。
5. ✅ 将 OMS 权限名 `Read/Write` 迁移为 `ViewValue/ReplaceValue`，并增加 `CreateChild/Invoke` 权限。
6. ✅ 将现有 Program、Process、Variable 改为注册的稳定 TypeId。
7. ✅ 实现 Namespace Object，用原子命名 Link 和路径解析代替核心 File/Directory。
8. ✅ 实现 TCP Network Provider 和持久 Effect Intent。
9. ✅ 定义 Driver Provider 接口，让宿主适配器发布 Device Object。
10. ❌ 最后提供可选 File View Adapter，服务外部兼容，不污染内部模型。

当前实现进度：

- ✅ 统一递归 Value 及稳定编码。
- ✅ 内置与动态 Type Registry、Schema、创建策略和 Type 索引。
- ✅ OMS/CLI `create/find/value/replace/query`。
- ✅ 统一基础权限和通用领域 Capability Provider。
- ✅ Program、Process、Variable 使用注册的稳定 TypeId。
- ✅ `core.namespace`、原子 bind/unbind 和路径解析。
- ✅ Network Provider、Driver Provider 和 Effect Intent；File View Adapter 仍是可选兼容层，不属于核心需求。
- ✅ Praxis `object` 与 Object 方法表面语法；旧复数 `objects` 已删除。

## 15. 不变原则

1. Object 是唯一可独立管理的系统实体。
2. Object 可以保存 Value，但 Value 不必都是独立 Object。
3. Object Type 可以选择，但必须已注册且通过创建权限检查。
4. Capability 由 Type 和 Provider 安装，普通调用者不能伪造。
5. Process、Network、Device 和数据共享 Object 基础结构。
6. File 不是基础本体，只是可选兼容视图。
7. 物理 Device 由驱动发布，应用通过 Registry 发现。
8. 普通状态修改必须原子、持久且提交后可见。
9. 外部 Effect 必须被记录和跟踪。
10. 统一语义不能以全局锁、每值一个 Object 或强制磁盘 I/O 为代价。
