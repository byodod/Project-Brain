# Project Brain

Project Brain 是一个独立于具体 Coding Agent 的项目决策控制面。它不依赖 Agent 主动“想起”记忆，而是在生命周期 Hook 中，根据仓库级规则和实际变更给出确定性决策。

当前版本包含：

- `ALLOW / ALLOW_WITH_CONTEXT / BLOCK / ESCALATE` 四态规则引擎；
- 带 authority、strength、scope 和 lifecycle 的版本化规则模型；
- Project-scoped Internal Hook Protocol v1；
- Codex 与 Claude Code `SessionStart`、`UserPromptSubmit`、`PreToolUse`、`PostToolUse`、`Stop` 协议适配；
- 按项目和 adapter 隔离、可重放的 SQLite 本地审计记录；
- Git Change Envelope 范围核对；
- Codex `Stop` 自动 Change Envelope 对账与防循环保护；
- 基于 Tree-sitter 的 Rust changed-symbol 与纯删除符号提取；
- Project-scoped Provider-neutral 符号身份协议、完整工作区快照与本地派生符号图；
- 按项目显式配置的 SCIP 导入与机器级安全 Runner，首批契约覆盖 rust-analyzer、scip-dotnet 与 scip-python；
- 开放 language ID、逐文档语言映射和四态语义能力声明；
- Project-scoped semantic lineage ledger、不可变证据与 append-only 显式裁决；
- SQLite schema v1→v13 迁移、按项目隔离的符号 removed 历史、Evidence ledger 与幂等增量更新；
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

正式标签会在 Linux x86-64、Windows x86-64、macOS Intel 与 Apple Silicon 上分别构建、
自检并打包，同时发布 `SHA256SUMS`。完整流程见 [docs/RELEASING.md](docs/RELEASING.md)。
源码以 MIT 或 Apache-2.0 双许可证发布；安全问题请按 [SECURITY.md](SECURITY.md) 私密报告。

## 机器级安装

生产使用先执行：

```text
project-brain install
```

它把当前版本安装为“稳定 launcher + 版本化 payload”。默认根目录为：

```text
Windows  %LOCALAPPDATA%\ProjectBrain
Linux    ${XDG_DATA_HOME:-~/.local/share}/project-brain
macOS    ~/Library/Application Support/ProjectBrain
```

Hook 永远调用稳定 launcher 的绝对路径，不依赖 `PATH`。`--install-root` 只用于便携安装、
测试或管理员部署。同一版本目录不可被不同二进制覆盖；新版本通过新版本号并排安装。需要
回退时执行：

```text
project-brain rollback
```

它只原子切换安装清单中的 `current / previous`，稳定 launcher 与 Codex Hook 定义保持不变。

## 快速开始

在目标仓库根目录执行：

```text
project-brain init
```

语言能力必须显式选择；Project Brain 不扫描项目文件或扩展名进行猜测。单语言项目例如：

```text
project-brain init --profile rust
project-brain init --profile dotnet
project-brain init --profile python
```

同一项目可组合多个 profile：

```text
project-brain init --profile dotnet --profile python --profile rust
```

`dotnet` 模板声明 `csharp` 与 `visual-basic`，并绑定 `scip-dotnet`；`rust` 绑定
`rust-analyzer`；`python` 绑定显式允许空 `Document.language` 的 `scip-python` 契约。
重复参数会被幂等去重。省略所有 `--profile` 仍只创建基础控制面，不引入任何语言假设。

仓库只声明 profile，不保存本机命令或绝对路径。每台机器需要对每个 profile 做一次显式信任绑定：

```text
project-brain provider bind \
  --profile rust-main \
  --executable /absolute/path/to/rust-analyzer \
  --trust-local-executable

project-brain provider bind \
  --profile dotnet-main \
  --executable /absolute/path/to/scip-dotnet \
  --trust-local-executable
```

Windows 的 npm `.cmd/.bat` shim 不会被执行。使用 Node launcher 的 scip-python 必须把原生
`node.exe` 与官方包 `package.json` 声明的实际 JS 入口分别绑定：

```text
project-brain provider bind \
  --profile python-main \
  --executable C:\Program Files\nodejs\node.exe \
  --script C:\absolute\path\to\node_modules\@sourcegraph\scip-python\index.js \
  --trust-local-executable
```

绑定必须是仓库外的绝对普通文件。对 scip-python，Project Brain 除了固定 node 与入口文件，还会
递归固定官方 npm 包根目录的完整文件清单；`dist/` 中任一传递 bundle 漂移都会使绑定失效。
内容变化后必须用 `--replace` 重新显式信任。
可用 `project-brain provider list` 查看当前项目的机器绑定。

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

项目初始化并提交配置后，每台开发机器执行：

```text
project-brain bootstrap --codex
```

应先完成上一节所示的 Provider 绑定；否则 `bootstrap` 的最终 `doctor` 会拒绝把项目报告为
ready。`bootstrap` 把项目根和已提交的 `project_key` 注册到机器本地，并将 Project Brain dispatcher 结构化合并
到用户级 `~/.codex/hooks.json`。已有用户 Hook、未知顶层字段和 matcher group 都会保留；重复
执行不添加副本。未注册项目会静默 NO-OP，项目仓库不再保存开发机绝对路径。

Codex 会按 Hook 精确定义哈希要求审核非托管命令 Hook。安装后若 Codex 提示待审核，请在
`/hooks` 中检查并信任；`doctor` 会诚实报告该信任状态无法通过正式机器接口验证。

```text
project-brain doctor
project-brain uninstall-hooks codex
```

`doctor` 默认检查 Codex；也可执行 `project-brain doctor claude-code`，并通过全局
`--claude-home` 指定 Claude 配置根。报告使用通用的 `adapter`、`adapter_hooks` 与
`adapter_trust_state` 字段。`doctor` 总会先把结构化报告写到标准输出；只要状态不是 `ready`，进程同时返回非零退出码，
因此既适合人工诊断，也可以作为 CI/bootstrap 的硬门禁。

卸载只删除 manifest 中精确记录的 Project Brain handler，保留用户后来增加的 Hook。检测到
人工修改或重复 handler 时默认返回 Integration Drift，不覆盖用户配置。

[examples/codex-hooks.json](examples/codex-hooks.json) 仅保留为 adapter 协议演示，不是生产安装方案。
示例使用同步 `PreToolUse`，因此硬规则可以在工具执行前拒绝调用。

查看当前 Codex adapter 明确声明的能力：

```text
project-brain capabilities codex
```

Claude Code 的直接协议适配器使用独立的 adapter identity、事件幂等域和审计记录：

```text
project-brain capabilities claude-code
project-brain hook claude-code pre-tool-use
```

安装 Claude Code 用户级 `settings.json` dispatcher：

```text
project-brain install-hooks claude-code
```

可用 `--claude-home` 显式指定配置根；省略时读取 `CLAUDE_CONFIG_DIR` 或 `~/.claude`。
安装器只追加五个 Project Brain handler，保留用户字段和现有 hooks；重复安装幂等，检测到
托管 handler 漂移时拒绝覆盖。handler 使用 Claude Code 的 `command` + `args` exec form 直接
启动绝对路径 launcher，不经过 Git Bash、PowerShell 或 POSIX shell。卸载同样只删除 manifest
精确记录的 handler。

## Prime Agent 独立 Runtime 接入

Prime Agent 不是挂载在 Codex/Claude Code 上的客户端。Project Brain 为其保留独立 adapter、
事件幂等域与审计域。当前已开放 Rust 侧直接协议入口，供 Prime Extension 开发和 fixture 使用：

```text
project-brain capabilities prime-agent
project-brain hook prime-agent pre-tool-use
project-brain dispatch prime-agent pre-tool-use
```

输入由 Prime Extension 从正式 `input`、`before_agent_start`、`tool_call`、`tool_result` 与
`agent_end` event 规范化。输出是 Project Brain 自有 JSON，不复用 Codex/Claude vendor JSON；
`tool_call` 可以依据 `block` 拒绝执行。Prime 当前没有已确认的 settled/Stop continuation 契约，
因此 `continue_after_stop` 必须保持 `unsupported`。本阶段尚未开放
`install-hooks prime-agent`，不会向 `~/.prime/agent/extensions/` 写入未经原子安装测试的文件。

## Godot Engine Evidence Provider

对已初始化的 Godot 4 项目，可以显式信任机器上的 editor/console binary，并让真实引擎执行导入、
UID 解析、主场景与 autoload 解析，以及 `.tscn/.tres` 全量加载：

```text
project-brain evidence godot \
  --executable /absolute/path/to/godot \
  --trust-local-executable
```

Windows 必须直接指向 `.exe`，不接受 `.cmd/.bat` shell shim。Provider 会固定 executable SHA-256，
验证 Godot 4 的 `--headless/--script/--import` 能力，在机器私有临时目录运行 Project Brain 自带的
GDScript probe，并隔离 HOME/APPDATA/XDG。该命令只做 import/load evidence，**不会执行任何 export**。

Engine Snapshot 记录 `project.godot`、main scene、autoload、scene/resource、script/source asset 与
实际依赖边。探针会在加载前后各采集一次引擎解析状态及文件 SHA-256；任一变化都拒绝提交结果。
`.godot/` 始终被排除在 ArtifactGraph 之外，只是可删除的引擎执行缓存。当前 CLI 输出已验证快照，
并原子写入项目隔离的 SQLite Evidence ledger。查看当前 head：

```text
project-brain evidence status
```

完整 ArtifactGraph 按 fingerprint 只保存一次；每次真实运行追加轻量 attestation。Codex、Claude Code
或 Prime Agent 的 `PostToolUse` 一旦观察到明确的 Create/Modify/Delete，现有 Semantic、Engine、Build、
Test、Runtime Evidence heads 会作为一个幂等事件变为 `stale`，并在后续 Session/Intent/PreTool/Stop 注入提示。
Provider 应用新快照时会校验其显式 upstream fingerprint，并把失效传递给真正依赖它的下游；只有逐层
重新运行对应 Provider 才能恢复各自 `fresh`。
`stale` 证据永远没有硬阻断资格。即便是 `fresh + complete + deterministic + error` finding，也仍须
仓库规则显式提供 authority/strength/effect，Provider 本身不能自动 block。

Test 已作为独立 Evidence Plane 进入协议与 SQLite schema v14。`EvidenceFinding` 默认是 advisory；
只有精确命中仓库 `finding_effect_mappings`，并同时满足 fresh、complete、deterministic provider 与
`deterministic_violation` 的 error，才可能在 Stop 产生 ContinueWork。当前尚未开放具体语言的 Test
执行命令，因此不会把普通 Build/Runtime 结果伪装成 Test Evidence。

## Build Evidence Provider

Build Evidence 与 Engine、Runtime 分开。仓库不能声明命令、参数、脚本或环境变量；CLI 只暴露
Project Brain 内置的三种固定合同：

```text
project-brain evidence build dotnet \
  --profile godot-debug \
  --executable /absolute/path/to/dotnet \
  --target game.csproj \
  --require-engine \
  --trust-local-executable \
  --trust-repository-build-code

project-brain evidence build rust \
  --profile workspace-debug \
  --executable /absolute/path/to/cargo \
  --manifest Cargo.toml \
  --trust-local-executable \
  --trust-repository-build-code

project-brain evidence build python \
  --profile source-validation \
  --executable /absolute/path/to/python \
  --source-root . \
  --trust-local-executable
```

.NET 固定执行 Debug、`--no-restore --no-incremental --disable-build-servers`，只接受单个 `.csproj`；
已准备的 NuGet restore metadata 会复制到机器私有临时目录，bin/obj 也只写入该目录。Godot C# 可用
`--require-engine` 强制引用唯一的 fresh、complete、deterministic Engine head。Rust 固定执行
`cargo build --workspace --all-targets --frozen` 并使用临时 target。两者都可能执行仓库控制的
MSBuild task、build.rs 或 proc macro，因此除了信任机器 executable，还必须单独确认
`--trust-repository-build-code`。

Python v1 是 `validation_only`：以 `-I -S -B` 启动 Project Brain 内置 bootstrap，对项目内 `.py`
逐文件调用 `compile()`，不 import、不 exec、不构建 wheel。三种合同都会固定 executable SHA-256，
清空并重建环境，比较运行前后 worktree 指纹，限制输出大小，并为最终产物生成文件级哈希清单；
.NET 的 `obj` 路径相关 cache 不进入权威产物清单。
命令不会运行测试、应用或任何 export。

成功的 Godot C# Build 还会在 machine scratch 删除前，将最终输出闭包逐文件按 SHA-256 原子提升到
机器级 content-addressed store，并生成绑定 `project_key`、Build provider、Source fingerprint、
完整相对路径清单与 Godot 主程序集的不可变 RuntimeArtifactBundle。Evidence 只保存内容身份，不保存
机器绝对路径。Runtime 不允许重新构建后冒充该 Build；它只能重新校验并物化 bundle 的精确字节。
当前 store 不执行隐式 GC；object 缺失或损坏只会令 Runtime 拒绝准备。

`coverage=complete` 表示“完整观测了本次合同”，不表示构建成功。非零退出可同时是
`complete + build_exit_failure`；工具链、链接器或预还原状态缺失则是
`partial + build_unavailable`，不能冒充项目违规。CLI 会先保存这份失败证据再返回非零。

## Godot 隔离 Runtime Evidence

Runtime v1 只接受已绑定在当前 fresh、complete、deterministic Build head 上的
`RuntimeArtifactBundle`：

```text
project-brain evidence runtime \
  --bundle sha256_<bundle> \
  --executable /absolute/path/to/godot \
  --trust-local-executable \
  --quit-after 120
```

Provider 从 Git 权威文件清单物理复制隔离 project mirror，拒绝 symlink/junction/reparse，排除 `.git`、
`.godot`、`.project-brain` 与旧 bin/obj/artifacts；源码复制前后必须保持同一 worktree fingerprint。
精确 Build bundle 随后物化到 staged `.godot/mono/temp/bin/Debug`，并在 import 前后、Runtime 前后
重复校验完整文件集合、大小与 SHA-256。主程序集还必须与 `project.godot` 的明确声明一致。

Godot 只使用内置固定 argv 进行 `--import` 和主场景 `--quit-after` headless 运行。合同禁止 restore、
build、test、`--script`、editor/project-manager 与全部 export/release 入口；仓库不能注入 scene、参数、
环境或 shell。`user://` 被隔离到 machine-private run root，已有 `override.cfg` 的项目在 v1 中直接拒绝。
每个 run 保留 marker、原子 journal 与日志用于审计/崩溃恢复；Project Brain 不会自动删除或导出项目。

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

路径按项目相对前缀匹配，`.` 明确表示整个项目。`init` 生成的初始 Envelope 使用 `.`，不会
夹带 Project Brain 自身仓库的目录假设；开始具体任务后应把它收窄为该任务实际允许的文件或目录。

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

### SCIP 语义索引

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

推荐让机器级 Runner 直接执行已绑定 producer：

```text
project-brain provider index --profile rust-main --timeout-seconds 300
```

Runner 从固定 adapter 构造 argv，不接收仓库命令或任意参数，不使用 shell；它固定 executable/
Node entrypoint 哈希，并对 scip-python 固定完整 npm 包清单哈希。Runner 关闭 stdin，使用环境白名单和净化后的 `PATH`，把输出写入机器私有临时目录，
限制并完整哈希 stdout/stderr，并按项目、profile、worktree 使用 OS 文件锁。索引前后及 SCIP 解析后
都会核对完整工作区内容指纹；源码变化、超时、非零退出、二进制漂移、链接输出、过大/非法 SCIP
均不会提交 semantic snapshot。过程 provenance 与失败只写入机器级有界 JSONL audit。

每次导入同时输出 `coverage`：Project Brain 把 Git 已跟踪及未忽略文件与 Provider 实际 Document
逐语言比较。Rust、Python、C#、Visual Basic、F# 使用显式扩展名契约；自定义语言不会被猜测，
而是报告 `unverifiable`。报告包含 expected/indexed/provider document 数量以及最多 200 条缺失和
Provider-only 路径样本。只有 `complete` 可以提交为 semantic snapshot；`partial` 与
`unverifiable` 会在写入 SQLite 前失败，因此不会替换 latest semantic snapshot，也不能产生新的
hard semantic truth。

对不稳定或刚升级的 Provider，先执行只读稳定性验证：

```text
project-brain provider verify-stability --profile rust-main --runs 5 --timeout-seconds 300
```

该命令固定同一源码指纹、机器绑定 revision 与 executable SHA-256，逐次比较完整 Document 路径集合
和完整 provider-neutral semantic snapshot 指纹。只有所有运行均 `complete` 且两类指纹都一致时返回
成功；任何不完整或非确定性结果都返回非零。原始 Provider audit 会保留，但命令永远不提交快照，
也不会把多次失败结果取并集。最终状态会作为项目级 append-only qualification 写入 SQLite：一旦
记录为 `nondeterministic` 或 `stable_incomplete`，普通 `provider index` 即使偶然得到一次 complete
也不能提交；必须再次显式运行稳定性验证并得到 `stable_complete`。资格绑定 registration revision
与 executable SHA-256，绑定变化后必须重验。

如果 `.scip` 由 CI 或其他流程生成，仍可按项目内稳定 profile ID 手工导入：

```text
project-brain index-scip --provider rust-main --input index.scip
```

手工导入永远标记为 `offline_import`，可以用于查询、lineage 候选和 advisory，但不能单独获得
Hook 硬阻断权。只有 `provider index` 通过当前机器已登记且哈希未漂移的 executable 产生的快照，
才追加 `trusted_provider` attestation；证明同时固定 registration ID、executable SHA-256、SCIP
artifact SHA-256、Git HEAD 与完整 worktree 指纹。

`doctor` 会读取 SQLite 中与快照同事务保存的 source manifest，并按当前 worktree/HEAD 重新计算
覆盖率。已有索引若为 `partial`、`stale`、损坏或来自未保存 manifest 的旧库，doctor 会降级并返回
非零；尚未运行过索引只报告 `not_indexed` warning，不阻断首次 bootstrap。旧库迁移不会从符号表
猜测文档清单；必须真实重跑一次 `provider index` 或 `index-scip` 才能补录 manifest。

只运行覆盖率 CI 门禁而不检查用户级 Hooks：

```text
project-brain provider coverage --require-indexed
```

`partial`、`stale`、存储损坏始终返回非零；`--require-indexed` 还会把 `not_indexed` 与
`unverifiable` 视为失败。

一个 `.scip` 可以逐文档映射多种语言，例如同一 scip-dotnet 索引内的 C# 与 Visual Basic。
Python 的空 `Document.language` 只有在 profile 显式声明 `raw_language: null` 和
`allow_missing_language: true` 时才接受。Producer 版本只记录来源，不参与 Brain contract 版本。
`language_profiles[].roots` 为空数组或只包含 `.` 时都表示项目根；空字符串、绝对路径和 `..`
仍会被拒绝。

## Semantic lineage ledger

连续导入同一 semantic provider 的新快照时，Project Brain 先按 provider/language/kind/定义证据形成
lineage group。只有 1×1 group 自动物化一个 `proposed` candidate；1×N、M×1、M×N 只保存成员集合
和潜在 pair 计数，不生成笛卡尔积。稳定 ID 不生成 self-lineage，本地 symbol 不参与跨快照 lineage。
任何候选都不会自动确认；SCIP 定义 token 不冒充定义正文，producer 缺少完整签名时不产生跨快照匹配。

查看 group 与成员，并显式选择一个潜在 pair：

```text
project-brain lineage groups
project-brain lineage group --group <group-id>
project-brain lineage materialize \
  --group <group-id> \
  --from <old-symbol-id> \
  --to <new-symbol-id> \
  --human-confirmed
```

`materialize` 仍只产生 `proposed` candidate；随后必须单独 confirm。超过单侧 4096 members 的恶意或
病态 group 仅保存计数和成员集摘要，不能直接物化。

V7 曾把歧义 group 物化为 pair-first 笛卡尔积。升级不会静默删除这些历史行；先运行只读预演：

```text
project-brain lineage compact-legacy-proposals
```

只有完整笛卡尔积、仍为 `proposed`、只有旧算法证据且未被任何 decision 引用的 group 才会列为
可压缩。确认报告后才能显式执行：

```text
project-brain lineage compact-legacy-proposals \
  --apply \
  --request-id <request-id> \
  --human-confirmed
```

执行会在同一事务中保存 group/member、候选与证据 manifest hash 及追加式审计，再删除已证明冗余的
旧 pair/evidence。重放同一 request ID 返回原报告；命令不会执行 `VACUUM`。由旧 token 指纹压缩成的
group 只保留历史事实，禁止重新 materialize。

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
  --reason "confirmed rename" \
  --human-confirmed

project-brain lineage reject \
  --candidate <candidate-id> \
  --request-id <request-id> \
  --reason "different responsibility" \
  --human-confirmed
```

修正既有确认必须原子完成：

```text
project-brain lineage confirm \
  --candidate <new-candidate-id> \
  --supersede <old-confirmed-candidate-id> \
  --request-id <request-id> \
  --human-confirmed
```

状态为 `proposed / confirmed / rejected / superseded / invalidated`。Ambiguity 是候选组属性，
不是状态。新快照、算法升级或置信度变化不会修改旧候选或人工裁决；裁决也不会修改 symbol ID、
tombstone 或历史快照。

## Symbol-scoped rules

规则的 `symbol_scopes` 保存仓库可审查的历史锚点，而不是虚构一个跨重构永久不变的 ID。每个锚点
固定 provider profile、实际 provider contract ID、language、snapshot fingerprint 和 symbol ID，
解析策略当前只能是 `confirmed_lineage_only`。

推荐先把新规则以 `status: proposed` 写入 `.project-brain/config.json`，运行可信 Provider 索引并从
`provider index` / `symbols` 输出取得 contract、snapshot 和 symbol，然后由人工绑定：

```text
project-brain rules bind-symbol \
  --rule ARCH-001 \
  --provider rust-main \
  --contract <provider-contract-id> \
  --language rust \
  --snapshot <snapshot-fingerprint> \
  --symbol <symbol-id> \
  --human-confirmed

project-brain rules symbol-scopes --rule ARCH-001
```

审查并激活规则、提交配置后，必须再次执行 `provider index`。配置或 HEAD 变化会让旧 attestation
过期；相同 semantic snapshot 的可信重跑会追加新的来源证明，而不会改写旧快照。删除精确锚点
使用 `rules unbind-symbol` 的同组参数并显式提供 `--human-confirmed`。

硬门控资格矩阵：

- `PreToolUse`：最新解析必须是 direct semantic 或逐跳 confirmed lineage；来源必须是当前机器仍
  ready 且 registration/executable hash 与 attestation 一致的 trusted Provider；HEAD/worktree 必须
  新鲜；工具影响必须是结构化 whole-file Write/Delete，或 old string 唯一可定位的 Edit range。
- `Stop`：必须有当前 clean `HEAD` 的 trusted semantic baseline，并由实际 Git hunk 与 definition
  range 相交；纯插入也按基线插入位置计算。
- apply_patch update、shell 文本、syntax fallback、proposed/ambiguous lineage、local symbol、离线
  SCIP、过期快照、Provider 缺失/失败/二进制漂移都只能 warning/advisory，不能伪装成规则违规或
  制造 Stop 循环。

## Workspace

```text
crates/
├── brain-analyzer/   # Tree-sitter changed-symbol 提取
├── brain-core/       # 协议、规则验证、确定性决策
├── brain-evidence/   # Source/Semantic/Engine/Build/Test/Runtime 证据与 ArtifactGraph 协议
├── brain-godot/      # Godot 4 真实加载探针到 Engine Evidence 的确定性转换
├── brain-scip/       # 离线 SCIP protobuf、项目 profile 与语义快照
├── brain-store/      # SQLite schema 与审计
├── brain-symbols/    # Provider-neutral 符号、边、快照与身份协议
└── project-brain/    # CLI、Git、Agent adapters、机器级 Evidence runners
```

进一步设计见：

- [架构说明](docs/architecture.md)
- [协议说明](docs/protocol.md)

## 当前限制

- Codex 与 Claude Code 已提供直接适配器、用户级 Hook 安装器和按 adapter 选择的 `doctor`；
  Prime Agent 已有独立 direct adapter，但用户级 Extension 安装器与 doctor 尚未实现。
- Godot Engine Provider、Evidence ledger、Hook 失效传播、.NET/Rust/Python Build Provider v1、Godot
  C# RuntimeArtifactBundle CAS、隔离 Godot headless Runtime Evidence v1，以及 Test plane/finding 显式
  effect 映射核心已完成；具体 .NET/Rust/Python/Godot Test Provider 仍未完成，不能声称全部治理能力
  已经完结。
- shell 命令只做保守的显式危险模式识别，不承诺成为完整 shell 安全沙箱。
- changed-symbol 与内置 Tree-sitter syntax Provider 当前只支持 Rust；.NET/Python 通过显式配置的
  SCIP semantic Provider 接入。
- SCIP 当前可靠导入 definition、reference、contains，以及 producer 明确提供的 implementation/
  type-definition 关系；不会从 occurrence 猜测 call/import/implementation。
- Project Brain 不下载、安装、修补或自动发现外部 producer；用户必须显式绑定机器路径。Rust Runner
  已记录真实 rust-analyzer 非确定性反证；scip-dotnet 0.2.14 已在 Windows 真实验证为
  `stable_complete`。scip-python 0.6.6 的官方 npm 包在原生 Windows 存在启动缺陷；本仓库用固定整包
  清单的审计补丁验证了后续链路，但这不是对官方原包 Windows 可用性的声明。Linux/macOS 仍需在
  对应平台 CI 中补充真实 producer 资格证据。
- Runner 保证自身不执行仓库声明的命令，但语言 indexer、Cargo/build script/proc macro、.NET 和
  Python 环境仍是独立信任面；当前版本不是通用 OS 沙箱。Windows 超时使用 `taskkill /T`，Unix
  使用独立 process group；尚未提供 Windows Job Object 级的强隔离证明。
- syntax fallback 不自动关联 rename/move lineage；这必须由语义证据或显式确认完成。
- semantic lineage 当前只支持同项目、同 provider profile/contract、同语言、相邻快照的一对一
  predecessor/successor；split/merge、跨 provider equivalence 和传递闭包不在本阶段。
- Symbol scope 当前是一对一 definition 范围；split/merge、跨 provider equivalence、调用图影响面
  和符号集合表达式尚未加入。
- Semantic Sentinel / Architecture Judge 尚未加入；这是有意的 V0 边界。
