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
  └── SCIP: semantic
      ├── rust-analyzer / rust
      ├── scip-dotnet / C# + Visual Basic
      ├── scip-python / explicit missing-language mapping
      └── configured custom producer/language
          │ project-scoped SymbolSnapshot
          ▼
  SQLite derived graph
  ├── active nodes
  ├── removed history
  └── lexical/semantic edges
```

`brain-symbols` 只定义 Provider、节点、边、快照和身份质量；它不依赖 Tree-sitter、
Git 或 SQLite。`brain-analyzer` 与 `brain-scip` 是 Provider，`brain-store` 只消费完整快照。

SCIP 路径以 `project_key + semantic provider profile ID + producer + contract_version` 建立
Provider 命名空间，并把规范化 language ID 写入 provider key。语言映射逐 Document 执行，
因此单一 scip-dotnet index 可同时容纳 C# 与 Visual Basic。Producer 自身版本只作为 provenance
输出，不冒充 Project Brain 的解释契约版本。能力矩阵绑定 producer + language，并使用
supported/partial/unsupported/unknown 四态，不从某个索引“恰好出现了什么”反推保证。

自动执行 producer 时，仓库 profile 仍只保存声明式 producer 契约；`providers.json` 按
`project_key + profile_id` 保存机器绝对路径、registration revision、version probe 与 executable/
entrypoint SHA-256。Runner 仅对三种已知 adapter 构造固定 argv，拒绝仓库内 executable、相对路径、
Windows shell shim 和任意 repo args。外部进程运行期间不持有 SQLite 写事务；只有输出、provenance、
profile/root 与工作区前后指纹全部通过后，才进入 semantic snapshot 事务。

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
        └── symbol graph 按 project_key 隔离、可从工作区完整重建的派生索引

<ProjectBrainData>/state/providers.json
        │
        ├── executable/entrypoint path + SHA-256  机器本地信任，不提交
        ├── registration revision + probe version
        └── provider-audit.jsonl  有界本地执行/失败 provenance
```

SQLite 中的代码事实不能成为不可恢复的唯一来源。完整快照以事务应用；快照中消失的节点
进入 `removed` 状态而非物理删除，使历史规则引用仍可诊断。
符号 ID、快照 revision、节点/边主键、查询和墓碑更新都包含 `project_key`。数据库 schema v4
首次建立这组项目隔离约束；对应迁移会清除旧版无项目归属的可重建符号缓存，但保留动作与
adapter 审计，避免把旧节点错误归入某个项目。当前数据库版本为 schema v5，并在这些约束上
增加独立的语义血缘账本。
数据库迁移拒绝缺失或非整数的已有 `schema_version`，不会把损坏元数据静默当作 v1。
Adapter 审计依赖 SQLite 唯一约束和 busy timeout，使并发连接对同一项目事件收敛到首次 outcome；
失败记录可在重开数据库后由成功重试升级，后续重复成功不能覆盖首次成功。

Semantic lineage 使用独立 ledger，不改变符号图：

```text
immutable semantic snapshots + observations
                 │ adjacent removed/inserted only
                 ▼
         proposed candidate
           │ evidence append-only
           │ explicit user request
           ▼
confirmed / rejected / superseded / invalidated
           │ decision append-only
           └── never rewrites SymbolNode / tombstone / snapshot
```

SQLite schema v5 保存 semantic snapshots、symbol observations、candidate、evidence 和 decision。
Candidate endpoint 唯一键负责算法重跑幂等，算法版本只产生新的 evidence observation；人工状态
永远不会被 generator 恢复或覆盖。Partial unique indexes 约束同 snapshot pair 中 predecessor 与
successor 一对一，竞争确认不会自动选择赢家。`request_id + request_hash` 提供 at-least-once 命令
提交的幂等与碰撞检测，状态更新使用 revision CAS。

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
2. 已接入机器级 SCIP Runner：Rust 用真实 rust-analyzer 端到端验证；.NET/Python 先用符合 producer
   行为的合成 fixture 固定 C#/VB、空 Python language、未指定 kind 与 implementation 契约。
3. 当前 semantic lineage 只产生项目内候选；下一步实现可审计确认记录，再加入 symbol-scoped rules。
4. 内部协议经验证后实现 Claude Code 与 Prime Agent adapter；Prime 继续按独立 runtime 处理。
5. 后续增加 TypeScript 等 provider，并加入只读、可拔插的 Semantic Sentinel；LLM 不能
   直接 hard block。

相关决策见 [ADR-0001](adr/0001-provider-neutral-symbol-identity.md)、
[ADR-0002](adr/0002-internal-hook-protocol.md)、
[ADR-0003](adr/0003-project-identity-and-adapter-audit.md) 与
[ADR-0004](adr/0004-project-scoped-symbol-graph.md)、
[ADR-0005](adr/0005-project-language-and-scip-profiles.md) 与
[ADR-0006](adr/0006-semantic-lineage-ledger.md)、
[ADR-0007](adr/0007-machine-bootstrap-and-codex-dispatcher.md) 与
[ADR-0008](adr/0008-machine-provider-runner.md)。
