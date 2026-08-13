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

引擎、构建与运行时事实走独立的 `brain-evidence` 协议。它把 Source、Semantic、Engine、Build、
Runtime 作为不同 Evidence Plane，并用 ArtifactGraph 表达场景、资源、脚本绑定、构建产物和运行
场景。每个下游快照记录实际消费的 upstream fingerprint；源码或任一上游变化都会使证据 stale。
SymbolGraph 与 ArtifactGraph 不共享身份，后续只能通过显式 evidence edge 连接。

Godot Provider v1 分为两个进程边界：先由固定 SHA-256 的 Godot 4 editor 执行 `--import`，再运行
Project Brain 自带且位于机器临时目录的 GDScript probe。Probe 在加载前后分别采集 project、UID、
main scene、autoload、resource dependency 与文件哈希；Rust 侧要求两份 engine-resolved state 除
`loaded` 结果外完全一致，并再次读取文件核对哈希。HOME、APPDATA 与 XDG 路径指向本次临时目录，
避免读取或写入用户 editor 配置。`.godot/` 不进入 ArtifactGraph，也不参与 source fingerprint。

SCIP 路径以 `project_key + semantic provider profile ID + producer + contract_version` 建立
Provider 命名空间，并把规范化 language ID 写入 provider key。语言映射逐 Document 执行，
因此单一 scip-dotnet index 可同时容纳 C# 与 Visual Basic。Producer 自身版本只作为 provenance
输出，不冒充 Project Brain 的解释契约版本。能力矩阵绑定 producer + language，并使用
supported/partial/unsupported/unknown 四态，不从某个索引“恰好出现了什么”反推保证。

自动执行 producer 时，仓库 profile 仍只保存声明式 producer 契约；`providers.json` 按
`project_key + profile_id` 保存机器绝对路径、registration revision、version probe 与 executable/
entrypoint SHA-256。scip-python 额外校验 package.json 的官方包身份、bin 入口，并固定整个包目录的
有界文件清单哈希，避免只固定薄入口而遗漏 `dist/` 传递 bundle。Runner 仅对三种已知 adapter 构造
固定 argv，拒绝仓库内 executable、相对路径、Windows shell shim 和任意 repo args。Windows 内部
可继续使用 verbatim canonical path，但传给不接受 `\\?\` 的 producer argv/JS 入口会转换为等价本机路径。
外部进程运行期间不持有 SQLite 写事务；只有输出、provenance、
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
        ├── symbol graph 按 project_key 隔离、可从工作区完整重建的派生索引
        ├── semantic source manifest 快照实际覆盖的完整 Document 证据
        └── evidence snapshot/head/attestation/staleness 分层证据账本

<ProjectBrainData>/state/providers.json
        │
        ├── executable/entrypoint path + SHA-256  机器本地信任，不提交
        ├── scip-python package manifest SHA-256  固定传递 bundle
        ├── registration revision + probe version
        └── provider-audit.jsonl  有界本地执行/失败 provenance
```

SQLite 中的代码事实不能成为不可恢复的唯一来源。完整快照以事务应用；快照中消失的节点
进入 `removed` 状态而非物理删除，使历史规则引用仍可诊断。
符号 ID、快照 revision、节点/边主键、查询和墓碑更新都包含 `project_key`。数据库 schema v4
首次建立这组项目隔离约束；对应迁移会清除旧版无项目归属的可重建符号缓存，但保留动作与
adapter 审计，避免把旧节点错误归入某个项目。当前数据库版本为 schema v12，并在这些约束上
增加独立的语义血缘账本、append-only 来源证明、不可伪造的源码 Document manifest，以及
Evidence Plane 当前 head 与 staleness 事件。
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

SQLite schema v12 保存 semantic snapshots、append-only source attestations、source manifests、
symbol observations、group/member/generation run、candidate/evidence/decision，以及显式旧账压缩的
run/group 审计和 append-only Provider qualification events。压缩默认只读；apply 必须携带人工确认与幂等 request ID，且逻辑删除与审计同事务。
物理 `VACUUM` 不属于压缩事务，也不能替代候选资格证明。
同一 semantic snapshot 可由不同机器绑定重复产生。V11 的 attestation 唯一身份包含 trust、
registration、executable 与 artifact 证明；图内容未变化时仍可追加新的来源证明，并由最新 sequence
参与 hard-gate 新鲜度判断，不能让旧绑定证明遮蔽当前已资格化绑定。
Candidate endpoint 唯一键负责算法重跑幂等，算法版本只产生新的 evidence observation；人工状态
永远不会被 generator 恢复或覆盖。Partial unique indexes 约束同 snapshot pair 中 predecessor 与
successor 一对一，竞争确认不会自动选择赢家。`request_id + request_hash` 提供 at-least-once 命令
提交的幂等与碰撞检测，状态更新使用 revision CAS。

Source manifest 与新快照在同一 SQLite 事务内写入，并保存路径/language/内容摘要、数量和 manifest
摘要。旧库迁移绝不从“仍有 symbol 的文件”推断完整文档集合；只有真实重跑完整 Provider 输入才可
补录。索引报告与显式 doctor 将 manifest 和 Git 当前文件集合比较，区分 complete、partial、stale、
not_indexed 与 unverifiable，避免“Provider 成功退出”等价于“全仓源码已覆盖”的错误推论。
新 Provider 输出只有覆盖率为 complete 才允许进入 snapshot transaction；partial/unverifiable 只留下
机器级运行审计。`provider verify-stability` 以 workspace index 为快速路径，固定源码与 executable
身份后重复比较 Document manifest 和完整 semantic snapshot 指纹，且永不提交观测结果。多次失败
workspace run 的 union 不构成一致的语义世界，因此协议明确禁止。
稳定性验证的最终结论持久化到项目数据库。最新结论为 `nondeterministic` / `stable_incomplete` 时，
普通 `provider index` 即使偶然得到 complete 输出也不得提交；只有相同机器绑定的显式重复验证达到
`stable_complete` 才恢复提交资格。资格存在后，Provider registration revision 或 executable hash
变化会使它过期并要求重新验证。

Engine/Build/Runtime 等跨语言证据使用独立 ledger：不可变 `evidence_snapshots` 只按 fingerprint 保存
一次完整 JSON，重复真实运行只追加小型 `evidence_attestations`；`evidence_heads` 保存每个项目、plane、
provider 当前指向及 fresh/stale 状态。明确的文件 Create/Modify/Delete 完成事件以 project + event ID
幂等写入 `evidence_staleness_events`，并把当前 Engine head 标为 stale；重新执行真实 Provider 才能将
head 恢复 fresh。这个结构避免重复保存大型 ArtifactGraph，也不把 stale finding 提升为硬阻断。

## 阻断权限

阻断必须同时满足：

```text
effect = block
strength = hard
authority ∈ { explicit_user, repository_rule, accepted_decision }
```

配置加载阶段即拒绝其他组合，避免把概率判断意外提升为强制规则。

Symbol scope 还要求 direct semantic 或逐跳 confirmed lineage、非 local 且唯一的 definition、
trusted Provider attestation、当前机器 registration/executable hash 匹配及新鲜源码。确定性工具影响
或 clean HEAD baseline Git hunk 才能把这些事实提升为硬证据；其他情况只注入 advisory。

## Stop 闭环

Codex `Stop` 读取 `stop_reconcile` 配置，对当前 Git 文件集合执行 Change Envelope 对账。
`block` 或 `escalate` 会转换为 Codex 的 Stop block 响应，使 Agent 继续处理；当
`stop_hook_active=true` 时直接放行，防止 hook 自触发循环。
Envelope 在读取前会规范化并限制在项目根目录内；所有 Git diff 调用显式禁用
external diff，避免分析动作执行仓库配置中的外部程序。

符号 Stop 对账独立使用 clean `HEAD` semantic baseline 与真实 diff hunk；纯插入保留旧文件插入
锚点。Provider、attestation、lineage 或数据库基础设施不可用时只记录 warning 并 fail-open，不能
伪装成规则违规。

## Adapter 能力不对称

公共协议统一治理语义，不统一 vendor JSON。Codex/Claude Code 可以拒绝工具并要求 Stop 后继续；
Prime Agent 是独立 runtime；其 Extension `tool_call` 可同步 block，但当前正式文档中的
`agent_end` 只表示一次 prompt 结束，且未提供稳定 `agent_settled` 契约，因此 Stop continuation
必须报告 unsupported。Prime direct adapter 使用独立身份、幂等域、审计域与自有输出 JSON，
不复用 Codex/Claude vendor JSON。Codex 与 Claude Code adapter 都包含用户级安装器；Prime 的
Extension 安装器仍留在后续阶段。

## 下一阶段

1. Internal Hook Protocol v1、Codex adapter 与 Claude Code direct adapter 已加入文件和 symbol scope；继续在真实项目验证项目
   隔离、重放、并发交错、Provider 漂移降级、Stop 防循环与长会话延迟。
2. 已接入机器级 SCIP Runner、complete-only commit 与重复运行稳定性证明；真实 rust-analyzer
   workspace 结果已证明非确定，且 package root 会重新提升到 workspace root，因此不启用假的
   package-shard fallback。.NET/Python 继续用符合
   producer 行为的合成 fixture 固定 C#/VB、空 Python language、未指定 kind 与 implementation 契约。
3. Semantic lineage 裁决与 symbol-scoped rules 已实现；下一步扩展 symbol set、split/merge 和调用图
   影响面，但仍不允许自动确认或 LLM hard block。
4. Claude Code 已覆盖安装后 exec-form handler 的真实子进程 fixture；Prime Agent 独立 direct
   adapter 已完成，下一步增加原子 Extension 安装与真实 Prime runtime fixture。按 adapter 选择的
   doctor 已由 ADR-0016 完成。
5. Source、Semantic、Engine、Build、Runtime 分层 Evidence Plane、独立 ArtifactGraph、SQLite
   快照/attestation/head/staleness ledger 与 Hook 新鲜度提示已经完成；Godot Engine Evidence Provider
   v1 已通过真实 Godot 4.6 项目验证。下一阶段实现 Build/Runtime Provider 和规则到 finding 的显式映射。
6. 后续增加 TypeScript 等 provider，并加入只读、可拔插的 Semantic Sentinel；LLM 不能
   直接 hard block。

相关决策见 [ADR-0001](adr/0001-provider-neutral-symbol-identity.md)、
[ADR-0002](adr/0002-internal-hook-protocol.md)、
[ADR-0003](adr/0003-project-identity-and-adapter-audit.md) 与
[ADR-0004](adr/0004-project-scoped-symbol-graph.md)、
[ADR-0005](adr/0005-project-language-and-scip-profiles.md) 与
[ADR-0006](adr/0006-semantic-lineage-ledger.md)、
[ADR-0007](adr/0007-machine-bootstrap-and-codex-dispatcher.md) 与
[ADR-0008](adr/0008-machine-provider-runner.md) 与
[ADR-0009](adr/0009-symbol-scoped-hard-gates.md)、
[ADR-0010](adr/0010-semantic-source-coverage.md)、
[ADR-0011](adr/0011-complete-only-and-provider-stability.md) 与
[ADR-0012](adr/0012-group-first-lineage-and-signature-evidence.md)，以及
[ADR-0019](adr/0019-evidence-planes-and-artifact-graph.md) 与
[ADR-0020](adr/0020-godot-engine-evidence-provider.md) 与
[ADR-0021](adr/0021-evidence-ledger-and-hook-staleness.md)。
