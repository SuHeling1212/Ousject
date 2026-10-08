# Ousject 文档

这套文档描述当前源码能够运行和验证的 Ousject `0.0.0`。除非明确标记为“规划”，文中
的功能陈述都应能在源码、测试或验收脚本中找到依据。

## 第一次接触项目

按以下顺序阅读：

1. [快速开始](getting-started/quickstart.md)：运行 Praxis 示例、持久 Store 和系统启动。
2. [系统架构](concepts/architecture.md)：理解各 crate 与运行时边界。
3. [对象模型](concepts/object-model.md)：理解 Object、Value、Type、权限和关系。
4. [Praxis 语法参考](reference/praxis-syntax.md)：完整的 `.px` 现行语法。
5. [Praxis API 总表](reference/praxis-api.md)：Registry、内核服务与 Object 能力。
6. [事务、持久化与恢复](concepts/durability.md)：理解提交与故障恢复保证。
7. [当前状态](project/status.md)：确认实现范围与已知限制。

## 开发与验证

- [构建与测试](getting-started/build-and-test.md)
- [`examples/`](../examples)：语言和对象能力示例
- [`system/`](../system)：Praxis 用户空间程序
- [`scripts/check-m1`](../scripts/check-m1)：OMS 验收入口
- [`scripts/check-system`](../scripts/check-system)：完整宿主系统验收入口

## 核心概念

- [系统架构](concepts/architecture.md)
- [对象模型](concepts/object-model.md)
- [事务、持久化与恢复](concepts/durability.md)
- [Native Platform 仓库审计](project/native-platform-audit.md)
- [Native UEFI Stage A1 研究记录](project/native-uefi-notes.md)

## 参考手册

- [Praxis（`.px`）语法参考](reference/praxis-syntax.md)
- [Praxis API 总表](reference/praxis-api.md)

## 文档目录规划

后续专题资料放在下列位置：

```text
docs/guides/       Praxis、系统运行和包管理指南
docs/reference/    CLI、Praxis 语言、Object API、Runtime internals
docs/project/      Roadmap 与性能基线
docs/adr/          架构决策记录
```

旧的根目录文档同时混合了实现、设想和阶段性计划，因此不会逐文件恢复。有效内容会经过
源码校验后并入上述结构；历史背景保留在 Git 历史中。

## 文档维护规则

1. 入口文档保持短小，通过链接进入专题文档。
2. “已实现”必须有源码或测试依据；仅有设计稿的内容放入 Roadmap 或 ADR。
3. 命令示例应从仓库根目录可执行，并尽量使用临时 Store，避免污染工作区。
4. CLI 参考以 `ousject help` 和命令解析代码为准。
5. Praxis 参考以编译器、VM 和端到端测试的共同能力为准。
6. 状态只在 `project/status.md` 维护，其他文档只链接它。
7. 持久格式均为预发布格式；文档不能暗示向后兼容承诺。
