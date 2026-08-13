# ADR-0014：Claude Code direct adapter v1

日期：2026-08-14

## 状态

Accepted

## 背景

Internal Hook Protocol v1 已通过 Codex 的五个同步生命周期事件验证。Claude Code 当前相同事件
提供可映射的公共字段子集，但 vendor identity、delivery 重放域和工具 operation identity 不能与
Codex 共用。用户级配置安装还需要单独验证 `settings.json` 合并、跨平台命令和漂移恢复，不能由
协议字段相似性直接推出。

## 决策

1. 新增 Claude Code adapter v1，覆盖 `SessionStart`、`UserPromptSubmit`、`PreToolUse`、
   `PostToolUse` 与 `Stop` 的直接 `hook/dispatch` 入口。
2. 输入只消费已有 `CodexHookInput` 所代表的公共字段子集；未知 vendor 字段由 serde 忽略，不据此
   推断额外语义。
3. 规则执行、Provider trust、文件影响提取、失败输出与 outcome 映射共用已经测试的确定性路径。
4. adapter identity 固定为 `claude_code`，adapter version 为 1；session、event 与 operation 哈希使用
   `claude_code_*` namespace，绝不与 Codex 去重。
5. `install-hooks claude-code` 与 `uninstall-hooks claude-code` 在安装器完成前明确拒绝，不修改用户
   配置。
6. `SubagentStart`、`SessionEnd` 和 Prime Agent lifecycle 不映射到现有事件，不伪造支持。

## 后果

- 可以用真实 Claude Hook JSON 直接验证项目隔离、阻断和审计语义。
- 还不能宣称 Claude Code 已具备开箱即用的机器级安装体验。
- 后续安装器必须独立完成原子合并、精确 handler hash、卸载保留用户配置及跨平台 fixture，之后
  才能移除此明确拒绝。

## 参考

- Claude Code Hooks：<https://code.claude.com/docs/en/hooks>（访问：2026-08-13）
- [ADR-0002：Internal Hook Protocol](0002-internal-hook-protocol.md)
- [ADR-0003：Project Identity 与 Adapter Audit](0003-project-identity-and-adapter-audit.md)
