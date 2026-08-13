# 协议说明

## 规则 Schema 版本

所有跨边界对象必须包含：

```json
{
  "schema_version": 1
}
```

当前 Runtime 对未知版本 fail closed，避免把新字段静默解释成旧语义。

`ActionDescriptor` 是 preflight/规则引擎的兼容输入，不再作为 Agent adapter 的公共边界。

## Internal Hook Protocol v1

所有 Agent adapter 先转换成内部强类型事件，再调用确定性内核。信封示例：

```json
{
  "protocol_version": 1,
  "project_key": "pb_0123456789abcdef0123456789abcdef",
  "event_id": "codex_event_<sha256>",
  "idempotency": {
    "identity_quality": "vendor_stable"
  },
  "adapter": {
    "kind": "codex",
    "adapter_version": 1
  },
  "session_key": "agent-session-id",
  "cwd": "D:/repo",
  "turn_key": "turn-id",
  "payload": {
    "event": "tool_about_to_run",
    "data": {
      "operation_id": "codex_operation_<sha256>",
      "tool_name": "apply_patch",
      "action": {
        "kind": "modify",
        "target_files": ["src/domain/order.rs"]
      }
    }
  }
}
```

公共事件只有：

```text
SessionOpened
IntentDeclared
ToolAboutToRun
ToolFinished
TaskStopping
```

`project_key` 由项目配置持久化，是事件、幂等键和审计查询的项目边界。`cwd` 只是本次
delivery 的位置证据，不能代替项目身份。旧配置首次打开时会生成并写回 `project_key`；
迁移键只由旧配置稳定内容派生，不依赖 checkout 绝对路径；同一份受版本控制配置在移动或
clone 后仍保持项目身份。新项目初始化时直接生成并持久化独立 key。

`event_id` 是 delivery 身份，`operation_id` 是一次工具调用的因果身份。Pre/Post 可以交错，
Runtime 不建立全局顺序，也不假设最后一个 Pre 必然对应下一个 Post。
Codex operation ID 的派生域包含 `project_key`、规范化 `session_key` 与 vendor tool ID，
不能跨项目或会话复用。

`identity_quality` 明确 adapter 能提供的重放保证：

- `vendor_stable`：vendor 提供稳定调用 ID；
- `derived_stable`：从稳定 turn 等字段派生；
- `per_delivery`：vendor 没有稳定键，每次 delivery 只能生成新 ID，不能声称跨进程去重。

SQLite 以 `(project_key, adapter_kind, event_id)` 唯一约束处理 at-least-once 重放；重复事件
返回首次持久化 outcome。不同项目即使 vendor session/event ID 完全相同也不会串审计。
关键 gate 的治理计算或审计写入失败时，Codex `PreToolUse` 显式 deny；`Stop` 显式要求继续，
但 `stop_hook_active=true` 时仍直接放行以避免自触发循环。不能依赖 hook 进程异常退出实现阻断。

## Internal Hook Outcome

Outcome 与事件一一对应，不存在通用 `block`：

```text
SessionOpened    -> inject
IntentDeclared  -> NoVeto | Deny + inject
ToolAboutToRun  -> NoVeto | Deny + inject
ToolFinished    -> post feedback
TaskStopping    -> AllowStop | ContinueWork + feedback
```

`NoVeto` 只表示 Project Brain 没有治理异议。Adapter 不得把它映射成 vendor 的显式权限批准；
例如 Codex PreToolUse 的 `NoVeto` 输出空对象，让 Codex 自己继续正常权限流程。

## ActionDescriptor

```json
{
  "schema_version": 1,
  "event_id": "tool-call-id",
  "session_id": "agent-session-id",
  "cwd": "D:/repo",
  "action": "modify",
  "operation": "apply_patch",
  "target_files": ["src/domain/order.rs"],
  "command": null,
  "metadata": {}
}
```

`action` 当前可取：

```text
read
create
modify
delete
execute
dependency_change
git_operation
unknown
```

## Decision

```json
{
  "schema_version": 1,
  "decision": "block",
  "summary": "命中确定性硬规则，拒绝执行",
  "evidence": [
    {
      "rule_id": "PB-CORE-001",
      "effect": "block",
      "message": "禁止删除项目规则配置",
      "rationale": "规则控制面必须显式修订"
    }
  ]
}
```

决策优先级固定为：

```text
block > escalate > allow_with_context > allow
```

这是聚合优先级，不是规则 authority 的自动冲突解决。未来出现相互冲突的有效规则时，应显式生成 conflict/elevation，而不是暗中选择一条。

## Adapter 责任

Adapter 只负责：

1. 把外部 Hook 输入转换为 `InternalHookEvent`；
2. 生成非空 event/session/operation 身份并如实标注幂等质量；
3. 调用确定性内核；
4. 把事件专属 `InternalHookOutcome` 转换回外部协议；
5. 记录按项目隔离的 adapter、延迟、outcome 和 failure 审计。

Adapter 不得自行重新解释某条项目规则。

当前 Codex 与 Claude Code adapter 都覆盖 `SessionStart`、`UserPromptSubmit`、`PreToolUse`、
`PostToolUse` 和 `Stop`。两者共享已确认的 vendor 字段子集和 outcome 映射，但必须使用不同的
adapter identity、event ID namespace 与 operation ID namespace，不能跨 vendor 去重。能力矩阵通过
`project-brain capabilities codex` 和 `project-brain capabilities claude-code` 输出。能力模型明确保留 Prime Agent
的 `continue_after_stop=unsupported`，不把独立 runtime 的 `agent_end` 假装成 Codex Stop。
当前 `IntentDeclared` 只进入审计，尚未接入独立的意图规则模型，因此 Codex 有效能力如实报告
`deny_intent=unsupported`；核心协议保留 `Deny` 类型供后续 adapter/rule 实现使用。
`PostToolUse.tool_response` 只有存在可识别的 success、exit code、error 或 status 证据时才映射
为 succeeded/failed，否则记录 unknown，不从事件名称猜测成功。

Claude Code adapter v1 提供直接 `hook/dispatch` 协议入口和用户级 `settings.json` 安装器。
安装器使用独立 manifest、精确 handler hash 与原子替换；只管理五个已实现事件。
`SubagentStart` 与 `SessionEnd` 不在这一阶段；未实现的 lifecycle 不会被折叠成现有五类事件。
handler 使用 `command` 指向稳定 launcher、`args` 保存三个独立参数的 exec form；不得通过 shell
字符串拼接 launcher 路径或生命周期参数。固定 `statusMessage` 用作托管签名的一部分，实际路径与
完整 handler 仍由 manifest hash 校验。
`doctor [codex|claude-code]` 选择对应的配置、manifest 和 handler hash 域；未给参数时为兼容旧调用
默认检查 Codex。Doctor v2 使用通用 adapter 字段，不把 Claude 状态伪装成 Codex 状态。

Prime Agent direct adapter v1 通过 `project-brain hook/dispatch prime-agent <event>` 暴露 Rust
控制面。Extension 应把正式 runtime event 映射到同一内部事件语义，但输出使用独立 schema：
pre-tool 返回 `block/reason/context`，post-tool 返回 `feedback`，停止阶段返回带
`supported=false` 的 continuation 描述。Project Brain 不因内部规则想继续而伪造 Prime 已支持
settled continuation。

## Evidence Protocol v1

`brain-evidence` 将 `source/semantic/engine/build/runtime` 建模为独立 Evidence Plane。快照包含
项目与 provider 身份、source fingerprint、独立 snapshot fingerprint、coverage、显式 upstream
引用、ArtifactGraph 与 findings。下游只在当前源码和全部 upstream fingerprint 一致时为 fresh；
缺少当前证据为 unknown，任一指纹不同为 stale。

Artifact ID 绑定 `project_key + provider_id + provider_key`，边的两端必须同时存在于本快照。
只有 deterministic provider 产生的 complete、fresh、error finding 才具有 hard-block 资格；
资格仍不等于自动阻断，最终必须继续经过规则 authority/strength/effect 判定。

## AnalysisReport

`project-brain analyze` 输出版本化报告。每个受支持文件包含：

```json
{
  "path": "src/worker.rs",
  "language": "rust",
  "has_syntax_errors": false,
  "changed_ranges": [{ "start_line": 12, "end_line": 18 }],
  "changed_symbols": [
    {
      "name": "impl Worker::run",
      "kind": "function_item",
      "start_line": 12,
      "end_line": 18
    }
  ],
  "removed_symbols": []
}
```

行号为一基、闭区间。`has_syntax_errors` 不会阻止输出 Tree-sitter 可恢复的局部结果，
但调用方不得把存在语法错误的结果提升为强阻断事实。

## SymbolSnapshot

符号协议独立使用 `protocol_version`。Provider 必须声明身份质量：

```json
{
  "protocol_version": 2,
  "project_key": "pb_0123456789abcdef0123456789abcdef",
  "provider": {
    "id": "tree-sitter-rust-syntax",
    "version": "0.1.0+tree-sitter-rust-0.24.2",
    "identity_quality": "syntax_fallback"
  },
  "source_revision": "worktree_v3_<sha256>",
  "sources": [
    {
      "path": "src/lib.rs",
      "language": "rust",
      "content_fingerprint": "sha256_<digest>",
      "has_syntax_errors": false
    }
  ],
  "symbols": [],
  "edges": []
}
```

`SymbolNode.id` 是 `project_key`、Provider ID 与不歧义 `provider_key` 的摘要。它保证同一项目、
同一个 Provider 声明下可重复，不表示跨项目或跨 Provider 的全局真相。Provider ID 同时定义 `provider_key` 的
语义契约：破坏性 key 变更必须使用新 ID；兼容的实现或工具链升级只更新 version，
以保持已有符号身份。

身份质量：

- `syntax_fallback`：路径、声明种类、限定名与 occurrence 驱动；rename/move 后产生新 ID。
- `semantic`：由语言语义 Provider 给出；其跨版本保证必须由对应 Provider contract 定义。

`source_revision` 覆盖 `project_key`、HEAD（unborn 仓库使用显式 symbolic-ref 标记）、Provider、全部受支持
源文件的路径/语言/原始内容摘要/语法错误状态，以及节点和边。无符号文件的变化也必须改变 revision。

完整快照的规则：

1. 源文件路径必须规范化且唯一，摘要必须是完整 SHA-256；
2. 所有节点必须对应源文件清单中的路径；
3. 快照、所有节点与边必须属于同一个 `project_key` 和 Provider；
4. 边不得引用快照外节点；
5. 输入节点必须为 `active`；
6. 应用快照时，旧的 active 节点若消失则转为 `removed`；
7. 相同快照重复应用必须得到全量 `unchanged`；
8. 任何 rename/move lineage 都不能仅由 `syntax_fallback` 自动批准。
9. 查询、墓碑失效和边更新必须显式限定 `project_key`，不得依赖数据库文件路径作为唯一隔离。

## Project language 与 SCIP provider profile

`language_profiles` 声明开放的规范 language ID 及其项目相对根；`semantic_providers` 独立声明
数据格式、稳定 profile ID、实际 producer、Brain contract 版本和原始语言映射。两者分离，避免
把 producer 名称误当语言，也允许一个 provider profile 逐 Document 输出多种语言。

SCIP 导入遵循以下 fail-closed 规则：

1. CLI 必须通过 `--provider` 指定项目中已声明的 profile；
2. `tool_info.name` 必须与 profile producer 匹配；Rust 的实际 producer 是 `rust-analyzer`，
   `scip-rust` wrapper 不进入白名单；
3. `Document.language` 必须精确匹配显式 raw mapping；空值只匹配
   `raw_language=null + allow_missing_language=true`；
4. 映射后的 language 必须存在于 `language_profiles`，源码路径必须位于对应 roots；
5. Provider contract version 与 producer version 分离；后者仅作 provenance；
6. Provider 不从扩展名、项目文件、cwd、shebang 或观察到的单条关系猜测语言/能力；
7. global provider key 包含规范 language、原始 SCIP symbol、文档和 range；local symbol 额外包含
   index digest，禁止跨快照 lineage；
8. reference 只在唯一目标且有最小 enclosing definition 时建边；不推断 calls/imports/implements。
9. Provider ID 的可读规范名后附原始 profile contract 摘要，避免 `a-b`、`a_b`、`a.b` 等名称
   归一化后发生身份碰撞。
10. lineage 候选只在同一 project、provider 与 language 内比较；Git rename similarity 必须位于
    0..10000，且达到 5000 才能单独把候选提升为 high confidence。

## Semantic lineage ledger

Lineage 连接两个历史 observation，而不是合并或重命名 `SymbolNode`。持久化边界为：

- `semantic_snapshots`：按项目、provider profile/contract 排序的不可变导入事实；
- `semantic_snapshot_attestations`：同一快照在不同已验证 worktree 状态上的 append-only 来源证明；
- `semantic_source_manifests`：每个 v7+ 快照的完整 Document 清单计数与摘要；
- `semantic_source_observations`：清单内路径、language、内容摘要和语法状态；
- `semantic_symbol_observations`：某次快照实际看到的 symbol；
- `semantic_lineage_groups` / `semantic_lineage_group_members`：相邻快照的兼容等价类与成员集合；
- `semantic_lineage_generation_runs`：算法版本、group manifest、潜在 pair 与实际物化数；
- `semantic_lineage_candidates`：只有 1×1 自动生成或人工从 group 选择的 endpoint materialization；
- `semantic_lineage_evidence`：算法 ID、版本、输入摘要、结构化证据与置信度的 append-only 观察；
- `semantic_lineage_decisions`：显式用户裁决的 append-only 日志；
- `semantic_lineage_compaction_runs` / `semantic_lineage_compaction_groups`：V7 pair-first 旧账的
  幂等逻辑压缩审计、候选/证据 manifest hash 与目标 group；
- `semantic_provider_qualification_events`：显式多轮稳定性验证的 append-only 最终结论、机器绑定、
  源码指纹与整组 evidence manifest hash。

候选状态只有：

```text
proposed | confirmed | rejected | superseded | invalidated
```

V8 的 ambiguity 属于 `semantic_lineage_groups`；candidate 的旧 `ambiguity_group_id` 只保留 V7
历史语义。ambiguous 不属于 candidate 生命周期。允许
`rejected -> confirmed`，但必须是新的显式请求并保留两条 decision。禁止
`confirmed -> rejected`；纠错使用原子 `confirmed -> superseded` 加替代候选确认，结构性损坏才使用
`invalidated`。

硬不变量：

1. 只比较同 project、provider profile、provider contract、language 的相邻 semantic snapshot；
2. 只比较旧快照 removed 与新快照 inserted symbol；稳定 symbol ID 不产生 self-lineage；
3. 只有 1×1 group 自动产生 `proposed`；歧义 group 自动 pair 数恒为 0，人工选择后仍不确认；
4. 单侧超过 4096 members 的 group 为 `summary_only`，必须从 immutable snapshots 用同算法重算并
   验证成员摘要后才能物化；
5. 新快照和算法重跑不改变旧 candidate state；只可追加去重后的 evidence；
6. confirm/reject 只能来自带 `--human-confirmed` 的显式用户命令，必须携带 request ID；同 request 同 payload 重放首次结果，
   同 request 不同 payload 拒绝；
7. 一次裁决在单个事务内写 decision、执行 revision CAS、更新 materialized state；
8. 同 snapshot pair 的 confirmed predecessor/successor 都是一对一；split/merge 留待独立协议；
9. 不自动确认、拒绝竞争项、supersede、延伸传递 lineage、修改 symbol ID、恢复 tombstone、改写
   snapshot 或跨 provider 建 equivalence；
10. 已导入但不是当前最新的历史 snapshot 不能重新应用为当前符号图。

SQLite schema v11 保存 semantic snapshots、source attestations、source manifests、symbol observations、
lineage groups/members/generation runs、candidate/evidence/decision 与 legacy compaction audit。旧快照迁移后的来源字段为空且默认为 `offline_import`，不会被提升
为硬证据，也不会从现存 symbol 反推缺失 Document。真实重跑相同 snapshot 时可以首次补录 manifest；
可信重跑只追加 attestation，不改写 symbol observations 或人工 lineage 状态。attestation 的唯一身份
包含 trust、registration、executable 与 artifact；相同 snapshot/worktree/HEAD 由新绑定重跑时仍会
追加新证明，读取以最新 sequence 为准，完全相同的证明才幂等去重。

V7 legacy compaction 默认是 dry-run。只有一个 group 的所有行仍为 `proposed`、每条恰有一份
`project-brain-lineage` version 1 evidence、没有 decision/related decision 引用，且按 kind 与 definition
fingerprint 重建后的实际 pair 集精确等于 from×to，才可进入 apply。任何缺行、附加证据、裁决或损坏
observation 都保护整个 group。apply 先保存 group/member 与 append-only compaction audit，再在同一事务
删除对应 evidence/candidate；不执行 `VACUUM`，压缩后的 legacy group 不得重新物化。

覆盖率是独立的确定性证据：对 Provider profile 显式映射的 Rust/Python/C#/VB/F# language，比较
Git 已跟踪及未忽略、位于声明 roots 且扩展名属于该 language 契约的文件，与 SCIP Document 清单。
未知 language 必须报告 `unverifiable`，不得猜扩展名。已有快照的 `partial` 或与当前 worktree/HEAD
不一致的 `stale` 会使显式 `doctor` 降级；从未索引则只报告 `not_indexed` warning。

新导入只有 `complete` 才能进入 snapshot transaction。`partial` 与 `unverifiable` 在 store mutation
之前失败，不能更新 latest semantic snapshot。稳定性验证必须在相同源码指纹、Provider registration
revision 与 executable SHA-256 下重复运行，分别比较完整 Document path set 和完整 semantic snapshot
fingerprint；诊断重试不得把多次不完整输出取并集。

## Symbol scope 与证据等级

仓库规则通过 `symbol_scopes` 固定 provider profile/contract、language、anchor snapshot/symbol 和
`confirmed_lineage_only` 策略。解析只能是 `direct_semantic`、逐跳
`confirmed_lineage` 或 `unresolved`，不得从路径相似度、LLM 判断或候选置信度发明稳定身份。

硬规则除 authority/strength/effect 权限外还必须满足：definition 非 local、provider symbol 在该
snapshot 内唯一；最新来源是 `trusted_provider`；当前机器 binding 仍 ready 且 registration ID 与
executable SHA-256 匹配；worktree/HEAD 新鲜。`PreToolUse` 还要求 whole-file 或唯一 Edit range 的
确定性工具影响；`Stop` 则要求 clean `HEAD` baseline 与实际 Git hunk 相交。

证据等级为 `deterministic_path`、`semantic_direct`、`semantic_confirmed_lineage`、
`semantic_baseline_diff`、`advisory_syntax`、`inferred`、`unavailable`。offline SCIP、syntax fallback、
proposed/ambiguous lineage、Provider 缺失/失败/二进制漂移和过期快照只能 advisory fail-open，不能
伪装成 rule violation 或制造 Stop continuation 循环。
