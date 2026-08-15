# Project Brain Architecture

## 1. 目标

Project Brain 是独立于 Coding Agent 的确定性治理运行时。它在 Agent 生命周期边界自动恢复项目上下文、检查工具动作、记录实际结果并裁决是否允许停止。

最终内建 Agent 范围固定为：Codex、Pi、OpenCode、dsh。框架、语言工具链和项目类型不进入 Hook adapter。

## 2. 分层与依赖方向

```text
Agent native lifecycle
        │
        ▼
project-brain adapter + installer
        │
        ▼
Internal Hook Protocol v2
        │
        ├── brain-core      规则、能力、纯决策
        ├── brain-store     项目级审计与证据状态
        ├── brain-symbols   Provider-neutral 符号协议
        ├── brain-evidence  Evidence/输入依赖协议
        └── external providers through process protocols
```

硬性边界：

- `brain-core` 不依赖文件系统、Git、SQLite、Agent SDK 或 Provider SDK；
- `brain-symbols` 不依赖 Git、Tree-sitter、SCIP 或 SQLite；
- `brain-evidence` 不包含具体框架、引擎、编译器或测试运行器类型；
- Adapter 只负责 vendor event 与内部事件/结果映射；
- SQLite、安装器、Git、进程执行只存在于宿主层。

## 3. 决策内核

内核输入是规范化 `ActionDescriptor`、有效规则、符号解析与可信 Evidence。输出为：

```text
allow < allow_with_context < escalate < require_review < block
```

`require_review` 与 `block` 的配置合法性由加载阶段强制：

```text
effect ∈ {require_review, block}
strength = hard
authority ∈ {explicit_user, repository_rule, accepted_decision}
```

低权限来源可以提示风险，但不能转换为硬阻断。Evidence finding 即使声称确定性违规，也必须同时满足 Provider authority ceiling、freshness、coverage 与显式规则映射。

## 4. 项目身份

`project_key` 是所有持久状态的第一隔离键。cwd、仓库路径、session ID 或 Agent 名都不能替代它。

以下对象全部绑定 `project_key`：

- Hook 事件、幂等键、操作因果链；
- Source baseline 与路径 delta；
- SymbolGraph、snapshot、lineage；
- Evidence head、input manifest、staleness；
- 机器 Provider 绑定和项目注册。

项目注册表保存规范路径与项目键。Dispatcher 对未注册项目保持 NO-OP，避免用户级 Hook 干扰无关仓库。

## 5. 四适配器架构

四个 Agent 使用同一内部事件，但保留独立 adapter identity、版本、幂等命名空间、安装 manifest 与原生输出。

### Codex

用户级 `hooks.json` 中由 Project Brain 自身拥有并逐项哈希的 groups 覆盖 SessionStart、UserPromptSubmit、PreToolUse、PostToolUse、Stop。它们不是 Codex 企业 managed hooks，仍受 Codex hook trust 控制。Project Brain 不设置工具 matcher，因此所有工具都经过工具前/工具后入口；用户已有 groups 原样保留。

### Pi

用户级 Extension 订阅 session、input、agent start、tool call/result 与 agent end。工具前 `block=true` 映射为 Pi veto。Pi 的 `agent_end` 不是正式的停止前 veto 边界；Project Brain 使用官方 follow-up API 模拟最多一次续轮，因此 `continue_after_stop=emulated`，不冒充 Codex/dsh 的 supported。

### OpenCode

用户级 Plugin 订阅 chat、tool before/after、session created/idle。工具前阻断通过抛出错误完成。`session.idle` 可用于 Stop 审计，但 OpenCode 没有已确认的强制续轮合同，因此能力必须报告 unsupported。

### dsh

Project Brain 生成机器托管 bundle source，再通过官方 profile plugin 命令安装。插件在每个
`agent/pre-step` 请求主动控制上下文，并订阅 tools pre/post 与 turn-stopping；工具前返回 deny，停止时使用
agent steer。compact/resume 会提升 lifecycle epoch 并强制重新水合，subagent 记录 parent session 与 delegation
depth。每个 profile 独立验证 package dependency、bundle 列表和目标哈希。

## 6. 安装与漂移

安装遵循同一不变量：

1. 先验证机器稳定 launcher 与 capability roundtrip；
2. 只写 Project Brain 独占文件或 Project Brain 自有的精确哈希 group；
3. 保存 target、launcher、事件合同与 SHA-256；
4. 重复安装相同内容为 NO-OP；
5. 不同内容必须显式 replace；
6. 卸载遇到漂移默认拒绝；
7. `--force` 仍只删除 Project Brain 目标，不触碰用户其它扩展。

用户级脚本不直接打开项目数据库；它们只调用稳定 launcher。升级替换版本化 payload，Hook 路径不随版本变化。

## 7. 主动纠偏状态机

```text
session/compact/subagent
        ↓
目标锚点 + 项目上下文 revision 水合
        ↓
每个 model pre-step 按需交付 ContextDeliveryReceipt
        ↓
PreTool 结构化 ProposedChange
   ├─ hard block → deny
   ├─ authorized require_review → 撤回本次工具选择，下一步重规划
   └─ allow → 保存 proposal + Source baseline
        ↓
PostTool 读取完整结果并计算 ObservedChange
   ├─ 与 proposal 一致 → 正常继续
   └─ 出现额外路径 → repair_required，暂停无关写入
        ↓
Stop 同时要求规则、Evidence、Change Envelope 与 active-control hold 全部闭环
```

上下文交付回执只证明某 revision 已在模型调用前可见，不是写入许可。上游没有原生 replan seam 时，
Adapter 以“拒绝当前 mutation + 下一 pre-step 注入”的方式模拟，能力必须报告 `emulated`。

Agent 的 GoalInterpretation、CompatibilityAssessment 和 VerificationClaim 使用 append-only claim ledger；
它们只能影响后续提示，不能删除记忆、授予 hard authority、豁免规则或将自身声明升级为“已实现”。

## 8. Hook 因果链

工具操作使用：

```text
PreToolUse
  ├── event_id
  ├── session_key
  └── operation_id
          │
          ▼
PostToolUse
```

有 vendor 稳定 ID 时直接使用并标记 `vendor_stable`；没有时从规范字段派生并如实标记 `derived_stable` 或 `per_delivery`。不同 adapter 永不跨域去重。

PreToolUse 在执行前保存 Source baseline。PostToolUse 对照实际工作区计算精确路径 delta；纯删除、rename、untracked 与无法验证状态都有显式结果。意图描述不能替代实际 diff。

## 9. Evidence 与精准 freshness

Evidence 分为 Source、Semantic、Engine、Build、Test、Runtime 六个中立 plane。名称表达证据层级，不表达具体框架。

新 head 携带 `InputDependencyContractV1` 与 `EvidenceInputManifestV1`：

- exact path 支持 presence-sensitive 缺失状态；
- tree selector 指定 repository-visible 或 project-filesystem universe；
- 有限 glob 禁止路径逃逸、shell expansion 和歧义语法；
- manifest 固定每个输入的路径、状态、角色、内容哈希和大小；
- complete/conservative 可参与 hard authority，incomplete 不可。

路径 delta 只 stale 相交的 head，并沿显式 upstream 传播。出现未知工具语义、基线缺失、Source 在验证中变化或文件系统不可信时，结果为 `verification_unknown`；系统保守降权而不伪造项目违规。

查询和 hard consumer 会实时重算输入 manifest。持久化 `fresh` 不是永久授权。

## 10. 外部 Provider

### Semantic Provider

SCIP Provider 采用机器绑定 runner：固定 executable/entrypoint 哈希、环境白名单、路径边界、输出上限、执行锁和 complete-only commit。内置 Tree-sitter Rust 只标记 `syntax_fallback`。

### Evidence Provider

Provider Process Protocol v1 是通用插件边界：

```text
machine binding + descriptor + authority ceiling
                     │
                     ▼
resolve input contract twice around stable Source
                     │
                     ▼
copy declared files to private staging
                     │
                     ▼
fixed executable run < request.json
                     │
                     ▼
validate response, hashes, identity, TOCTOU
                     │
                     ▼
core constructs and commits EvidenceSnapshot
```

Provider 私有 payload 只作为内容寻址 artifact 保存。核心不解析它，也不允许它声明 hard-block 权限。

独立进程优于动态库：Rust ABI、panic、allocator 和版本漂移不会进入控制面进程。WASI 可作为未来更强隔离后端，但不是 v1 必需条件。

## 11. 外部执行

所有受治理子进程经过统一 execution layer，具备：

- direct spawn，不经过 shell；
- 有界 stdout/stderr 与 SHA-256；
- timeout；
- Unix process group / Windows Job Object 进程树生命周期；
- scratch 与项目路径边界；
- 完成后再验证产物和 Source TOCTOU。

这提供进程树 containment，不等于网络隔离、文件系统虚拟化或恶意代码沙箱。仓库控制的 build/test code 和本地 Provider executable 必须显式信任。

## 12. 存储

SQLite schema 采用单调前向迁移。关键数据：

- append-only adapter audit；
- operation baseline 与 Source delta；
- provider-neutral SymbolGraph；
- lineage ledger 与人工裁决；
- Evidence snapshots、heads、attestations、input manifests 与 impact events；
- qualification runs。

快照更新事务化，消失符号变为 removed 而非物理删除。危险历史清理由显式 request、revision CAS、不可覆盖备份和重放验证保护。

## 13. Production Qualification

Qualification 以当前二进制哈希、数据库 schema、协议合同与目标平台形成 target hash。Q1-Q7 验证 adapter、隔离、幂等、规则权限、Provider 失败关闭、数据库恢复和安装路径。证明保存在机器级状态，不污染项目数据库。

## 14. Non-goals

- Agent 自主决定何时读取长期记忆；
- 通用聊天记录 RAG；
- 自动授予 LLM/Provider 阻断权限；
- 自动下载或发现外部 Provider；
- 强行对齐四 Agent 不同的生命周期能力；
- 核心内建具体框架、引擎或 IDE 语义；
- 自动确认 symbol lineage split/merge 或跨 Provider 等价。
