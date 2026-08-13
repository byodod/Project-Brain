# ADR-0017：Claude Code Hook 使用 Exec Form

日期：2026-08-14

## 状态

Accepted；修订 ADR-0015 的 command 形态

## 背景

ADR-0015 最初在 Windows 生成一个再次启动 PowerShell 的 shell-form 字符串。安装后的真实进程
fixture 证明：经 Windows shell 转发时，该字符串可能只输出内层命令文本而没有启动稳定 launcher。
这使只比较 JSON 结构和 handler hash 的测试产生假阳性。

Claude Code 正式 Hook 协议已提供 exec form：存在 `args` 时，`command` 被解析为可执行文件，参数
按数组逐项传入，不经过 shell tokenization。Windows 要求目标是真实 `.exe`；Project Brain 的稳定
launcher 正好满足该条件。

## 决策

1. Claude handler 的 `command` 是稳定 launcher 的绝对路径。
2. `args` 固定为 `['dispatch', 'claude-code', '<event>']`；事件只能是已实现的五个 lifecycle。
3. 不写 `shell`，不启动 PowerShell/Git Bash/POSIX shell，也不把路径和参数拼成一个字符串。
4. 托管签名同时要求固定 `statusMessage` 标记和三个参数完全匹配；内容 hash 仍覆盖完整 handler，
   因此 launcher 路径、timeout、标记或参数漂移均会被拒绝。标记避免依赖可执行文件的发行文件名。
5. 黑盒 fixture 必须从安装后的 `settings.json` 读取 `command` 和 `args`，以真实子进程执行并传入
   Claude PreToolUse JSON；只调用内部函数不算安装链验证。

## 验证

fixture 在带空格的安装路径中直接启动托管 handler，将删除 `.project-brain/config.json` 的 Bash
工具输入写入 stdin，并确认稳定 launcher 输出 Claude `PreToolUse` deny JSON。测试不启动 Claude
模型，也不依赖用户真实配置。

## 后果

- 路径中空格、引号和 shell 元字符不再参与命令解析。
- Windows、Linux 与 macOS 共用同一种 handler 结构。
- 未来升级 Claude Code 时仍需对照正式 Hook schema；未知字段或协议变化不得静默猜测。

## 参考

- Claude Code Hooks reference：<https://code.claude.com/docs/en/hooks>（访问：2026-08-14）
- [ADR-0015：Claude Code 用户级 Hook 原子安装](0015-claude-code-atomic-hook-install.md)
