# Ousject Process 当前如何运行

## 一句话

当前的 `core.process` 是由 Ousject VM 执行和调度的持久 Object，不是一个 Linux Process。Linux 只负责启动整个 Ousject 应用，并在硬件适配层提供 stdout、文件 I/O、时钟、内存和同步。

## 启动顺序

```text
Linux 启动 Ousject 应用
        │
        ▼
Linux 硬件适配器发现 stdout
        │
        ▼
发布或复用一个共享 core.console Object
        │
        ▼
为本次启动注册 ConsoleProvider
        │
        ▼
Ousject VM 创建并执行 core.process
```

Console Object 可以持久化，但对象记录不等于硬件。每次启动都必须重新发现端点并注册 Provider。没有 Provider 时，`console.println(...)` 返回 `MissingProvider("console")`。

## 创建 Process

`VirtualMachine::create_process(program)` 在一个 OMS 原子事务中创建：

1. `core.program` Object，状态是 OTF0 Token Stream。
2. `core.process` Object，状态是 OPS0。
3. Process 的 `program` Link。
4. Process 指向自身的 `process` Link。
5. 如果发现了 Console，再建立指向共享 Console Object 的 `console` Link。

持久 Process 状态包含：

- 下一条 Token 的位置；
- 计算栈；
- 变量名到 Value Object ID 的映射；
- `Ready`、`Running`、`Waiting`、`Suspended`、`Halted`、`Terminated` 或 `Failed` 状态；
- 统一 `WaitReason`：Timer、IPC、Effect、Input 或 Process；
- Worker lease owner、generation 和 deadline；
- Program Object ID、可选结束时间；
- 函数/方法调用帧与异常处理帧。

变量名绑定 Object。第一次执行 `x = 42` 会创建 Process 的 `core.value` 子对象；以后修改 `x` 会更新这个对象。

## 持久 Terminal Session

Praxis Shell 为当前 Subject 打开或恢复一个 `core.terminal_session`。会话固定关联一个 Program 和一个 Process；每次输入都复用同一个 Process，所以变量、函数和 Class 定义跨提示符以及对象库重启后保留。

```text
terminal.open() → Terminal Session
                         ├── Program Object
                         └── one persistent Process Object
```

会话对象保存最近 100 次提交、未闭合括号的多行输入和终端尺寸。提交时，Session、Program 和 Process 位置在同一个 OMS 事务中更新。下一次提交前会压缩旧命令 Token，只保留仍有效的函数/Class 定义。交互式 `process.wait()` 收到 Ctrl+C 时会清空当前调用栈并停下这一段；已经提交的 Token 修改保留，同一个 Process 可以继续执行下一段。

## 执行一条 Token

```text
读取 Process ──> 读取 Program ──> 取当前 Token ──> 计算候选结果
                                                      │
                                                      ▼
                                      OMS 原子提交 Object 修改、
                                      Stack 和下一 Token 位置
                                                      │
                                                      ▼
                                         提交成功后结果才可见
```

对象创建、替换、Link 修改、变量值和 Process 位置会放进同一个 OMS Transaction。提交失败时，这些变化全部不生效。

`VirtualMachine::run` 执行一个 Process 的 bounded slice，最多 4096 条 Token 或 20ms。`CooperativeScheduler` 将 Ready Process 放入 round-robin 队列；Praxis 不暴露独立的 `scheduler` 对象，进程列表通过 `object.query("core.process")` 查询。Worker 先持久 claim 一个 30 秒 lease，每片续期；generation 变化会拒绝旧 Worker 提交。

执行片中的 Process 位置、Stack、Frames 和 Object 写入同属一个 OMS Transaction。持久提交成功后再可见；提交失败则保持原位置与对象状态。等待 Process 使用 `Waiting + WaitReason`，不占 Worker。Timer 等待在队列空闲时让调度器直接等待最近的 deadline。

结束 Process 的结果和错误会保留七天；后台进程回收器之后原子退役 Process 与其拥有的数据。它指向的 Program 不会被误删。退役后再按统一七天墓碑策略回收内容，元数据仍保留。

## 创建子 Process 与通信

Praxis 可从当前 Program 的无参数函数创建子 Process：

```praxis
child = object.create("core.process", {
    entry: "worker",
    links: { channel: channel.id }
})
child.start()
result = child.wait()
```

子 Process 初始为 `Suspended`，`start/resume` 使其运行，`suspend` 暂停，`terminate` 终止。`wait` 运行目标 Process 直到停止或达到安全步数上限。子进程与 Subject 活跃 Process 数量均有限制。

进程间不直接读取彼此的局部变量。可通过持久 `core.channel` 传递 FIFO Value，或将同一个 `core.swap_pool` Object ID 交给多个进程，让它们按名称发现同一个普通 Object。两种方式都仍受 OMS capability、版本冲突和原子持久化规则约束；SwapPool 不共享内存地址。细节见 [IPC.md](./IPC.md) 和 [SWAPPOOL.md](./SWAPPOOL.md)。

## 打印流程

Praxis 只能这样打印：

```praxis
console = object.find("console")
console.println("hello")
```

执行过程是：

1. `object.find("console")` 读取当前 Process 的 `console` Link，并把目标 Object 绑定到变量名。
2. `console.println(...)` 检查 Object 类型、`Invoke` 权限和本次启动注册的 Provider。
3. 先持久提交包含参数和稳定 EffectId 的 Pending `core.effect`，Process 暂不前进。
4. VM 用 EffectId 调用 Console Provider；同一启动内的完成重试不会再次打印。
5. Provider 返回后，Effect 完成状态和 Process 下一 Token 位置原子提交。
6. Linux 版本的 Provider 将文字写到本次发现的 stdout；以后可以替换成正式终端驱动。

Effect 状态为 `pending/running/completed/failed/unknown`，策略为 `manual` 或 `retry_idempotent`。手动策略遇到中途崩溃时会成为 `unknown` 并等待 local 处理；幂等策略用同一 Effect ID 重试。整机在外部终端收到文字、但完成提交前崩溃，远端不支持幂等时仍可能重复输出，不能承诺跨崩溃 exactly-once。见 [EFFECTS.md](./EFFECTS.md)。

旧 `io.println`、`Println` Token、`println` 能力和隐式 Console 已删除。

## 恢复

程序退出后，OPS0 Process 状态仍在 OMS0 Object Store。下次 Scheduler recovery 会扫描 Process、失效旧 Worker lease、恢复 Timer/Effect 状态，并将 Ready Process 重新排队：

1. 打开 Object Store；
2. 重新发现硬件并注册 Provider；
3. 把 Process 的 `console` Link 重新连接到本次发现的 Console；
4. 从最近已提交的 Token 位置继续运行。

## 当前边界

| 已由 Ousject 自己完成 | 当前仍由 Linux 适配器或 Rust 运行时提供 |
| --- | --- |
| Object、Type、权限、生命周期 | 应用启动 |
| Program、Process、Value Object | stdout 与快照文件 I/O |
| Token VM 与协作调度 | 堆内存分配和线程同步原语 |
| 原子事务、冲突检查、WAL 恢复 | 时钟、随机源和文件/Socket API |
| Praxis 编译与对象能力分发 | 正式裸机硬件驱动尚未实现 |

发布前二进制格式固定为 OTF0、OPS0、OVL0、OMS0，只读取版本 0。
