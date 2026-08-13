# ADR-0015：Claude Code 用户级 Hook 原子安装

日期：2026-08-14

## 状态

Accepted；Windows command 形态由 ADR-0017 修订

## 背景

ADR-0014 只允许 Claude Code direct adapter，不允许在缺少合并和漂移测试时改写用户配置。
本机 Claude Code 2.1.150 的真实 `~/.claude/settings.json` 已确认 `hooks.<event>[]` group 与
`hooks[]` command handler 结构；生产安装仍必须避免读取或改写任何非托管设置的语义。

## 决策

1. `install-hooks claude-code` 管理 `CLAUDE_CONFIG_DIR/settings.json` 或 `~/.claude/settings.json`；
   `--claude-home` 可显式覆盖。
2. 只安装 `SessionStart`、`UserPromptSubmit`、`PreToolUse`、`PostToolUse` 与 `Stop`。工具 matcher
   固定为 `Bash|Edit|Write|NotebookEdit`。
3. 原始决策使用平台 shell 包裹稳定 launcher；ADR-0017 根据 Claude Code 的正式 exec-form 契约
   将其修订为不经过 shell 的 `command` + `args`。
4. Claude 使用独立的 `state/integrations/claude-code.json`、锁文件和 handler signature；不得与
   Codex manifest 或 hash 混用。
5. 安装采用 compare-and-swap 原子替换。已有 manifest 且 handler 完全一致时返回 no-op；任何缺失、
   重复或内容漂移默认拒绝覆盖。
6. 卸载只删除 manifest 中精确记录的 handler，保留用户字段、其他事件和后来添加的 handler。

## 验证

黑盒 fixture 从真实可执行文件完成 install → merge → idempotent reinstall → drift rejection →
uninstall，验证五个 handler、用户 `language` 和自定义 `Stop` hook 全程保留。

## 后果

- Claude Code 获得与 Codex 同级的可回滚用户级接入，但两套配置和审计仍完全隔离。
- 按 adapter 选择的就绪检查由 ADR-0016 补充。
- 安装后 handler 的无 shell 真实子进程验证由 ADR-0017 补充。
- 不因安装器存在而声称 `SubagentStart`、`SessionEnd` 或 Prime Agent 已实现。

## 参考

- Claude Code Hooks：<https://code.claude.com/docs/en/hooks>（访问：2026-08-14）
- [ADR-0014：Claude Code direct adapter v1](0014-claude-code-direct-adapter-v1.md)
