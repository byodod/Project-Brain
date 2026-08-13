# Project Brain

Project Brain 是一个独立于具体 Coding Agent 的项目决策控制面。它不依赖 Agent 主动“想起”记忆，而是在生命周期 Hook 中，根据仓库级规则和实际变更给出确定性决策。

当前版本包含：

- `ALLOW / ALLOW_WITH_CONTEXT / BLOCK / ESCALATE` 四态规则引擎；
- 带 authority、strength、scope 和 lifecycle 的版本化规则模型；
- Project-scoped Internal Hook Protocol v1；
- Codex `SessionStart`、`UserPromptSubmit`、`PreToolUse`、`PostToolUse`、`Stop` 协议适配；
- 按项目和 adapter 隔离、可重放的 SQLite 本地审计记录；
- Git Change Envelope 范围核对；
- Codex `Stop` 自动 Change Envelope 对账与防循环保护；
- 基于 Tree-sitter 的 Rust changed-symbol 与纯删除符号提取；
- Project-scoped Provider-neutral 符号身份协议、完整工作区快照与本地派生符号图；
- 按项目显式配置的离线 SCIP 导入，首批契约覆盖 rust-analyzer、scip-dotnet 与 scip-python；
- 开放 language ID、逐文档语言映射和四态语义能力声明；
- Project-scoped semantic lineage ledger、不可变证据与 append-only 显式裁决；
- SQLite schema v1→v4 迁移、按项目隔离的符号 removed 历史与幂等增量更新；
- Windows、Linux、macOS 可构建的 Rust CLI。

## 核心原则

1. 没有 LLM 时，确定性 Runtime 仍然必须完整工作。
2. 只有 `hard` 且 authority 为 `explicit_user`、`repository_rule` 或 `accepted_decision` 的规则可以阻断操作。
3. `agent_inference` 和 `observed_pattern` 只能提供上下文或升级为待决策事项。
4. SQLite 是本地审计和派生状态，不是仓库规则的权威来源。
5. `PostToolUse` 无法撤销已经发生的副作用，只能阻止 Agent 把结果视为完成。
6. 语法 Provider 的身份必须标记为 `syntax_fallback`，不得冒充跨 rename/move 稳定语义。
7. `project_key` 是项目边界；`cwd`、session ID 和 event ID 都不能单独代表项目。
8. 核心 `NoVeto` 不等于批准 Agent vendor 权限。

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
├── config.json      # 应提交：project_key 与项目规则权威来源
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

查看当前 Codex adapter 明确声明的能力：

```text
project-brain capabilities codex
```

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

内部事件和审计均携带配置中持久化的 `project_key`。相同 session/tool ID 在不同项目中
会落入不同幂等域；重复 delivery 则复用首次 outcome。`audit` 命令同时输出当前项目的
`adapter_events` 与旧 preflight 的 `legacy_actions`。

## Change Envelope

检查当前工作区相对 `HEAD` 的所有已跟踪和未跟踪文件：

```text
project-brain reconcile --base HEAD --envelope .project-brain/envelope.json
```

- 触及 `forbidden_paths`：`block`；
- 超出 `allowed_paths`：`escalate`；
- 完全处于声明范围：`allow`。

Envelope 文件必须位于项目根目录内；绝对路径、`..` 或符号链接解析后若越出仓库，
Runtime 会拒绝读取。

仓库配置可让 Codex `Stop` 自动执行同一检查：

```json
{
  "stop_reconcile": {
    "enabled": true,
    "base": "HEAD",
    "envelope": ".project-brain/envelope.json"
  }
}
```

若 Codex 正在响应 Stop hook 自己发起的继续请求，适配器读取
`stop_hook_active` 并直接放行，避免无限循环。

## 变更符号分析

提取工作区相对基线实际触及的 Rust 符号：

```text
project-brain analyze --base HEAD
```

输出按文件区分 `changed_symbols` 与 `removed_symbols`。未跟踪 Rust 文件按全文分析；
纯删除 hunk 从 Git 基线读取旧源码，因此删除函数不会丢失。当前同时报告叶级符号和词法所有者，
例如 `impl Worker` 与 `impl Worker::run`。

## 符号图

对当前工作区的已跟踪与未忽略、未跟踪 Rust 文件建立完整快照：

```text
project-brain index
```

重复执行相同快照是幂等的；消失的符号保留为 `removed` 历史。查询当前符号：

```text
project-brain symbols --path crates/brain-core --limit 50
```

需要查看历史时增加 `--include-removed`。当前 Tree-sitter Provider 明确输出
`identity_quality: syntax_fallback`：相同路径、种类和限定名具有可重复 ID，但 rename/move
会产生新 ID，Runtime 不会自动声称其 lineage 相同。快照 revision 还覆盖所有受支持源文件
的内容摘要和语法错误状态，因此无符号文件的变化也可检测；没有首个 commit 的仓库使用显式
unborn HEAD 标记。符号 ID、快照、查询和 tombstone 都显式绑定配置中的 `project_key`；相同代码
在不同项目中生成不同身份，即使未来共用一个数据库也不会串图。跨快照 lineage 由 Brain
自己维护，目前只生成 `proposed`/`ambiguous` 候选，不会自动复用 ID 或改写历史。

### 离线 SCIP 语义索引

Project Brain 不自动根据扩展名、`Cargo.toml`、solution 或 `pyproject.toml` 猜测项目语言。
每个项目需要在 `.project-brain/config.json` 中显式声明语言和 provider：

```json
{
  "language_profiles": [
    { "language": "rust", "roots": [] },
    { "language": "csharp", "roots": ["src"] },
    { "language": "visual-basic", "roots": ["src"] },
    { "language": "python", "roots": ["python"] }
  ],
  "semantic_providers": [
    {
      "id": "rust-main",
      "format": "scip",
      "producer": "rust-analyzer",
      "contract_version": 1,
      "language_mappings": [
        { "raw_language": "rust", "language": "rust", "allow_missing_language": false }
      ]
    },
    {
      "id": "dotnet-main",
      "format": "scip",
      "producer": "scip-dotnet",
      "contract_version": 1,
      "language_mappings": [
        { "raw_language": "C#", "language": "csharp", "allow_missing_language": false },
        { "raw_language": "Visual Basic", "language": "visual-basic", "allow_missing_language": false }
      ]
    },
    {
      "id": "python-main",
      "format": "scip",
      "producer": "scip-python",
      "contract_version": 1,
      "language_mappings": [
        { "raw_language": null, "language": "python", "allow_missing_language": true }
      ]
    }
  ]
}
```

生成 `.scip` 后按项目内稳定 profile ID 导入：

```text
project-brain index-scip --provider rust-main --input index.scip
```

一个 `.scip` 可以逐文档映射多种语言，例如同一 scip-dotnet 索引内的 C# 与 Visual Basic。
Python 的空 `Document.language` 只有在 profile 显式声明 `raw_language: null` 和
`allow_missing_language: true` 时才接受。Producer 版本只记录来源，不参与 Brain contract 版本。

## Semantic lineage ledger

连续导入同一 semantic provider 的新快照时，Project Brain 只对相邻快照中真正消失和新增的
symbol 生成 lineage 候选。稳定 ID 不会生成“自己指向自己”的候选，本地 symbol 不参与跨快照
lineage。候选默认永远是 `proposed`；高置信、唯一匹配或 raw SCIP symbol 相同都不能自动确认。

查看候选：

```text
project-brain lineage candidates --state proposed
```

显式确认或拒绝时必须提供调用者生成的幂等 request ID：

```text
project-brain lineage confirm \
  --candidate <candidate-id> \
  --request-id <request-id> \
  --actor-ref user@example \
  --reason "confirmed rename"

project-brain lineage reject \
  --candidate <candidate-id> \
  --request-id <request-id> \
  --reason "different responsibility"
```

修正既有确认必须原子完成：

```text
project-brain lineage confirm \
  --candidate <new-candidate-id> \
  --supersede <old-confirmed-candidate-id> \
  --request-id <request-id>
```

状态为 `proposed / confirmed / rejected / superseded / invalidated`。Ambiguity 是候选组属性，
不是状态。新快照、算法升级或置信度变化不会修改旧候选或人工裁决；裁决也不会修改 symbol ID、
tombstone 或历史快照。

## Workspace

```text
crates/
├── brain-analyzer/   # Tree-sitter changed-symbol 提取
├── brain-core/       # 协议、规则验证、确定性决策
├── brain-scip/       # 离线 SCIP protobuf、项目 profile 与语义快照
├── brain-store/      # SQLite schema 与审计
├── brain-symbols/    # Provider-neutral 符号、边、快照与身份协议
└── project-brain/    # CLI、Git、Codex Hook 适配
```

进一步设计见：

- [架构说明](docs/architecture.md)
- [协议说明](docs/protocol.md)

## 当前限制

- 当前只提供 Codex 适配器；Claude Code 和 Prime Agent 尚未实现。
- shell 命令只做保守的显式危险模式识别，不承诺成为完整 shell 安全沙箱。
- changed-symbol 与内置 Tree-sitter syntax Provider 当前只支持 Rust；.NET/Python 通过显式配置的
  离线 SCIP semantic Provider 接入。
- SCIP 当前可靠导入 definition、reference、contains，以及 producer 明确提供的 implementation/
  type-definition 关系；不会从 occurrence 猜测 call/import/implementation。
- scip-dotnet 与 scip-python 使用合成契约 fixture；本阶段不捆绑或自动运行外部 producer。
- syntax fallback 不自动关联 rename/move lineage；这必须由语义证据或显式确认完成。
- semantic lineage 当前只支持同项目、同 provider profile/contract、同语言、相邻快照的一对一
  predecessor/successor；split/merge、跨 provider equivalence 和传递闭包不在本阶段。
- `Stop` 自动对账只核对文件范围；符号级 Change Envelope 约束尚未加入。
- Semantic Sentinel / Architecture Judge 尚未加入；这是有意的 V0 边界。
