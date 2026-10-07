# 快速开始

本文从仓库根目录出发，依次验证内存执行、持久 Store、编译后的 TF 和 Praxis 用户空间。

## 前置条件

- Rust `1.85.0`；仓库中的 `rust-toolchain.toml` 会选择该版本。
- `rustfmt` 和 `clippy` 组件。
- Unix 风格环境。宿主 CLI 当前使用 `nix` 的终端接口。

仓库脚本会优先使用 `.tools/rust/usr` 中的本地工具链，否则使用 `PATH` 中的 Cargo：

```bash
./scripts/cargo-local --version
```

## 1. 运行最小示例

```bash
./scripts/ousject run examples/hello.px --memory --local
```

输出前四行应为：

```text
Ousject running
Ousject running
Ousject running
3
```

随后 CLI 会输出类似 `process=<id> status=Halted steps=68` 的执行报告；Object ID 每次运行
都可能不同。

这里：

- `run` 编译 Praxis 源码，创建 Program 和 Process Object，再由 VM 执行。
- `--memory` 使用单 Shard 内存 Store，退出后状态消失。
- `--local` 以可信本机身份运行。这是显式开发/恢复权限，不是普通登录方式。

## 2. 使用持久 Store

创建一个临时目录，避免在仓库中产生 `.ousject` 状态：

```bash
state_dir="$(mktemp -d /tmp/ousject-quickstart.XXXXXX)"
./scripts/ousject run examples/objects.px \
  --state "$state_dir/objects.oms" --local
./scripts/ousject check --state "$state_dir/objects.oms" --local
./scripts/ousject list --state "$state_dir/objects.oms" --local
```

`objects.px` 会创建和更新 Object、建立命名 Link，并通过稳定 Object ID 再次找到对象。
`check` 验证持久状态的结构一致性，`list` 列出可见对象。

同一个 Store 同时只允许一个进程持有。若另一个进程已经打开它，命令会返回
`StoreInUse`，不会绕过独占保护。

## 3. 分开编译与执行

Praxis 编译器输出 OTF0 Program：

```bash
./scripts/ousject compile examples/hello.px "$state_dir/hello.tf"
./scripts/ousject tf-dump "$state_dir/hello.tf"
./scripts/ousject run-tf "$state_dir/hello.tf" \
  --state "$state_dir/objects.oms" --local
```

TF 是预发布格式，目前不承诺跨版本兼容。

## 4. 运行更多语言示例

```bash
./scripts/ousject run examples/control-flow.px --memory --local
./scripts/ousject run examples/collections.px --memory --local
./scripts/ousject run examples/classes.px --memory --local
./scripts/ousject run examples/transaction.px --memory --local
./scripts/ousject run examples/processes.px --memory --local
```

这些示例依次覆盖控制流、集合、Class、原子事务和子 Process。

## 5. 安装并启动 Praxis 用户空间

安装系统程序需要明确使用 `local` 权限：

```bash
./scripts/ousject system-install system \
  --state "$state_dir/system.oms" --local
./scripts/ousject boot --state "$state_dir/system.oms"
```

首次启动会初始化最高本地用户 `local` 并要求设置密码。之后启动需要登录；普通命令通过
`--session <token>` 使用认证会话。密码不应以明文持久化。

Shell 输入 `exit` 只退出当前前端，保留该用户的持久 Shell 子 Terminal 和 Process；下次登录会
重新连接，之前定义的变量仍可使用。输入 `close-terminal` 才会关闭当前 Shell Terminal。

`system/` 中的程序属于 Praxis 用户空间。它们通过 Object 能力访问系统服务，不直接调用
宿主文件 API 或 Rust CLI API。重复安装未变化的程序会复用现有 Program；替换后不再引用的旧
系统 Program 会被退役。

## 6. 清理临时状态

确认不再需要上面的测试数据后，可以删除刚创建的临时目录：

```bash
find "$state_dir" -type f -delete
find "$state_dir" -depth -type d -empty -delete
```

## 常见问题

### 为什么普通命令提示需要认证？

持久命令默认不授予系统权限。开发和恢复时显式传入 `--local`；正常使用应先登录，再传入
`--session <token>`。两者不能同时使用。

### 为什么例子使用 `/tmp`？

默认 Store 路径是 `.ousject/objects.oms`。快速体验使用临时路径，便于多次运行并避免把
本地状态误当成源码改动。

### 数据保存在哪里？如何清空？

`--state` 后面的路径就是持久 Store 的主文件路径。例如传入
`--state /tmp/ousject.oms` 时，数据保存在该 Store 及其同目录的 manifest、WAL 和可能的
generation 文件中。省略 `--state` 时，默认路径是相对于当前工作目录的
`.ousject/objects.oms`。`--memory` 则不写持久 Store 文件。`boot` 还会把同一路径的扩展名
改为 `.blocks` 作为宿主块设备文件；本例对应 `/tmp/ousject.blocks`。

清理前先退出 `boot` 或其他正在使用该 Store 的 Ousject 进程。确认不再需要数据后，下面的
命令会删除本例 `/tmp/ousject.oms` Store 的主文件和配套文件：

```bash
rm -f /tmp/ousject.oms /tmp/ousject.manifest /tmp/ousject.wal /tmp/ousject.lock \
  /tmp/ousject.oms.tmp /tmp/ousject.manifest.tmp /tmp/ousject.wal.tmp
find /tmp -maxdepth 1 -type f \
  \( -name 'ousject.oms.g*' -o -name 'ousject.wal.g*' \) -delete
```

如果连宿主块设备里的持久内容也要清空，并确认不再需要，再单独删除：

```bash
rm -f /tmp/ousject.blocks
```

若使用默认路径，应在项目目录下删除 `.ousject/objects.oms` 及对应的
`.ousject/objects.manifest`、`.ousject/objects.wal`、`.ousject/objects.lock` 和 generation
文件；如果也要清空宿主块设备数据，再删除 `.ousject/objects.blocks`。删除后无法从该 Store
或块文件恢复；想保留账号、对象或块数据时不要清理对应文件。

### `--memory` 是否仍然执行事务？

是。它只是不写入持久 Backend；Object、权限、版本和事务规则仍由 OMS 执行。

### 下一步读什么？

编写 `.px` 前先看 [Praxis 语法参考](../reference/praxis-syntax.md)和
[Praxis API 总表](../reference/praxis-api.md)；理解底层模型则继续阅读
[系统架构](../concepts/architecture.md)和[对象模型](../concepts/object-model.md)。
