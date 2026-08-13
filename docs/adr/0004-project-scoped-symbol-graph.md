# ADR-0004：Project-scoped 符号图

- 状态：Accepted
- 日期：2026-08-13

## 背景

Internal Hook Protocol 与 adapter 审计已经以持久化 `project_key` 隔离项目，但符号协议 v1
仍只用 Provider ID 与 provider key 生成节点 ID，SQLite 图也默认依赖“每个仓库一个数据库”
实现物理隔离。这会阻碍多项目汇总、共享存储和后续 semantic lineage，并可能让两个项目中
形状相同的符号发生身份碰撞。

## 决策

1. 符号协议升级为 v2；`SymbolSnapshot`、`SymbolNode` 与 `SymbolEdge` 显式携带 `project_key`。
2. 节点 ID 由 `project_key`、Provider ID 与 provider key 共同派生；快照 revision 同样覆盖
   `project_key`。
3. SQLite schema v4 的节点、边、索引、外键、查询与 tombstone 更新全部按 `project_key`
   限定，不把数据库路径当作身份边界。
4. 单个快照只能包含同一项目与同一 Provider 的节点和边；跨项目引用 fail closed。
5. v1-v3 的符号图没有可信项目归属。升级时清除这部分可重建缓存并保留动作、Hook 与 adapter
   审计，下一次 `project-brain index` 重新生成项目化图。
6. 后续 SCIP Provider 与 lineage candidate 必须继承相同项目边界；任何跨项目 lineage 都必须
   通过未来显式的跨项目关系协议表达，不能由符号相似度隐式创建。

## 结果

- 同一 Provider key 在同一项目中保持可重复，在不同项目中生成不同节点 ID。
- 多项目可以安全共用存储而不串节点、边、查询或 removed 状态。
- 迁移会丢弃旧的可重建符号缓存，但不会丢失权威配置或审计历史。

## 验收不变量

- 两个项目的相同源码和 Provider key 生成不同 symbol ID。
- 对项目 A 应用空快照不会 tombstone 项目 B 的节点或边。
- 查询必须提供 `project_key`，只返回该项目记录。
- 节点或边的项目身份与快照不一致时拒绝整个事务。
- v3 数据库升级后 schema 为 v4，旧无归属符号被清除，adapter 审计仍可读取。
