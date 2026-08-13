# ADR-0016：按适配器选择 Doctor

日期：2026-08-14

## 状态

Accepted

## 背景

Codex 与 Claude Code 已分别拥有 direct adapter、用户级配置和独立 integration manifest。旧版
`doctor` 仍固定读取 Codex 配置，并输出 `codex_hooks` 字段；这会使 Claude 安装无法进入同一条
确定性就绪检查，也容易把适配器身份表达错误。

## 决策

1. CLI 接受 `doctor [codex|claude-code]`；省略参数时默认 Codex，保持现有脚本兼容。
2. App 在进入 setup 层前显式选择对应的 home：Codex 使用 `--codex-home`，Claude 使用
   `--claude-home`。
3. setup 层分别读取对应配置文件和 integration manifest，并比较精确托管 handler hash；不得
   跨适配器复用 manifest、签名或配置路径。
4. Doctor 报告 schema 升级为 v2，输出通用 `adapter`、`adapter_hooks` 与
   `adapter_trust_state`。不保留含义错误的 Codex 专用字段别名。
5. Hook 内容完整只能证明 Project Brain 的托管配置没有缺失、重复或漂移；vendor UI 中的人工
   信任仍报告 `not_programmatically_verifiable`。

## 验证

黑盒 fixture 在同一个已注册项目和已绑定 Provider 环境中分别安装 Codex 与 Claude Code Hook，
验证默认 Doctor 选择 Codex、显式 Doctor 选择 Claude，且两者均报告对应 adapter 和 pass；卸载
Codex 后默认 Doctor 必须降级，不受仍存在的 Claude 配置误导。

## 后果

- 两个已实现适配器具备相同的确定性健康检查入口。
- Doctor v2 是有意的结构化输出变更；调用方应从通用 adapter 字段读取状态。
- 这不等于已完成 Claude 真实进程 lifecycle fixture，也不扩展 Prime Agent 能力。

## 参考

- [ADR-0014：Claude Code direct adapter v1](0014-claude-code-direct-adapter-v1.md)
- [ADR-0015：Claude Code 用户级 Hook 原子安装](0015-claude-code-atomic-hook-install.md)
