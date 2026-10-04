# Praxis 全系统控制与 `local` 用户实施计划

## 目标

最终用户不通过 Rust CLI 管理 Ousject。Rust 只实现内核机制、Praxis VM 和临时宿主驱动适配；启动、登录、Shell、用户管理、进程管理、对象管理和系统维护全部由 Praxis 程序完成。

“Praxis 拥有整个系统的控制权”定义为：**所有可管理的内核能力都有稳定的 Object/Capability API，可由 Praxis 调用。** 权限仍由 Process 的 Subject 决定；普通 Praxis 程序不能自动获得最高权限，`local` 身份运行的 Praxis 系统程序才拥有全系统权限。

## 1. Console 输入输出

统一 Console Object 的能力：

```praxis
console = object.find("console")
console.println("hello")
line = console.read_line()
secret = console.read_secret()
```

实施内容：

1. 用 `println` 替换当前预发布的 `print`，不保留旧兼容入口。
2. 增加 `read_line()`，返回一行 UTF-8 Text，不包含末尾换行。
3. 无输入时挂起当前 Process，而不是阻塞整个 Ousject 调度器。
4. 输入到达后创建持久 Input Event/Effect Result，再原子唤醒等待 Process。
5. Process 位置、返回文本、Effect 完成状态共同提交。
6. 同一个 EffectId 重试时必须返回同一份输入，不能再次读取下一行。
7. EOF、无效 UTF-8、设备断开和权限拒绝返回可捕获 Error。
8. `device.keyboard.next_event()` 保留给按键/事件输入；Console `read_line()` 负责文本命令输入，两者不混为 File API。

验收：两 Process 同时等输入、提交故障重试、重启恢复、EOF、Unicode 和非授权读取测试全部通过。

## 2. 最高系统用户 `local`

1. 固定最高身份名称为 `local`。
2. `local` 对应当前保留 SubjectId：

```text
00000000000000000000000000000001
```

3. Object Store 第一次初始化时原子创建或验证唯一的 `core.user`：`name = "local"`。
4. `local` 是真实 User Object，不再只是代码中的无名 `SYSTEM_SUBJECT`。
5. 内核内部可以继续使用常量，但公开显示统一为 `local`。
6. 禁止 Rust CLI 在没有 Session 时默认获得最高权限。
7. 首次启动由本地控制台上的 Praxis 初始化程序设置 `local` 凭据；不提供默认密码。
8. `local` 可以管理所有 Object、Type、Process、Provider、用户、Session、Store 和系统服务。
9. 普通用户仍按 Grant/Capability 运行，不能因为使用 Praxis 就绕过权限。

验收：Store 中只能存在一个保留 `local` 身份；普通 Session 无法伪造、删除或替换它；无 Session 操作不会自动升级为 `local`。

## 3. 把全部内核管理能力暴露成 Object

新增或补齐以下受保护系统 Object：

| Object | Praxis 能力 |
|---|---|
| `core.system` | `status`、`shutdown`、`restart`、`health_check` |
| `core.authentication` | `login`、`logout`、`change_password` |
| `core.user_registry` | `create_user`、`users`、`disable_user` |
| `core.scheduler` | `enqueue`、`processes`、`suspend`、`resume`、`terminate` |
| `core.compiler` | `compile`、`validate`、`disassemble` |
| `core.type_registry` | `register`、`types`、`descriptor` |
| `core.provider_registry` | `providers`、`devices`、`bind`、`unbind` |
| `core.object_store` | `stats`、`health_check`、`checkpoint`、`effects` |

要求：

1. 所有能力使用统一 `object.find/query` 和 `name.capability(...)` 形式。
2. Type Descriptor 只能公布真正可调用的能力。
3. `core.session.revoke`、`core.effect.status/result` 已接通；未实现的 `retry` 已从 Descriptor 删除。
4. 每个管理调用都经过 Process Subject 权限检查。
5. 修改型调用使用 OMS Transaction；外部行为使用 Effect Intent。
6. 不向 Praxis 暴露内存地址、磁盘偏移、Rust 类型或宿主文件句柄。

## 4. Praxis 系统程序

按启动顺序实现：

```text
init.px
  → local-setup.px（仅第一次启动）
  → login.px
  → shell.px
  → 系统管理程序
```

第一批程序：

- `init.px`：发现核心服务、启动登录进程、监督系统进程。
- `local-setup.px`：首次设置 `local` 密码。
- `login.px`：从 Console 读取用户名/密码，调用 Authentication Object。
- `shell.px`：读取命令、启动 Program、等待 Process、显示结果。
- `users.px`：用户、Session 和权限管理。
- `objects.px`：查询、检查、创建、Link、授权和退役 Object。
- `processes.px`：列出、启动、挂起、恢复和终止 Process。
- `types.px`：查看和注册动态 Type。
- `system.px`：健康检查、Checkpoint、设备和 Effect 状态。

密码使用 `console.read_secret()`：Linux 终端适配器会临时关闭回显；持久 Process/Variable/Effect 中只保存启动期不透明句柄，不保存明文。非交互管道仍按普通输入读取，便于自动化启动测试；正式驱动直接实现对应的安全输入模式。

## 5. 缩减 Rust 交互层

1. 当前 Rust CLI 标记为开发/引导工具，不再继续增加用户功能。
2. Rust 版 `user-create/login/logout` 已删除；对象管理和调度命令只保留为显式 `--local`/Session 的开发与离线恢复工具。
3. 最终宿主入口只负责：打开 Store、发现硬件、注册驱动、创建/恢复 `init.px`。
4. `oms-tools shell` 仅保留为离线恢复工具，不作为正常系统 Shell。
5. 正式启动路径没有“未提供 Session 就自动使用最高权限”的行为。

## 6. 实施顺序

```text
S1  Console.println/read_line + 非阻塞等待
S2  local User Object + 禁止隐式最高权限
S3  Authentication/User Registry Object
S4  System/Scheduler/Compiler/Type/Store Object API
S5  init.px + local-setup.px + login.px
S6  shell.px + 管理程序
S7  删除正常路径中的 Rust 用户交互
S8  完整权限、故障、恢复和端到端验收
S9  开始性能优化
```

每阶段必须保持版本号 `0.0.0` 和格式版本 `0`，直到明确宣布发布。

## 7. 完成标准

从启动后开始，用户可以只通过 Console 和 Praxis 完成：

1. 首次设置 `local`。
2. 登录与退出。
3. 编译和运行 Praxis Program。
4. 管理用户、权限、Object、Process、Type、Provider 和设备。
5. 检查 Store、Effect 和系统健康状态。
6. 关闭或重启系统。

除开发/故障恢复模式外，不需要调用 Rust CLI。

## 8. 与性能计划的关系

先完成上述系统控制面，再按 [PERFORMANCE-PLAN.md](./PERFORMANCE-PLAN.md) 实施增量 WAL、Group Commit、Segment COW、无损 Checkpoint、缩小锁范围和 VM 热路径优化。任何优化都不能改变持久提交、权限和 Effect 语义。
