# Ousject 完整包管理器实施计划

状态：范围收窄为“可用的简单包管理”。本地构建、导入/导出、安装、精确依赖、运行、升级/回滚、卸载/恢复已接通；市场索引、搜索和可续传下载已接通。市场安装只接受 Ousject Package。测试验收命令由 [STATUS.md](STATUS.md) 记录。

2026-10-07 Hosted Core 验收新增端到端覆盖：Package 构建、Bytes 导出/导入、依赖闭包和 coordinate/SHA 锁、安装校验、Manifest Export 调用、私有 Data、升级/回滚/卸载/恢复；升级和回滚提交故障会验证 Process 位置与已提交 Installation 集合不变。Application 测试还验证升级后运行 Process 仍使用旧 Package SHA 与旧 Program。本文第 17 节列的是完整发布清单，超出本轮 Hosted Core 验收范围的条目仍是后续工作。

## 当前有效范围（2026-10）

用户已明确要求包管理不要继续扩展成复杂平台，只补上 CilExec 包管理里实际有用的部分。当前有效的工作范围是：本地包导入/导出、市场配置与索引、搜索、详情、按 SHA 下载、必需依赖自动安装、普通安装和卸载生命周期。以下旧草案中的签名体系、市场发布服务器、系统扩展/驱动包、独立 Service 运行时、迁移框架和多层权限界面不属于本轮目标，不应据此继续扩展内核。

格式边界：CilExec 市场当前发布 SQLite/FCL，Ousject 使用 TF/Praxis；协议一致不等于包文件可互换。当前能读取 CilExec v1 索引并下载不超过 15 MiB 的文件；其 SQLite 包不能直接安装。Ousject Market 安装只适用于返回 Ousject Package 字节的兼容镜像。

本文定义包管理器达到“完整可用”时必须具备的功能、最终 PX 使用方式、内核对象、原子性规则、权限边界、实施顺序和验收标准。现有 [PACKAGE-MANAGEMENT-PLAN.md](PACKAGE-MANAGEMENT-PLAN.md) 保留架构背景；本文是后续实现的执行清单。

## 1. 最终目标

用户应当能够完成下面这条完整流程：

1. 用 PX 编写 Module 或 Application。
2. 把已有 Module、Program 和资源 Object 构建成不可变 Package。
3. 查看包的版本、SHA、依赖、权限和发布者。
4. 发布到本地或远程 Repository。
5. 搜索、下载并安装 Package。
6. 导入 Library，或启动 Application。
7. 调用 Package 对外公开的能力。
8. 安全升级、回滚、卸载和恢复。
9. 系统崩溃或断网后，不出现半安装、错误版本或信息丢失。

包管理器属于内核。包商店、图形界面和便捷工具可以由 PX 编写，但以下工作只能由内核完成：哈希校验、签名验证、权限授予、依赖锁定、Process 绑定、原子安装、持久化和生命周期管理。

### 当前基线

已经具备：本地 Package 构建与验证、Bytes 导入/导出、coordinate 和 SHA 索引、每用户 Installation、Package Module、Terminal Session 精确导入、私有 Data、资源读取、原子依赖安装、Application Process 启动、Export、升级/回滚、卸载和七天恢复。Market 客户端已接入 CilExec Market v1 的索引、搜索、详情和分块续传下载。CilExec 的 SQLite/FCL 包格式与 Ousject 的 TF/Praxis Package 不兼容，因此不能直接安装。

Package 变更写入不可变 `core.package_audit` Object。Application 以独立 Package Subject 运行；`run(arguments, grants)` 只发放 Manifest 已声明且调用者明确列出的能力，并逐次校验具体方法。原始磁盘和共享 Network Endpoint 暂不授予 Application。CilExec SQLite/FCL 格式转换、远程 Ousject 发布端、签名、系统扩展包、独立 Service 运行时、安装 UI 和迁移框架不在本轮范围。

## 2. 最终使用方式

### 2.1 构建 Library

Package Builder 直接接收 Module Object，不再要求用户把源码写进字符串。

```praxis
modules = object.find("modules")
packages = object.find("packages")

greetings = modules.find("greetings", "0.1.0")

package = packages.build({
    namespace: "example",
    name: "greetings",
    version: "0.1.0",
    modules: { greetings: greetings }
})
```

`kind` 可以从是否存在入口自动判断：没有入口是 `library`，有入口是 `application`。需要时仍可显式填写，显式值与内容不一致时构建失败。

### 2.2 构建 Application

```praxis
editor_module = modules.find("editor", "0.1.0")
compiler = object.find("compiler")
editor_program = object.find(compiler.compile("func main() { }"))

package = packages.build({
    namespace: "example",
    name: "editor",
    version: "0.1.0",
    modules: { editor: editor_module },
    entry: editor_program,
    resources: { defaults: { tab_width: 4 } },
    capabilities: ["console", "keyboard", "time"]
})
```

这里假设 `editor-program` 是编译器已经生成并发布到 Namespace 的 Program Object。入口必须是已编译、可验证的 Program Object，并且包含无参数 `main()`。资源是 Value 或 Object 内容的不可变快照，不使用宿主文件路径。

### 2.3 安装和使用

```praxis
packages = object.find("packages")
editor = packages.install("example/editor/0.1.0")
process = editor.run()
```

Library 可以精确导入：

```praxis
import "package/example/greetings/0.1.0/greetings"
message = hello()
```

也可以取得包对象并调用它公开的能力：

```praxis
editor = packages.require("example/editor/0.1.0")
document = editor.open("notes")
```

`open` 等名字来自 Package Manifest 的 exports。运行时根据 Manifest 分派，无需为每个 Package 生成一个新的内核 Type。

### 2.4 升级、回滚和恢复

```praxis
new_editor = editor.upgrade("example/editor/0.2.0")
old_editor = new_editor.rollback(editor)
editor.uninstall()
restored = packages.restore(editor_id)
```

退役后 7 天内保留完整内容，可以恢复。7 天后可以清理大内容，但永久保留 ObjectId、类型、所有者、版本、哈希、来源、安装时间和退役时间等元数据。

## 3. 版本和 SHA

人使用版本坐标：

```text
example/editor/0.1.0
```

系统使用 SHA-256 锁定确切内容：

```text
example/editor/0.1.0
    -> sha256: 8c...f1
```

规则如下：

- 一个完整坐标永久只能对应一个 SHA。
- 相同内容只保存一份 Package。
- 代码、资源、Manifest 或依赖锁中任意一处变化，SHA 都必须变化。
- 依赖记录完整坐标和 SHA，运行时以 SHA 为最终依据。
- 已运行 Process 永久绑定启动时的 Package Object 和 SHA。
- SHA 证明内容相同，不证明作者身份；作者身份由数字签名证明。

PX 同时获得通用哈希服务：

```praxis
crypto = object.find("crypto")
digest = crypto.sha256(value)
```

它计算规范化 Value/Bytes/Text 的 SHA-256。Package 内核仍自行计算并验证，不信任调用者提交的哈希文本。

## 4. Package 内容模型

一个 Package 包含：

```text
Package
├── format_version = 0
├── namespace / name / version
├── kind: library | application | service | kernel_extension | driver
├── modules
│   ├── 可读 PX 源码
│   ├── 源码 SHA-256
│   └── 已编译 Program 与其 SHA-256
├── entrypoints
├── exports
├── resources
├── exact_dependencies
├── requested_capabilities
├── publisher identity
├── signature
└── complete package SHA-256
```

开发阶段所有格式版本继续保持 `0`。格式字段发生变化时同步迁移开发数据，不为了尚未发布的旧格式保留永久兼容代码。

### Package 种类

| 种类 | 行为 |
|---|---|
| `library` | 源码导入到调用者 Process，使用调用者已有权限 |
| `application` | 创建独立 Process，在 Package Subject 下运行入口 |
| `service` | 创建长期 Process，通过 exports 接收调用 |
| `kernel_extension` | 注册受信 Type 或内核能力，只接受受信签名 |
| `driver` | 发布硬件 Provider，只接受受信签名和硬件授权 |

普通 Library 不会因为 Manifest 写了某项能力，就自动得到调用者没有的权限。需要独立权限的功能必须做成 Application、Service、Kernel Extension 或 Driver。

## 5. 内核 Object

| Object Type | 作用 |
|---|---|
| `core.package_registry` | 本机 Package 总入口和用户安装索引 |
| `core.package` | 全局共享、不可变、按 SHA 标识的完整包 |
| `core.package_installation` | 一个用户安装了一个确切 Package 的记录和调用入口 |
| `core.package_module` | 某次安装暴露的模块身份；源码仍从共享 Package 读取 |
| `core.package_data` | 某用户、某 Package 版本的私有可变 Value |
| `core.package_subject` | Application/Service 的独立执行身份与授权集合 |
| `core.package_instance` | 一次运行实例，连接 Installation、Process 和 Package |
| `core.package_repository` | 可发现的仓库服务和已验证索引 |
| `core.package_download` | 可恢复的分块下载和暂存状态 |
| `core.publisher` | 发布者公钥、名称和信任状态 |
| `core.package_audit` | 安装、授权、升级、回滚、卸载和恢复记录 |

现有 `core.program`、`core.process`、`core.module_instance`、Effect、Session 和 Namespace 继续复用。

## 6. 最终 API

API 保持精简。能由统一 Object API 完成的读取、Link 和普通 Value 修改，不再重复增加 Package 专用能力。

### Package Registry

| API | 用途 |
|---|---|
| `packages.build(spec)` | 从 Module、Program 和资源 Object 构建 Package；仅有发布权限者可用 |
| `packages.install(source)` | 安装 Package Object 或仓库坐标，并原子安装依赖闭包 |
| `packages.installed()` | 返回当前用户安装的 Package |
| `packages.require(coordinate)` | 取得当前用户的确切 Installation |
| `packages.restore(object_id)` | 在 7 天内容保留期内恢复已退役安装 |

### Package

| API | 用途 |
|---|---|
| `package.info()` | 返回坐标、SHA、种类、依赖、权限、发布者和大小摘要 |
| `package.verify()` | 重新验证完整哈希、内部哈希、编译结果和签名 |
| `package.publish(repository)` | 发布到指定 Repository；需要发布权限 |

### Installation

| API | 用途 |
|---|---|
| `installation.info()` | 查看当前版本、SHA、依赖、权限和运行实例 |
| `installation.verify()` | 验证安装索引和全部引用关系 |
| `installation.module(name)` | 获取包内 Module Object |
| `installation.resource(name)` | 获取只读资源 Value/Object |
| `installation.data()` | 获取当前用户专属可变数据 Object |
| `installation.run([entry], [arguments])` | 创建 Package Instance 和 Process |
| `installation.upgrade(source)` | 原子切换到新版本，返回新 Installation |
| `installation.rollback(target)` | 将默认版本切回已验证旧版本 |
| `installation.uninstall()` | 安全退役安装；活动实例或反向依赖存在时拒绝 |

Manifest 中声明的 export 直接成为 Installation 的可调用能力，例如 `editor.open()`。内核会把调用送到确切版本的 Service Process，并检查参数中每个 Object 引用的权限。

### Repository

| API | 用途 |
|---|---|
| `repository.search(query)` | 查询本地已验证索引，不要求联网 |
| `repository.find(coordinate)` | 返回坐标对应的 Package 描述 |
| `repository.update()` | 通过 Effect 更新签名索引并原子切换 |
| `repository.fetch(reference)` | 分块下载、续传、验证并返回 Package |

安装时允许自动调用 fetch，所以普通用户不必手工管理下载步骤。

## 7. 依赖规则

Manifest 中每个依赖都保存：

```text
coordinate
artifact_sha256
required_exports
reason
```

Builder 接收 Installation 或 Package Object，自动写入这些锁定字段：

```praxis
package = packages.build({
    name: "editor-plugin",
    version: "0.1.0",
    modules: { plugin: plugin_module },
    dependencies: [editor]
})
```

实现规则：

- 不支持 `^1.2`、`latest` 等模糊范围。
- 构建时解析并锁定完整依赖闭包。
- 检查依赖环、重复坐标不同 SHA、缺失 export 和最大深度。
- 安装前先下载和验证全部 Package。
- 发布安装状态时，将完整闭包放入一次 OMS 原子提交。
- 共享依赖按 Package SHA 去重；每个用户仍有独立安装记录。
- 卸载根包时只退役没有其他安装或运行实例引用的依赖安装。

## 8. 权限和运行隔离

每个 Application 或 Service Installation 建立独立 Package Subject。用户在安装时看到请求能力，并明确授予允许的 Object Capability。

流程如下：

```text
Manifest 声明能力
    -> 安装器解析为具体 Object + Capability
    -> 用户/管理员授权
    -> 创建 Package Subject
    -> 把最小权限授给 Subject
    -> Process 使用该 Subject 运行
```

安全规则：

- Package 不能自行扩大权限。
- 未授予的能力在运行时直接失败。
- Package 不能默认发现用户的全部 Object。
- Package Data 只对所有者和对应 Package Subject 可见。
- 导出调用不能把调用者无权访问的 Object 偷渡给 Package。
- Package 调用另一个 Package 时保留调用链和审计身份。
- Driver 和 Kernel Extension 不能由普通 Package 身份安装。
- 不加载 Rust/C 动态库，不执行宿主 Shell 或安装脚本。

## 9. Application、Service 和 Export

`installation.run()` 必须在一个事务中建立：

- `core.package_instance`
- `core.process`
- Process 到确切 Package、Program、Installation 和 Package Subject 的 Link
- 初始参数 Value Object
- 调度器可运行状态

启动失败时不留下“已运行”的实例。

Service export 的调用采用内核消息机制：

1. 验证调用者对 Installation 的 Invoke 权限。
2. 验证 export 名称、参数数量和参数类型。
3. 创建持久请求 Object，并投递给确切 Service Instance。
4. Service 在自己的 Package Subject 下执行。
5. 原子保存结果或错误并唤醒调用者。
6. 崩溃恢复后继续处理尚未完成的请求，使用幂等请求 ID 防止重复提交结果。

## 10. 数据、资源和迁移

Package Data 是 Object，不是文件夹。修改仍由 OMS 自动持久化。

- 每个用户、每个 Package 主版本拥有独立 Data Object。
- Data 有逻辑大小和配额，写入前检查。
- 资源是 Package 中的只读 Value/Bytes/Object 快照。
- 升级默认先复制旧 Data 的一致性快照，再运行声明式 PX 迁移函数。
- 迁移在新版本的受限 Subject 下执行。
- 迁移结果和新 Installation 一次原子发布。
- 失败时旧版本和旧 Data 完全不变。
- 回滚不会覆盖新数据；保留两个版本的数据并要求显式选择或反向迁移。

## 11. Repository、发布者和签名

Repository 索引包含坐标、SHA、大小、依赖、发布者和签名。索引本身也必须签名。

下载流程：

```text
取得签名索引
  -> 建立 Download Object
  -> 分块下载并逐块校验
  -> 断线后按已验证块续传
  -> 验证完整 Package SHA
  -> 验证发布者签名和撤销状态
  -> 转为不可变 Package
  -> 才允许进入安装事务
```

普通 Library/Application 可以允许用户信任的新发布者。Kernel Extension 和 Driver 必须由 `local` 信任列表中的系统发布者签名，并单独显示将注册的 Type、Provider 和能力。

Repository、下载缓存和外部回执永远不是本机安装状态的权威。OMS 中的 Installation Object 才是唯一权威。

## 12. 升级、回滚、卸载和恢复

### 升级

- 完整验证新 Package 和依赖闭包。
- 显示新增、移除和变化的权限。
- 权限扩大必须重新授权。
- 新 Process 使用新 Package；旧 Process 继续绑定旧 Package。
- 默认版本 Link、安装记录和迁移后的 Data 一次提交。

### 回滚

- 目标 Package 必须仍完整且签名有效。
- 默认版本 Link 原子切换。
- 已运行的新版本 Process 不被偷偷换代码。
- 数据不自动倒退，避免丢失升级后产生的信息。

### 卸载

- 计算反向依赖和活动 Package Instance。
- 有使用者时拒绝，并返回具体阻塞对象。
- 一次事务移除用户索引并退役 Installation、Module、Data 和无引用依赖记录。
- 全局共享 Package 只有在没有安装、运行实例、保留期和审计要求时才可回收大内容。

### 恢复

- 7 天内根据永久 ObjectId 恢复完整对象树、Link 和权限。
- 恢复前重新验证 Package 和依赖仍存在。
- 已被其他内容占用的不可变坐标不能覆盖，必须报告冲突。

## 13. 原子性和崩溃恢复

必须始终满足：

- 构建结果要么完整登记，要么完全不可见。
- 完整依赖安装要么全部安装，要么全部不安装。
- Process 启动记录和可运行状态同时提交。
- 升级默认版本、数据迁移结果和权限同时提交。
- 卸载不会留下断裂的依赖 Link。
- 已确认提交的信息在断电恢复后仍存在。
- 未确认提交的信息不会恢复成“成功”。

网络下载属于外部 Effect，不能假装与远端服务器形成同一个事务。系统先把下载保存为可恢复暂存对象，全部验证成功后，才用一次本地 OMS 事务发布 Package 和 Installation。

启动恢复时检查：

- Package SHA、内部 Program 和资源哈希。
- 坐标索引是否指向正确 Package。
- Installation 的依赖闭包、所有者和权限。
- Package Instance 与 Process 的确切版本绑定。
- 未完成下载、安装 Effect、迁移和 export 请求。

发现损坏时隔离对象并保留原始信息，不静默生成替代内容。

## 14. 性能和限制

热路径不能扫描全部 Object。需要持久索引：

- `coordinate -> package`
- `sha256 -> package`
- `(subject, coordinate) -> installation`
- `package -> installations/instances`
- `installation -> reverse dependants`
- `publisher -> packages`
- `repository query term -> entries`

限制至少包括：

- 单 Module 源码 1 MiB。
- 单 Package 原始内容 64 MiB。
- 编译结果 32 MiB。
- 最多 256 个 Module、256 个资源、256 个依赖和 256 个 export。
- 最多声明 128 项能力；依赖深度最多 64。
- Manifest 文本、参数、返回值、Data 和下载缓存分别设置上限。
- 构建、验证和依赖分析使用有界内存和线性/近线性算法。
- 编译 Program 以 `(source SHA, language version, target ABI)` 缓存。
- 已验证 Package 在内存中缓存成功结果；缓存项绑定 Package 与 Repository Registry 的 Object 版本，任一改变即失效，缓存有 1024 项上限且可随时清空。
- 相同 Package 和资源内容全局去重。

## 15. 审计和可观察性

下列动作写入不可变审计 Object：

- 构建和发布。
- 安装及实际依赖闭包。
- 权限授予、拒绝和变化。
- 启动、停止和崩溃。
- 升级、迁移、回滚、卸载和恢复。
- 签名失败、哈希失败和仓库索引拒绝。

`installation.info()` 返回用户需要的摘要；完整内部对象仍可通过统一 Object Inspect 能力检查。错误必须包含具体坐标、SHA、阻塞对象或失败阶段，不能只返回“安装失败”。

## 16. 实施阶段

### 阶段 1：修正构建体验和内容格式（核心功能已实现并验收）

- 增加 `modules.find(name, version)`。
- `packages.build()` 接受 Module/Program/Object，不再接受嵌入源码字符串作为正式接口。
- 自动推断 Package kind。
- 加入资源、exports、入口和规范化 Manifest。
- 增加 `crypto.sha256(value)` PX 能力。
- 统一当前代码中的 `version`/`release` 命名为 `version`。

完成标准：用户不用复制源码字符串即可构建 Library 和 Application；相同对象输入得到相同 SHA。Module/Program 输入、资源快照、Export 声明检查和 SHA API 已实现；端到端用例验证 Package 字节往返和 `verify()`。

### 阶段 2：精确依赖闭包和完整安装验证（核心实现完成；基础闭包验收通过）

- [x] Builder 从 Package/Installation 自动生成坐标和 SHA 锁。
- [x] 实现依赖图、环检测、冲突检测和数量限制。
- [x] 一次事务安装完整闭包，并写入正向及反向依赖 Links。
- [x] `installation.verify()` 覆盖直接依赖、Module、Package、资源和索引。
- [x] 卸载检查活动反向依赖。

端到端用例验证依赖随 Package 安装、依赖坐标/SHA 与依赖 Package 一致，并在升级/回滚持久提交失败时检查 Installation 集合和 Process 位置。并发安装冲突、恶意错误数据与所有安装阶段断电点仍属于扩展发布清单。

### 阶段 3：Application 和 Process 版本绑定（核心实现与版本固定验收完成）

- [x] 实现 `installation.run()`。
- [x] 增加 Package Subject、Package Instance 和 Process 精确 Package Link。
- [x] 把启动参数、资源及私有数据入口与 Process 一起原子保存。
- [x] Process 作为持久 Process 由现有调度器恢复。
- [x] 活动实例阻止卸载对应版本。

完成标准：Application 可以启动并绑定安装时的 Package/SHA/Program；端到端用例在升级后继续运行旧 Process，并核对它执行旧 Program。进程重启恢复由 Hosted Process Recovery 覆盖。

### 阶段 4：权限强制和 Export（核心授权、Export 与审计已实现）

- [x] Application 启动时把用户明确批准的 capability 名称映射为本机 Object Capability。
- [x] `installation.permissions()` 返回声明能力和本机可授予状态，供 PX Shell/未来 UI 展示。
- [ ] 用 PX 实现交互式授权界面。
- [x] Application Process 使用独立 Subject；默认不授予外部能力。
- [x] 每次外部能力调用都检查具体方法；`console.println` 不会隐含允许 `console.read_line`。
- [x] 没有按 Application 隔离的 Provider 时，拒绝授予共享 Network Endpoint 和原始 Block Storage。
- [x] 实现 Export 名称分派：调用 Installation 导出方法会创建绑定固定 Program 的 Process；Library Export 以调用者身份运行。
- [ ] 支持 Service Subject 下的异步 Export；独立 Service 运行时不在当前 Hosted Library/Application 范围。
- [x] 成功变更与 Process 创建在同一事务中建立不可变审计记录；`packages.audit()` 使用持久索引查询。
- [x] Library 继续只使用调用者权限。

完成标准：未授予的 Console、Network、Keyboard、Data 或其他 Object 访问在运行时被拒绝；Export 在约定的 Process 身份下执行，结果、调用者身份和日志均可追踪。

### 阶段 5：数据、资源、升级、回滚和恢复（核心流程已接通）

- [x] 实现资源读取和私有 Data 保存。
- [x] 增加每个 Installation 8 MiB 的 Package Data 上限。
- [ ] 增加数据迁移脚本和升级权限差异确认。
- [x] 实现默认版本原子切换、版本并存和回滚。
- [x] 实现 7 天内恢复安装与 Data；到期后依赖 OMS 自动清理 payload。
- 保留永久元数据和审计。

完成标准：迁移框架仍未实现；已实现的版本切换不覆盖其他版本 Data，端到端用例验证 Data 保存、卸载和七天内恢复。

### 阶段 6：精简 Market 客户端（索引和下载已实现；格式转换未实现）

- [x] 实现本机 Package 坐标搜索和查找。
- [x] 实现 CilExec Market v1 索引获取、校验、用户隔离保存和搜索。
- [x] 实现 SHA-256 校验、分块下载、ETag/Range 续传和完整下载对象。
- [x] 对 Ousject Package 导入依赖先行、安装一次提交。
- [ ] 为大于 15 MiB 的下载增加分块 Object 读取；当前受单个 Value 编码上限约束。
- [ ] 设计/提供 Ousject Package 的发布端；CilExec 当前 SQLite/FCL 包不兼容，不能直接安装。

Effect 化外部请求和按坐标自动获取不是这次精简实现的一部分；`market.install(sha256)` 使用已缓存索引里的精确 SHA，并在本地 OMS 事务中完成安装。

完成标准：CilExec v1 索引可以读取；断网或中断后可按 Range 续传；完整 SHA 不匹配不返回可读取 Bytes；Ousject 依赖齐全后在同一事务安装。现有 CilExec SQLite 包格式边界明确，不假称兼容。

### 阶段 7：发布者、签名和系统 Package（本轮不实施）

- 实现 Publisher Object、公钥、信任和撤销。
- Package 和 Repository Index 签名验证。
- Kernel Extension/Driver 需要系统发布者和 `local` 授权。
- Type、Provider、服务对象和安装记录一次原子发布。

完成标准：篡改内容、伪造发布者和已撤销签名均不能安装；普通包不能注册内核能力。

### 阶段 8：性能、恢复和发布验收

- 建立全部持久索引和缓存。
- 测量构建、验证、冷安装、热安装、启动、升级、回滚、卸载和恢复。
- 验证并发安装/卸载/启动冲突。
- 验证 WAL、Checkpoint、备份和恢复后的哈希、权限、依赖、数据和审计完全一致。
- 更新 API 文档、PX 示例和系统快速开始文档。

完成标准：所有验收矩阵通过，文档只把真实完成的能力标为已实现。

## 17. 完整包管理器的扩展发布清单

下列场景记录超出当前 Hosted Core 验收的完整发布要求。本轮已有覆盖范围见本文开头、阶段 1–5 与 `PRE-HOST-ACCEPTANCE.md`；未执行的条目不应描述为已验收。

1. 同样输入重复构建得到相同 SHA 和同一个 Package。
2. 改动一个字符、一个资源或一个依赖后 SHA 改变。
3. 相同坐标不能发布不同 SHA。
4. 两个用户安装同一包，共享 Package，但 Installation 和 Data 分离。
5. 完整依赖闭包原子安装，注入失败后对象数和索引不变化。
6. Library 无法取得调用者未拥有的权限。
7. Application 只能使用安装时授予的能力。
8. Process 永久绑定确切 Package；升级不会替换运行中的代码。
9. 活动实例和反向依赖阻止卸载。
10. 迁移失败后旧安装和数据可继续使用。
11. 回滚不会删除新版本及其数据。
12. 7 天内恢复完整内容；过期后元数据仍可查询。
13. 下载中断可以续传；坏块、坏 SHA 和坏签名被拒绝。
14. 崩溃发生在每个事务边界时，只能恢复到完整的前态或后态。
15. 并发安装同一 SHA 幂等；并发安装同坐标不同 SHA 只有一个成功。
16. 备份恢复后 Package SHA、权限、依赖、Data 和审计完全一致。
17. Kernel Extension/Driver 未受信时不能注册 Type 或 Provider。
18. 恶意超大 Manifest、深依赖和循环依赖在资源上限内被拒绝。

## 18. 明确不做

- 不支持模糊版本范围和运行时自动选择 `latest`。
- 不运行安装/卸载 Shell 脚本。
- 不把 Package Data 伪装成文件目录。
- 不允许普通包加载宿主 Rust/C 动态库。
- 不让包通过声明自行获得权限。
- 不把仓库缓存或远程回执作为安装权威。
- 不在卸载时猜测并删除普通用户创建的 Object。

## 19. 最终完成定义

只有同时满足以下条件，包管理器才可以标记为“完整完成”：

- 本文阶段 1 至 8 全部实现。
- 18 个验收场景全部通过。
- 所有持久变更具有原子提交和崩溃恢复覆盖。
- 权限在运行时强制执行，不只是 Manifest 文本。
- Application、Library、Service、Repository、升级、回滚、恢复和签名均可由 PX 使用。
- 用户不需要把 PX 源码塞进字符串，不需要操作宿主文件或执行 Rust 命令。
- API、实现状态和快速开始文档与实际行为一致。
