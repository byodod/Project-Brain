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
