# ADR-0001：Provider-neutral 符号身份

- 状态：Accepted
- 日期：2026-08-13

## 背景

Project Brain 已能用 Tree-sitter 从 Git diff 提取 changed symbols，但文件位置和声明名
不足以证明跨 rename、move、宏展开或不同语言工具链下的稳定语义身份。后续可能消费
rust-analyzer、SCIP 或其他语言原生索引，核心和存储不能绑定单一实现。

## 决策

1. `brain-symbols` 定义 Provider-neutral 的节点、边、完整快照和身份质量。
2. Provider 必须声明 `syntax_fallback` 或 `semantic`，不得省略。
3. Tree-sitter Rust Provider 仅承诺相同路径、声明种类、限定名与 occurrence 下 ID 可重复。
4. syntax fallback 在 rename/move 后产生新 ID；它可以成为 lineage 候选证据，但不能自动合并。
5. SQLite 保存可重建图和 removed 历史，不成为项目语义的唯一权威来源。
6. 后续语义 Provider 通过相同快照协议接入；是否采用 rust-analyzer/SCIP 由真实实验决定。
7. Provider ID 定义 provider key 的语义契约；破坏性 key 变更必须使用新 Provider ID，
   兼容的实现或工具链升级只更新 version。
8. Provider key 只表示单个快照来源的身份；跨快照 lineage 由 Brain-owned
   `IdentityTransition` 单独表达，不能把 SCIP key 或启发式匹配直接当成稳定全局身份。

## 结果

- 规则和存储可以逐步升级到符号 scope，而无需先选定最终语言索引器。
- 当前不会对 rename/move 做过强声明，召回率低于启发式自动合并，但不会制造错误 lineage。
- 完整快照允许确定性失效和历史诊断，代价是当前每次 `index` 需要扫描全部受支持源码。

## 验收不变量

- 相同 Provider key 重复生成相同 ID。
- syntax fallback rename 生成不同 ID。
- 相同完整快照重复应用不产生 inserted、updated 或 removed。
- 快照消失节点标记为 removed，不能物理丢失。
- Provider 边不能引用快照外节点。
