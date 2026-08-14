# Project Brain Protocols

本文固定当前跨进程与持久化合同。未知版本、未知必填语义或身份不一致必须拒绝，不能静默降级。

## 1. Internal Hook Protocol v1

规范事件：

```json
{
  "protocol_version": 1,
  "project_key": "project-a",
  "event_id": "adapter_event_identity",
  "idempotency": { "identity_quality": "vendor_stable" },
  "adapter": { "kind": "codex", "adapter_version": 1 },
  "session_key": "session-a",
  "cwd": "/absolute/project/path",
  "turn_key": "turn-a",
  "payload": {}
}
```

`adapter.kind` 仅允许：`codex`、`pi`、`opencode`、`dsh`。

事件 payload：

- `session_opened`：进入原因；
- `intent_declared`：用户输入和可用元数据；
- `tool_about_to_run`：`operation_id`、tool name、规范化 action；
- `tool_finished`：同一 `operation_id`、tool name、结果摘要；
- `task_stopping`：本轮完成声明与 stop state。

`event_id` 在 `(project_key, adapter_kind)` 域内幂等。工具前/工具后通过 `(project_key, adapter_kind, session_key, operation_id)` 建立因果关系。

身份质量：

- `vendor_stable`：Agent 提供稳定 ID；
- `derived_stable`：由稳定字段规范哈希得到；
- `per_delivery`：只能去重当前投递，不能声称跨重试稳定。

## 2. Internal Hook Outcome v1

内部输出 payload：

- `session_opened { inject[] }`
- `intent_declared { gate, inject[] }`
- `tool_about_to_run { gate, inject[] }`
- `tool_finished { feedback[] }`
- `task_stopping { stop, feedback[] }`

`gate` 为 `no_veto` 或 `deny { reason }`。`stop` 为 `allow_stop` 或 `continue_work { reason }`。

Adapter 必须根据能力映射；无法表达的能力保持 unsupported，不能通过不可靠消息伪造。

## 3. Agent capability contract

```json
{
  "deny_intent": "supported|unsupported",
  "deny_tool": "supported|unsupported",
  "inject_context": "supported|unsupported",
  "post_feedback": "supported|unsupported",
  "continue_after_stop": "supported|emulated|unsupported"
}
```

当前矩阵：

| adapter | deny_intent | deny_tool | inject_context | post_feedback | continue_after_stop |
|---|---|---|---|---|---|
| codex | unsupported | supported | supported | supported | supported |
| pi | supported | supported | supported | supported | emulated |
| opencode | unsupported | supported | supported | supported | unsupported |
| dsh | unsupported | supported | supported | supported | supported |

能力查询本身不访问项目：`project-brain capabilities <adapter>`。

## 4. 原生事件映射

### Codex

| Codex event | Internal event | 控制语义 |
|---|---|---|
| SessionStart | session_opened | stdout context |
| UserPromptSubmit | intent_declared | context；不声称 prompt veto |
| PreToolUse | tool_about_to_run | permissionDecision allow/deny |
| PostToolUse | tool_finished | feedback/context |
| Stop | task_stopping | decision=block 要求继续 |

Codex Hook 输入直接来自 stdin JSON。安装器为所有工具事件注册 group，不使用易遗漏工具名的 matcher。

### Pi

| Pi event | Internal event | 控制语义 |
|---|---|---|
| session_start | session_opened | display/context message |
| input | intent_declared | input interception/context |
| before_agent_start | 注入缓存上下文 | system prompt append |
| tool_call | tool_about_to_run | `{ block: true, reason }` |
| tool_result | tool_finished | feedback message |
| agent_end | task_stopping | 官方 follow-up API 模拟最多一次续轮 |

Extension 使用 `ctx.cwd` 和 session manager 提供的 session file 形成会话身份；tool call ID 作为 operation ID。
Pi 没有正式的停止前 veto 事件；`agent_end` 触发的 follow-up 只能标记为 `emulated`，不能等同于 Codex `Stop` 或 dsh `agent/turn-stopping`。

### opencode

| opencode hook/event | Internal event | 控制语义 |
|---|---|---|
| session.created | session_opened | 建立 session state |
| chat.message | intent_declared | 修改 text part 注入上下文 |
| tool.execute.before | tool_about_to_run | 阻断时 throw |
| tool.execute.after | tool_finished | 修改 output/metadata |
| session.idle | task_stopping | 只审计；不可强制续轮 |

`sessionID` 与 `callID` 分别用于 session/operation identity。`session.idle` 缺少已确认的 continuation API，所以 `continue_after_stop=unsupported`。

### dsh

| dsh event | Internal event | 控制语义 |
|---|---|---|
| agent/session-start | session_opened | 初始化 session state |
| agent/pre-step | intent_declared/context | 注入 UserMessage |
| tools/pre-execute | tool_about_to_run | `{ kind: "deny", reason }` |
| tools/post-execute | tool_finished | feedback message |
| agent/turn-stopping | task_stopping | `agent.steer(...)` |

`exec.callId` 是 operation ID，`exec.agent.id` 与 session header 形成会话域。安装必须显式指定 profile。

## 5. Adapter 输出失败语义

- 未注册项目：静默 NO-OP，不产生项目状态；
- Session/Intent/Post/Stop 内部错误：返回 degraded feedback；
- PreToolUse 内部错误：返回原生阻断，防止绕过治理；
- Pi 的 continuation 明确为 emulated 且最多一次；OpenCode 不支持；dsh 的失败重入最多一次；
- 重复 event ID 返回首次已提交结果；同 ID 不同 payload hash 视为碰撞并拒绝。

## 6. Input Dependency Contract v1

```json
{
  "contract_version": 1,
  "project_key": "project-a",
  "profile_id": "app-build",
  "provider_contract_id": "example-provider",
  "provider_contract_version": 1,
  "profile_contract_hash": "sha256_...",
  "dependency_contract_hash": "sha256_...",
  "selectors": [],
  "coverage": "complete"
}
```

Selector：

```json
{
  "kind": "exact_path",
  "path": "manifest.toml",
  "role": "dependency_declaration",
  "presence_sensitive": true
}
```

```json
{
  "kind": "tree",
  "root": "src",
  "universe": "repository_visible",
  "matcher": {
    "matcher_version": 1,
    "include": ["**/*.rs", "*.rs"],
    "exclude": ["generated/**"]
  },
  "role": "source"
}
```

有限 glob：`**` 只能作为完整 segment；`*`/`?` 不跨 `/`；禁止 `..`、反斜线、绝对路径、brace、character class、变量与重复/非规范排序。

`repository_visible` 与 Source fingerprint 边界一致；`project_filesystem` 只用于显式声明 ignored/generated 输入。后者不会自动扩大 Provider 对项目的访问授权。

`coverage`：

- `complete`：声明覆盖全部真实输入；
- `conservative`：有意扩大到安全超集；
- `incomplete`：仅观测，永不参与 hard authority。

## 7. Evidence Input Manifest v1

Manifest 固定合同解析结果：

```json
{
  "manifest_version": 1,
  "contract": {},
  "source_fingerprint_at_creation": "sha256_...",
  "manifest_hash": "sha256_...",
  "entries": [
    {
      "path": "manifest.toml",
      "state": "present_regular_file",
      "role": "dependency_declaration",
      "content_sha256": "sha256_...",
      "size": 123
    }
  ]
}
```

presence-sensitive 文件缺失时记录 `state=absent` 且无 hash/size，因此后续新增同一路径会使 head stale。

Manifest 必须在 Source fingerprint 前后相同的窗口内解析。实时 freshness 重新解析同一合同并比较 manifest hash；解析失败返回 unknown，而不是 fresh。

## 8. Provider Process Protocol v1

Provider 支持两个进程入口：

```text
provider describe
provider run < request.json
```

### Descriptor

```json
{
  "schema_version": 1,
  "protocol_version": 1,
  "provider_id": "example-provider",
  "provider_version": "1.0.0",
  "provider_contract_version": 1,
  "capabilities": ["build"]
}
```

Capabilities 是规范排序且不重复的 Evidence plane 列表。

### Run request

```json
{
  "schema_version": 1,
  "protocol_version": 1,
  "request_id": "sha256_...",
  "provider_id": "example-provider",
  "profile_id": "app-build",
  "project_key": "project-a",
  "plane": "build",
  "source_fingerprint": "sha256_...",
  "input_manifest": {},
  "staged_project_root": "/machine/private/staging/project",
  "output_root": "/machine/private/staging/output",
  "opaque_config": {},
  "opaque_config_hash": "sha256_...",
  "timeout_ms": 600000
}
```

Provider 只能读取 request 中的 staged 输入。协议本身不是恶意进程沙箱，因此 executable 绑定必须显式 `--trust-local-executable`。

### Run response

```json
{
  "schema_version": 1,
  "protocol_version": 1,
  "request_id": "sha256_...",
  "provider_id": "example-provider",
  "profile_id": "app-build",
  "project_key": "project-a",
  "source_fingerprint": "sha256_...",
  "input_manifest_hash": "sha256_...",
  "status": "succeeded",
  "candidate": {
    "plane": "build",
    "provider_version": "1.0.0",
    "provider_contract_version": 1,
    "coverage": "complete",
    "upstream": [],
    "artifacts": [],
    "edges": [],
    "findings": [],
    "payload_schema": "provider-private-v1",
    "payload": {},
    "payload_hash": "sha256_..."
  },
  "error_code": null,
  "error_message": null
}
```

失败响应必须 `status=failed`、无 candidate，且携带规范 `error_code/error_message`。

Provider finding 只含 `deterministic_violation_claim: bool`，不含最终 authority 或 block。核心按绑定 authority ceiling 转换；启发式 ceiling 永远得到 advisory。

核心在提交前再次验证 executable SHA-256、当前 Source fingerprint、request/response 全部身份、payload hash、ArtifactGraph 边界和 upstream。任一失败都不写 trusted Evidence head。

## 9. Source delta 与 Evidence impact

Post-tool impact 结果：

- `reconciled`：基线与当前 Source 可比较，得到规范 changed paths；
- `verification_unknown`：缺基线、状态损坏、Source 竞态或 Git/文件系统失败。

`reconciled` 时：

1. 直接输入合同匹配 changed path 的 head stale；
2. presence-sensitive exact path 对新增/删除同样匹配；
3. stale 沿有效 upstream DAG 传播；
4. 不相交且 complete/conservative 的 head 保留持久状态；
5. hard consumer 仍执行实时 manifest 验证。

`verification_unknown` 时不得保留未经证明的 hard freshness；审计记录 unknown reason、transition plan 和保留/影响 head 集合。

## 10. 数据库版本与迁移

当前数据库 schema 为 v20。v20 移除 Source operation baseline 对固定 adapter 列表的旧 SQL CHECK，由 Rust `AdapterKind` 和协议验证控制当前写入，并允许四适配器迁移后正常审计。

迁移规则：

- 只接受 `1..=current`；
- 非整数或更高版本拒绝；
- DDL 后执行 quick check；
- 项目级记录不得跨 `project_key`；
- append-only 审计不得通过普通更新重写。

## 11. 版本演进

下列版本独立演进：

- Internal Hook Protocol；
- adapter version；
- Evidence Protocol；
- Input Dependency Contract / Manifest；
- Provider Process Protocol；
- Provider private payload schema；
- SQLite schema。

一个版本变化不能通过修改另一个版本号来掩盖。兼容性必须由显式迁移或明确 unsupported 表达。
