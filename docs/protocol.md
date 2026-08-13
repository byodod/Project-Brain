# 协议说明

## Schema 版本

所有跨边界对象必须包含：

```json
{
  "schema_version": 1
}
```

当前 Runtime 对未知版本 fail closed，避免把新字段静默解释成旧语义。

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

1. 把外部 Hook 输入转换为 `ActionDescriptor`；
2. 调用确定性内核；
3. 把 `Decision` 转换回外部协议；
4. 记录审计事件。

Adapter 不得自行重新解释某条项目规则。

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
