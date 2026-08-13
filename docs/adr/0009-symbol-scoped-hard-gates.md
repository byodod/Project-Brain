# ADR-0009：Symbol-scoped hard gate 只接受可信语义证据

- 状态：Accepted
- 日期：2026-08-13

## 背景

路径规则无法表达“只保护这个语义所有者”，但把 SCIP symbol、rename 相似度或 LLM 判断直接变成
硬规则会把概率事实错误提升为强制权限。Provider 失败、旧索引或人工构造的 `.scip` 也不能被解释
为代码违规。

## 决策

1. 规则锚点保存在仓库 `config.json`，固定 project 内 provider profile/contract、language、历史
   snapshot 和 symbol；SQLite 是可重建的 observation、lineage 与 provenance ledger，不是规则权威。
2. 不创建跨重构的 `StableSymbolId`。解析只允许当前 snapshot 的 direct semantic identity，或逐个
   相邻 snapshot 经过人工 confirmed lineage；任一跳缺失即 `unresolved`。
3. local symbol、syntax fallback、缺失/重复 provider symbol、proposed/ambiguous lineage 和跨
   project/provider/language 关系永不具备 hard eligibility。
4. `index-scip` 产生 `offline_import` attestation，只能 advisory。机器 Runner 成功后追加
   `trusted_provider` attestation，固定 registration ID、executable SHA-256、artifact SHA-256、
   Git HEAD、worktree 指纹和 clean 状态。同一 semantic snapshot 的可信重跑追加证明，不改写事实。
5. Hook 每次检查当前机器 binding；registration 或 executable 漂移、Provider 缺失、快照过期、
   resolver/数据库失败均 fail-open 为 warning，不产生 rule violation。
6. `PreToolUse` 只有在 trusted + fresh 语义解析与确定性工具影响相交时才可 hard gate。当前确定性
   影响包括 whole-file Write/Delete 与 old string 唯一可定位的 Edit range；shell/apply_patch update
   保持 advisory。
7. `Stop` 只有在 trusted、clean、当前 `HEAD` baseline 的 definition 与实际 Git hunk 相交时继续
   工作；纯插入使用旧文件插入锚点。`stop_hook_active` 仍直接放行以防循环。
8. lineage confirm/reject/supersede 与规则锚点 bind/unbind 要求显式 `--human-confirmed`。LLM 可以
   提议候选，不能替代人工治理事实。

## 后果

- 硬门控牺牲部分覆盖率以换取可解释、可重放和低误阻断率。
- 修改或提交规则配置会自然使旧 worktree/HEAD 证明过期；操作者必须在最终配置状态再次运行可信
  Provider 索引。
- Provider 不可用不会阻塞普通开发，但 Hook 审计会保留降级原因。
- split/merge、symbol set、跨 provider equivalence 与调用图影响面需要后续独立协议，不能通过
  放宽本 ADR 的证据门槛实现。
