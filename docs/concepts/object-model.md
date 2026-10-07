# 对象模型

Ousject 用同一套 Object 语义管理普通数据、Program、Process、用户、Namespace、IPC 和
设备服务。“万物皆对象”指所有需要独立身份与管理的实体遵循同一协议，不表示每个整数、
字符或集合元素都有独立 Object ID。

## Object 与 Value

Object 是具有稳定身份、权限和生命周期的实体；Value 是 Object 当前携带的内容。

```text
Object
├── ObjectId
├── TypeId
├── ParentId
├── Version
├── Lifecycle
├── Value / encoded state
├── Children
├── Named links
├── Capabilities
└── Owner and grants
```

当前统一 Value 支持：

```text
Null
Bool
Integer(i64)
Float(f64 bit pattern)
Text
Bytes
Array<Value>
Map<String, Value>
Record<String, Value>
Error { code, message }
```

Value 使用预发布的 `OVL0` 编码。Object ID 在 Value 中以文本表示；受管理关系使用 Parent、
Child 或命名 Link，不存在可持久化的内存 Pointer。

## 身份与版本

`ObjectId`、`TypeId`、`TransactionId` 和 `SubjectId` 都是 128 位标识，显示为 32 位十六进制
文本。Object 的身份不会因状态更新、Checkpoint 或重启而改变。

每个成功修改的既有 Object 都递增 `ObjectVersion`。事务修改既有对象时必须声明期望版本；
若实际版本已经变化，提交返回 `Conflict`，不会覆盖并发结果。

## Type

`TypeDescriptor` 定义：

- 稳定 Type ID 与名称；
- Value schema；
- `Public` 或 `ProviderOnly` 创建策略；
- 基础 Capability；
- 领域能力名称。

内建数据类型包括 `core.value`、`core.text`、`core.bytes`、`core.collection` 和
`core.namespace`。Program、Process、Session、Effect、系统服务与硬件类型通常是
`ProviderOnly`。

可信系统身份可以注册持久的动态 Type。注册本身创建 Type Descriptor Object，因此也受
事务和恢复规则保护。

## 创建规则

普通数据对象通过统一 Registry 创建：

```praxis
note = object.create("core.text", "hello")
settings = object.create("core.collection", { theme: "dark" })
```

创建流程会先解析 Type，再检查创建策略和 Value schema。调用者不能通过传入任意能力名称
绕过 Type 与 Provider 规则。

物理设备、Process 和其他 Provider-owned 类型走可信 Provider 或 VM 路径。例如 Praxis
创建子 Process 时，VM 校验并初始化 Process 状态，而不是接受任意伪造的内部 Record。

## 关系

### Parent 与 Child

每个 Object 最多有一个 Parent。Parent 保存 Children 集合，事务同时维护关系两端，并拒绝
Parent 环。创建 Child 需要 Parent 的 `CreateChild` 权限。

Parent 表达隶属与生命周期结构，不表示内存包含关系。

### Link

Link 是从一个 Object 到另一个 Object 的命名关系：

```praxis
root = object.create("core.namespace", {})
note = object.create("core.text", "hello")
root.link("note", note)
linked = object.find(root.links["note"])
```

Link 保存目标 Object ID。更新和删除 Link 属于 OMS 事务；Namespace 还会检查合法名称。
普通赋值复制 Value，不等同于建立 Link。

## Capability 与授权

OMS 基础 Capability 包括：

| Capability | 含义 |
|---|---|
| `ViewValue` | 读取 Object Value |
| `ReplaceValue` | 替换 Value |
| `CreateChild` | 在该 Object 下创建 Child |
| `Invoke` | 调用对象能力 |
| `Link` | 修改命名 Link |
| `Reparent` | 修改隶属关系 |
| `Retire` | 退役 Object |
| `Inspect` | 查看 Header 与允许公开的元数据 |
| `ManagePolicy` | 管理授权 |

每个 Object 有 owner，并可向其他 `SubjectId` 授予部分 Capability。一次操作同时要求：

1. Object 本身公布该 Capability；
2. 当前 Subject 是系统身份、owner，或拥有对应 grant；
3. Object 生命周期允许该操作。

`local` 对应可信本机身份。普通用户 Process 携带独立、持久的 Subject ID。

## 生命周期

源码定义以下状态：

```text
Creating → Active ↔ Suspended
              ↓
          Migrating / Terminating → Tombstoned
```

并非所有状态转换都已经作为公共 Praxis API 暴露。退役后 Object 进入 `Tombstoned`；除
`Inspect` 外的普通能力被拒绝。

持久 Store 当前保留 Tombstone 内容 7 天，然后回收较大的状态，同时保留必要元数据，避免
把旧 ID 重新解释成新对象。涉及未完成 Effect 的对象不能被不安全地清理。

## Praxis 中的统一访问

```praxis
terminal = object.find("terminal")
terminal.println("hello")

note = object.create("core.text", "first")
note.replace("second")
terminal.println(note.value)
```

`object` 负责创建、发现和查询；获得对象后，通过公共属性和 `name.capability(...)` 调用统一
分发。VM 根据 Type、Provider、Object Capability 和 Subject 授权共同决定调用是否成立。

## 不应作出的假设

- Object ID 不是 RAM 地址、文件路径或设备编号。
- Value 内的文本 ID 不自动产生受管理关系。
- Type 名称不自动授予权限。
- Object 持久存在不代表它对应的宿主设备当前已连接。
- “统一模型”不等于所有 Type 都支持同一组领域能力。
