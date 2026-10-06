# Praxis

**Praxis** 是面向 Ousject 的高级编程语言。

Praxis 与 Ousject 共享同一个核心原则：

> **万物皆对象。**

Praxis 源代码经过编译后生成 TF，其中的 Token Stream 最终由 Ousject 执行。

```text
Praxis
   ↓
Compiler
   ↓
TF
   ↓
Token Stream
   ↓
Ousject
```

本文档定义 Praxis 的基础语言语义和表层语法。

---

## 1. Object 是语言基础

在 Praxis 中，程序能够操作的实体都属于 Object 世界。

例如：

```text
Variable
Class Instance
Array
Map
Function
Process
Network
Device
Program
```

它们可以具有不同的结构和能力，但都遵循相同的对象模型。

---

## 2. Object 的组成

一个 Object 在语义上包含：

```text
Object
├── 身份
├── 隶属关系
├── 状态
├── 能力
└── 生命周期
```

Praxis 不把 Object 定义为：

```text
一段内存
```

也不把它定义为：

```text
一个地址
```

Object 是语言和系统中的实体本身。

---

## 3. 对象的隶属关系

Object 可以属于另一个 Object。

例如：

```text
Process
├── Variable a
├── Variable counter
├── Network Object
└── Child Process
```

Praxis 中的变量本身也是 Object。

当：

```praxis
count = 10
```

发生时，当前 Process 中建立了一个名为 `count` 的 Variable Object。

因此：

```text
Process Object
└── Variable Object: count
```

Variable Object 隶属于当前 Process。

---

## 4. 不存在语言级 Pointer

Praxis 不提供：

```text
pointer
address-of
dereference
```

之类的语言模型。

不存在：

```text
&value
*pointer
```

这样的基础语义。

程序员不应该知道 Object 的内存地址。

---

## 5. 不存在 Reference 语义

Praxis 也不使用“赋值意味着两个名字隐式引用同一个对象”的模型。

例如：

```praxis
a = 46
b = a
```

`a = 46` 创建一个值为 46 的 `core.value` 对象并绑定 `a`；`b = a` 复制 46 并为 `b` 创建独立对象，不会偷偷建立 Pointer、Reference 或 Link。`note = object.create("core.value", 42)` 是可以指定类型的显式创建写法。

Praxis 不把程序行为建立在隐藏共享关系上。

需要建立明确关联时，应显式使用：

```praxis
link
```

---

## 6. link

`link` 是 Praxis 中显式表达名字关联的机制。

```praxis
b link a
```

这意味着：

> `b` 与 `a` 建立明确的 Link 关系。

这种关系是 Praxis 语言语义的一部分。

它不是：

- Pointer
- Reference
- Memory Address
- Rust Reference
- CPU Address

例如：

```praxis
count = 1
displayed link count

displayed++

console.println(count)
```

结果：

```text
2
```

如果之后：

```praxis
count = 100
```

Link 关系仍然存在。

---

## 7. 普通赋值与 link 不同

普通赋值：

```praxis
b = a
```

表示普通的值或对象赋值语义。

显式关联：

```praxis
b link a
```

表示两个名字之间存在持续的 Link。

Praxis 不让共享状态偷偷发生。

如果存在共享关系，它应该能够在源码中明确看到。

---

## 8. Variable 是 Object

Praxis 没有：

```text
var
let
```

变量直接通过赋值产生：

```praxis
count = 0
name = "Praxis"
enabled = true
```

这些 Variable 都是当前 Process 的子 Object。

例如：

```text
Process
├── count
├── name
└── enabled
```

---

## 9. 基本值

Praxis 的基本字面量包括：

```praxis
123
3.14
"hello"
true
false
null
```

这些值可以成为 Variable Object 的状态。

例如：

```praxis
count = 123
```

可以理解为：

```text
Variable Object: count
State: 123
```

---

## 10. 注释

Praxis 使用 `//` 单行注释：

```praxis
// comment
count = 10 // comment
```

`#` 不是注释符号。

---

## 11. 语句与代码块

Praxis 使用 `{}` 表示代码块。

语句可以通过换行分隔：

```praxis
a = 1
b = 2
c = a + b
```

也允许使用 `;`：

```praxis
a = 1; b = 2; c = a + b
```

---

## 12. Array

```praxis
items = [1, 2, 3]
```

访问：

```praxis
items[0]
```

修改：

```praxis
items[0] = 10
```

Array 同样属于 Object 世界。

---

## 13. Map

```praxis
user = {
    name: "Ada",
    age: 18
}
```

访问：

```praxis
user["name"]
```

修改：

```praxis
user["age"] = 19
```

Map 字面量与 Class 实例可以拥有不同语义，但二者都属于 Object Model。

---

## 14. 运算

算术：

```praxis
a + b
a - b
a * b
a / b
a % b
```

比较：

```praxis
a == b
a != b
a < b
a <= b
a > b
a >= b
```

布尔：

```praxis
a and b
a or b
not a
```

也可以支持符号形式：

```praxis
a && b
a || b
!a
```

---

## 15. 长度运算符

`#` 表示长度：

```praxis
#items
#name
```

例如：

```praxis
items = [1, 2, 3]
length = #items
```

---

## 16. 条件

```praxis
if value > 10 {
    console.println("large")
} else {
    console.println("small")
}
```

也可以：

```praxis
if (value > 10) {
    ...
}
```

支持：

```praxis
else if
```

---

## 17. while

```praxis
count = 0

while count < 10 {
    count++
}
```

支持：

```praxis
break
continue
```

---

## 18. 自增与自减

支持后缀形式：

```praxis
count++
count--
```

也可以作用于字段和索引：

```praxis
counter.value++
items[0]--
```

不存在前缀：

```text
++count
--count
```

---

## 19. 函数

使用 `func`：

```praxis
func add(a, b) {
    return a + b
}
```

调用：

```praxis
result = add(1, 2)
```

无参数：

```praxis
func hello() {
    console.println("Hello")
}

hello()
```

---

## 20. return

```praxis
return value
```

也可以：

```praxis
return
```

---

## 21. Class

Class 描述一种 Object 可以拥有的：

- 初始状态
- 字段
- 能力
- 行为

例如：

```praxis
class Counter {
    value = 0

    func increment() {
        this.value++
    }
}
```

字段不需要任何声明关键字：

```praxis
class User {
    name = ""
    age = 0
}
```

---

## 22. 创建 Object

Class 只定义 Object 的类型、字段和能力。创建仍统一经过 Object Registry：

```praxis
counter = object.create("Counter", {})
user = object.create("User", { name: "Ada", age: 18 })
```

Praxis 不提供第二套 `new` 创建机制。第二个参数用于覆盖 Class 字段的默认值。

---

## 23. 初始化

没有依赖 `new` 的特殊 `init` 构造器。初始字段由 `object.create` 原子写入；需要额外行为时，创建后显式调用普通能力：

```praxis
user = object.create("User", { name: "Ada" })
user.activate()
```

---

## 24. this

`this` 表示当前正在执行能力的 Object。

```praxis
class Counter {
    value = 0

    func increment() {
        this.value++
    }
}
```

---

## 25. public 与 private

成员默认公开。

可以显式：

```praxis
public class User {
    private password = ""

    public func name() {
        return "User"
    }

    private func verify() {
    }
}
```

---

## 26. 继承

Praxis 支持单继承：

```praxis
class Admin extends User {
    ...
}
```

父类方法：

```praxis
super.method(...)
```

---

## 27. 方法重载

Praxis 可以根据参数数量区分同名方法：

```praxis
class Message {
    func show(text) {
        console.println(text)
    }

    func show(title, text) {
        console.println(title + ": " + text)
    }
}
```

---

## 28. try / catch

```praxis
try {
    risky()
} catch (error) {
    console.println(error)
}
```

错误也可以作为 Object 值参与系统。

---

## 29. 对象能力

`object` 只负责创建、发现和查询。获得 Object 后，通过 `name.property` 读取公共属性，通过 `name.capability(...)` 调用能力。

Ousject 的系统 Object 同样通过能力进行操作。

所有 Object 都遵循同一个基础协议：

```text
id              只读属性：稳定 Object 身份
type            只读属性：Type 名称
parent          只读属性：父 Object
status          只读属性：生命周期和可用状态
owner           只读属性：Owner Subject
version         只读属性：当前 Object 版本
permissions     只读属性：有权管理时可见的授权关系
inspect         只读属性：当前调用者有权查看的描述
capabilities    只读属性：当前调用者可以调用的能力
value           只读属性：Object 的本质内容
children        只读属性：可见的子 Object
links           只读属性：可见的 Link 关系
replace         能力：原子替换 Value
link            能力：原子建立一个显式 Link
unlink          能力：原子移除一个显式 Link
retire          能力：原子退役 Object 及其子 Object，并清理显式名字和 Link
```

这些属性和能力使用统一的小写名称，并且受权限控制。Process 的 `value` 是可读运行状态；`process.variables` 返回变量值，`process.x` 可直接读取变量 `x`。

能力调用的规范形式是：

```praxis
result = target.capability_name(argument)
```

对象必须先通过 `object.create/find/query` 创建或发现并绑定变量名，随后才用 `变量名.能力(...)` 调用。Console、Network、Display、Keyboard、Block Storage、Time 和 DNS Resolver 都通过对象能力调用；时间不是伪装成传感器的设备。

系统提供一个预绑定的 Object Registry Object：

```praxis
object
```

它使用小写名称，因为它是一个已经存在的对象实例，不是 Class。它提供统一的对象入口：

```praxis
found_id = object.find(object_id)
matches = object.query(type_name, capability_name)
note = object.create(type_name, initial_value)
note.retire()
```

`object.create` 直接赋给变量时绑定新 Object；在其他表达式中返回普通 ID 文本。`object.find/query` 返回普通 ID 文本。获得 Object 后通过 `name.id`、`name.value` 等属性读取信息，通过 `name.retire()` 等能力执行操作。物理 Device 由驱动发现并发布到 Object Registry。

命名规则是：

- 运行时对象、变量和能力：`lower_snake_case`。
- 用户声明的 Class：`PascalCase`。
- 不再使用 `Process.create`、`Network.create`、`Device.open` 这种混合的静态入口。
- 对象能够做什么，由它公布的 Capability 决定，而不是由类似文件的固定接口决定。

---

## 30. Process Object

Process 是 Object。

可以创建同一 Program 中以无参数函数为入口的子 Process：

```praxis
channel = object.create("Channel", {})
process = object.create("core.process", {
    entry: "worker",
    links: { channel: channel.id }
})
```

然后调用能力：

```praxis
process.start()
result = process.wait()
process.suspend()
process.resume()
process.terminate()
state = process.status
```

当前进程可通过 `object.find("process")` 发现自己，通过 `object.find("program")` 发现自己的 Program。`wait()` 会运行目标 Process，直到它停止或到达运行步数安全上限，并返回 `halted`、`failed` 等状态。`process.result` 是正常结束时的结果；失败时用 `process.error` 查看错误。进程间共享状态必须通过 `links` 显式传入，子进程再用 `object.find(link_name)` 发现。

Process 可以具有的典型能力包括：

```text
start
wait
suspend
resume
terminate
status
inspect
```

具体能力和权限由系统决定。

---

## 31. Network Object

网络通过 Object 表示。

例如：

```praxis
network = object.create("net.endpoint", {
    transport: "tcp"
})
```

然后：

```praxis
network.connect("example.com", 443)
network.send(data)
data = network.receive()
network.close()
```

Network Object 可以具有：

```text
connect
listen
accept
send
receive
close
status
```

等能力。

---

## 32. Device Object

设备本身就是由驱动发布的 Object，不是必须套用 File API 的特殊文件。时间和 DNS 是系统服务，不属于设备。

程序通过统一的 Object Registry 查找设备：

```praxis
display_id = object.query("device.display", "present")[0]
display = object.find(display_id)

keyboard_id = object.query("device.keyboard", "capture")[0]
keyboard = object.find(keyboard_id)
keyboard.capture()
```

然后调用设备实际公布的能力：

```praxis
display.present(frame)
display.configure(display_mode)

key_event = keyboard.poll_event()
keyboard.release()
```

这些设备领域调用由宿主硬件适配器提供；没有发现对应硬件时，`object.query` 不会伪造设备。它们不提供隐式对象方法简写。

键盘提供 `capture/release/next_event/poll_event`，与 `console.read_line/read_secret` 共用独占输入源。事件记录包含 `key`、`text`、`pressed`、`ctrl`、`alt`、`shift`，覆盖 Unicode、方向键、导航键和 F1–F12；终端通常只提供按下事件，不能可靠提供按键释放事件。块存储 API 仅供内核和驱动使用，普通 Praxis 程序通过更新 Object 保存数据。具体 Device 能力取决于设备本身；Praxis 不要求所有设备都实现 `open/read/write`。

## 常用系统对象

```praxis
console = object.find("console")
dimensions = console.size() // Record: columns、rows
interactive = console.is_interactive()
console.print("prompt> ")
console.println("hello")

time = object.find("time")
unix_milliseconds = time.now()
uptime_milliseconds = time.monotonic()
time.sleep(100) // 等待 100 毫秒

math = object.find("math")
fraction = math.random()                 // [0, 1)
die_roll = math.random_integer(1, 6)     // 含 1 和 6

resolver = object.find("resolver")
addresses = resolver.resolve("example.com")

name = "  Praxis  "
trimmed = name.trim()
clean_name = trimmed.lower()
```

文本 Value 直接提供 `slice(start, length)`、`find(text)`、`contains(text)`、`split(separator)`、`replace_all(from, to)`、`trim()`、`lower()` 和 `upper()`；`slice` 的位置按 Unicode 字符计。

---

## 33. Program Object

Program 本身也是 Object。

可以具有：

```text
execute
inspect
instantiate
```

等能力。

例如：

```praxis
process = program.execute()
```

`execute()` 和 `execute("entry")` 已实现，分别从 Program 起点或指定无参数入口创建 Process Object。

可以产生一个新的 Process Object。

---

## 34. Collection Object

`core.collection` 的值可以是 Array 或 Map；它们和普通变量使用同一套操作，没有额外的 `get/set/length/insert/remove` API。

```praxis
items = object.create("core.collection", [1, 2, 3])
first = items[0]             // 读取数组元素
items[1] = 9                 // 改写已有数组位置
size = #items                // 读取长度
items.replace([7, 9, 11])    // 整体替换；数组长度可随新值改变

settings = object.create("core.collection", {"theme": "dark"})
settings["theme"] = "light" // Map 可读写键值
settings["font"] = "large"  // 也可新增键
```

---

## 35. 能力属于 Object

Praxis 倾向于：

```praxis
process.terminate()
network.send(data)
display.present(frame)
```

而不是：

```praxis
process.terminate()
network.send(data)
display.present(frame)
```

因为能力属于 Object。对象创建或发现并绑定名字后，领域调用写成 `名字.能力(...)`；不同对象不必假装拥有同一组领域能力。已注册 Provider 的领域能力也走这条路径。

---

## 36. 能力可以失败

统一对象模型不意味着所有操作永远成功。

例如：

```praxis
try {
    network.send(data)
} catch (error) {
    console.println(error)
}
```

Network Object 仍然可能：

- 断开
- 超时
- 被拒绝
- 远端失效

Device Object 也可能发生硬件错误。

---

## 37. 能力受到权限控制

一个 Object 具有某项能力，并不意味着当前 Process 一定有权调用。

例如：

```praxis
process.terminate()
```

可能因为权限不足而失败。

因此：

```text
Object Capability
```

与：

```text
Caller Permission
```

是两个不同概念。

---

## 38. 子对象

一个 Object 可以创建属于自己的 Object。

例如：

```praxis
child = object.create("core.process", {}, {
    links: {
        program: program
    }
})
```

可能形成：

```text
Parent Process
└── Child Process
```

同样：

```praxis
connection = object.create("net.endpoint", {
    transport: "tcp"
})
```

可以形成：

```text
Process
└── Network Object
```

这种隶属关系由 Ousject 管理。

---

## 39. 生命周期

当前 `retire` 采用明确的树形退役规则：目标 Object 及其全部子 Object 一起退役。系统同时删除 Process 变量中的所有对应名字、所有指向这些 Object 的显式 Link，以及父子关系。整个动作与当前 Process 的下一个 Token 位置处在同一个 OMS Transaction 中；任一权限、版本、存储或分片检查失败，都不会留下半删除状态。

退役不是抹掉记录。稳定 ObjectId 对应的 Tombstone 会保留，ID 永不复用；当前实现也保留退役时的编码状态，但普通 Value/领域能力不再可用。之后再次给旧变量名赋值，会创建并绑定一个全新的 Object。

任意 Value 中碰巧出现的 ID 文本不是 Link，系统不会猜测并改写它。需要可追踪关系时必须使用 `link`。当前运行中的 Process 不能退役自身，应使用 `terminate()`。跨 Shard 清理使用同一个全局候选和持久提交边界，完整成功或完整回滚。

---

## 40. Object 修改默认持久化

Praxis 中对持久 Object 的修改默认具有持久语义。

例如：

```praxis
counter.value++
```

并不是：

```text
先修改 RAM
以后再尝试保存
```

而是逻辑上：

```text
旧正式状态
   ↓
生成候选状态
   ↓
提交到 Ousject 持久对象空间
   ↓
原子提交成功
   ↓
新状态正式可见
```

因此：

> **修改只有在持久提交成功之后，才真正发生。**

---

## 41. 修改失败时状态不变

假设：

```praxis
counter.value = 10
```

修改之前：

```text
counter.value = 9
```

如果持久提交成功：

```text
Persistent State = 10
Visible State = 10
```

如果持久提交失败：

```text
Persistent State = 9
Visible State = 9
```

Praxis 不会观察到一个“RAM 已经变成 10，但持久状态仍然是 9”的正式状态。

因此一次对象修改：

> **要么完整成功，要么完全不发生。**

---

## 42. Transaction

多个对象修改可以组成同一个原子 Transaction。

例如：

```praxis
transaction {
    source.balance = source.balance - 10
    target.balance = target.balance + 10
}
```

事务成功：

```text
两个修改同时生效
```

事务失败：

```text
两个对象都保持旧状态
```

事务中间状态不会成为正式可见状态。

即使没有显式写出 `transaction`，Ousject 也可以在底层把一段相关修改合并为原子提交单元。

---

## 43. Process 状态与对象修改共同提交

Process 的执行位置本身也是持久状态。

因此一个 Process 在执行：

```praxis
counter++
```

时，系统可以把：

```text
counter 的新状态
Process 当前 Token Position
Process Stack
其他相关状态
```

一起提交。

这样系统重启之后不会出现：

```text
counter 已经 +1
但 Process 又从旧 Token 重新执行一次
```

这种错误。

---

## 44. Process Persistence

Process 本身是 Object，因此也可以持久化。

它的 Variable Objects：

```text
Process
├── variable a
├── variable b
└── variable state
```

也属于 Process 的持久状态之一。

系统恢复 Process 时，可以恢复整个所属对象结构。

---

## 45. link 也必须持久化

如果：

```praxis
displayed link count
```

已经成为程序状态的一部分，那么系统重启后 Link 关系也必须保持。

它不能因为 Object 被移动到 Storage 或 Process 被恢复而消失。

---

## 46. Object 不关心驻留位置

同样的 Praxis 代码：

```praxis
object.doSomething()
```

不应该因为 Object 当前位于：

```text
RAM
Storage
Network
Device
```

而使用完全不同的基础语法。

Ousject 负责完成必要的访问过程。

---

## 47. 外部副作用

不是所有能力调用都能像普通 Object State 一样回滚。

例如：

```praxis
network.send(data)
display.present(frame)
sensor.calibrate()
```

如果外部世界已经发生变化，本地事务失败不能自动撤销远端或物理世界。

因此 Praxis/Ousject 必须区分：

```text
Object State Change
```

与：

```text
External Effect
```

外部 Effect 应由系统进行持久记录、调度和结果跟踪，而不是假装它们具有普通对象字段修改一样的回滚能力。

---

## 48. import / include

完整的可执行 Praxis 源码必须声明一个无参数 `main()`。入口源码的顶层只能放
`func`、`class`、`import` 和 `include` 等声明，实际执行语句必须写进 `main()`：

```praxis
import "math"

func main() {
    console = object.find("console")
    console.println(double(21))
}
```

入口源码缺少 `main()`、声明多个 `main()`，或给 `main()` 添加参数都会在编译期报错。
交互终端的逐段提交不是完整程序，因此不要求每次输入都声明 `main()`。

顶层可以导入源码：

```praxis
import "module"
include "source"
```

这些属于编译组织能力，不改变 Ousject 的 Object Model。

`import` 对同一路径只展开一次，`include` 每次都会展开。CLI 从主源码所在目录加载；省略扩展名时自动使用 `.px`。循环加载会在编译期报错。

被导入的源码是 Module，不允许声明 `main()`。Module 可以包含顶层初始化语句；这些语句按导入顺序执行。全部导入初始化完成后，系统才调用入口源码的 `main()`。Module 中声明的函数和 Class 可以直接在 `main()` 中使用。

```praxis
// math.px
factor = 2

func double(value) {
    return value * factor
}
```

```praxis
// main.px
import "math"

func main() {
    result = double(21)
}
```

---

## 49. Praxis 与 Token Stream

例如：

```praxis
process.terminate()
```

在语言层表示：

> 调用 Process Object 的 terminate 能力。

Compiler 会将其转换成对应 TF Token。

概念上可能类似：

```text
LOAD_OBJECT process
CALL_CAPABILITY terminate
```

但 Praxis 规范不规定真实 Opcode。

真正的 Token 编码由 TF 规范决定。

---

## 50. Praxis 与 Ousject 的边界

Praxis 定义：

```text
语言语义
对象操作
能力调用
link
控制流
Class
```

Ousject 定义：

```text
Object 实际存在
Object 隶属关系
能力执行
Process 调度
驻留
持久化
原子提交
设备
权限
```

TF / Token Stream 位于两者之间。

---

## 51. 核心原则

Praxis 必须长期遵循：

1. 万物皆对象。
2. Variable 本身也是 Object。
3. Variable 通常隶属于当前 Process Object。
4. Object 可以存在父子和其他隶属关系。
5. Pointer 不属于 Praxis。
6. Reference 不属于 Praxis。
7. 普通赋值不能制造隐藏 Reference。
8. 显式共享关系使用 `link`。
9. `link` 不是 Pointer 或 Reference。
10. Object 可以拥有能力。
11. 系统 Object 通过和普通 Object 一致的能力调用形式使用。
12. Process 是 Object。
13. Network 是 Object。
14. Device 是 Object。
15. Program 是 Object。
16. Object 当前是否位于 RAM 不改变语言语义。
17. Object 是否持久化不改变正常访问方式。
18. 持久 Object 修改默认进行同步。
19. 修改只有在持久提交成功后才正式可见。
20. 提交失败时，内存与持久状态都保持旧值。
21. 多个修改可以组成原子 Transaction。
22. Process 执行状态可以与 Object 修改共同提交。
23. Process 的变量和其他子对象可以与 Process 一同持久化。
24. `link` 关系必须能够随 Process/Object 状态持久化。
25. 外部 Effect 与普通 Object State Change 必须明确区分。
26. Praxis 不依赖具体 CPU 架构。
27. 所有 Praxis 行为最终都必须能够编译成 TF Token Stream。

---

## 52. Praxis 的核心模型

Praxis 中最基本的关系不是：

```text
Variable
→ Pointer
→ Memory
```

而是：

```text
Process Object
    │
    ├── Variable Object
    │       └── State
    │
    ├── Network Object
    │       └── Capabilities
    │
    ├── Child Process Object
    │       └── Capabilities
    │
    └── Other Objects
```

必要的显式共同状态关系使用：

```praxis
link
```

而持久 Object 的修改遵循：

```text
Old State
   ↓
Candidate State
   ↓
Atomic Persistent Commit
   ↓
Publish
   ↓
New State
```

程序只需要面对：

> **Object、Object 的隶属关系、Object 能做什么，以及修改是否成功。**

至于对象当前存在于 RAM、Storage、Network 还是 Device，由 Ousject 负责。
