# ADR-0018：Prime Agent 独立 Runtime Direct Adapter

日期：2026-08-14

## 状态

Accepted

## 背景

Prime Agent 是 daemon-backed 的独立 Coding Agent runtime，不是 Codex 或 Claude Code 的插件。
它基于 Pi Extension API，正式提供 `input`、`before_agent_start`、`tool_call`、`tool_result` 与
`agent_end` 等事件；其中 `tool_call` 可在执行前返回 `block`。

Prime 也提供 JSON/RPC 模式、follow-up/steer、heartbeat 与 schedule，但这些 runtime 控制面不能
被等同于另一个 vendor 的 Stop Hook。当前 Extension 文档把 `agent_end` 定义为一次 user prompt
的结束，且没有稳定 `agent_settled`；因此不能声称 Stop 后继续已验证。

## 决策

1. 增加 `prime-agent` CLI adapter，使用 `AdapterKind::PrimeAgent`、adapter version 1、独立事件与
   operation ID namespace，以及 SQLite `prime_agent` 审计域。
2. Rust 侧 direct adapter 接受由未来 Prime Extension 规范化的字段子集，但输出 Project Brain
   自有 schema，不复用 Codex/Claude Hook JSON。
3. Pre-tool 输出 `block`、`reason` 与 `context`；确定性处理失败时 fail-closed，返回 `block=true`。
4. Session/intent/post-tool 输出上下文或反馈；失败要显式标记 degraded，不伪造成功。
5. Task-stopping 始终声明 continuation `supported=false`。即便核心产生 ContinueWork，也只能保留
   `requested=true` 与原因作为证据，不得触发未确认的自动续轮。
6. `install-hooks prime-agent`、用户级 Extension 写入与 Prime doctor 在具备原子安装、漂移拒绝和
   真实 runtime fixture 前必须明确拒绝。

## 验证

- 单元测试验证受保护文件修改返回 `block=true`，审计记录属于 `prime_agent`，且 continuation
  capability 为 unsupported。
- CLI 黑盒 fixture 初始化临时项目，经 `hook prime-agent pre-tool-use` 返回独立 schema 的 block，
  并核对 `capabilities prime-agent` 不声称 Stop continuation。

## 后果

- Project Brain 可以独立演进 Prime Extension，不污染 Codex/Claude 配置或审计。
- direct adapter 可测试不等于用户安装已完成；README 与 CLI 必须继续暴露此边界。
- Prime 的 heartbeat/schedule 属于 runtime 唤醒机制，不属于 Project Brain Hook 事件本身。

## 依据

- Prime Agent repository：<https://github.com/PrimeIntellect-ai/prime-agent>（访问：2026-08-14）
- Prime Agent Extensions：<https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/docs/extensions.md>（访问：2026-08-14）
- Prime Agent RPC mode：<https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/docs/rpc.md>（访问：2026-08-14）
