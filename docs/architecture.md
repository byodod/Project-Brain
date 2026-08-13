# 架构说明

## 运行时边界

Project Brain 把仓库记忆视为“改变未来行为的规则”，而不是聊天片段检索。当前数据流为：

```text
Agent Hook JSON
      │
      ▼
Agent Adapter
      │ ActionDescriptor
      ▼
Deterministic Rule Engine
      │
      ├── allow
      ├── allow_with_context
      ├── block
      └── escalate
      │
      ├── Agent-specific Hook JSON
      └── SQLite audit event
```

Git 变更进入另一条同样确定性的分析路径：

```text
Git zero-context diff
      │
      ├── 当前/未跟踪源码
      └── 基线旧源码（纯删除）
              │
              ▼
       Tree-sitter parser
              │
              ├── changed_symbols
              └── removed_symbols
```

完整仓库语义基础走 Provider-neutral 快照路径：

```text
Tracked + unignored files
          │
          ▼
  Symbol Provider
  ├── Tree-sitter Rust: syntax_fallback
  └── future semantic provider
          │ SymbolSnapshot
          ▼
  SQLite derived graph
  ├── active nodes
  ├── removed history
  └── lexical/semantic edges
```

`brain-symbols` 只定义 Provider、节点、边、快照和身份质量；它不依赖 Tree-sitter、
Git 或 SQLite。`brain-analyzer` 是 Provider，`brain-store` 只消费完整快照。

`brain-core` 不依赖文件系统、Git、SQLite 或任何 Agent SDK。相同输入、配置和 schema_version 必须产生相同决策。

## 权威来源

```text
.project-brain/config.json
        │
        ├── rules        权威、应进入版本控制
        └── lifecycle    权威、保留 superseded/retired 轨迹

.project-brain/brain.db
        │
        ├── audit_events 本地派生记录、不进入版本控制
        └── symbol graph 可从工作区完整重建的派生索引
```

SQLite 中的代码事实不能成为不可恢复的唯一来源。完整快照以事务应用；快照中消失的节点
进入 `removed` 状态而非物理删除，使历史规则引用仍可诊断。

## 阻断权限

阻断必须同时满足：

```text
effect = block
strength = hard
authority ∈ { explicit_user, repository_rule, accepted_decision }
```

配置加载阶段即拒绝其他组合，避免把概率判断意外提升为强制规则。

## Stop 闭环

Codex `Stop` 读取 `stop_reconcile` 配置，对当前 Git 文件集合执行 Change Envelope 对账。
`block` 或 `escalate` 会转换为 Codex 的 Stop block 响应，使 Agent 继续处理；当
`stop_hook_active=true` 时直接放行，防止 hook 自触发循环。
Envelope 在读取前会规范化并限制在项目根目录内；所有 Git diff 调用显式禁用
external diff，避免分析动作执行仓库配置中的外部程序。

## 下一阶段

1. 先拆分 runtime-neutral 的 event phase、verdict、effect 与 adapter receipt，并补齐幂等、
   恢复、Stop continuation cap 和 sidecar 最小权限边界。
2. 在公共 capability negotiation 之上增加 Claude Code 和 Prime Agent 适配器；Goal 与
   Heartbeat 分别建模为 WorkIntent、WakeupSchedule，不伪装成 Hook。
3. 消费 rust-analyzer SCIP 作为 Rust semantic Provider，并由 Brain 维护独立的
   IdentityTransition/lineage；不自建解析器或类型检查器。
4. 把符号图纳入符号级 Change Envelope 与规则 scope，再试点 C#/TypeScript/Python。
5. 最后才加入只读、可拔插且首发仅 inject 的 Semantic Sentinel；LLM-only 结论永不 hard block。

相关决策见 [ADR-0001](adr/0001-provider-neutral-symbol-identity.md)。
