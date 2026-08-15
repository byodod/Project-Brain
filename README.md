# Project Brain

Project Brain 是面向 Coding Agent 的确定性项目决策与长期记忆控制面。它把仓库规则、实际工具动作、
源码状态、语义索引和可验证证据组合成可审计决策；Agent 不需要“想起来”调用记忆工具。

核心在没有 LLM 时完整工作。LLM 可以作为低权限语义 Provider，但不能自行获得阻断权限。

当前稳定版本为 [`v0.2.3`](https://github.com/byodod/Project-Brain/releases/tag/v0.2.3)。

## 为什么使用 Project Brain

- 在 Agent 执行工具之前强制检查仓库规则，而不是依赖模型主动检索记忆；
- 在每个可用的模型步骤恢复目标锚点与待处理纠偏状态，compact/resume/subagent 后重新水合；
- 以实际工具输入、Git 变更和 Evidence 为准，不把 Agent 的文字意图当作事实；
- 将结构化写入提案与工具后的真实 Source delta 对比，发现外溢后暂停无关写入并要求修复；
- 只有明确、受信且可验证的规则可以阻断操作；
- 所有状态按 `project_key` 隔离，可审计、可重放；
- 核心不绑定语言、IDE、游戏引擎或应用框架，专用能力通过 Provider 扩展。

## 支持的 Agent

Project Brain `v0.2.3` 正式支持四个 Agent 接入：

| Agent | 接入方式 | 模型前主动上下文 | 工具前阻断 | 工具后反馈 | Stop 续轮 |
|---|---|---:|---:|---:|---:|
| Codex | 用户级 `hooks.json` | 不支持 | 支持 | 支持 | 支持 |
| Pi | 用户级 Extension | 不支持 | 支持 | 支持 | 模拟（最多一次） |
| OpenCode | 用户级 Plugin | 不支持 | 支持 | 支持 | 不支持 |
| dsh | 显式 profile Plugin bundle | 支持 | 支持 | 支持 | 支持 |

能力不对称是上游生命周期协议的事实。使用以下命令查看机器可读能力，不支持的能力不会被伪装成支持：

```text
project-brain capabilities codex
project-brain capabilities pi
project-brain capabilities opencode
project-brain capabilities dsh
```

## 快速开始

通过 npm 安装包含四个平台原生二进制的官方包：

```text
npm install --global @byodod/project-brain
```

也可以从 [GitHub Releases](https://github.com/byodod/Project-Brain/releases) 下载当前平台压缩包并校验
`SHA256SUMS`。然后让当前二进制安装机器级稳定 launcher：

```text
project-brain install
```

在目标仓库中初始化并注册项目。Profile 只声明项目事实，不会自动下载外部工具：

```text
project-brain init --profile rust
project-brain bootstrap
```

安装并检查所需 Agent 接入，以 Codex 为例：

```text
project-brain install-hooks codex
project-brain doctor codex
```

.NET、Python、多语言项目以及 Pi、OpenCode、dsh 的完整命令见
[快速开始与 Agent 接入](docs/getting-started.md)。

## 复制给 AI 的安装提示词

不想手工判断项目语言和 Agent 接入时，可以在目标仓库中把下面整段提示词交给正在使用的 Coding
Agent。它要求 AI 只安装当前需要的接入，并保留已有 Project Brain 配置：

```text
请在当前仓库中通过 npm 正式包安装并初始化 Project Brain，完成后给出可核对的验证结果。

要求：
1. 先检查当前仓库、操作系统、Node.js/npm 是否可用，并根据实际项目文件判断语言；只允许使用 rust、dotnet、python 中真实存在的 profile，可同时选择多个。不要把框架或游戏引擎写入 Project Brain 核心配置。
2. 从 npm registry 安装 @byodod/project-brain 的 latest 正式版本，验证 project-brain --version；不要用仓库源码构建、本地 tarball 或其它旧二进制冒充 npm 安装。
3. 运行 project-brain install，建立机器级稳定 launcher。若仓库尚未初始化，使用识别出的语言 profile 执行 project-brain init；若 .project-brain/config.json 已存在，必须保留并验证，禁止覆盖、删除或重新生成。
4. 运行 project-brain bootstrap 注册当前项目。
5. 识别我当前实际使用的 Agent，只安装对应接入：Codex、Pi、OpenCode 或 dsh，不要顺带安装其它 Agent 的 Hook。若为 dsh，必须根据 DSH 的实际启动命令确定 profile，例如 dsh web 对应 web；不要把界面里的 Agent preset 名称当成 profile。安装后重启对应的 DSH 进程。
6. 运行对应的 project-brain doctor 和 project-brain capabilities。doctor 因 semantic Provider 未绑定而 degraded 时，应把它与 Hook 安装失败分开报告，不要擅自下载或绑定 Provider。
7. 进行一次非破坏性的真实 Agent 生命周期验收：必须由当前 Agent 正常触发 Hook/Plugin，并用 project-brain audit 确认真实 session 产生了事件；不能只用 project-brain dispatch 或 preflight 的手工调用代替。
8. 不修改业务源码，不创建提交，不推送，不发布，不安装无关工具。任何可能覆盖现有配置或无法确定的 Agent/profile 选择都先停止并说明。

最后报告：npm 包版本、原生二进制版本、机器 launcher 路径、project_key、语言 profile、安装的 Agent/profile、doctor 各项状态、真实生命周期验收证据，以及仍需人工处理的事项。
```

该提示词适合已有仓库和首次安装；更严格的来源校验、卸载及各 Agent 差异见
[快速开始与 Agent 接入](docs/getting-started.md)和
[dsh 接入、远程安装与验收](docs/dsh-integration.md)。

## 决策规则

仓库规则位于 `.project-brain/config.json`。以下 hard repository rule 会阻止直接删除 Domain 文件：

```json
{
  "id": "ARCH-001",
  "status": "active",
  "authority": "repository_rule",
  "strength": "hard",
  "effect": "block",
  "include_paths": ["src/domain/**"],
  "exclude_paths": [],
  "actions": ["delete"],
  "operations": [],
  "operation_contains": [],
  "symbol_scopes": [],
  "message": "Domain 文件不可直接删除",
  "rationale": "迁移必须保留兼容路径"
}
```

决策状态为 `allow`、`allow_with_context`、`escalate`、`require_review`、`block`。
`require_review` 会撤回当前变更选择，在下一模型步骤交付目标相关约束后，才允许重新提出相同提案。
只有 `strength=hard` 且 authority 为 `explicit_user`、`repository_rule` 或 `accepted_decision` 的规则
可以配置 `require_review` 或 `block`。

Agent 可以用 `project-brain claims submit` 追加 `GoalInterpretation`、`CompatibilityAssessment` 等声明；
这些记录不可删除、只有低权限，不能豁免规则或由 Agent 自行标记“已实现”。完整执行模型见
[架构说明](docs/architecture.md) 和 [协议说明](docs/protocol.md)。

```text
project-brain claims submit --agent dsh --session <session-id> --claim-id <unique-id> --kind compatibility_assessment --content <structured-summary>
project-brain claims list --limit 20
```

## 安全边界

- 相同输入、配置与 schema 产生相同规则决策；
- `agent_inference`、`observed_pattern` 和启发式 Provider 只能提供上下文或升级处理；
- 未知协议、损坏配置、安装漂移和证据身份不匹配会失败关闭；
- Provider executable、entrypoint 和 descriptor 使用 SHA-256 固定；
- 外部进程通过统一 containment runner 执行，但这不是通用网络、内核或容器沙箱；
- 符号 rename/move lineage 只能由人工确认。

## 文档

| 文档 | 内容 |
|---|---|
| [文档索引](docs/README.md) | 文档入口与阅读顺序 |
| [快速开始与 Agent 接入](docs/getting-started.md) | 安装、初始化、Hook/Plugin、检查与卸载 |
| [Provider 与 Evidence](docs/providers.md) | Semantic Provider、Evidence Provider、输入合同与 freshness |
| [运维、资格与发布](docs/operations.md) | 数据库、审计、Production Qualification、构建与 Release |
| [架构说明](docs/architecture.md) | 分层、依赖方向、身份、存储与安全边界 |
| [协议说明](docs/protocol.md) | Hook、能力矩阵、Evidence 与 Provider Process Protocol |
| [架构决策记录](docs/adr/) | 关键设计选择及其验证依据 |

## 明确不做

- 不把聊天记录检索当作治理依据；
- 不让 Agent 或 Provider 自行授予 hard authority；
- 不自动下载、安装或发现外部 Provider；
- 不从 occurrence 猜测 Provider 未明确证明的语义边；
- 不自动确认跨 rename/move 的 semantic lineage；
- 不为能力矩阵对称而伪造 Agent 不支持的生命周期行为；
- 不在核心内建 Godot 或其它框架、引擎、IDE 的专用逻辑。

## 开发

项目要求 Rust 1.92。开发门禁和发布流程见 [运维、资格与发布](docs/operations.md)。

## 许可证

Project Brain 采用 `MIT OR Apache-2.0` 双许可证。使用者可以自行选择遵守
[MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 中的任意一种，无需同时遵守两者；完整说明见
[LICENSE](LICENSE)。

Copyright (c) 2026 byodod and Project Brain contributors。
