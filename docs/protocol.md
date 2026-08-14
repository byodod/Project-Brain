# 协议说明

## 规则 Schema 版本

所有跨边界对象必须包含：

```json
{
  "schema_version": 1
}
```

当前 Runtime 对未知版本 fail closed，避免把新字段静默解释成旧语义。

`ActionDescriptor` 是 preflight/规则引擎的兼容输入，不再作为 Agent adapter 的公共边界。

## Internal Hook Protocol v1

所有 Agent adapter 先转换成内部强类型事件，再调用确定性内核。信封示例：

```json
{
  "protocol_version": 1,
  "project_key": "pb_0123456789abcdef0123456789abcdef",
  "event_id": "codex_event_<sha256>",
  "idempotency": {
    "identity_quality": "vendor_stable"
  },
  "adapter": {
    "kind": "codex",
    "adapter_version": 1
  },
  "session_key": "agent-session-id",
  "cwd": "D:/repo",
  "turn_key": "turn-id",
  "payload": {
    "event": "tool_about_to_run",
    "data": {
      "operation_id": "codex_operation_<sha256>",
      "tool_name": "apply_patch",
      "action": {
        "kind": "modify",
        "target_files": ["src/domain/order.rs"]
      }
    }
  }
}
```

公共事件只有：

```text
SessionOpened
IntentDeclared
ToolAboutToRun
ToolFinished
TaskStopping
```

`project_key` 由项目配置持久化，是事件、幂等键和审计查询的项目边界。`cwd` 只是本次
delivery 的位置证据，不能代替项目身份。旧配置首次打开时会生成并写回 `project_key`；
迁移键只由旧配置稳定内容派生，不依赖 checkout 绝对路径；同一份受版本控制配置在移动或
clone 后仍保持项目身份。新项目初始化时直接生成并持久化独立 key。

`event_id` 是 delivery 身份，`operation_id` 是一次工具调用的因果身份。Pre/Post 可以交错，
Runtime 不建立全局顺序，也不假设最后一个 Pre 必然对应下一个 Post。
Codex operation ID 的派生域包含 `project_key`、规范化 `session_key` 与 vendor tool ID，
不能跨项目或会话复用。

`identity_quality` 明确 adapter 能提供的重放保证：

- `vendor_stable`：vendor 提供稳定调用 ID；
- `derived_stable`：从稳定 turn 等字段派生；
- `per_delivery`：vendor 没有稳定键，每次 delivery 只能生成新 ID，不能声称跨进程去重。

SQLite 以 `(project_key, adapter_kind, event_id)` 唯一约束处理 at-least-once 重放；重复事件
返回首次持久化 outcome。不同项目即使 vendor session/event ID 完全相同也不会串审计。
关键 gate 的治理计算或审计写入失败时，Codex `PreToolUse` 显式 deny；`Stop` 显式要求继续，
但 `stop_hook_active=true` 时仍直接放行以避免自触发循环。不能依赖 hook 进程异常退出实现阻断。

## Internal Hook Outcome

Outcome 与事件一一对应，不存在通用 `block`：

```text
SessionOpened    -> inject
IntentDeclared  -> NoVeto | Deny + inject
ToolAboutToRun  -> NoVeto | Deny + inject
ToolFinished    -> post feedback
TaskStopping    -> AllowStop | ContinueWork + feedback
```

`NoVeto` 只表示 Project Brain 没有治理异议。Adapter 不得把它映射成 vendor 的显式权限批准；
例如 Codex PreToolUse 的 `NoVeto` 输出空对象，让 Codex 自己继续正常权限流程。

## ActionDescriptor

```json
{
  "schema_version": 1,
  "event_id": "tool-call-id",
  "session_id": "agent-session-id",
  "cwd": "D:/repo",
  "action": "modify",
  "operation": "apply_patch",
  "target_files": ["src/domain/order.rs"],
  "command": null,
  "metadata": {}
}
```

`action` 当前可取：

```text
read
create
modify
delete
execute
dependency_change
git_operation
unknown
```

## Decision

```json
{
  "schema_version": 1,
  "decision": "block",
  "summary": "命中确定性硬规则，拒绝执行",
  "evidence": [
    {
      "rule_id": "PB-CORE-001",
      "effect": "block",
      "message": "禁止删除项目规则配置",
      "rationale": "规则控制面必须显式修订"
    }
  ]
}
```

决策优先级固定为：

```text
block > escalate > allow_with_context > allow
```

这是聚合优先级，不是规则 authority 的自动冲突解决。未来出现相互冲突的有效规则时，应显式生成 conflict/elevation，而不是暗中选择一条。

## Adapter 责任

Adapter 只负责：

1. 把外部 Hook 输入转换为 `InternalHookEvent`；
2. 生成非空 event/session/operation 身份并如实标注幂等质量；
3. 调用确定性内核；
4. 把事件专属 `InternalHookOutcome` 转换回外部协议；
5. 记录按项目隔离的 adapter、延迟、outcome 和 failure 审计。

Adapter 不得自行重新解释某条项目规则。

当前 Codex 与 Claude Code adapter 都覆盖 `SessionStart`、`UserPromptSubmit`、`PreToolUse`、
`PostToolUse` 和 `Stop`。两者共享已确认的 vendor 字段子集和 outcome 映射，但必须使用不同的
adapter identity、event ID namespace 与 operation ID namespace，不能跨 vendor 去重。能力矩阵通过
`project-brain capabilities codex` 和 `project-brain capabilities claude-code` 输出。能力模型明确保留 Prime Agent
的 `continue_after_stop=unsupported`，不把独立 runtime 的 `agent_end` 假装成 Codex Stop。
当前 `IntentDeclared` 只进入审计，尚未接入独立的意图规则模型，因此 Codex 有效能力如实报告
`deny_intent=unsupported`；核心协议保留 `Deny` 类型供后续 adapter/rule 实现使用。
`PostToolUse.tool_response` 只有存在可识别的 success、exit code、error 或 status 证据时才映射
为 succeeded/failed，否则记录 unknown，不从事件名称猜测成功。
明确 Create/Modify/Delete 始终失效全部 Source-bound Evidence，因为失败工具仍可能部分写入。Execute、
GitOperation 与未知工具结束后不解析命令文本，而是把当前 Git Source 指纹逐一与 non-stale heads 的
`source_fingerprint` 对账：只把不一致者记录为 `stale`；Git/文件系统无法验证时把 fresh 记录为
`unknown`，已 stale 不会被覆盖，已 unknown 且重新匹配也不会自动 fresh。事件审计保存实际转换的
plane/provider/snapshot 身份及观察到的 Source 指纹。

Claude Code adapter v1 提供直接 `hook/dispatch` 协议入口和用户级 `settings.json` 安装器。
安装器使用独立 manifest、精确 handler hash 与原子替换；只管理五个已实现事件。
`SubagentStart` 与 `SessionEnd` 不在这一阶段；未实现的 lifecycle 不会被折叠成现有五类事件。
handler 使用 `command` 指向稳定 launcher、`args` 保存三个独立参数的 exec form；不得通过 shell
字符串拼接 launcher 路径或生命周期参数。固定 `statusMessage` 用作托管签名的一部分，实际路径与
完整 handler 仍由 manifest hash 校验。
`doctor [codex|claude-code]` 选择对应的配置、manifest 和 handler hash 域；未给参数时为兼容旧调用
默认检查 Codex。Doctor v2 使用通用 adapter 字段，不把 Claude 状态伪装成 Codex 状态。

Prime Agent direct adapter v1 通过 `project-brain hook/dispatch prime-agent <event>` 暴露 Rust
控制面。Extension 应把正式 runtime event 映射到同一内部事件语义，但输出使用独立 schema：
pre-tool 返回 `block/reason/context`，post-tool 返回 `feedback`，停止阶段返回带
`supported=false` 的 continuation 描述。Project Brain 不因内部规则想继续而伪造 Prime 已支持
settled continuation。

## Evidence Protocol v1

`brain-evidence` 将 `source/semantic/engine/build/test/runtime` 建模为独立 Evidence Plane。快照包含
项目与 provider 身份、source fingerprint、独立 snapshot fingerprint、coverage、显式 upstream
引用、ArtifactGraph 与 findings。下游只在当前源码和全部 upstream fingerprint 一致时为 fresh；
缺少当前证据为 unknown，任一指纹不同为 stale。
SQLite v16 的 invalidation event 显式保存目标 freshness、观察到的 Source 指纹和实际转换的 head
身份，确保“已证明漂移”的 stale 与“无法验证”的 unknown 不共享幂等身份；两者均不具备 hard-block
资格。

`evidence_heads.freshness` 是 persisted freshness，只回答 ledger 最后一次已知状态。任何当前权限消费都
必须把它与现场 `git::worktree_fingerprint` 合成为 effective freshness：只有 persisted fresh、当前指纹
可验证且与 snapshot Source 指纹相同，才是 effective fresh。指纹不一致为 effective stale；无法取得为
effective unknown。自动路径只允许 fresh→stale、fresh→unknown、unknown→stale，不允许 stale/unknown
因源码再次相同而恢复 fresh。

Artifact ID 绑定 `project_key + provider_id + provider_key`，边的两端必须同时存在于本快照。
只有 deterministic provider 产生的 complete、fresh、`deterministic_violation` error finding，在精确
命中 `finding_effect_mappings` 后才具有 hard-block 资格；未知 finding 与缺少 authority 的旧 finding
均为 advisory。资格仍不等于自动阻断，最终必须继续经过规则 authority/strength/effect 判定。

仓库映射必须精确声明，不支持通配或“所有 error”规则：

```json
{
  "id": "TEST-SAVE-001",
  "status": "active",
  "authority": "repository_rule",
  "strength": "hard",
  "effect": "block",
  "plane": "test",
  "provider_id": "dotnet-test.game-debug",
  "provider_contract_version": 1,
  "finding_code": "save_roundtrip_assertion_failed",
  "message": "存档往返断言失败，必须继续修复"
}
```

Stop 只读取当前项目相同 plane/provider 的 head，并再次核对 contract version、effective freshness、
当前 Source 指纹、coverage、Provider authority 与 finding authority。缺少 head、合同漂移、Source
不可验证/不一致、stale/unknown、partial、heuristic、warning、advisory finding 或未知 code 均不会产生
隐式 Block。

.NET Test run schema v1 使用 `dotnet-test.<profile>` provider。它只接受指定
`dotnet-build.<build_profile>` 当前 head，并要求该 Build 为 effective-fresh、Source 匹配、complete、deterministic、无
finding；CAS manifest 的 project/provider/source/build_target、测试程序集条目与 dotnet executable
SHA-256 必须精确匹配。实际 argv 固定为 `dotnet vstest <bundle assembly> --Logger:trx
--ResultsDirectory:<scratch> --nologo`，不读取仓库 runner 参数。

TRX 汇总的 `status` 是 passed/failed/crashed/timed_out/no_tests/provider_failed，`coverage` 是
covered/partial/empty/unknown。存在未执行测试时为 partial。Test Evidence 的通用 coverage 仍表示
Provider 合同观测是否完整：NoTests 可为 complete + empty，但 partial、TimedOut/ProviderFailed 不具备
hard-block 资格。v1 无法从 TRX Counters 证明失败一定是声明的 assertion，因此 failure finding 默认为
advisory；这刻意阻止“所有测试 error 自动 block”。

Rust Test run schema v1 使用 `cargo-test.<profile>` provider。它只接受指定
`cargo-build.<build_profile>` 当前 head，并要求该 Build 为 effective-fresh、Source 匹配、complete、deterministic、无 finding；
Source fingerprint、Build Snapshot 的规范 `build_target` artifact 与 cargo executable SHA-256 必须
分别匹配当前工作树、当前 `Cargo.toml` 和本次工具链。

实际 argv 固定为 `cargo test --manifest-path <PROJECT_ROOT>/Cargo.toml --workspace --all-targets
--frozen --target-dir <scratch>/target`，环境固定 Cargo offline、incremental off；仓库不能声明 package、
feature、filter、runner、shell、env 或网络。多个稳定版 libtest `test result:` 摘要在有界 UTF-8 输出内
聚合。无测试为 complete + empty；timeout/输出截断/缺少完整摘要为 partial 或 provider failure。普通
`rust_test_failed` 只能是 advisory，因为文本 v1 不能证明失败一定来自声明断言。

Python Test run schema v1 使用 `python-test.<profile>` provider。它只接受指定
`python-compile.<build_profile>` 当前 head，并要求 Build 为 effective-fresh、Source 匹配、complete、deterministic、无 finding；
Source fingerprint、规范 `build_target=source_root` 与 Python executable SHA-256 必须一致。仓库 manifest
只接受 schema_version 与有界、无重复、按顺序的 ASCII `module/function`，module 必须唯一映射到
source_root 内 Git Source 的 Python 文件。

Provider 物理复制 Git Source，固定执行 isolated/no-site/no-bytecode UTF-8 adapter bootstrap；不接受 pytest、
discovery、plugin、pip、repo runner、shell、args、env 或 install。bootstrap 只调用同步零参数模块自有函数，
并输出无消息的 adapter-owned 结构化状态。合法 `AssertionError` 为 deterministic violation；其他 exception、
runner failure、截断、超时和非法结果为 advisory。任何 finding 仍必须精确命中显式 mapping 才能产生治理
effect。

Godot Scenario Test schema v1 使用 `godot-scenario-test.<profile>` provider。它只接受与当前 Source、
`build_target` 和主程序集绑定一致的 `dotnet-build.<build_profile>` CAS，并要求 Build upstream 恰好含
一个 effective-fresh、Source 匹配、complete、deterministic、无 finding 且 executable SHA-256 匹配的 Engine head。Source
从 Git manifest 物理复制；Build bundle 固定物化到 staged Godot Debug 输出目录。import 与场景 argv
由 adapter 构造，不允许 repository args、shell、script、build、restore 或 export。

仓库场景必须生成 `.project-brain-test-result-v1.json`。结果只接受 schema_version、scenario_id、status
和有界 assertions；scenario_id 必须等于 Test profile，status 必须与全部 assertion 布尔值一致。合法
失败断言产生 `godot_scenario_assertion_failed` 且 authority 为 deterministic_violation；缺失/非法结果、
import 或 runtime diagnostics、进程崩溃、超时、输出截断都不能冒充断言违规。Source/CAS/executable
TOCTOU 校验失败则整次结果不提交。与所有 finding 相同，deterministic_violation 仍需仓库 hard rule 对
plane/provider/contract/code 的精确显式映射才能阻断。

Godot probe schema v1 返回 `before/after` 两份 `ProbeProjectState`。每份状态包含
`project_sha256`、main scene、autoloads，以及所有 `.tscn/.tres` 的 UID、SHA-256、load result 与
`ResourceLoader.get_dependencies()` 解析结果。转换层会规范排序并忽略 before/after 的 `loaded`
差异；其余字段必须完全一致，且 after 哈希必须与 Rust 再次读取的项目文件一致。

Godot Engine Snapshot 使用固定 provider ID `godot-engine-resolver`；provider version 同时携带实际
Godot version 与 executable SHA-256。Engine 导出的 diagnostics、load failure、missing dependency、
unresolved UID 和 cache reference 都成为明确 finding，而不是依赖自由文本猜测。

Build run schema v1 的 provider contract v2 只允许内置 adapter 构造固定 argv：`dotnet-build`、`cargo-build` 与
`python-compile`。provider ID 为 `adapter + profile_id`，provider version 绑定真实工具版本和
executable SHA-256。`.NET` 与 Rust 的 execution class 是 `repository_build_code`；Python 是
`compiler_only`，输出类型为 `validation_only`。RepositoryBuildCode 必须取得独立的显式信任位，
不能从 executable 信任推导。

contract v2 显式记录 working-directory policy：`.NET` 的版本探测和 build 都以项目根为工作目录，
保证 `global.json` 约束被探测与实际构建共同消费；Cargo/Python 继续以 machine scratch 为工作目录。
所有 `.NET` bin/obj 输出仍被固定重定向到 scratch，进程前后 worktree 指纹漂移仍使证据失效。

Build Snapshot 的 `coverage` 描述观测是否完整，而不是进程是否成功。固定合同完整执行但返回非零时，
保存 `complete + build_exit_failure(error)`；输出被截断、链接器/SDK/离线依赖或预还原状态不可用时，
保存 `partial + build_unavailable(warning)`。成功的 ArtifactSet 合同没有普通产物时保存
`required_artifact_missing(error)`。CLI 在这些情况下仍原子保存 Evidence 后返回非零。下游 Runtime
必须检查 Build findings，而不能只检查 complete/fresh。Godot C# 的 Build Snapshot 还必须通过
显式 `EvidenceReference` 固定其 Engine upstream。`.NET` manifest 只覆盖最终 bin output；包含
scratch 绝对路径的 obj cache 是执行中间态，不进入 artifact identity。所有 Build Snapshot 另含规范
`build_target` artifact，供下游 Test 精确绑定项目入口；旧 snapshot 缺少该 artifact 时必须重跑 Build。

成功的 Godot C# Build 在 scratch 回收前创建 `RuntimeArtifactBundle v1`。bundle 的规范 JSON 包含
`project_key`、Build provider、Source fingerprint、ArtifactManifest fingerprint、排序后的全部最终
文件 `(relative_path,size,sha256)`，以及从 `project.godot` 明确解析出的主程序集路径与 SHA-256。
每个文件以 SHA-256 为 key 原子写入机器级 CAS；同名 object 或 manifest 若内容不符即视为损坏并
fail closed。Build Evidence 中的 `runtime_artifact_bundle` 节点绑定完整规范 JSON 字节，不记录 CAS
绝对路径或把当前可用性写进不可变事实。

CAS 的 Present/Evicted/Corrupt 是机器运行状态，不改写历史 Build Evidence。Runtime 必须在准备时
重新校验 bundle manifest 与全部 object；缺失、逐出或损坏只会拒绝本次 Runtime 准备，不会自动
重建并冒充原 Build。v1 Runtime 禁止 restore、build、test、script 与全部 export/release 路径。
CAS 提升失败时仍保存已完成的 Build 观测，但状态为 `incomplete` 并加入
`runtime_bundle_unavailable(warning)`；错误文本只以 fingerprint 进入 Evidence，避免把机器绝对路径
写入不可变快照。该 Build 不能供 Runtime 使用。

Godot Runtime run schema v1 要求指定 bundle 必须由当前项目同 provider 的 effective-fresh、Source 匹配、complete、
deterministic Build head 直接绑定，Build 不得含 finding，Source fingerprint 必须与当前 worktree
相同，Build 引用的 Engine head 也必须仍为 current+effective-fresh，且 Godot executable SHA-256 与该 Engine
证明一致。任一准备条件失败只拒绝本次 run，不伪造 Runtime Snapshot。

通过准备后，Runtime 以 Git `ls-files --cached --others --exclude-standard` 建立 staged Source manifest；
排除控制面和旧生成目录，拒绝全部 link/reparse component，并执行 live Source A → 物理复制 → live
Source B 的 TOCTOU 检查。之后从 CAS 物理复制 bundle 到 Godot 固定程序集目录；import 前、import 后、
主场景运行前、主场景运行后都要求目录内文件集合、大小和 SHA-256 与 bundle 完全一致。

固定 import argv 只有 `--headless --no-header --path <STAGED_PROJECT> --import --log-file
<RUNTIME_LOG>`；固定 runtime argv 只增加 `--quit-after <bounded>`，不接受自定义 scene 或用户参数。
所有路径在 Evidence contract 中规范化，不保存 machine run root。诊断文本只以 fingerprint 进入不可变
Evidence，原始日志保留在 machine-private run 目录。每个目录包含 project-bound marker 与原子 journal；
自动清理在精确 DB/marker/root 匹配的恢复合同完成前保持禁用。

SQLite schema v16 为 Evidence Protocol 维护四类项目隔离记录：

- `evidence_snapshots`：不可变完整快照；相同 project/plane/provider/fingerprint 只保存一次 JSON；
- `evidence_attestations`：每次真实 Provider 运行的轻量 append-only 证明；
- `evidence_heads`：每个 project/plane/provider 的当前 fingerprint 与 fresh/stale/unknown persisted 状态；
- `evidence_staleness_events`：以 project + event ID 幂等记录目标状态、规范 plane 集合、精确 head 身份、
  Source 观察和路径辅助证据。

生产 CLI 在成功 Provider run 后重新计算当前 Git Source 指纹；若与结果不一致，任何 snapshot、
attestation 或 head 提升都不会发生。验证一致后，单个事务会追加 snapshot/attestation、移动 head，并
把同项目其它不同 Source 指纹的 fresh heads 标 stale，再用当前 upstream heads 验证所有显式引用。缺失引用
得到 unknown；fingerprint 不一致或上游 stale 得到 stale。上游 head 变化会沿显式引用传递到下游，
但上游恢复不会自动恢复旧下游；每个 Provider 必须真实重跑才能恢复自己的 head。三种 Agent adapter
共用的内部 `PostToolUse` 路径在观察到明确 Create/Modify/Delete 后，把现有 Source、Semantic、Engine、
Build、Test、Runtime heads 作为一个幂等事件原子标为 stale；不透明操作使用逐 head Source 对账。
Session、Intent、PreTool 与 Stop 注入 recorded/effective 双层状态，Finding hard gate 只接受 effective fresh。
失败或未知状态的修改工具也可能已产生部分写入，因此同样保守失效。

## AnalysisReport

`project-brain analyze` 输出版本化报告。每个受支持文件包含：

```json
{
  "path": "src/worker.rs",
  "language": "rust",
  "has_syntax_errors": false,
  "changed_ranges": [{ "start_line": 12, "end_line": 18 }],
  "changed_symbols": [
    {
      "name": "impl Worker::run",
      "kind": "function_item",
      "start_line": 12,
      "end_line": 18
    }
  ],
  "removed_symbols": []
}
```

行号为一基、闭区间。`has_syntax_errors` 不会阻止输出 Tree-sitter 可恢复的局部结果，
但调用方不得把存在语法错误的结果提升为强阻断事实。

## SymbolSnapshot

符号协议独立使用 `protocol_version`。Provider 必须声明身份质量：

```json
{
  "protocol_version": 2,
  "project_key": "pb_0123456789abcdef0123456789abcdef",
  "provider": {
    "id": "tree-sitter-rust-syntax",
    "version": "0.1.0+tree-sitter-rust-0.24.2",
    "identity_quality": "syntax_fallback"
  },
  "source_revision": "worktree_v3_<sha256>",
  "sources": [
    {
      "path": "src/lib.rs",
      "language": "rust",
      "content_fingerprint": "sha256_<digest>",
      "has_syntax_errors": false
    }
  ],
  "symbols": [],
  "edges": []
}
```

`SymbolNode.id` 是 `project_key`、Provider ID 与不歧义 `provider_key` 的摘要。它保证同一项目、
同一个 Provider 声明下可重复，不表示跨项目或跨 Provider 的全局真相。Provider ID 同时定义 `provider_key` 的
语义契约：破坏性 key 变更必须使用新 ID；兼容的实现或工具链升级只更新 version，
以保持已有符号身份。

身份质量：

- `syntax_fallback`：路径、声明种类、限定名与 occurrence 驱动；rename/move 后产生新 ID。
- `semantic`：由语言语义 Provider 给出；其跨版本保证必须由对应 Provider contract 定义。

`source_revision` 覆盖 `project_key`、HEAD（unborn 仓库使用显式 symbolic-ref 标记）、Provider、全部受支持
源文件的路径/语言/原始内容摘要/语法错误状态，以及节点和边。无符号文件的变化也必须改变 revision。

完整快照的规则：

1. 源文件路径必须规范化且唯一，摘要必须是完整 SHA-256；
2. 所有节点必须对应源文件清单中的路径；
3. 快照、所有节点与边必须属于同一个 `project_key` 和 Provider；
4. 边不得引用快照外节点；
5. 输入节点必须为 `active`；
6. 应用快照时，旧的 active 节点若消失则转为 `removed`；
7. 相同快照重复应用必须得到全量 `unchanged`；
8. 任何 rename/move lineage 都不能仅由 `syntax_fallback` 自动批准。
9. 查询、墓碑失效和边更新必须显式限定 `project_key`，不得依赖数据库文件路径作为唯一隔离。

## Project language 与 SCIP provider profile

`language_profiles` 声明开放的规范 language ID 及其项目相对根；`semantic_providers` 独立声明
数据格式、稳定 profile ID、实际 producer、Brain contract 版本和原始语言映射。两者分离，避免
把 producer 名称误当语言，也允许一个 provider profile 逐 Document 输出多种语言。

SCIP 导入遵循以下 fail-closed 规则：

1. CLI 必须通过 `--provider` 指定项目中已声明的 profile；
2. `tool_info.name` 必须与 profile producer 匹配；Rust 的实际 producer 是 `rust-analyzer`，
   `scip-rust` wrapper 不进入白名单；
3. `Document.language` 必须精确匹配显式 raw mapping；空值只匹配
   `raw_language=null + allow_missing_language=true`；
4. 映射后的 language 必须存在于 `language_profiles`，源码路径必须位于对应 roots；
5. Provider contract version 与 producer version 分离；后者仅作 provenance；
6. Provider 不从扩展名、项目文件、cwd、shebang 或观察到的单条关系猜测语言/能力；
7. global provider key 包含规范 language、原始 SCIP symbol、文档和 range；local symbol 额外包含
   index digest，禁止跨快照 lineage；
8. reference 只在唯一目标且有最小 enclosing definition 时建边；不推断 calls/imports/implements。
9. Provider ID 的可读规范名后附原始 profile contract 摘要，避免 `a-b`、`a_b`、`a.b` 等名称
   归一化后发生身份碰撞。
10. lineage 候选只在同一 project、provider 与 language 内比较；Git rename similarity 必须位于
    0..10000，且达到 5000 才能单独把候选提升为 high confidence。

## Semantic lineage ledger

Lineage 连接两个历史 observation，而不是合并或重命名 `SymbolNode`。持久化边界为：

- `semantic_snapshots`：按项目、provider profile/contract 排序的不可变导入事实；
- `semantic_snapshot_attestations`：同一快照在不同已验证 worktree 状态上的 append-only 来源证明；
- `semantic_source_manifests`：每个 v7+ 快照的完整 Document 清单计数与摘要；
- `semantic_source_observations`：清单内路径、language、内容摘要和语法状态；
- `semantic_symbol_observations`：某次快照实际看到的 symbol；
- `semantic_lineage_groups` / `semantic_lineage_group_members`：相邻快照的兼容等价类与成员集合；
- `semantic_lineage_generation_runs`：算法版本、group manifest、潜在 pair 与实际物化数；
- `semantic_lineage_candidates`：只有 1×1 自动生成或人工从 group 选择的 endpoint materialization；
- `semantic_lineage_materialization_requests`：人工选择 pair 的 request ID、payload hash 与 candidate 绑定；
- `semantic_lineage_evidence`：算法 ID、版本、输入摘要、结构化证据与置信度的 append-only 观察；
- `semantic_lineage_decisions`：显式用户裁决的 append-only 日志；
- `semantic_lineage_compaction_runs` / `semantic_lineage_compaction_groups`：V7 pair-first 旧账的
  幂等逻辑压缩审计、候选/证据 manifest hash 与目标 group；
- `semantic_provider_qualification_events`：显式多轮稳定性验证的 append-only 最终结论、机器绑定、
  源码指纹与整组 evidence manifest hash。

候选状态只有：

```text
proposed | confirmed | rejected | superseded | invalidated
```

V8 的 ambiguity 属于 `semantic_lineage_groups`；candidate 的旧 `ambiguity_group_id` 只保留 V7
历史语义。ambiguous 不属于 candidate 生命周期。允许
`rejected -> confirmed`，但必须是新的显式请求并保留两条 decision。禁止
`confirmed -> rejected`；纠错使用原子 `confirmed -> superseded` 加替代候选确认，结构性损坏才使用
`invalidated`。

硬不变量：

1. 只比较同 project、provider profile、provider contract、language 的相邻 semantic snapshot；
2. 只比较旧快照 removed 与新快照 inserted symbol；稳定 symbol ID 不产生 self-lineage；
3. 只有 1×1 group 自动产生 `proposed`；歧义 group 自动 pair 数恒为 0，人工选择后仍不确认；
4. 单侧超过 4096 members 的 group 为 `summary_only`，必须从 immutable snapshots 用同算法重算并
   验证成员摘要后才能物化；
5. 新快照和算法重跑不改变旧 candidate state；只可追加去重后的 evidence；
6. materialize/confirm/reject 只能来自带 `--human-confirmed` 的显式用户命令，必须携带 request ID；同 request 同 payload 重放首次结果，
   同 request 不同 payload 拒绝；
7. 一次裁决在单个事务内写 decision、执行 revision CAS、更新 materialized state；
8. 同 snapshot pair 的 confirmed predecessor/successor 都是一对一；split/merge 留待独立协议；
9. 不自动确认、拒绝竞争项、supersede、延伸传递 lineage、修改 symbol ID、恢复 tombstone、改写
   snapshot 或跨 provider 建 equivalence；
10. 已导入但不是当前最新的历史 snapshot 不能重新应用为当前符号图。
11. V7 逻辑压缩的 dry-run manifest 覆盖完整候选分类、受保护身份、精确 deletion set 摘要和目标 group；apply 必须
    在独占协作维护锁与 immediate 事务内重新计划，并与显式批准的 manifest hash 相等。任何计划漂移或
    decision/candidate 跨项目引用都 fail-closed，request ID 同时绑定 operation version 与批准 hash。
12. V7 逻辑压缩 apply 没有无备份模式。删除前必须用独立只读连接执行 SQLite Online Backup，把整个数据库
    保存到项目工作树外的机器级数据目录；备份的全库逻辑清单、quick check 与外键检查必须和持有
    `BEGIN IMMEDIATE` 的删除前事务一致。备份发布并复验成功前不得写 group、run、审计或执行删除。

SQLite schema v17 保存 semantic snapshots、source attestations、source manifests、symbol observations、
lineage groups/members/generation runs、candidate/evidence/decision 与 legacy compaction audit。旧快照迁移后的来源字段为空且默认为 `offline_import`，不会被提升
为硬证据，也不会从现存 symbol 反推缺失 Document。真实重跑相同 snapshot 时可以首次补录 manifest；
可信重跑只追加 attestation，不改写 symbol observations 或人工 lineage 状态。attestation 的唯一身份
包含 trust、registration、executable 与 artifact；相同 snapshot/worktree/HEAD 由新绑定重跑时仍会
追加新证明，读取以最新 sequence 为准，完全相同的证明才幂等去重。

V7 legacy compaction 默认是 dry-run。只有一个 group 的所有行仍为 `proposed`、每条恰有一份
`project-brain-lineage` version 1 evidence、没有 decision/related decision 引用，且按 kind 与 definition
fingerprint 重建后的实际 pair 集精确等于 from×to，才可进入 apply。任何缺行、附加证据、裁决或损坏
observation 都保护整个 group。apply 必须携带人工审核的 dry-run manifest hash，在独占协作维护锁和
immediate 事务内重新计划；计划漂移或跨项目引用直接拒绝。验证一致后先保存 group/member 与
删除前全库备份。备份使用独立只读连接的 SQLite Online Backup API，不 checkpoint、不按文件复制裸库，
也不执行 `VACUUM`。备份路径由 database instance ID、request ID、批准 manifest 和删除前逻辑清单确定，
位于 `<install-root>/state/backups/lineage-compaction/`；同名最终文件只允许在全库逻辑清单完全相同时
重用，禁止覆盖。备份通过 quick check、外键和全库逻辑清单复验后，才写入 group/member 与 append-only
compaction audit（包括 backup ID、相对路径、文件 SHA-256 和逻辑清单），再在同一事务删除对应
evidence/candidate。文件 SHA-256 只证明备份制品本身，不与 WAL 模式的源 `.db` 裸文件摘要比较。
schema v17 的存储触发器也拒绝任何缺少完整备份证明或前后逻辑清单不相等的 operation v2+ run，
防止绕过 Rust API 写入伪成功审计。
成功 request 重放前会重新验证原备份路径边界、文件 SHA-256、逻辑清单和完整性；缺失或漂移时
fail-closed。压缩后的 legacy group 不得重新物化。

物理文件维护使用独立的 Database Maintenance Protocol v1：`database stats` 以只读连接和一致性事务
读取当前 schema，不初始化或迁移数据库；`database compact` 的 dry-run 计算全库逻辑清单、文件摘要
和保守磁盘预算。apply 必须提供人工确认和 request ID，并依次通过独占协作锁、无 busy 的 WAL
`TRUNCATE` checkpoint、`VACUUM INTO` 候选、quick/integrity/foreign-key 检查、schema 与全部表内容清单
等价、源/目标文件 SHA-256 复核及同文件系统原子替换。默认备份源库。外部 JSON 操作日志使用
`running / verified / swapped / completed / failed` 状态；`verified` 表示候选及源未漂移验证均已通过，
`swapped` 仅表示原子替换已提交，`completed` 还要求 post-swap 重开和完整验证通过。除 `completed` 外都要求恢复并阻止普通
运行。同 request/同参数恢复或重放，同 request/不同参数拒绝。completed 重放重新验证当前数据库，
但把“仍是当时目标”和“之后合法演进”分开报告。Windows 临时占用有限重试，失败保持 `verified`；
操作日志保存替换前的原子临时文件基线，崩溃恢复仅清理本次新增的精确命名临时文件。磁盘预算显式
包含当前 WAL。该协议输出 `cooperative_only`，不把协作锁描述为通用 OS 沙箱；同时输出
`replacement_durability=temp_file_synced_atomic_replace;power_loss_directory_durability_platform_dependent`，
不把进程崩溃原子性描述为跨平台断电 write-through 保证。

覆盖率是独立的确定性证据：对 Provider profile 显式映射的 Rust/Python/C#/VB/F# language，比较
Git 已跟踪及未忽略、位于声明 roots 且扩展名属于该 language 契约的文件，与 SCIP Document 清单。
未知 language 必须报告 `unverifiable`，不得猜扩展名。已有快照的 `partial` 或与当前 worktree/HEAD
不一致的 `stale` 会使显式 `doctor` 降级；从未索引则只报告 `not_indexed` warning。

新导入只有 `complete` 才能进入 snapshot transaction。`partial` 与 `unverifiable` 在 store mutation
之前失败，不能更新 latest semantic snapshot。稳定性验证必须在相同源码指纹、Provider registration
revision 与 executable SHA-256 下重复运行，分别比较完整 Document path set 和完整 semantic snapshot
fingerprint；诊断重试不得把多次不完整输出取并集。

## Symbol scope 与证据等级

仓库规则通过 `symbol_scopes` 固定 provider profile/contract、language、anchor snapshot/symbol 和
`confirmed_lineage_only` 策略。解析只能是 `direct_semantic`、逐跳
`confirmed_lineage` 或 `unresolved`，不得从路径相似度、LLM 判断或候选置信度发明稳定身份。

硬规则除 authority/strength/effect 权限外还必须满足：definition 非 local、provider symbol 在该
snapshot 内唯一；最新来源是 `trusted_provider`；当前机器 binding 仍 ready 且 registration ID 与
executable SHA-256 匹配；worktree/HEAD 新鲜。`PreToolUse` 还要求 whole-file 或唯一 Edit range 的
确定性工具影响；`Stop` 则要求 clean `HEAD` baseline 与实际 Git hunk 相交。

证据等级为 `deterministic_path`、`semantic_direct`、`semantic_confirmed_lineage`、
`semantic_baseline_diff`、`advisory_syntax`、`inferred`、`unavailable`。offline SCIP、syntax fallback、
proposed/ambiguous lineage、Provider 缺失/失败/二进制漂移和过期快照只能 advisory fail-open，不能
伪装成 rule violation 或制造 Stop continuation 循环。
