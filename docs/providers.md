# Provider 与 Evidence

Project Brain 核心保持语言和框架中立。语言语义通过 Semantic Provider 接入；构建、测试、运行或框架
特有事实通过通用 Evidence Provider 接入。

## 1. Semantic Provider

内置 Rust 语法分析只提供 `syntax_fallback` 身份。跨语言语义索引使用显式 SCIP profile：

```text
project-brain provider bind \
  --profile rust-main \
  --executable /absolute/path/to/provider \
  --trust-local-executable

project-brain provider index --profile rust-main
project-brain provider coverage --require-indexed
```

Provider executable 与可选 entrypoint 都固定 SHA-256，机器级绑定不写入仓库。只有完整覆盖结果可以提交
为 semantic snapshot。符号 rename/move lineage 必须人工确认。

SCIP 导入只接受 Provider 明确给出的 definition、reference、contains，以及明确提供的
implementation/type-definition；核心不会从 occurrence 猜测调用或依赖关系。

## 2. 通用 Evidence Provider

框架或工具链特有能力使用 `Provider Process Protocol v1`，通过 JSON stdin/stdout 独立进程接入。核心只
理解通用 Evidence plane、ArtifactGraph、finding、输入依赖和 freshness，不理解 Provider 私有 payload。

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

Provider 绑定按 `project_key + profile_id` 隔离，并固定 descriptor、executable SHA-256、authority ceiling
和 revision。

## 3. Provider 运行边界

一次 Evidence Provider 运行遵循以下顺序：

1. 核心解析并复验 `InputDependencyContractV1`；
2. 在同一 Source 状态上生成不可变 input manifest；
3. 只把声明输入复制到机器私有 staging；
4. 向固定 executable 发送 `ProviderRunRequestV1`；
5. 校验响应身份、版本、plane、payload hash、输入 manifest 和 Source TOCTOU；
6. 核心重新构造 `EvidenceSnapshot` 并事务化提交。

Provider 只能声明 `deterministic_violation_claim`。最终 finding authority 受机器绑定的
`authority_ceiling` 限制，且仍需仓库规则显式映射才能阻断。

协议 crate 位于 `crates/brain-provider-protocol`；
[reference_provider.rs](../crates/brain-provider-protocol/examples/reference_provider.rs) 是可运行的最小参考
实现。精确 JSON 合同见 [协议说明](protocol.md)。

## 4. Evidence 输入与精准 freshness

每个新 Evidence head 可以携带：

- `InputDependencyContractV1`：profile、Provider 合同、精确文件或有限 glob tree selector；
- `EvidenceInputManifestV1`：解析时存在/缺失状态、内容 SHA-256、大小和规范 hash；
- `DependencyCoverage`：`complete`、`conservative` 或 `incomplete`。

明确的工具前/工具后 Source baseline 会计算精确路径 delta。只有输入合同受影响的 head 才会标记 stale；
无法证明 delta 时进入 `verification_unknown` 并保守降权。实时查询仍会重算声明输入，不能只相信持久化
stale 标记。

```text
project-brain evidence status
project-brain evidence inputs show --plane build --provider <provider-id>
```

## 5. 内置 Build/Test Evidence

当前保留 provider-neutral 的固定工具链合同：

- `.NET`：固定项目、配置、无隐式 restore；
- Rust：workspace、all-targets、frozen；
- Python：isolated compile，不 import 或执行项目模块；
- `.NET`、Rust、Python 的固定测试合同。

这些合同用于 Project Brain 自身与通用项目验证。新增 Godot 或其它框架、引擎、IDE 专用逻辑应实现为
外部 Evidence Provider，而不是加入核心条件分支。
