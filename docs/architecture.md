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

`brain-core` 不依赖文件系统、Git、SQLite 或任何 Agent SDK。相同输入、配置和 schema_version 必须产生相同决策。

## 权威来源

```text
.project-brain/config.json
        │
        ├── rules        权威、应进入版本控制
        └── lifecycle    权威、保留 superseded/retired 轨迹

.project-brain/brain.db
        │
        └── audit_events 本地派生记录、不进入版本控制
```

后续加入符号图时，也应能从仓库重新推导；SQLite 中的代码事实不能成为不可恢复的唯一来源。

## 阻断权限

阻断必须同时满足：

```text
effect = block
strength = hard
authority ∈ { explicit_user, repository_rule, accepted_decision }
```

配置加载阶段即拒绝其他组合，避免把概率判断意外提升为强制规则。

## 下一阶段

1. 增加事件幂等键和 schema migration 测试。
2. 把 `Stop` 与 Change Envelope reconcile 连接起来。
3. 增加 Claude Code 和 Prime Agent 适配器；核心协议保持不变。
4. 增加 Tree-sitter changed-symbol 提取。
5. 通过语言原生分析器补足跨文件语义解析。
6. 最后才加入只读、可拔插的 Semantic Sentinel。
