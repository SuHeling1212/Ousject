# Ousject

> Ousject 系统级最小运行版本已经可用。系统程序先安装为 Object，再从 Praxis `init` 启动：`./scripts/ousject system-install system --local`，随后运行 `./scripts/ousject boot`。开发运行单个程序需显式 `--local` 或有效 `--session`。详见 [SYSTEM-QUICKSTART.md](./SYSTEM-QUICKSTART.md)。

当前真正可运行和可调用的功能清单见 [IMPLEMENTED-FEATURES.md](./IMPLEMENTED-FEATURES.md)；设计目标与现状在该文档中明确分开。

脱离宿主之前已经完成的边界见 [PRE-HOST-ACCEPTANCE.md](./PRE-HOST-ACCEPTANCE.md)；不牺牲已提交信息的性能优化顺序见 [PERFORMANCE-PLAN.md](./PERFORMANCE-PLAN.md)。

将 Console 输入、`local` 最高用户和全部系统管理迁移到 Praxis 的实施顺序见 [PRAXIS-SYSTEM-CONTROL-PLAN.md](./PRAXIS-SYSTEM-CONTROL-PLAN.md)。

Process 的创建、调度、原子提交、硬件发现和恢复流程见 [PROCESS-RUNTIME.md](./PROCESS-RUNTIME.md)。

Rust 内核与 Praxis 用户空间的明确边界见 [KERNEL-USERSPACE.md](./KERNEL-USERSPACE.md)。

Object、Value、Type、Process、Network、Device 与传统 File 的统一设计见 [UNIFIED-OBJECT-MODEL.md](./UNIFIED-OBJECT-MODEL.md)。

**Ousject 是一个以对象、统一访问、Token Stream 与原生持久化为核心的操作系统。**

Ousject 主要使用 Rust 编写。

Ousject 的基本出发点非常简单：

> **万物皆对象。**

Process 是对象。

变量是对象。

Program 是对象。

网络是对象。

设备是对象。

数据是对象。

传统 File 不再是系统基础类型；需要兼容时，它只是 Object 的外部 File View。

对象不只是数据容器。一个对象同时具有：

- 身份
- 状态
- 隶属关系
- 能力
- 生命周期

Ousject 的目标，是让整个操作系统建立在这一套统一模型之上。

---

## 万物皆对象

传统操作系统会把很多东西划分成完全不同的类别：

```text
Process
Memory
File
Socket
Device
Service
```

Ousject 不认为这些差异应该决定整个系统的基本访问模型。

在 Ousject 中，它们首先都是：

```text
Object
```

不同类型的 Object 可以拥有不同的状态和能力，但它们依然属于同一个对象世界。

例如：

```text
Process Object
Network Object
Device Object
Program Object
Variable Object
Data Object
```

---

## 对象具有隶属关系

Ousject 中的 Object 不是一堆彼此孤立的实体。

对象可以隶属于其他对象。

例如：

```text
System Object
│
├── Process A
│   ├── Variable x
│   ├── Variable user
│   ├── Network Object
│   └── Child Process
│
└── Process B
    ├── Variable state
    └── Device Object
```

这里 `Variable x` 是 Object，`Process A` 同样也是 Object。

Variable x 隶属于 Process A。

Process A 又处于更大的系统对象世界中。

对象的隶属关系属于系统语义，而不是物理内存布局。

---

## 隶属关系不是内存关系

对象 A 隶属于对象 B，并不意味着：

```text
A 位于 B 的内存区域中
```

也不意味着：

```text
B 保存着一个指向 A 的指针
```

隶属关系描述的是对象之间的系统关系。

物理存储位置属于实现细节。

即使 Process 的一部分状态当前位于 RAM，而某个 Variable Object 的持久状态当前位于存储设备，它们的隶属关系也不会改变。

---

## 不以指针和引用作为对象模型

Ousject 不把：

```text
pointer
reference
memory address
```

作为对象世界的基本抽象。

程序不应该因为一个对象移动到了另一个物理位置，就需要改变自己访问它的方式。

对象的身份与：

- RAM 地址
- 物理页
- 磁盘位置
- 网络地址

无关。

底层 Rust 和硬件实现当然可能需要真实机器地址，但这些属于实现细节，不构成 Ousject 的对象语义。

> **对象不是一个地址。**

---

## 对象拥有能力

Object 不只是数据。

Object 可以拥有能力。

能力表示：

> **这个对象能够做什么。**

例如，一个 Process Object 可以具有：

```text
start
wait
suspend
resume
terminate
inspect
```

一个 Network Object 可以具有：

```text
connect
listen
accept
send
receive
close
```

一个 Device Object 可以具有：

```text
present
sample
next_event
calibrate
load_block
store_block
```

这不是要求每个 Device 实现同一组操作。显示器、传感器、键盘和存储控制器只公布符合自身语义的能力；Device 不需要伪装成 File。

一个 Program Object 可以具有：

```text
execute
inspect
instantiate
```

一个 Collection Object 可以具有：

```text
get
set
insert
remove
length
```

一个对象只能执行自己实际拥有的能力。

因此：

```text
object.capability(...)
```

是 Ousject 中非常重要的操作形式。

---

## 能力属于对象

传统系统经常出现：

```text
kill(process)
send(socket, data)
present(display, frame)
```

Ousject 更倾向于：

```text
process.terminate()
network.send(data)
display.present(frame)
```

因为能力属于执行能力的对象本身。

对象是什么，以及对象能够做什么，被统一放在同一个模型中。

所有 Object 同时共享统一的基础协议，例如：

```text
id
type
parent
status
inspect
capabilities
value
replace
call
children
links
link
unlink
retire
```

对象先用 `object.create/find/query` 创建或发现并绑定变量名，再以 `变量名.能力(...)` 调用。Console Object 可写成 `aaa = object.find("console")`，随后调用 `aaa.println(value)`；系统、认证、用户、调度、编译、类型、Provider、Store、Network 和已发现设备都使用同一调用形式。

系统中的预绑定 Object Registry 使用小写名称 `object`，因为它是对象实例而不是类型：

```text
object = object.find(object_id)
object.query(criteria)
object.create(type_name, initial_value)
object.retire(target)
```

`object.retire(target)`（也可写作 `target.retire()`）会在一次原子事务中退役目标及其子 Object，清理所有 Process 变量别名和显式 Link，并保留不可复用的 Tombstone。任一步骤失败都不会产生半删除状态。

这取代 `Process.create`、`Network.create`、`Device.open` 等大小写和创建方式不统一的入口。

---

## 创建系统对象

程序可以创建新的 Object。

例如：

```text
创建 Process Object
创建 Network Object
创建 Data Object
创建 Collection Object
```

创建完成后，新对象会获得自己的：

- 身份
- 状态
- 隶属关系
- 能力
- 生命周期

例如，一个 Process 创建另一个 Process：

```text
Process A
    │
    │ create
    ▼
Process B
```

在默认情况下，新 Process 可以成为创建者 Process 的子对象。

---

## 对象能力与权限

拥有一个 Object，并不意味着能够执行它的所有能力。

能力本身可以受到权限控制。

例如，一个 Process Object 可能允许某个调用者：

```text
inspect
wait
```

但不允许：

```text
terminate
```

因此 Ousject 把以下问题区分开：

```text
对象是什么
对象拥有什么能力
当前调用者被允许使用哪些能力
```

---

## 统一访问

调用者首先应该关心：

> **这个对象是什么，以及它有哪些能力？**

而不是：

> **它现在到底在哪？**

一个对象可能：

- 正在 RAM 中
- 位于持久存储中
- 部分驻留
- 由某个设备提供
- 由网络提供
- 当前没有驻留

这些状态不应该改变对象本身的基本访问方式。

---

## 内存与存储属于同一个对象世界

Ousject 不把 RAM 与 Storage 当作两套完全不同的数据模型。

传统程序常常需要：

```text
读取
↓
反序列化
↓
创建内存结构
↓
修改
↓
序列化
↓
重新写入
```

Ousject 希望让对象在不同驻留层之间自然移动。

```text
Object
│
├── RAM
├── Cache
└── Persistent Storage
```

这些只是 Object 当前的物理状态。

对象本身没有因此变成另一个对象。

---

## 默认持久同步

Ousject 的 Object 修改默认具有持久语义。

当内存中的一个持久 Object 被修改时，系统不会先把新的状态正式暴露给程序，再在未来某个时间尝试写入磁盘。

Ousject 的逻辑顺序是：

```text
当前正式状态
    ↓
产生候选新状态
    ↓
写入持久存储
    ↓
校验
    ↓
原子提交
    ↓
发布新状态
```

因此：

> **修改只有在持久化提交成功后，才真正发生。**

如果提交失败：

```text
Persistent State = 旧状态
Visible Memory State = 旧状态
```

新的状态不会成为正式的内存状态。

如果提交成功：

```text
Persistent State = 新状态
Visible Memory State = 新状态
```

这意味着从程序视角看，一次对象修改要么完整成功，要么完全没有发生。

---

## 持久化先于可见性

Ousject 的默认原则是：

> **Persistence before visibility.**

RAM 中正式可观察到的持久 Object 状态，不应领先于已经完成的持久提交。

实现可以使用缓存、批处理、写合并和事务等方式优化性能，但这些优化不能改变语言和系统所承诺的原子语义。

多个修改可以被合并进同一个 Transaction：

```text
修改 Object A
修改 Object B
修改 Process 状态
        ↓
形成候选 Transaction
        ↓
持久提交
        ↓
全部同时发布
```

提交失败则全部保持旧状态。

---

## Process 是对象

Process 不是只能存在于 RAM 中的特殊结构。

Process 本身就是 Object。

一个 Process 可以拥有自己的：

```text
Process
├── Variables
├── Execution State
├── Network Objects
├── Child Processes
├── Temporary Objects
└── Other Objects
```

因此变量天然拥有所属 Process。

系统能够明确知道某个对象属于哪个 Process。

---

## Process 不以 RAM 为存在前提

一个 Process 可以正在运行，也可以：

- 等待
- 暂停
- 部分驻留
- 完全不驻留
- 已经持久化

当 Scheduler 决定再次运行它时，Ousject 只需要恢复执行所必需的状态。

> **Process 的身份与 Process 是否当前存在于 RAM 中无关。**

---

## Token Stream

Ousject 使用 **Token Stream** 作为标准程序执行形式。

高级语言不会被 Kernel 直接执行。

例如 Praxis：

```text
Praxis Source
      ↓
   Compiler
      ↓
      TF
      ↓
 Token Stream
      ↓
   Process
      ↓
   Ousject
```

TF 是程序的标准执行表示。

Token Stream 是 TF 中真正描述程序行为的核心内容。

---

## Token

每一个 Token 都表示一个确定的执行操作。

概念上可以包含：

```text
LOAD
STORE

CREATE_OBJECT
ACCESS_OBJECT
CALL_CAPABILITY

ADD
SUB
MUL
DIV

COMPARE

JUMP
JUMP_IF

CALL
RETURN

EXIT
```

具体 Token 集合由独立规范定义。

最重要的原则是：

> **每一个 Token 都必须拥有明确且稳定的语义。**

---

## Token Stream 是 Ousject 的字节码

Token Stream 可以理解为 Ousject 自己的标准字节码。

Praxis：

```text
高级语言
```

TF：

```text
程序容器
```

Token Stream：

```text
真正被执行的操作序列
```

因此：

```text
Praxis
   ↓
Compiler
   ↓
TF
   ↓
Token Stream
```

Ousject Kernel 不需要理解 Praxis。

---

## Process 与 Token Stream

Process 可以被理解为：

> **正在沿着 Token Stream 执行的对象。**

它需要保存自己的执行状态，例如：

```text
当前 Program
当前 Token 位置
执行状态
局部对象
所属对象
调用状态
等待状态
```

如果 Process 在一个安全状态被持久化，那么系统重启以后可以恢复这些信息，并继续执行。

---

## Process 状态与对象修改共同提交

Process 的 Token 位置、调用状态和它在执行过程中产生的 Object 修改不能彼此分离地持久化。

例如：

```text
Object A → 新状态
Object B → 新状态
Process Token Position → 812
Process Stack → 新状态
```

这些状态可以属于同一个 Transaction。

提交成功：

```text
新 Object 状态 + 新 Process 状态
```

同时生效。

提交失败：

```text
旧 Object 状态 + 旧 Process 状态
```

继续存在。

不能出现：

```text
新 Object 状态 + 旧 Token Position
```

这种半提交状态。

这样可以避免系统恢复后重复执行已经生效的操作。

---

## 持久化操作系统

Ousject 是一个持久化操作系统。

持久化不是程序主动调用：

```text
save()
serialize()
writeFile()
```

之后才出现的附加行为。

持久状态属于系统自身的数据模型。

Object 可以跨越：

- 调度
- 内存驱逐
- Process 暂停
- 系统关机
- 突然断电
- 系统重新启动

继续存在。

---

## 突然断电

Ousject 从设计上假设：

> **电源可以在任何时刻消失。**

系统不能依赖正常关机来保持数据正确。

持久修改必须具有清晰的提交边界：

```text
完整提交
```

或者：

```text
没有提交
```

不能向系统暴露半完成状态。

如果设备在修改过程中突然断电，Ousject 应能够恢复到最后一个完整且有效的状态。

---

## 外部副作用

并不是所有现实世界行为都能像 Object State 一样回滚。

例如：

```text
network.send(...)
device.move(...)
display.present(...)
```

一旦远端主机已经收到数据、硬件已经动作，软件无法通过回滚本地状态让外部世界自动恢复。

因此 Ousject 必须区分：

```text
Object State Change
```

与：

```text
External Effect
```

Ousject 可以先持久记录 Effect Intent，再在提交后执行外部 Effect，并记录执行结果。

这样系统能够恢复和追踪外部副作用，而不会假装不可撤销的现实操作能够像普通对象字段一样回滚。

---

## 软件层面的持久安全

这种能力由 Ousject 自己的软件设计提供。

它不依赖：

- UPS
- 电池
- 特殊掉电保护硬件
- 永不丢失的设备缓存

软件无法保护从未到达系统的数据。

但是：

> **一旦 Ousject 确认某个状态已经持久化，该状态就必须可以在突然断电之后恢复。**

---

## File 不是基础本体

Ousject 不要求：

```text
File + Path
```

成为所有持久数据的共同基础。

Process 可以直接作为 Object 存在。

Program 可以直接作为 Object 存在。

配置可以直接作为 Object 存在。

普通数据也可以直接作为 Object 存在。

需要与传统工具交换数据时，可以临时提供 File View，用于：

- 与其他系统交换数据
- 文档
- 图片
- 视频
- 外部存储
- 传统文件系统兼容

但 Ousject 内部不创建核心 File 类型。文档、图片、视频和其他内容分别作为 Text、Bytes、Collection 等 Object Value 存在；路径由 Namespace Object 的命名 Link 解析。

当前实现已经支持 `core.namespace`：

```text
Namespace Object
├── "README" → Text Object
├── "assets" → Namespace Object
└── "server" → Process Object
```

命名绑定、移除和路径解析使用原子持久 Link，同一套机制可以命名任何 Object，而不仅是字节内容。

---

## 网络是对象

网络连接和网络资源同样可以作为 Object。

Network Object 可以拥有：

```text
connect
listen
accept
send
receive
close
```

等能力。

程序只需要调用这个对象具备的能力。

网络本身仍然存在：

- 延迟
- 断开
- 丢包
- 远端失效

统一对象模型不会抹去这些现实问题。

---

## Device 是对象

硬件 Device 本身就是由驱动发布到 Object Registry 的 Object。

程序通过统一入口发现它，而不是把它当作 File 打开：

```text
display = object.query({ type: "device.display", capability: "present" })[0]
sensor = object.query({ type: "device.sensor", capability: "sample" })[0]
```

不同类型 Device 可以具有完全不同的领域能力，例如：

```text
display.present(frame)
sensor.sample()
keyboard.next_event()
storage.load_block(index)
```

`open/read/write` 不再是 Device 的统一接口。设备只公布符合自身真实行为的 Capability。

统一的是 Object Model、基础协议、权限检查和能力调用方式，而不是强迫设备行为相同。

---

## 硬件驱动

Ousject 不计划为现代计算机上的全部设备重新编写驱动。

Ousject 可以使用已有 Linux Hardware Driver 的设备控制实现。

Linux Driver 负责解决：

> **如何让具体硬件工作。**

包括：

- 初始化
- MMIO
- DMA
- IRQ
- Firmware
- Command Queue
- Reset
- 硬件协议

但是 Ousject 不使用 Linux 的实际操作系统逻辑。

Linux 不决定 Ousject 的：

- Process
- Scheduler
- Object Model
- Persistence
- Memory Model
- Token Execution
- System Services
- User Environment

可以概括为：

> **Linux Driver 解决硬件问题。**

> **Ousject 自己解决操作系统问题。**

---

## Rust

Ousject 主要使用 Rust 编写。

底层不可避免需要 `unsafe`，例如：

- Boot
- Page Table
- Interrupt
- Context Switch
- DMA
- MMIO
- Hardware Register

但是 `unsafe` 应被限制在清晰、狭窄并可审查的边界中。

---

## Praxis

Praxis 是面向 Ousject 的高级编程语言。

Praxis 与 Ousject 使用相同的基本对象哲学：

> **程序中的实体也是对象。**

Praxis 通过 TF 和 Token Stream 运行：

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

Kernel 不需要理解 Praxis。

---

## 核心设计原则

1. 万物皆对象。
2. Object 具有身份、状态、隶属关系、能力和生命周期。
3. Object 可以隶属于其他 Object。
4. Variable 是 Object，并通常隶属于 Process Object。
5. Process 本身也是 Object。
6. Device、Network、Program 等系统实体同样都是 Object。
7. Pointer 和 Reference 不是 Ousject 的对象抽象。
8. 对象身份不等于内存地址或存储位置。
9. 对象通过自己的能力进行操作。
10. 能力属于 Object，而不是散落的全局操作。
11. RAM 与 Storage 是同一对象世界的不同驻留状态。
12. Process 不以驻留 RAM 为存在前提。
13. Token Stream 是 Ousject 的标准程序执行形式。
14. TF 是高级语言与 Ousject 之间的执行边界。
15. 所有持久 Object 修改默认同步到持久存储。
16. 持久提交成功之前，新状态不能成为正式可见状态。
17. 提交失败时，内存与持久状态都保持旧值。
18. 多个 Object 修改可以作为一个原子 Transaction 一起提交。
19. Process 执行状态与它产生的 Object 修改必须能够共同提交。
20. 已提交状态必须能够承受突然断电。
21. 外部副作用与可回滚 Object State 必须明确区分。
22. Linux Driver 只用于硬件控制。
23. Ousject 自己定义全部操作系统语义。
24. File 不是核心类型；Path 是 Namespace Link 的兼容表示，File View 只存在于外部适配边界。
25. 系统应让程序关心对象及其能力，而不是对象的物理位置。

---

## Ousject 的核心模型

Ousject 可以概括成：

```text
Object
├── Identity
├── Parent / Belonging
├── State
├── Capabilities
└── Lifetime
```

所有这些 Object 共同组成 Ousject。

而 Object 当前究竟位于：

```text
RAM
Storage
Network
Device
```

只是实现问题。

不是对象本身的定义。

一个持久 Object 的修改则遵循：

```text
Old State
   ↓
Candidate State
   ↓
Persistent Write
   ↓
Atomic Commit
   ↓
Publish
   ↓
New State
```

如果任何一步在 Commit 前失败：

```text
Old State remains valid
```

这就是 Ousject 对对象、内存和持久化关系的基本定义。
