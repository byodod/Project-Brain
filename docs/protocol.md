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

当前 Codex adapter 覆盖 `SessionStart`、`UserPromptSubmit`、`PreToolUse`、`PostToolUse`
和 `Stop`。能力矩阵通过 `project-brain capabilities codex` 输出。能力模型明确保留 Prime Agent
的 `continue_after_stop=unsupported`，不把独立 runtime 的 `agent_end` 假装成 Codex Stop。
当前 `IntentDeclared` 只进入审计，尚未接入独立的意图规则模型，因此 Codex 有效能力如实报告
`deny_intent=unsupported`；核心协议保留 `Deny` 类型供后续 adapter/rule 实现使用。
`PostToolUse.tool_response` 只有存在可识别的 success、exit code、error 或 status 证据时才映射
为 succeeded/failed，否则记录 unknown，不从事件名称猜测成功。

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
  "protocol_version": 1,
  "provider": {
    "id": "tree-sitter-rust-syntax",
    "version": "0.1.0+tree-sitter-rust-0.24.2",
    "identity_quality": "syntax_fallback"
  },
  "source_revision": "worktree_v2_<sha256>",
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

`SymbolNode.id` 是 Provider ID 与不歧义 `provider_key` 的摘要。它保证同一个 Provider
声明下可重复，不表示跨 Provider 的全局真相。Provider ID 同时定义 `provider_key` 的
语义契约：破坏性 key 变更必须使用新 ID；兼容的实现或工具链升级只更新 version，
以保持已有符号身份。

身份质量：

- `syntax_fallback`：路径、声明种类、限定名与 occurrence 驱动；rename/move 后产生新 ID。
- `semantic`：由语言语义 Provider 给出；其跨版本保证必须由对应 Provider contract 定义。

`source_revision` 覆盖 HEAD（unborn 仓库使用显式 symbolic-ref 标记）、Provider、全部受支持
源文件的路径/语言/原始内容摘要/语法错误状态，以及节点和边。无符号文件的变化也必须改变 revision。

完整快照的规则：

1. 源文件路径必须规范化且唯一，摘要必须是完整 SHA-256；
2. 所有节点必须对应源文件清单中的路径；
3. 所有节点与边必须属于同一个 Provider；
4. 边不得引用快照外节点；
5. 输入节点必须为 `active`；
6. 应用快照时，旧的 active 节点若消失则转为 `removed`；
7. 相同快照重复应用必须得到全量 `unchanged`；
8. 任何 rename/move lineage 都不能仅由 `syntax_fallback` 自动批准。
