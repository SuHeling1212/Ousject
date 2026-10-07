# Praxis 系统用户程序

这个目录是 Ousject 的用户空间，不是内核。

- Rust 内核：OMS、权限检查、事务、WAL、Process 调度、TF VM、Provider ABI，以及当前临时的 Linux 硬件适配。
- Praxis 用户空间：初始化、登录、Shell 和系统管理界面。

这些程序只通过 `object.find(...)` 发现对象，再调用对象能力；它们不调用 Rust CLI API、宿主文件 API 或 Linux 系统调用。当前开发启动器可用 `ousject run system/<程序>.px --local` 引导它们，正式启动路径将直接恢复 `init.px` 的 Process。

编写和维护这些程序时，以 [`Praxis 语法参考`](../docs/reference/praxis-syntax.md)和
[`Praxis API 总表`](../docs/reference/praxis-api.md)为准。
