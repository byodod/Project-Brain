# ADR-0002：Agent adapter 统一到事件专属的内部 Hook 协议

- 状态：Accepted
- 日期：2026-08-13

## 背景

Codex、Claude Code 与 Prime Agent 的生命周期和控制能力不对称。vendor JSON 中相同的
`block` 字样可能分别表示拒绝工具、阻止输入或要求 Agent 继续工作，不能成为核心语义。

## 决策

Project Brain v1 公共边界仅包含 `SessionOpened`、`IntentDeclared`、`ToolAboutToRun`、
`ToolFinished` 与 `TaskStopping`。Outcome 也按事件区分：gate 使用 `NoVeto | Deny`，
工具执行后只产生 feedback，停止阶段使用 `AllowStop | ContinueWork`。

`NoVeto` 永远不表示替用户批准 vendor permission。Adapter 只转换协议和能力，不重新解释规则。
Codex/Claude Code/Prime Agent 的能力通过显式 capability model 表达；不支持的能力必须报告
`unsupported`。

V3a 的 `IntentDeclared` 只做规范化和审计，不把 prompt 强塞进文件动作规则引擎。Codex adapter
因此报告 `deny_intent=unsupported`，直到独立意图规则具备确定性 authority/scope 契约。

## 结果

- 核心不依赖任何 vendor 的 block JSON。
- Pre 与 Post 通过 operation ID 相关，而不是全局先后状态机。
- 本阶段只迁移 Codex；Claude Code 与 Prime Agent 等协议经真实使用验证后再加入。
- rust-analyzer/SCIP、稳定 lineage、symbol-scoped rules 与 LLM 不属于本决策的实现范围。

## 依据

- Codex Hooks：<https://developers.openai.com/codex/hooks>（访问：2026-08-13）
- Claude Code Hooks：<https://code.claude.com/docs/en/hooks>（访问：2026-08-13）
- Prime Agent：<https://github.com/PrimeIntellect-ai/prime-agent>（访问：2026-08-13）
