# 内核与用户程序边界

## 内核（Rust）

内核只提供机制：Object/Type/Link/Capability、原子事务、增量 WAL 与 Checkpoint、Process 状态与调度、TF VM、Effect/Provider ABI、认证密码学，以及当前用于接硬件的 Linux 适配器。

内核发布以下服务 Object，Praxis 通过“发现对象 → 调用能力”使用它们：

| 发现名 | Type | 已接通能力 |
|---|---|---|
| `system` | `core.system` | `status` `health_check` `shutdown` `restart` |
| `authentication` | `core.authentication` | `local_initialized` `initialize_local` `login` `logout` `change_password` |
| `users` | `core.user_registry` | `create_user` `users` `disable_user` |
| `compiler` | `core.compiler` | `compile` `validate` `disassemble` |
| `types` | `core.type_registry` | `register` `types` `descriptor` |
| `providers` | `core.provider_registry` | `providers` `devices`（仅 `local`） |
| `store` | `core.object_store` | `stats` `health_check` `effects`（仅 `local`） |

调度器是内核机制，不单独发布为用户对象；程序以 `object.query("core.process")` 发现进程并调用进程自身的生命周期能力。块设备读写同样是内核/驱动 API，普通 Praxis 程序直接操作持久化对象。

Console、设备与网络同样是 Provider-owned Object，不是 File API。密码由 `console.read_secret()` 返回启动期不透明句柄，认证内核解析句柄；明文不写入普通变量、Process 状态、Effect 或 WAL，Linux 终端适配器读取期间关闭回显。

## 用户空间（Praxis）

`system/` 中的 `init.px`、`local-setup.px`、`login.px`、`shell.px` 和管理程序属于用户空间。`system-install` 只负责开发期打包：它把这些源码编译成 Program Object，并原子安装到 `system` Namespace。正常 `boot` 不读取这些源码文件，只发现 Namespace 中的 `init` Program Object 并创建/恢复 Process。

首次启动链：

```text
宿主硬件适配 → init.px → local-setup.px → login.px → shell.px
```

以后启动跳过 `local-setup.px`。登录成功会改变当前 Process 的持久 Subject，随后创建的 Shell Process 继承该身份。

## Rust 工具不是系统 Shell

`run/inspect/object-*` 等命令仅用于开发和离线恢复，必须提供 Session 或显式 `--local`，不会因缺少 Session 自动升级。Rust 版用户创建、登录和退出命令已经删除；正常用户交互由 Praxis 完成。

## 当前宿主边界

目前 Linux 只负责启动应用、终端输入输出、TCP、时钟和块设备适配。Ousject Process 不是 Linux Process，对象权限、调度、持久化和系统服务语义均由 Ousject 内核实现。迁移到真实驱动时替换的是 Provider/启动适配层，不是 Praxis 用户程序。
