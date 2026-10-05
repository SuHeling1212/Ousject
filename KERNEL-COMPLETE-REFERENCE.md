# Ousject 内核完整功能说明

> 范围：以当前仓库的 Rust 源码为准，记录对象内核、Praxis 编译器、TF 格式、虚拟机、内建对象能力、权限、持久化、Provider 和宿主硬件适配。`system/*.px` 与 `examples/*.px` 是 Praxis 用户程序；它们提供的 Shell、安装流程或界面行为不计入内核功能。本文记录的是当前实现，不是设计目标。文件名中的“内核”也包括使其运行的 Rust 启动适配层，不意味着已经脱离 Linux 独立启动。

## 1. 系统由什么组成

| Rust crate | 当前职责 |
|---|---|
| `oms-types` | ID、值、生命周期、基础权限、错误和二进制值编码 |
| `oms-shard` | 按 ObjectId 确定固定 Shard |
| `oms-runtime` | 类型表、对象库、查询、授权、原子事务、持久化、退役内容清理 |
| `tf-format` | TF 指令及 `OTF0` 程序编码、解码、目标位置校验 |
| `praxis-compiler` | Praxis 词法、语法、编译和导入展开 |
| `ousject-vm` | Process 状态、解释执行、内建对象能力、调度、定时器、Process 清理 |
| `ousject-auth` | User、密码验证、Session |
| `ousject-provider` | Provider 接口、注册表、持久 Effect |
| `ousject-cli` | 开发/恢复命令与当前 Linux 控制台、终端、TCP、DNS、块设备适配 |
| `oms-tools` | 早期、独立的内存 OMS 演示工具，不是 Ousject 启动入口 |

内核核心模型：**每个有身份的实体是 Object；Object 有 Type、内容、权限、父子关系和命名 Link。** 普通值也是 Object 的内容，Process 的变量则绑定到 `core.value` Object。内存里修改对象后，持久模式在返回成功前把事务写到存储；程序无需自己调用保存命令。

### 1.1 ID、值和生命周期

- `ObjectId`、`TypeId`、`TransactionId`、`SubjectId` 都是 128 位 ID，显示为 32 位十六进制文本；解析允许 1–32 位及可选 `0x` 前缀。新 ID 由启动时前缀加原子递增序号构成。
- `ObjectVersion` 为单调递增的 `u64`；写入时校验期望版本，冲突则整笔事务失败。
- 值的完整种类是 `Null`、`Bool`、`Integer(i64)`、`Float(f64 位模式)`、`Text`、`Bytes`、`Array`、`Map`、`Record`、`Error {code,message}`。`Map` 和 `Record` 都使用文本键；`Record` 用于有结构的状态。ObjectId 在 Praxis 值中表现为**文本**，并非隐含的引用值。
- 值编码为 `OVL0`，最多嵌套 64 层、单容器最多 100 万项、编码后最多 16 MiB。浮点值保留原始 IEEE 754 位模式。
- 值的真假：`null`、`false`、整数 0、浮点 0、空文本/字节/数组/Map/Record 为假；其他为真，`Error` 为真。
- Object 生命周期枚举完整包含 `Creating`、`Active`、`Suspended`、`Migrating`、`Terminating`、`Tombstoned`。Process 另有自己的运行状态，不等于 Object 生命周期。

### 1.2 内建 Type 总表

`public` 表示普通程序可经 `object.create` 创建；`provider_only` 表示创建由内核、相应 Provider 或受控路径完成。类型存在不代表该设备实例已发现。

| Type（固定 ID） | Schema / 创建 | 专有能力或状态 |
|---|---|---|
| `core.type` (`0100`) | record / provider_only | Type 描述对象 |
| `core.value` (`1000`) | any / public | 值；内容为 Text 时有文本能力 |
| `core.text` (`1001`) | text / public | 文本能力 |
| `core.bytes` (`1002`) | bytes / public | 原始字节 |
| `core.collection` (`1003`) | collection / public | Array 或 Map 内容，使用统一值/索引能力 |
| `core.namespace` (`1004`) | record / public | `resolve, bind, unbind` |
| `core.instance` (`1005`) | record / provider_only | Praxis Class 实例，公开方法成为能力 |
| `core.program` (`1100`) | bytes / provider_only | `execute`，内容是 TF 程序 |
| `core.process` (`1101`) | record / provider_only | `start, wait, suspend, resume, terminate, bindings` |
| `core.user` (`1102`) | record / provider_only | 用户记录 |
| `core.console` (`1103`) | record / provider_only | `print, println, read_line, read_secret, size, is_interactive` |
| `core.session` (`1104`) | record / provider_only | `revoke` |
| `core.effect` (`1105`) | record / provider_only | `status, result` |
| `core.channel` (`1106`) | collection / public | `send, receive, wait`；正常初值为 `[]` |
| `core.system` (`1107`) | record / provider_only | `status, health_check, shutdown, restart` |
| `core.authentication` (`1108`) | record / provider_only | `local_initialized, initialize_local, login, logout, current_user, change_password` |
| `core.user_registry` (`1109`) | record / provider_only | `create_user, users, disable_user` |
| `core.scheduler` (`110a`) | record / provider_only | **只有类型描述；当前未发布可供 Praxis 调用的调度器对象** |
| `core.compiler` (`110b`) | record / provider_only | `compile, validate, disassemble` |
| `core.type_registry` (`110c`) | record / provider_only | `register, types, descriptor` |
| `core.provider_registry` (`110d`) | record / provider_only | `providers, devices` |
| `core.object_store` (`110e`) | record / provider_only | `stats, health_check, effects` |
| `core.math` (`110f`) | record / provider_only | 数学函数；`pi`、`e` 字段 |
| `core.time` (`1110`) | record / provider_only | `now, monotonic, sleep` |
| `net.endpoint` (`1200`) | record / provider_only | TCP `connect, listen, accept, send, receive, close`；已注册网络 Provider 允许用户请求创建 |
| `net.resolver` (`1201`) | record / provider_only | `resolve` |
| `device.display` (`1300`) | record / provider_only | `present, configure`；交互 stdout 时发布实例 |
| `device.sensor` (`1301`) | record / provider_only | 声明 `sample, calibrate`；当前无实际 Sensor Provider/实例 |
| `device.keyboard` (`1302`) | record / provider_only | `capture, release, poll_event, next_event`；交互 stdin 时发布实例 |
| `device.block_storage` (`1303`) | record / provider_only | `load_block, store_block`；底层适配接口，普通程序没有其 Invoke 权限 |

Schema 有 `any`、`text`、`bytes`、`collection`、`record`；`collection` 接受 Array/Map，`record` 可把 Map 规范化为 Record。用户可注册新 Type，但不会因此自动获得 Rust Provider 实现。

## 2. 对象库的全部逻辑

### 2.1 Object 内容与关系

一个 Object 保存 ID、TypeId、父 ID、版本、生命周期、原始状态字节、子对象集合、名字到 ObjectId 的 Link、所有者 SubjectId、授权表和该类型的基础/领域能力。一个 Object 有至多一个父对象；父子关系防环。Link 是有名字的边，可有多个名字指向同一对象。Namespace 的路径解析沿 Link 走，允许按名字绑定、解析、解绑；它不是文件系统。

`object.find` 可按 16 进制 ObjectId 或当前 Process **Link** 中的名字寻找；它不会拿字符串去查普通变量表。变量名则绑定在 Process 的变量表或当前函数帧里；`a link b` 让两个变量名指向同一 Object。重赋值普通变量会更新它所绑定的 `core.value` 内容。`x = object.create(...)` 和 `x = object.find(...)` 则将变量名直接绑定到目标 Object，不额外创建包装值。删除/退役时会清除指向目标的相关名字和 Link；退役内容不能作为活动对象继续使用。

基础权限完整列表：`view_value`、`replace_value`、`create_child`、`invoke`、`link`、`reparent`、`retire`、`inspect`、`manage_policy`。访问上下文携带 SubjectId；所有者、显式 Grant 和最高身份 `local` 决定可做的操作。查询只返回对调用者可见且满足过滤条件的对象。授权表不是一份独立于对象库的文件。

### 2.2 事务与持久化

- Rust 事务操作完整包含 `create`、`update_state`、`set_link`、`remove_link`、`reparent`、`grant`、`revoke`、`tombstone`，以及 `expect(object, version)` 并发前置条件。
- 提交先按固定顺序锁住所有 Shard；只对参与写入的 Shard 取写锁，其余取读锁。它在候选状态中检查版本、权限、Type/Schema、生命周期、命名和父子关系，校验整笔成功后先持久化、再发布内存状态。读者不会看见跨 Shard 的半笔提交。
- `commit_batch` 把多笔事务按输入顺序验证、用一次后端持久写入提交；任一笔失败则整批不可见。`transaction { ... }` Praxis 块另见第 5 节。
- 内存模式没有落盘。默认持久模式使用 `FileSnapshotBackend`，对象状态是 Object Store 的快照和写前日志（WAL），**不是每个对象对应一个文件**。文件后缀有 `.manifest`、按 generation 编号的 `.oms` 快照及 `.wal` 日志、`.lock` 租约。WAL 每次提交先同步；大约每 64 次提交切换到新完整快照 generation。打开对象库时重放有效 WAL；损坏数据报错。检查点及压缩使用持久 generation 切换。后端在一次提交的耐久性无法确认时要求重新打开对象库。
- 这是“每次提交成功即持久化”的语义，不是每次修改都重写整个磁盘快照；每条 VM 指令的 Process 状态也会提交，带来明显写入成本。
- 退役 (`tombstone`) 后内容保留 7 天；后台 `TombstoneReaper` 到期清除原状态及可回收数据，保留 ID 和最小元数据。进程的自动清理是另一层：结束后保留 7 天，之后退役 Process 及其拥有的子树，再经过对象退役内容保留期；被其引用而非拥有的 Program 不随之删除。
- Rust 内部有 `stats`、`health_check`、`analyze_gc`、`compact`、`checkpoint`、`reap_expired_tombstones` 等维护接口。Praxis 只暴露 `store.stats/health_check/effects`，没有 `gc()` 或手动 `checkpoint()` 能力。

### 2.3 性能特征与约束

Shard 路由由 ObjectId 固定决定；查找/访问先路由到 Shard。程序按对象 ID+版本缓存已解码 Program。快照读视图用共享引用保存状态、子集和 Link。持久化使用 WAL 增量记录和批量提交/周期检查点；后台清理按到期时间唤醒。当前并非纯内存执行：虚拟机逐条指令保存可恢复 Process 状态，所以计算密集型程序性能受持久事务影响。`time.sleep` 挂起进程，不持续占用执行线程；输入等待经 Effect 重试恢复。对象查询和一些权限/用户操作需要遍历对象，不能假定常数时间。Type/Value/TF/Process 解码有限额，防止无限嵌套和无界长度。

## 3. 身份、用户与权限

最高用户固定叫 `local`，SubjectId 是 `00000000000000000000000000000001`。系统内核在受信入口以该 SubjectId 运行。普通用户有独立、持久 SubjectId。User Object 存用户名、随机盐、PBKDF2 HMAC SHA 256 密码校验值和迭代次数；默认 100,000 轮，不保存明文密码。登录建立 Session Object，随机 32 字节 token 只返回一次，库中仅保存其 SHA 256 摘要；默认有效期 24 小时。验证、注销、禁用用户、改密码均在 Rust `AuthService`。登录时 Session 创建和当前 Process 身份变化使用同一事务。`--local` 是 CLI 显式开发/恢复权限，不会由 CLI 默认授予。

## 4. 编译、TF、Process 和运行

`praxis_compiler::compile(source)` 将源码直接编译；遇到 import/include 指令会报错。`compile_with_loader` 对顶层 `import "path"` 只加载一次，对 `include "path"` 每次展开，并检测循环。CLI 可为源码文件提供加载器；Praxis 内建 `compiler.compile/validate` 不提供加载器。TF 程序编码魔数 `OTF0`，最多 16 Mi 条指令，解码校验跳转目标。Value 编码魔数 `OVL0`，Process 状态魔数 `OPS0`，对象库快照/WAL/manifest 分别为 `OMS0`/`OMW0`/`OMG0`。当前预发布格式保持 `0`；旧格式不作兼容保证。

### 4.1 TF 指令全集

| 类别 | 全部指令 |
|---|---|
| 值/变量 | `Push, Load, Store, LoadIdentity, BindCreated, BindFound, BindLink` |
| 算术/比较 | `Add, Subtract, Multiply, Divide, Modulo, Equal, NotEqual, Less, LessEqual, Greater, GreaterEqual, Not` |
| 流程 | `Jump, JumpIfFalse, Pop, Halt` |
| 集合 | `MakeArray, MakeMap, IndexGet, IndexSet, IndexIncrement, IndexDecrement, Length` |
| 对象调用 | `RegistryCall, ObjectCall, GetField, SetField` |
| 函数/Class | `DefineFunction, CallFunction, Return, DefineClass, DefineMethod, SuperCall` |
| 异常/事务 | `BeginTry, EndTry, Transaction, CommitTransaction` |

一个 Process 是 `core.process` Object，状态包含 Program ID、SubjectId、当前 Token 位置、值栈、变量名→ObjectId、运行状态、结果、错误、唤醒时间、结束时间、函数帧及异常处理栈。状态有 `Running`、`Suspended`、`Halted`、`Terminated`、`Failed`。程序是 `core.program` Object，内容是 TF；Process 引用 Program，二者不是同一个 Object。启动新程序可由编译、`program.execute(...)` 或当前 Program 内 `object.create("core.process", {entry: "函数名", start: true, links: {}})` 的受控内核路径完成；`entry` 必须是零参数函数，`start` 默认为 `false`，`links` 可省略。每次执行 Token 时同步更新 Process 状态与必要的变量/对象；错误可进入 `catch`，未捕获则标记 `Failed` 并保存错误。`Halt` 保存结果和结束时间。步数上限让未完成 Process 可从持久 Token 位置恢复。

变量在顶层属于 Process；函数参数/局部名字存于调用帧，未在当前帧找到时可读 Process 顶层变量。普通赋值首次创建 `core.value` 子对象，再次赋值替换原对象内容。`this`/`super` 在方法帧指向实例。一个 Process 可通过被授予权限的另一个 Process 的 `variables`、`bindings()` 和 `process.变量名` 查看状态，但不能绕过对象授权。

内核 `CooperativeScheduler` 维护队列，按轮执行 Process；Process 自身提供 `start/suspend/resume/terminate/wait`。`wait()` 会驱动被等待者运行、轮询 Effect 与定时器；若对方仅因 Channel 等待而悬挂，可返回 `"suspended"`。`time.sleep(ms)` 记录持久截止时间并挂起，期限到达由调度器恢复。Channel 用 Object 值数组作队列；`send` 与唤醒等待 Process 同一事务，`receive` 取首项，`wait` 在无消息时悬挂并登记等待关系。Process 结束时通知 Provider 释放持有的输入等租约。

### 4.2 Class 与方法的实现边界

Class 定义写入 TF 中，并非 `core.type` 注册项。`object.create("类名", {字段: 值})` 创建 `core.instance`，实例 Record 保存类信息及字段。支持单继承、公开/私有字段和方法、`this`、`super.方法(...)`，继承遍历限制 64 层。Class 字段默认值必须是常量。公开方法可由外部调用，私有字段/方法在 Class 内受检查。类定义与可执行代码属于同一个 Program；查找方法依赖相应 Program。`new` 与旧 `init` 构造方法已移除。顶层 `private class` 的修饰符当前被解析，但 Class 的顶层可见性并未用于执行限制，不能据此声称有模块级私有 Class。

## 5. Praxis 当前实现的完整语法

### 5.1 词法

- 变量/函数/Class 名以 ASCII 字母或 `_` 开头，后面可含 ASCII 数字、`_`、`.`；点号作为名称中的字段/能力分隔符。**中文可出现在字符串里，不能直接作为标识符**。只有双引号文本，转义支持 `\n`、`\r`、`\t`、`\"`、`\\`；`//` 到行尾注释。
- 整数为十进制 `i64`；有数字的小数部分才识别为 Float，例如 `1.5`。负数由一元 `-` 运算形成。字面量还有 `true`、`false`、`null`、`[ ... ]`、`{key: value, ...}`；Map 键可为标识符或字符串。数组/Map 可跨行，允许尾逗号。
- 完整保留字：`true false null if else while break continue func return class extends public private try catch transaction link new and or not`。`new` 是专门报错的已移除语法。单行结束符是换行或 `;`；空行和多余 `;` 可跳过。括号 `()`、大括号 `{}`、方括号 `[]`、逗号、冒号和 `#` 均被词法器识别。
- 运算符完整为 `= == != < <= > >= + - * / % ++ -- ! && ||`，另有文字 `and or not`。没有 `+=`、`-=`、`for`、三元运算、单引号字符串或独立的 `&`/`|` 语法。

### 5.2 语句与表达式

| 语法 | 当前精确含义 |
|---|---|
| `x = 表达式` | 绑定/更新变量 Object；表达式为 `object.create/find` 时直接绑定该 Object |
| `x++`、`x--` | 整数或兼容数值加减 1 |
| `x[i] = v`、`x[i]++`、`x[i]--` | 更新 Array/Map/Record 值后回写变量 Object |
| `obj.field = v`、`obj.field++/--` | 修改已有 Record/Map 字段并提交；不能凭空新增字段 |
| `alias link original` | 另一个名字绑定同一 Object；不是复制内容 |
| `if 条件 { ... } else if 条件 { ... } else { ... }` | 按真假分支；条件不要求括号 |
| `while 条件 { ... }`、`break`、`continue` | 循环与跳转；后二者只能在循环内 |
| `func f(a, b) { ... }`、`return [表达式]` | 函数、参数、局部帧与返回；无值返回 `null` |
| `class C [extends P] { [public/private] field = 常量; [public/private] func m(...) { ... } }` | Class、继承、字段与方法；默认 public |
| `try { ... } catch (error) { ... }` | 捕获执行错误，绑定错误值 |
| `transaction { ... }` | 将赋值、字段/索引更新、`link` 一起提交；不支持嵌套或在块内调用函数/对象能力 |
| `f(...)`、`obj.method(...)`、`super.method(...)` | 函数、对象、父 Class 方法调用；可作单独语句或表达式 |
| `object.create(type, value[, parent])`、`object.find(id_or_name)`、`object.query(type[, capability])` | 编译器专门识别的 3 个 Registry 调用 |
| `a[i]`、`obj.field`、`this.field`、`(表达式)` | 下标、字段、分组；可继续下标读取 |
| `#表达式` | 长度：Text 按 Unicode 字符，Bytes/Array/Map/Record 按元素数 |
| `!x`/`not x`、`-x` | 一元非、数值取负 |
| `* / %`、`+ -`、比较、`&&/and`、`||/or` | 按此顺序从高到低结合；逻辑运算短路并得到 Bool |
| `import "path"`、`include "path"` | 仅顶层、仅带 loader 的编译入口；前者同一路径只展开一次，后者每次展开 |

算术整数使用检查运算，溢出报错，整数除法向零截断；任一边为 Float 时转 Float。两个 Text 的 `+` 拼接。`==/!=` 是值相等比较；`< <= > >=` 仅按数值比较。数组索引必须为非负整数；Map/Record 索引为文本键；Text 可按 Unicode 字符索引但不可下标写。Map/Record 下标写可插入新键；缺失键读取报错。单独写 `a` 或 `ls` 不是合法语句，会出现“expected '=' ...”编译错误；当前语言没有自动回显表达式的语句。

### 5.3 原子块边界

`transaction {}` 里的内容在 VM 中预演为暂存变量/字段/索引变化，再作为一笔 OMS 事务提交，Process 的 Token 位置一同更新。内部不能调用任何函数或对象能力，不能用 `if/while/try/return` 或嵌套事务。失败则整块不提交。外部 Provider 操作不能包在该块中保证外界原子性。

## 6. Praxis 可发现的全部 Rust 内建 API

惯例：先 `x = object.find("名字")` 或 `x = object.create("类型", 值)`，之后 `x.能力(...)`。下表列的是 Rust/Provider 实现，不把任何 `.px` 文件里的函数当成内建。ObjectId、SubjectId 参数均为十六进制文本。

### 6.1 Registry、统一对象属性与能力

| 调用/属性 | 结果和作用 |
|---|---|
| `object.create(type, value[, parent])` | 创建 Type 指定的 Object，可设父 Object |
| `object.find(id_or_name)` | 查已授权 Object；非 ID 名字来自当前 Process Link |
| `object.query(type[, capability])` | 返回可见 ObjectId 数组；可按领域能力筛选 |
| `o.id`、`o.type`、`o.parent`、`o.status`、`o.version`、`o.owner` | 身份、类型、父 ID、状态、版本、所有者 |
| `o.permissions`、`o.inspect`、`o.capabilities` | 授权表、基本元数据、基础与领域能力；`permissions` 需策略管理权限 |
| `o.value`、`o.children`、`o.links` | 本质内容、子 ID 数组、命名 Link Map |
| `o.replace(value)` | 原子替换对象值，遵守 Schema/权限 |
| `o.link(name, target)`、`o.unlink(name)` | 建立或删除对象 Link |
| `o.grant(subject, capability)`、`o.revoke(subject, capability)` | 调整基础权限 |
| `o.retire()` | 退役对象及相关拥有的子树，清理关联名字与 Link |

通用只读信息使用上表的属性语法。`o.replace(...)`、`o.link(...)`、`o.unlink(...)` 返回对象身份，便于继续使用；`grant/revoke/retire` 返回 `null`。Process 的 `value` 是运行状态 Record，含 `program`、`subject`、`position`、`status`、`wake_at_unix_ms`、`ended_at_unix_ms`、`stack`、`variables`（名字到 ObjectId）、`frames`、`handlers`、`result` 和 `error`；`process.variables` 则返回名字到实际变量值。

### 6.2 Console、Time、Math、Text

| 对象 | 全部函数/字段 | 行为 |
|---|---|---|
| `console` | `print(value)`、`println(value)` | 输出，不换行/换行 |
| `console` | `read_line()` | 读取一行文本 |
| `console` | `read_secret()` | 不回显输入；返回本次启动有效的保密 token，认证能力会解析它 |
| `console` | `size()`、`is_interactive()` | 终端列行数 `{columns, rows}`、交互状态 |
| `time` | `now()`、`monotonic()`、`sleep(milliseconds)` | Unix 毫秒、启动后单调毫秒、挂起并按时恢复 |
| `math` | `abs(x)`、`min(a,b)`、`max(a,b)`、`clamp(x,min,max)` | 基础数值 |
| `math` | `sqrt(x)`、`pow(base,exponent)`、`floor(x)`、`ceil(x)`、`round(x)`、`trunc(x)` | 幂、根、舍入 |
| `math` | `sin(x)`、`cos(x)`、`tan(x)`、`atan2(y,x)`、`hypot(x,y)` | 三角函数/长度 |
| `math` | `log(x)`、`log2(x)`、`log10(x)`、`exp(x)` | 对数/指数 |
| `math` | `random()`、`random_integer(min,max)`、`pi`、`e` | `[0,1)` 随机 Float、含两端随机整数、数学常数 |
| `core.text` 或内容为 Text 的 `core.value` | `slice(start,length)`、`find(needle)`、`contains(needle)`、`split(separator)` | Unicode 字符截取/位置、查找、拆分 |
| 同上 | `replace_all(from,to)`、`trim()`、`lower()`、`upper()` | 文本替换、去空白、大小写转换 |

`slice` 第二参数是**长度**，不是结束位置。`find` 找不到返回 `-1`。随机数由当前宿主 `/dev/urandom` 提供。数学域错误或非有限结果报错。Console/Time 等服务必须先有相应内核对象及硬件/运行环境；单纯存在 Type 描述不等于有设备。

### 6.3 Program、Process、Channel、Namespace、Compiler

| 对象 | 全部函数/属性 | 行为 |
|---|---|---|
| Program | `execute()`、`execute(bindings)`、`execute(entry)` | 建 Process；bindings 为名字→`core.value` ObjectId，entry 为文本入口名并启动 |
| Process | `start()`、`suspend()`、`resume()`、`terminate()`、`wait()` | 生命周期控制；可对自身操作；`wait` 返回状态文本 |
| Process | `bindings()`、`variables`、`program`、`user`、`subject`、`position`、`result`、`error`、`process.变量名` | 绑定 ID、变量值、程序/身份/执行点/结果/错误/单变量 |
| Channel | `send(value)`、`receive()`、`wait()`、`#channel` | 发消息、取首消息、无消息时挂起、队列长度；空 `receive` 返回 `null` |
| Namespace | `bind(name,target)`、`resolve(path)`、`unbind(name)` | 命名空间 Link 操作 |
| Compiler | `compile(source)`、`validate(source)`、`disassemble(program_id)` | 编译成 Program Object、返回是否可编译、查看 TF Token 文本 |

### 6.4 System、Store、Auth、Type 与 Provider

| 发现名/对象 | 全部函数 | 行为/权限 |
|---|---|---|
| `system` | `status()`、`health_check()` | 健康检查和对象数、请求状态 |
| `system` | `shutdown()`、`restart()` | 仅 `local`；在 System Object 写入请求，由启动循环处理 |
| `store` | `stats()`、`health_check()` | Shard、活动/退役对象数与一致性检查 |
| `store` | `effects()` | Effect ID 数组；仅 `local` |
| `authentication` | `local_initialized()`、`initialize_local(password)` | 检查/初始化最高用户；初始化只限受信 `local` 入口 |
| `authentication` | `login(name,password)`、`logout(token)`、`current_user()`、`change_password(name,password)` | Session、当前身份和密码操作 |
| `users` | `create_user(name,password)`、`users()`、`disable_user(name)` | 用户管理；仅 `local` |
| Session | `revoke()` | 将 Session 退役 |
| `types` | `types()`、`descriptor(name)`、`register(name,schema,creation,capabilities)` | 查/注册 Type；注册仅 `local`；策略 `public/provider_only` |
| `providers` | `providers()`、`devices()` | 已注册 Provider Type ID、已发现设备 Object ID；仅 `local` |
| Effect | `status()`、`result()` | `pending/completed/failed` 状态与结果 |

已发布的内核服务名字是 `system`、`authentication`、`users`、`compiler`、`types`、`providers`、`store`、`math`、`time`、`resolver`；发现到 system Namespace 后还有 `programs` 名字。`console` 单独由硬件发现发布。**没有**已发布的 `scheduler` 服务；进程控制在 Process 自身。

### 6.5 网络与设备

| 对象 | 全部函数 | 当前实现 |
|---|---|---|
| `resolver` | `resolve(hostname)` | 宿主 DNS 解析，返回 IP 文本数组 |
| Endpoint | `connect(host,port)`、`listen(host,port)`、`accept()` | TCP 连接/监听/接受；`accept` 返回新 Endpoint |
| Endpoint | `send(text_or_bytes)`、`receive([maximum])`、`close()` | 收发/关闭，默认最多接收 65,536 字节 |
| Display | `present(text_or_bytes)`、`configure(configuration)` | 终端显示适配；有交互 stdout 时才有设备实例 |
| Keyboard | `capture()`、`release()`、`poll_event()`、`next_event()` | 独占、释放、非阻塞读、等待下个原始按键事件 |
| Block Storage | `load_block(index)`、`store_block(index,bytes)` | 底层 4,096 字节块读写；普通 Praxis 程序未获 Invoke 权限 |
| Sensor | `sample()`、`calibrate()` | Type 有声明；当前无 Provider，不能据此调用实际传感器 |

`Keyboard` 事件 Record 有 `key,text,pressed,ctrl,alt,shift`，可识别普通文本、方向键、Home/End、Insert/Delete、Page Up/Down、F1–F12 和修饰键。终端适配主要提供按下事件。`capture` 的输入所有权按 Process 管理，Process 结束会释放；Console 行输入与 Keyboard 原始输入共用输入源。`poll_event()` 无事件返回 `null`。Network Endpoint 由已注册的 Provider 受控创建，例如 `object.create("net.endpoint", {transport: "tcp"})`。这些 Linux API 是当前硬件适配层，不定义对象内核的持久格式。

## 7. Provider 与 Effect 的完整调用流程

`ObjectProvider` 必须给出 TypeId、`create`、`invoke`、`capabilities`；还可实现 `user_creatable`、按 Process 的 `invoke_for_process`、Process 结束通知与秘密 token 解析。注册表每 Type 只接收一个活动 Provider。`ProviderOutcome` 返回结果、可选的新对象状态和要创建的对象。

调用外部能力时，VM 先把 `core.effect` 的 **pending 意图**、目标/参数/Token 位置和 Process 的 `$effect` Link 原子持久化，再调用 Provider。完成后把结果、目标状态、新对象、Process 下一指令与 Effect 的 `completed` 状态一起提交；失败写 `failed`。Provider 可返回 `Pending`，VM 保留原调用现场并挂起等待重试；恢复时核对同一 Effect 是否匹配调用。Effect Object 可事后查询。这个机制防止本地状态出现半笔对象提交，并给 Provider 稳定幂等键。**远端系统若不支持幂等，跨整机崩溃无法承诺外部操作 exactly once。**

当前硬件适配的事实来源是 Linux 宿主：stdout/stdin 与终端原始输入、TCP socket、系统 DNS、块存储文件适配、`/dev/urandom`。内核对象语义不依赖 Linux 文件是用户可见的“万物皆文件”模型；对象持久化文件是现阶段存储后端。

## 8. Rust 开发接口与启动适配

为了覆盖不向 Praxis 公开的内核函数，下表列公开 Rust 接口家族。参数和返回类型可在对应源码中查到；内部私有辅助函数不构成稳定 API。

| 模块 | 对外 Rust API 全集或家族 |
|---|---|
| ID/Value | `ObjectId/TypeId/TransactionId/SubjectId::{new,from_u128,as_u128}`、`FromStr`；`FloatValue::{new,from_bits,bits,get}`；`Value::{is_truthy,kind,encode,decode}`；`ObjectVersion::{new,get,next,checked_next}`；`ShardId::{new,get}` |
| Shard | `FixedDirectory::{new,locate}` |
| OMS 类型 | `AccessContext::new`；`CreateSpec::{new,with_parent,with_link}`；`ObjectQuery::{new,with_type,with_parent,with_capability,with_domain_capability}`；`CreateObject::{new,with_id,with_parent,with_link,with_grant}`；`ObjectView::{header,state,children,links,capabilities,owner,grants}` |
| OMS 事务 | `Transaction::{new,id,expect,create,update_state,set_link,remove_link,reparent,grant,revoke,tombstone}`；`CommitResult`、`OmsStats`、`GcAnalysis`、`GcReport`、`StorageUsage` |
| OMS 服务 | `InMemoryObjectManager::{new,open_persistent,open_persistent_with_shards,open_with_backend,open_with_backend_and_shards,types,type_by_name,type_by_id,register_type,prepare_register_type,prepare_create,create_object,find,value,replace_value,prepare_replace_value,query,bind_name,unbind_name,resolve,shard_for,read,require_capability,inspect,list,stats,analyze_gc,analyze_gc_at,compact,compact_expired_tombstones_at,reap_expired_tombstones,health_check,checkpoint,begin,commit,commit_batch}`；`TombstoneReaper::start` |
| 存储接口 | `ObjectManager::{read,inspect,begin,commit,list}`；`SnapshotBackend::{load,store,checkpoint,storage_usage,compact}`；`FileSnapshotBackend::{new,path}` |
| TF/编译 | `Program::{validate,encode,decode}`；`praxis_compiler::{compile,compile_with_loader}` |
| VM | `VirtualMachine::{new,with_console,register_provider,publish_console,publish_provider_object,manager,create_process,create_process_as,reconnect_hardware,poll_pending_effect,time_until_wake,wake_due_timer,run,process_state,variable}`；`CooperativeScheduler::{new,enqueue,run}`；`ProcessReaper::start`；`ConsoleProvider::{print,println,size,is_interactive,try_read_line,try_read_line_for,try_read_secret,try_read_secret_for,release_process}` |
| 认证 | `AuthService::{new,create_user,stage_create_user,initialize_local,stage_initialize_local,login,stage_login,authenticate,logout,stage_logout,users,change_password,stage_change_password,disable_user,stage_disable_user}`；`EntropyProvider::fill` |
| Provider | `ObjectProvider::{type_id,user_creatable,create,invoke,invoke_for_process,process_ended,capabilities,resolve_secret}`；`ProviderRegistry::{new,register,get,types,process_ended}`；`ProviderOutcome::{result,with_state,with_created}`；`EffectRecord::{pending,complete,fail,value,create_object,encode,decode}` |

`ousject` Rust CLI 是现阶段启动与恢复适配，命令全集：`compile`、`system-install`、`boot`、`run`、`run-tf`、`resume`、`schedule`、`inspect`、`list`、`check`、`types`、`type-register`、`object-create`、`object-value`、`object-query`、`object-bind`、`object-unbind`、`object-resolve`、`object-grant`、`object-revoke`、`tf-dump`、`help`。通用选项包括 `--state`、`--memory`、`--local`、`--session`、`--steps`，具体命令接受范围见 `ousject help`。`system-install` 把目录中的 `.px` 编译为 Program Object 并放入 system Namespace；`boot` 找到 `init` Program，发现/发布当前硬件对象，启动清理线程并运行 Process。`init.px` 做什么属于用户程序，不在本文范围。`oms-tools demo/shell` 是内存 OMS 旧演示接口，不应作为正式系统交互方式。

## 9. 错误、安全边界与当前限制

- 错误类别包括编译位置错误、TF/Value 格式错误、对象不存在、版本冲突、权限拒绝、Schema 不匹配、非法生命周期、存储损坏/占用、VM 类型/索引/未定义变量/除零错误、Provider 未安装/等待/失败和认证错误。`try/catch` 处理 VM 执行错误；存储失败不会把半笔对象状态发布为已成功。
- Type 已声明 ≠ Provider 已安装 ≠ 已发现实例 ≠ 当前 Subject 有 `invoke` 权限。`core.scheduler`、`device.sensor` 是最明显例子。
- 目前运行路径仍由 Linux 进程启动、调度执行线程和访问硬件；这里的“内核”是 Ousject 自身实现的对象/执行/授权/持久化层，尚不是独立引导的硬件内核。
- 所有存储持久性声明以当前 `FileSnapshotBackend` 成功返回为前提；宿主硬件、文件系统、远端网络的不可恢复故障不在对象事务保证范围内。Provider 的外部效果无法单靠本地 WAL 获得跨崩溃 exactly once。
- Praxis 当前无任意顶层表达式语句、中文标识符、`for`、`new`、内建打印函数或 `io.println`。程序需发现 Console Object 并调用能力；输入设备实例取决于真实终端。

## 10. 源码索引

本说明按 `crates/oms-types/src/lib.rs`、`crates/oms-shard/src/lib.rs`、`crates/oms-runtime/src/lib.rs`、`crates/tf-format/src/lib.rs`、`crates/praxis-compiler/src/lib.rs`、`crates/ousject-vm/src/lib.rs`、`crates/ousject-auth/src/lib.rs`、`crates/ousject-provider/src/lib.rs`、`crates/ousject-cli/src/main.rs` 编写。日后行为变更应以这些实现同步修订本文。
