# Ousject 包管理系统方案

> 完整交付顺序、当前实现状态、最终 PX API 和逐阶段验收标准见 [PACKAGE-MANAGER-COMPLETE-PLAN.md](PACKAGE-MANAGER-COMPLETE-PLAN.md)。本文保留架构背景和早期设计记录；早期状态表不再作为当前实现状态依据。

本文依据 CilExec 仓库 `171191e68893e6805269aef9874f431dce3584d9` 的包管理实现，设计适合 Ousject 统一对象模型的包管理系统。

这是一份架构背景和设计记录。当前实现状态以 [PACKAGE-MANAGER-COMPLETE-PLAN.md](PACKAGE-MANAGER-COMPLETE-PLAN.md) 为准；下方早期状态表保留当时的设计进度，不代表现在的代码状态。

## 当前实现进度

- 当前已有：本地 Package 构建和验证、每用户安装、精确依赖原子安装、Package Module 和 Export、Application 独立 Subject 与显式能力授权、私有 Data 和资源、升级/回滚/卸载/七天恢复、本地坐标查询。Package 校验有受版本约束的有界内存缓存。
- 当前未完成：安装授权界面、数据迁移、完整审计、远程 Repository 与续传、发布者签名、系统扩展包、故障注入/性能/备份恢复验收。细项见完整实施计划。
- 本文其余章节中的 `core.package_repository`、签名对象和远程流程仍属于目标架构，除非完整实施计划明确标记已完成。

## 1. 人话结论

Package 是一个可以分发和安装的完整功能包。一个 Package 可以包含多个 Module、程序入口、公开能力和资源。

```text
Package                         一件可以下载、安装、升级和卸载的产品
├── Module                     Package 内可复用的 Praxis 代码
├── Program                    已编译、可执行的入口
├── Export                     安装对象公开给用户调用的能力
├── Resource                   只读数据
├── Dependency                 依赖的确切 Package 版本
└── Capability declaration     运行时需要的系统权限
```

Package 不替代 Module。Package 负责分发和生命周期，Module 负责组织代码。

Praxis 源码导入是正式能力，不会被 Package 或已编译 Program 取代。Application 的入口源码必须声明无参数 `main()`；Package 内被导入的 Module 禁止声明 `main()`，但可以执行顶层初始化，然后把其中声明的函数和 Class 提供给入口 `main()` 使用。宿主文件读取只是当前开发适配器，最终源码由 Module Object 保存和加载。

用户安装 Package 后得到一个 `core.package_installation` Object。用户首先发现包管理对象，然后使用安装对象的能力：

```praxis
packages = object.find("packages")
repository = object.find("package_repository")

release = repository.find("ousject/editor", "0")
editor = packages.install(release)
editor.open("notes")
```

这里没有宿主命令、传统文件路径或手动保存。每一次成功修改仍由 OMS 原子提交并自动持久化。

## 2. 从 CilExec 得到的经验

CilExec 当前包系统的主要结构是：

1. 把一个发布版本做成不可变的 SQLite `package.db`。
2. 用整个包文件的 SHA-256 作为精确身份。
3. 依赖记录对方的精确 SHA-256，不在安装时临时选择版本。
4. 全局只保存一份不可变发布内容，每个用户分别保存安装记录。
5. 每个安装根保存完整依赖闭包，因此可以解释某个依赖为什么还不能删除。
6. Process 保存自己实际使用的确切 Package 绑定。
7. 每个用户、每个 Package 版本有隔离的私有数据空间和配额。
8. 卸载前计算完整影响，发现依赖或活动 Process 时拒绝，成功时一次事务完成。
9. Market 提供索引、搜索、分块下载、哈希校验和发布服务。
10. Package 声明所需能力，CilExec 会审计源码调用。

CilExec 文档也明确记录了两个尚未完全解决的问题：Market 的安装回执仍可能被当作权威，整个依赖包集合还没有一次性原子发布。Ousject 从第一版就不使用独立回执文件，并把完整依赖集合放进一次 OMS 提交。

## 3. Ousject 采用和修改的部分

| CilExec 做法 | Ousject 决定 |
|---|---|
| 不可变发布物 | 采用 |
| 整包 SHA-256 身份 | 采用；哈希表示内容身份，不表示作者身份 |
| 精确哈希依赖 | 采用 |
| 每个用户独立安装 | 采用 |
| Process 绑定确切发布版本 | 采用 |
| 包私有数据和配额 | 采用，但实现为 Object，不实现为虚拟文件目录 |
| 原子安装和卸载 | 采用，并覆盖完整依赖集合 |
| SQLite 包文件 | 不采用；使用规范化 TF Package |
| PostgreSQL 安装表 | 不采用；使用 OMS Object、Link、Index 和 Transaction |
| VFS 路径和 JSON 回执 | 不采用；Object 是唯一权威记录 |
| 只依靠源码能力审计 | 加强；静态检查加运行时 Subject 强制检查 |
| 哈希代替发布者认证 | 只用于普通开发源；系统扩展和驱动必须验证发布者签名 |

## 4. 包制品格式

本地构建产生一个不可变 `core.package` Object。下载时得到相同的规范化字节内容。

格式版本在系统发布前保持 `0`：

```text
TF Package 0
├── manifest
│   ├── namespace
│   ├── name
│   ├── release
│   ├── praxis_language
│   ├── kind
│   ├── modules
│   ├── dependencies
│   ├── entrypoints
│   ├── exports
│   ├── resources
│   └── requested_capabilities
├── module source or portable compiled form
├── read-only resources
├── per-entry SHA-256
├── complete package SHA-256
└── optional publisher signature
```

首个实现只允许两种 `kind`：

| Kind | 用途 |
|---|---|
| `library` | 提供可复用能力 |
| `application` | 提供可启动入口，也可以有公开能力 |

`kernel_extension` 和 `driver` 沿用内核扩展计划，在权限隔离完成后再接入包管理器，不能把普通 Package 改名后获得内核权限。

同一个 `namespace/name/release` 永久对应同一个完整哈希。即使旧内容已退役，也不能用另一份内容覆盖相同坐标。

## 5. 核心 Object

### 5.1 `core.package_release`

表示一个全局不可变发布版本：

```text
id
coordinate = namespace/name/release
artifact_hash
manifest
modules
programs
resources
dependencies
requested_capabilities
publisher
signature
status
```

相同哈希只存一份大内容。多个用户安装时不会重复保存源码、资源和已编译 Program。

### 5.2 `core.package_installation`

表示某个用户安装了某个确切发布版本：

```text
owner
root_release
dependency_closure
granted_capabilities
installed_at
source_repository
status
```

它也是用户实际调用的对象。Manifest 中的 Export 会成为安装对象的公开能力：

```praxis
editor = packages.require("ousject/editor")
editor.open("notes")
```

### 5.3 `core.package_data`

表示 `(用户, 确切发布版本)` 的私有可变数据：

```text
owner
release
value
logical_size
quota
version
```

它保存普通 Value、Record、Collection 或 Bytes，不伪装成目录。包只能获得自己的数据对象，不能传入别的 Package ID 绕过隔离。升级后的新版本默认获得新的数据对象；数据迁移必须由用户明确执行。

### 5.4 `core.package_repository`

表示一个可发现的包仓库服务，负责索引、查询、下载和发布。仓库内容不是本机安装状态的权威；本机 OMS 中的 Installation Object 才是权威。

### 5.5 复用现有 Object

- 每个 Package Module 继续使用 `core.module`。
- 编译结果继续使用 `core.program`。
- Terminal 或 Process 实际加载模块时继续使用 `core.module_instance`。
- Process 增加到 `core.package_release` 的精确 Link，确保运行中不会被卸载。

## 6. 最小 API

API 遵守 Ousject 的统一规则：先发现对象，再调用对象能力。首版不增加包管理命令行指令。

### 6.1 Package Registry

```praxis
packages = object.find("packages")
```

| 能力 | 用途 |
|---|---|
| `packages.build(spec)` | 从规范化描述和对象内容构建本地 Package |
| `packages.install(release_or_artifact)` | 原子安装完整依赖集合，返回 Installation Object |
| `packages.installed()` | 返回当前用户的 Installation Object |
| `packages.require(coordinate)` | 找到当前用户已经安装的包 |
| `packages.restore(installation)` | 在 7 天内容保留期内恢复误卸载的安装 |

### 6.2 Installation Object

| 能力 | 用途 |
|---|---|
| `installation.info()` | 查看坐标、哈希、依赖、权限和状态 |
| `installation.verify()` | 重新检查 Package、Module 和 Program 哈希 |
| `installation.run(entrypoint, arguments)` | 创建 Process 并运行一个入口 |
| `installation.module(name)` | 返回包内的 Module Object |
| `installation.data()` | 返回当前用户与此版本的私有数据 Object |
| `installation.uninstall()` | 安全卸载；有活动使用者或反向依赖时拒绝 |

Manifest 中声明的 Export 会直接出现在 Installation Object 上，所以不需要通用的字符串调用 API：

```praxis
editor.open("notes")
math_extra.calculate(value)
```

### 6.3 Repository Object

```praxis
repository = object.find("package_repository")
```

| 能力 | 用途 |
|---|---|
| `repository.configure(origin)` | 配置仓库来源；修改需要相应权限 |
| `repository.update()` | 原子更新可查询索引 |
| `repository.search(text)` | 本地查询已验证索引 |
| `repository.find(coordinate, release)` | 返回准确 Release 描述 |
| `repository.fetch(release)` | 可恢复地分块下载并验证 Package |
| `repository.publish(package, metadata)` | 发布；需要发布者身份和权限 |

`packages.install(repository.find(...))` 可以内部触发下载，因此普通使用不必先手动调用 `fetch()`。

## 7. 构建流程

```text
读取 Package Spec 和引用的 Object
    ↓
规范化 Manifest 字段顺序与编码
    ↓
解析并编译全部 Praxis Module
    ↓
检查 Export 与 Entrypoint 确实存在
    ↓
检查依赖只使用 Manifest 声明的精确 Release
    ↓
静态计算所使用的系统能力
    ↓
拒绝未声明能力
    ↓
计算每项哈希和整包哈希
    ↓
可选签名
    ↓
创建不可变 Package Object
```

构建相同输入必须产生完全相同的规范化内容和哈希。时间戳、随机 ObjectId 和构建机器路径不能进入哈希内容。

## 8. 原子安装流程

耗时的下载、解码、哈希和编译在事务外的隔离暂存区完成。正式发布使用一次短 OMS 事务：

```text
1. 获取完整 Package 和依赖 Package
2. 检查大小、格式、坐标、所有哈希与签名策略
3. 构造精确依赖图，拒绝环和过深依赖
4. 编译并检查全部 Module、Entrypoint 和 Export
5. 检查当前用户能否授予请求的能力
6. 准备 Release、Module、Program、Data 和 Installation
7. 开始 OMS Transaction
8. 对仓库索引、依赖 Release 和权限版本设置 expect
9. 创建或复用全局 Release 内容
10. 创建当前用户的安装根和完整依赖闭包
11. 创建缺少的私有数据 Object
12. 创建全部 Parent 与 Link
13. 一次 commit
```

任意一步失败都不会出现“主包已安装但依赖没装完”的状态。重复安装同一版本返回现有 Installation，不复制内容，也不清空私有数据。

## 9. 权限模型

Package 的权限等于以下三者的交集：

```text
Manifest 请求能力
∩ 当前用户允许授予的能力
∩ 系统策略允许此 Package 获得的能力
```

安装时的静态检查用于尽早发现问题。真正的安全边界是运行时：

1. 每个 Package 执行使用独立 Subject，不继承调用者的全部权限。
2. 进入另一个依赖 Package 的函数时切换到依赖 Package 的 Subject。
3. 返回后恢复原 Package Subject。
4. 每次 Object Capability 调用仍由 OMS 权限检查。
5. 顶层用户代码没有 Package 身份，不能伪造包私有数据访问。

普通用户可以安装普通 Praxis Package，但只能授予自己已有且允许转授的能力。只有 `local` 可以安装内核扩展和驱动，且安装前必须显示完整能力变化。

## 10. 运行与版本绑定

创建 Package Process 时一次写入它使用的全部确切 Release Link。Process 恢复时根据这些 Link 加载原版本，不按名称重新寻找“最新版”。

升级执行以下动作：

1. 将新版本作为另一份不可变 Release 完整验证。
2. 建立新的 Installation 和新的私有数据对象。
3. 原子切换用户的默认坐标绑定。
4. 已运行 Process 继续使用旧版本。
5. 新 Process 使用新版本。

回滚只切换默认绑定，不修改任何 Release 内容。旧版本仍被 Process 或用户明确保留时不得清理。

## 11. 卸载、自动依赖清理和恢复

默认卸载先生成影响计划：

- 哪些 Process 正在使用目标版本；
- 哪些当前用户安装根依赖它；
- 哪些依赖只是自动安装且已经成为孤儿；
- 哪些 Module Instance 仍然活动；
- 哪些私有数据属于该安装；
- 是否还有其他用户引用同一全局 Release。

存在活动 Process、Module Instance 或反向依赖时拒绝卸载，并返回具体阻塞 Object ID。

成功卸载在一个事务中完成：

1. 退役用户的 Installation。
2. 退役只由它需要的孤立自动依赖安装。
3. 退役对应的用户私有数据。
4. 删除默认坐标绑定。
5. 没有任何用户、Process 或安装引用的全局 Release 进入退役状态。

所有退役 Object 遵守系统统一规则：立即不可再用，内容保留 7 天，最小元数据永久保留，ObjectId 永不复用。7 天内可以通过 `packages.restore(...)` 恢复误卸载内容。任何与该 Package 无关的用户 Object 都不会被猜测或删除。

## 12. 仓库协议

首版协议保持很小：

| 请求 | 用途 |
|---|---|
| `GET /packages/0/index.tf` | 获取带版本与 ETag 的规范化索引 |
| `GET /packages/0/packages/{sha256}` | 分块下载不可变 Package |
| `POST /packages/0/publish` | 认证后发布 Package |

要求：

- Package 最大 64 MiB，索引和限制以后可按系统能力调整。
- 支持 Range、ETag、断点续传和明确的 Content-Length。
- 下载过程中增量计算 SHA-256。
- 完成前只存在暂存 Object，不向 Package Registry 发布。
- 完成后重新验证整体哈希、内部哈希和 Manifest。
- 发布采用“先完整验证 Package，再原子替换索引快照”。
- 服务端绝不覆盖已经发布的哈希内容。
- 上一个有效索引在新索引验证失败时继续可用。

精确哈希只能证明“收到的内容与索引一致”。系统扩展和驱动还必须验证受信发布者签名。普通 Package 的签名策略可以由仓库和用户策略决定。

## 13. 性能设计

### 13.1 避免重复数据

- Release、Module 内容、Program 和资源按哈希去重。
- Installation 只保存用户关系和权限，不复制整包。
- 大 Bytes 使用 OMS 的不可变分段和 Copy-on-Write。

### 13.2 避免全库扫描

至少建立以下索引：

```text
artifact_hash → Release
coordinate → Release history
(owner, coordinate) → default Installation
(owner, release) → Installation
dependency release → dependent Installation
release → active Process and Module Instance
repository → verified index snapshot
```

现有 Module Registry 中按 Type 扫描全部 Module 的做法不能成为正式包管理热路径。

### 13.3 缩短事务时间

- 网络下载、解压、编译和完整哈希在事务外完成。
- 正式提交只写入已验证对象、索引和 Link。
- 事务用对象版本 `expect` 防止验证后内容或权限发生变化。
- 冲突时丢弃候选提交并按最新快照重新验证，不能覆盖别人的提交。

### 13.4 编译一次

- Release 保存版本化、可验证的 Program 指令制品。
- Process 启动和崩溃恢复直接使用已验证 Program。
- 可读 Praxis 源码仍然保留，用于检查、调试和未来重新编译。
- 指令格式、语言版本或目标 ABI 不匹配时明确拒绝，不能悄悄用错误格式执行。

### 13.5 限制恶意或失控输入

首版限制建议：

- Package：64 MiB。
- 单个 Module 源码：1 MiB。
- 必需依赖深度：64。
- Module、Export、Resource 和 Dependency 数量分别设置上限。
- Manifest 文本字段设置长度上限。
- 依赖图检查使用线性时间算法。
- 仓库下载和编译有内存、时间与并发预算。

## 14. 崩溃恢复和信息完整性

系统不依赖内存列表判断安装结果：

- OMS commit 成功，整个安装可见。
- OMS commit 未成功，整个安装不可见。
- 暂存下载可以继续或安全退役。
- 启动时检查 Installation 的闭包 Link、Release 哈希、Program 格式和 Data 所有者。
- 发现损坏时隔离该安装，保留原始内容并报告，不自行重建成另一份内容。
- 修复行为必须产生审计 Object。

备份应从同一个一致性快照导出 Package Release、Installation、Module、Program、用户权限、Package Data 和所有 Link。

## 15. 实施顺序

### 阶段 A：稳定现有 Module 基础

1. 为 Terminal Session 增加 close 和 module unload。
2. 清理失效 Module Instance。
3. 建立 Module 名称、哈希和反向依赖索引。
4. 将 Module 声明能力变成运行时强制权限。

完成标准：终端关闭后不再永久阻塞 Module 卸载；Module 无法调用未授权 Object Capability。

### 阶段 B：本地 Package

1. 增加 Package Manifest 数据结构和规范化 TF 编码。
2. 增加离线 Builder、Reader 和 Verifier。
3. 一个 Package 支持多个 Module、Export、Entrypoint 和 Resource。
4. 保存源码和已编译 Program 哈希。

完成标准：相同输入生成相同哈希；任意字节、Manifest 或内部哈希变化都会被拒绝。

### 阶段 C：按用户原子安装

1. 增加 Release、Installation 和 Package Data Type。
2. 增加完整精确依赖闭包。
3. 一次事务安装整个依赖集合。
4. 重复安装幂等，多个用户共享不可变 Release 内容。
5. Export 成为 Installation Object 的能力。

完成标准：故意在任一步注入失败都不会产生半安装；重启后可以直接调用 Export。

### 阶段 D：执行隔离和生命周期

1. 为每个 Package Installation 建立受限 Subject。
2. 实现嵌套 Package 调用的身份切换。
3. Process 锁定确切 Release。
4. 实现升级、回滚、安全卸载、孤儿依赖清理和 7 天恢复。

完成标准：旧 Process 在升级后仍运行旧版本；越权调用在运行时被拒绝；活动使用者不会被卸载。

### 阶段 E：Package Repository

1. 建立索引 Object 和查询 API。
2. 实现分块下载、断点续传、完整哈希验证与暂存恢复。
3. 安装时一次原子发布完整依赖集合。
4. 本地安装状态完全来自 OMS Installation Object。

完成标准：断网和崩溃不会生成已安装的残缺包；伪造缓存回执不能改变安装状态。

### 阶段 F：发布者身份和系统包

1. 建立发布者密钥 Object 和信任策略。
2. 验证 Package 签名与撤销状态。
3. 允许经过授权的 Kernel Extension 和 Driver Package 注册 Type 与 Provider。
4. Type、服务、权限和安装记录必须在一次事务中发布。

完成标准：普通 Package 无法获得内核能力；未受信签名不能安装系统扩展或驱动。

### 阶段 G：性能与恢复验收

1. 加入内容去重、索引、缓存和编译制品快速加载。
2. 测量构建、冷安装、重复安装、启动、升级、卸载和恢复。
3. 验证并发安装同一版本、并发卸载与启动、仓库索引切换。
4. 验证快照备份和恢复后哈希、权限、依赖与私有数据完全一致。

完成标准：热路径不扫描全部 Object；并发冲突不会丢失信息；崩溃前已提交的信息可以完整恢复。

## 16. 第一版明确不做的事

- 不实现类似 npm 的宽松版本范围求解。Manifest 必须锁定精确 Package 哈希。
- 不执行安装脚本、卸载脚本或任意宿主命令。
- 不加载 Rust、C 或操作系统动态库。
- 不把 Package 数据伪装成传统目录或文件。
- 不允许包自己扩大权限。
- 不因卸载 Package 猜测并删除普通用户 Object。
- 不让仓库缓存、下载记录或外部 JSON 成为安装权威。

这些限制使第一版的安装结果可重复、可审计，并让一次 OMS 原子提交可以描述完整状态变化。

## 17. 最终不变量

包管理系统必须始终满足：

```text
同一发布坐标永远对应同一内容
依赖永远指向确切内容
一个用户的安装不会自动成为另一个用户的安装
Package 只能使用实际授予的能力
Process 恢复时继续使用原来的确切版本
完整依赖集合要么全部安装，要么全部不安装
卸载不会破坏仍在使用的版本
任何已提交信息不会因为升级、回滚、卸载或崩溃而无记录地消失
```
