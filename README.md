# Project Brain

Project Brain 是面向 Coding Agent 的确定性项目决策与长期记忆控制面。它把仓库规则、实际工具动作、源码状态、语义索引和可验证证据组合成可审计决策；Agent 不需要“想起来”调用记忆工具。

核心在没有 LLM 时完整工作。LLM 可以作为未来的低权限语义 Provider，但不能自行获得阻断权限。

## 当前交付边界

正式支持四个 Agent 接入：

| Agent | 接入方式 | 工具前阻断 | 工具后反馈 | Stop 续轮 |
|---|---|---:|---:|---:|
| Codex | 用户级 `hooks.json` | 支持 | 支持 | 支持 |
| Pi | 用户级 Extension | 支持 | 支持 | 模拟（最多一次） |
| opencode | 用户级 Plugin | 支持 | 支持 | 不支持 |
| dsh | 显式 profile Plugin bundle | 支持 | 支持 | 支持 |

能力不对称是协议事实。`project-brain capabilities <agent>` 会输出已确认能力；不支持的能力不会被模拟成支持。

Project Brain 核心不包含任何特定游戏引擎、IDE 或应用框架逻辑。框架特有的分析、构建、测试和运行能力应通过独立进程 Evidence Provider 接入。

## 安全模型

- 相同输入、配置与 schema 产生相同规则决策。
- `block` 只有在 `strength=hard` 且 authority 为 `explicit_user`、`repository_rule` 或 `accepted_decision` 时合法。
- `agent_inference`、`observed_pattern` 和启发式 Provider 只能提供上下文或升级处理。
- 未知协议、损坏配置、安装漂移、Provider executable 漂移和证据身份不匹配均失败关闭。
- 所有审计、幂等键、符号、Evidence 和 Provider 绑定都以 `project_key` 隔离。
- 外部进程通过统一 containment runner 执行；它不是通用 OS 沙箱，绑定本地 Provider 必须显式信任。

## 构建

要求 Rust 1.92：

```text
cargo build --release --locked -p project-brain
```

开发门禁：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

公开仓库的 GitHub Actions 会在 Linux、Windows、macOS Intel 和 Apple Silicon 上运行测试，并构建 release CLI。推送版本标签 `vX.Y.Z` 后，Release workflow 会生成四平台压缩包和 `SHA256SUMS`。

## 最小使用流程

安装当前二进制到机器级稳定 launcher：

```text
project-brain install
```

在仓库中初始化。语言 profile 仅声明项目事实，不会自动下载工具：

```text
project-brain init --profile rust
project-brain init --profile dotnet --profile python
```

注册当前项目：

```text
project-brain bootstrap
```

安装所需 Agent 接入：

```text
project-brain install-hooks codex
project-brain install-hooks pi
project-brain install-hooks opencode
project-brain --dsh-profile default install-hooks dsh
```

检查：

```text
project-brain doctor codex
project-brain doctor pi
project-brain doctor opencode
project-brain --dsh-profile default doctor dsh
```

卸载只删除 Project Brain 管理且哈希匹配的片段；检测到漂移时默认拒绝。只有人工确认后才使用 `--force`：

```text
project-brain uninstall-hooks pi
project-brain uninstall-hooks opencode
project-brain --dsh-profile default uninstall-hooks dsh
```

可用全局参数显式覆盖机器配置根：`--codex-home`、`--pi-home`、`--opencode-home`、`--dsh-home` 和 `--install-root`。

## Agent 接入位置

- Codex：`$CODEX_HOME/hooks.json`，省略时为用户级 Codex 配置根。Project Brain 为五个生命周期事件追加自身拥有并逐项哈希的 group，不覆盖用户 handler。这不是 Codex 企业 managed hook，仍需 Codex 自身的 hook trust。
- Pi：`$PI_CODING_AGENT_DIR/extensions/project-brain/index.ts`，省略时为 `~/.pi/agent/extensions/...`。Pi 没有正式的停止前 veto 边界；`agent_end` 后的 follow-up 只作为最多一次的 emulated continuation，并明确区别于 Codex/dsh 的正式停止入口。
- opencode：`$OPENCODE_CONFIG_DIR/plugins/project-brain.js`，省略时为 `~/.config/opencode/plugins/...`。
- dsh：通过官方 `dsh plugin --profile <name> add/remove` 管理指定 profile；不修改其它 profile。

四个接入都调用稳定 launcher 的 `dispatch` 入口。未注册项目返回 NO-OP；已注册项目发生治理或审计错误时，工具前事件失败关闭。

## 决策规则

仓库规则位于 `.project-brain/config.json`。示例：

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

决策状态为 `allow`、`allow_with_context`、`escalate`、`block`，优先级依次提高。Hook 输出再映射到各 Agent 的原生协议。

## 语义 Provider

内置 Rust 语法分析只提供 `syntax_fallback` 身份。跨语言语义索引使用显式 SCIP profile：

```text
project-brain provider bind \
  --profile rust-main \
  --executable /absolute/path/to/provider \
  --trust-local-executable

project-brain provider index --profile rust-main
project-brain provider coverage --require-indexed
```

Provider executable 与可选 entrypoint 都固定 SHA-256；机器级绑定不写入仓库。只有完整覆盖结果可以提交为 semantic snapshot。符号 rename/move lineage 必须人工确认。

## 通用 Evidence Provider

框架或工具链特有能力使用 `Provider Process Protocol v1`，通过 JSON stdin/stdout 独立进程接入。核心只理解通用 Evidence plane、ArtifactGraph、finding、输入依赖和 freshness，不理解 Provider 私有 payload。

绑定：

```text
project-brain evidence provider bind \
  --profile app-build \
  --executable /absolute/path/to/evidence-provider \
  --authority-ceiling heuristic \
  --trust-local-executable
```

运行：

```text
project-brain evidence provider run \
  --profile app-build \
  --plane build \
  --contract .project-brain/providers/app-build.inputs.json \
  --config .project-brain/providers/app-build.config.json
```

查看或解绑：

```text
project-brain evidence provider list
project-brain evidence provider unbind --profile app-build
```

Provider 绑定按 `project_key + profile_id` 隔离，并固定 descriptor、executable SHA-256、authority ceiling 和 revision。运行时：

1. 核心解析并复验 `InputDependencyContractV1`；
2. 在同一 Source 状态上生成不可变 input manifest；
3. 只把声明输入复制到机器私有 staging；
4. 向固定 executable 发送 `ProviderRunRequestV1`；
5. 校验响应身份、版本、plane、payload hash、输入 manifest 和 Source TOCTOU；
6. 核心重新构造 `EvidenceSnapshot` 并事务化提交。

Provider 只能声明 `deterministic_violation_claim`。最终 finding authority 受机器绑定的 `authority_ceiling` 限制，且仍需仓库规则显式映射才能阻断。

协议 crate 位于 `crates/brain-provider-protocol`；`examples/reference_provider.rs` 是可运行的最小参考实现。

## Evidence 输入与精准 freshness

每个新 Evidence head 可以携带：

- `InputDependencyContractV1`：profile、Provider 合同、精确文件或有限 glob tree selector；
- `EvidenceInputManifestV1`：解析时存在/缺失状态、内容 SHA-256、大小和规范 hash；
- `DependencyCoverage`：`complete`、`conservative` 或 `incomplete`。

明确的工具前/工具后 Source baseline 会计算精确路径 delta。只有输入合同受影响的 head 才会标记 stale；无法证明 delta 时进入 `verification_unknown` 并保守降权。实时查询仍重算声明输入，不能只相信持久化 stale 标记。

```text
project-brain evidence status
project-brain evidence inputs show --plane build --provider <provider-id>
```

## 内置 Build/Test Evidence

当前保留 provider-neutral 的固定工具链合同：

- `.NET`：固定项目、配置、无隐式 restore；
- Rust：workspace/all-targets/frozen；
- Python：isolated compile，不 import 或执行项目模块；
- `.NET`、Rust、Python 的固定测试合同。

这些内置合同用于 Project Brain 自身与通用项目验证。新增框架专用逻辑应走外部 Evidence Provider，而不是加入核心分支。

## 数据库与审计

本地 `.project-brain/brain.db` 是项目级审计与派生状态。常用命令：

```text
project-brain audit --limit 20
project-brain database status
project-brain database verify
project-brain database compact --output /absolute/path/to/brain.compacted.db
```

数据库迁移严格前向、未知版本拒绝。大型 lineage 清理必须经过预览、显式批准、项目外 SQLite Online Backup 和重放验证，不会由 Hook 自动执行。

## Production Qualification

Qualification 在机器级隔离状态中验证当前二进制与控制面合同，不写项目 `brain.db`：

```text
project-brain qualification run --request-id local-qualification-001
project-brain qualification status
project-brain doctor codex --require-qualified
```

Release workflow 会在每个平台执行同一套 qualification。

## 明确不做

- 不把聊天记录检索当作治理依据；
- 不让 Agent 或 Provider 自行授予 hard authority；
- 不自动下载、安装或发现外部语义/Evidence Provider；
- 不把本地执行 containment 宣称为完整网络、内核或容器沙箱；
- 不从 occurrence 猜测未被 Provider 明确证明的 call/import/implementation 边；
- 不自动确认跨 rename/move 的 semantic lineage；
- 不为能力矩阵对称而伪造 Agent 不支持的生命周期行为。

架构与协议细节见 [docs/architecture.md](docs/architecture.md) 和 [docs/protocol.md](docs/protocol.md)。
