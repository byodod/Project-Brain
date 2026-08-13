# Project Brain

Project Brain 是一个独立于具体 Coding Agent 的项目决策控制面。它不依赖 Agent 主动“想起”记忆，而是在生命周期 Hook 中，根据仓库级规则和实际变更给出确定性决策。

当前版本是第一个可运行的纵向切片，包含：

- `ALLOW / ALLOW_WITH_CONTEXT / BLOCK / ESCALATE` 四态规则引擎；
- 带 authority、strength、scope 和 lifecycle 的版本化规则模型；
- Codex `SessionStart`、`PreToolUse`、`PostToolUse`、`Stop` 协议适配；
- SQLite 本地审计记录；
- Git Change Envelope 范围核对；
- Windows、Linux、macOS 可构建的 Rust CLI。

## 核心原则

1. 没有 LLM 时，确定性 Runtime 仍然必须完整工作。
2. 只有 `hard` 且 authority 为 `explicit_user`、`repository_rule` 或 `accepted_decision` 的规则可以阻断操作。
3. `agent_inference` 和 `observed_pattern` 只能提供上下文或升级为待决策事项。
4. SQLite 是本地审计和派生状态，不是仓库规则的权威来源。
5. `PostToolUse` 无法撤销已经发生的副作用，只能阻止 Agent 把结果视为完成。

## 构建与测试

需要 Rust 1.92 或更新版本：

```text
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
```

构建 release 可执行文件：

```text
cargo build --release --locked -p project-brain
```

## 快速开始

在目标仓库根目录执行：

```text
project-brain init
```

它会创建：

```text
.project-brain/
├── config.json      # 应提交：项目规则权威来源
├── envelope.json    # 应提交或按任务生成：声明变更范围
└── brain.db         # 不提交：本地审计数据库
```

通用 preflight 从标准输入读取 `ActionDescriptor`：

```json
{
  "schema_version": 1,
  "event_id": "tool-42",
  "session_id": "session-7",
  "cwd": "D:/repo",
  "action": "modify",
  "operation": "apply_patch",
  "target_files": ["src/domain/order.rs"]
}
```

然后执行：

```text
project-brain preflight
```

并把上面的 JSON 写入其标准输入。输出始终是一个结构化决策 JSON。

## Codex Hook 接入

将 [examples/codex-hooks.json](examples/codex-hooks.json) 复制或合并到仓库的 `.codex/hooks.json`，并确保 `project-brain` 在 `PATH` 中。示例配置使用同步 `PreToolUse`，因此硬规则可以在工具执行前拒绝调用。

手工验证适配器：

```text
project-brain hook codex pre-tool-use
```

标准输入示例：

```json
{
  "session_id": "session-7",
  "cwd": "D:/repo",
  "hook_event_name": "PreToolUse",
  "turn_id": "turn-2",
  "tool_name": "apply_patch",
  "tool_use_id": "tool-42",
  "tool_input": {
    "command": "*** Begin Patch\n*** Delete File: .project-brain/config.json\n*** End Patch"
  }
}
```

该请求会被仓库默认硬规则拒绝。

## Change Envelope

检查当前工作区相对 `HEAD` 的所有已跟踪和未跟踪文件：

```text
project-brain reconcile --base HEAD --envelope .project-brain/envelope.json
```

- 触及 `forbidden_paths`：`block`；
- 超出 `allowed_paths`：`escalate`；
- 完全处于声明范围：`allow`。

## Workspace

```text
crates/
├── brain-core/       # 协议、规则验证、确定性决策
├── brain-store/      # SQLite schema 与审计
└── project-brain/    # CLI、Git、Codex Hook 适配
```

进一步设计见：

- [架构说明](docs/architecture.md)
- [协议说明](docs/protocol.md)

## 当前限制

- 当前只提供 Codex 适配器；Claude Code 和 Prime Agent 尚未实现。
- shell 命令只做保守的显式危险模式识别，不承诺成为完整 shell 安全沙箱。
- 当前代码分析只覆盖 Git 文件范围和 Hook 载荷，尚未接入 Tree-sitter、LSP/SCIP 或符号图。
- `reconcile` 当前是显式命令，尚未自动挂入 `Stop`。
- Semantic Sentinel / Architecture Judge 尚未加入；这是有意的 V0 边界。

