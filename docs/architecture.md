# 架构说明

## 运行时边界

Project Brain 把仓库记忆视为“改变未来行为的规则”，而不是聊天片段检索。当前数据流为：

```text
Agent Hook JSON
      │
      ▼
Agent Adapter
      │ InternalHookEvent v1
      ▼
Protocol Processor
      │
      ├── project_key isolation
      ├── event idempotency
      └── capability-aware semantics
      │
      ▼
Deterministic Rule Engine / Reconcile
      │
      ├── NoVeto / Deny / inject
      ├── post feedback
      └── AllowStop / ContinueWork
      │
      ├── Agent-specific Hook JSON
      └── project-scoped SQLite adapter audit
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
        ├── project_key  项目稳定身份、应进入版本控制
        ├── rules        权威、应进入版本控制
        └── lifecycle    权威、保留 superseded/retired 轨迹

.project-brain/brain.db
        │
        ├── audit_events 本地派生记录、不进入版本控制
        ├── adapter_audit_events 按项目/适配器隔离的事件、结果、延迟与失败
        └── symbol graph 可从工作区完整重建的派生索引
```

SQLite 中的代码事实不能成为不可恢复的唯一来源。完整快照以事务应用；快照中消失的节点
进入 `removed` 状态而非物理删除，使历史规则引用仍可诊断。
数据库迁移拒绝缺失或非整数的已有 `schema_version`，不会把损坏元数据静默当作 v1。
Adapter 审计依赖 SQLite 唯一约束和 busy timeout，使并发连接对同一项目事件收敛到首次 outcome；
失败记录可在重开数据库后由成功重试升级，后续重复成功不能覆盖首次成功。

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

## Adapter 能力不对称

公共协议统一治理语义，不统一 vendor JSON。Codex/Claude Code 可以拒绝工具并要求 Stop 后继续；
Prime Agent 是独立 runtime，当前已确认的 Extension `agent_end` 不具备同等 Stop continuation
契约，因此能力模型必须报告 unsupported。当前提交只实现 Codex adapter，先用真实生命周期压力
验证协议；Claude Code 与 Prime Agent 不在本阶段实现。

## 下一阶段

1. Internal Hook Protocol v1 与 Codex adapter 先进入真实使用，验证项目隔离、重放、并发交错、
   失败审计和 Stop 防循环；当前仅依赖文件/模块 scope，不假装已有稳定语义身份。
2. 随后用真实 Rust 仓库实验 rust-analyzer 与 SCIP，验证 definition/reference、rename/move、
   trait、macro 和跨文件调用关系；Project Brain 消费外部语义索引，不重写编译器或类型检查器。
3. 在实验结果上实现版本化 semantic lineage，再加入 symbol-scoped rules。
4. 内部协议经验证后实现 Claude Code 与 Prime Agent adapter；Prime 继续按独立 runtime 处理。
5. 后续试点 C#/TypeScript/Python，最后才加入只读、可拔插的 Semantic Sentinel；LLM 不能
   直接 hard block。

相关决策见 [ADR-0001](adr/0001-provider-neutral-symbol-identity.md)、
[ADR-0002](adr/0002-internal-hook-protocol.md) 与
[ADR-0003](adr/0003-project-identity-and-adapter-audit.md)。
