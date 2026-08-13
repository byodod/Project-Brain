# ADR 0010：语义源码覆盖必须有可重放 manifest

## 状态

Accepted

其中“局部覆盖可保存快照”的决定已由 ADR 0011 取代；manifest 与 doctor 契约继续有效。

## 背景

Provider 进程成功、SCIP 可解析和符号数量非零，都不能证明声明范围内的源码被完整索引。真实
`rust-analyzer scip` 验收曾在同一 Cargo workspace 中只输出部分 crate；若只展示 definition 数量，
Project Brain 会制造错误的全仓安全感。

同时，旧数据库只保存有 symbol 的 observation。没有 symbol 的文档与被 Provider 完全遗漏的文件
无法从该表恢复，所以迁移不得补造历史覆盖证据。

## 决策

1. SQLite schema v7 为每个新 semantic snapshot 在同一事务中保存 source manifest 和逐文件
   observation；manifest 固定文档数量与规范化内容摘要。
2. 迁移只创建空表。旧快照标记为 manifest 未记录；真实重跑完全相同的快照时允许首次补录。
3. 覆盖率只对项目显式声明、Project Brain 已知扩展名契约的 Rust、Python、C#、Visual Basic 和
   F# 计算。未知 language 返回 `unverifiable`，不猜测。
4. expected 集合来自 Git 已跟踪及未忽略文件、language roots 和扩展名契约；indexed 集合来自
   Provider 实际 Document 清单。报告保留总数和有界路径样本。
5. `partial` 与 `stale` 使显式 doctor 降级；`not_indexed` 是明确 warning，避免破坏首次 bootstrap，
   也不声称覆盖完整。
6. 局部覆盖不会删除 Provider 已提供的真实语义事实；symbol hard gate 仍需其自身的 trusted、fresh、
   direct/confirmed 和确定性影响证据。

## 后果

- Provider 漏索引变成可见、可测试、可作为 CI 门禁的事实。
- 数据库略增大，但 source manifest 是可重建缓存且只按不可变快照追加。
- 自定义语言需要未来显式注册扩展名/文件发现契约，不能依赖启发式猜测。
