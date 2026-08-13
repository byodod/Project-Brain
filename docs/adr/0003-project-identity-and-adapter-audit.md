# ADR-0003：项目身份参与事件幂等与 adapter 审计

- 状态：Accepted
- 日期：2026-08-13

## 背景

`cwd` 会随 clone、目录移动和 worktree 改变；不同 Agent/项目也可能产生相同 session 或 tool ID。
仅按 vendor ID 去重会让多个项目互相命中缓存或串联审计。

## 决策

初始化时生成 `project_key` 并持久化在 `.project-brain/config.json`。所有 Internal Hook Event
必须携带非空 project key，SQLite 使用 `(project_key, adapter_kind, event_id)` 作为 delivery
唯一域。查询 adapter audit 时必须显式限定项目。

事件还声明 `vendor_stable`、`derived_stable` 或 `per_delivery` 幂等质量。没有稳定 vendor 键时，
adapter 生成非空 delivery ID，但不得声称能够跨进程识别重放。event ID 与工具 operation ID
分离，避免把投递身份误作调用因果身份。

旧配置首次打开时从配置稳定内容派生并写回 project key，不使用 checkout 绝对路径；受版本控制
的同一配置在路径变化后保持项目身份。新初始化项目使用独立生成并持久化的 key。

## 结果

- 不同项目可安全复用相同 session/event/tool ID。
- 同一幂等域的重复 delivery 返回首次持久化 outcome。
- adapter audit 记录 vendor、版本、事件、outcome、延迟与失败。
- `cwd` 保留为事件证据，但不参与项目授权或归属判断。
- operation ID 的摘要域包含 project key 与规范化 session key。
- PreToolUse 审计失败必须输出 vendor 可识别的 deny，不能用进程失败代替 fail-closed。
