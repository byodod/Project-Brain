# Changelog

本项目采用 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 的结构，并遵循语义化版本。

## [Unreleased]

### Added

- Internal Hook Protocol v2 与 active-control 状态机：每步按 revision/epoch 主动恢复目标上下文，
  compact/resume 重新水合，subagent 保存 parent/delegation identity。
- `require_review` 规则效果、持久化变更提案、PostTool 实际 Source delta 匹配，以及
  `replan`、`repair_required`、`verify_required` 纠偏闭环。
- append-only Agent claim ledger 与 `project-brain claims submit/list`；声明只有低权限，不能删除、
  豁免规则或标记已实现。
- `project-brain rules upsert-agent`：允许编程 Agent 自主创建或更新仅限
  `agent_inference/soft/inject_context` 的提示规则，不能覆盖高权限规则或自授阻断能力。
- `project-brain doctor` 现在要求显式指定 Agent，避免默认检查 Codex 导致其他适配器被误判。

### Changed

- dsh Plugin 在每个 `agent/pre-step` 请求按需控制上下文，工具后传递有界完整结果；能力合同细分为
  pre-model context、native replan、compact rehydrate、subagent lineage 等真实宿主 seam。
- SQLite schema 升级为 v21，加入项目隔离的控制会话、放行提案和 Agent claim 表。

## [0.2.3] - 2026-08-15

### Fixed

- Windows dsh 安装按 PATH 目录顺序显式发现 npm 生成的 `dsh.exe`、`dsh.cmd` 或 `dsh.bat`，并在启动
  失败时给出可执行路径和覆盖变量提示。
- npm OIDC 发布使用明确的 `./dist/...tgz` 文件路径，避免 npm 将 tarball 相对路径误判为 Git 地址。

### Documentation

- 新增 dsh 远程安装与验收说明，明确 npm 包来源、dsh lifecycle 和 Codex 独立验收不能互相替代，
  并记录首次外部安装中观察到的前置条件与诊断问题。

## [0.2.2] - 2026-08-15

### Changed

- 明确 `MIT OR Apache-2.0` 表示使用者可任选其一，统一 `byodod and Project Brain contributors`
  版权署名，并让 npm 包和原生 Release 归档携带顶层双许可说明。

## [0.2.1] - 2026-08-15

### Added

- 新增官方 `@byodod/project-brain` npm 分发：单包携带四平台已资格验证的原生 CLI、零安装脚本的
  Node launcher、Release 组装/安装 smoke test，以及首次人工引导后可启用的 GitHub OIDC 自动发布。

### Changed

- 精简项目首页，将 Agent 接入、Provider/Evidence 和数据库/资格/发布细节迁移到分区文档，并增加统一
  文档索引。

## [0.2.0] - 2026-08-15

### Added

- Production Qualification v1：机器级不可变账本、`run/status/show`、七项固定控制面资格用例，以及
  `doctor --require-qualified` 精确 target 门禁；资格证明不进入项目 Evidence Plane 或 hard gate。
- 最终四适配器范围：Codex 原生 Hook、Pi Extension、OpenCode Plugin 和 dsh profile bundle；包含
  原子安装、漂移保护、精确卸载、能力声明与真实 launcher 生命周期往返 fixture。
- Provider Process Protocol v1 与通用 `evidence provider bind/run/list/unbind`，外部 Provider 只能提交
  待验证候选，不能自行授予 hard authority。
- `InputDependencyContractV1`、`EvidenceInputManifestV1` 和 path/profile-aware 精确失效；完整输入未变化时
  可保留对应 Evidence head，未知或不完整输入保守降级。

### Changed

- SQLite schema v20 在 v18 规范事件载荷哈希与 v19 输入清单基础上，迁移旧 adapter CHECK 约束，确保
  `project_key` 继续隔离 Codex、Pi、OpenCode、dsh 的事件、幂等键、证据和审计。
- dsh 的 Stop 审计或 continuation 最多重试一次；Pi continuation 明确标为 emulated 且最多一次，OpenCode 明确为 unsupported。
- 四平台主分支 CI 在 release binary 构建后以独立进程运行完整 Q1-Q7；资格账本单测只验证账本语义，
  不再受并行测试进程的共享磁盘负载干扰。

### Removed

- 移除 Claude Code、Prime Agent 适配器及对应安装器；当前产品范围只包含最终四种编程 Agent。
- 移除 `brain-godot`、Godot CLI/Runtime/Scenario 分支和其它引擎专用核心逻辑；引擎能力以后只能通过
  通用外部 Evidence Provider 协议接入。

### Security

- Qualification run 只允许 `running` 一次性收口到终态，case 结果不可更新/删除，报告哈希在 replay、
  status 与 show 时复验；`Failed`、`Inconclusive`、中断运行和目标漂移均不能冒充 `Qualified`。
- 四适配器的工具前置门控全部 fail-closed；只有 hard 且具备受信 authority 的仓库规则能够阻断，
  Hook/Plugin 自身不授予 vendor permission，也不把未知事件猜成可阻断操作。

## [0.1.0] - 2026-08-14

### Added

- 确定性四态规则引擎、项目级 Hook 协议和 Codex 生命周期适配。
- Git Change Envelope、Rust changed-symbol 分析、Provider-neutral 符号图和离线 SCIP 导入。
- Project-scoped semantic lineage ledger 与 SQLite 审计、迁移和幂等重放。
- 显式 Rust、.NET、Python 项目初始化模板。
- 跨平台机器安装、版本化 payload、原子回滚、项目注册、用户级 Codex dispatcher 和 `doctor`。
- Project-scoped 机器 Provider 注册、哈希漂移检查、固定 argv 安全 Runner、源码指纹门禁与有界失败审计。
- 仓库级 symbol-scoped rules、confirmed-lineage-only 解析、SQLite v6 source attestation、证据等级与
  PreToolUse/Stop 语义门控。
- SQLite v7 semantic source manifest、逐语言 expected/indexed 覆盖率报告与 partial/stale doctor 降级。
- complete-only semantic commit 门禁，以及不提交快照的 Provider 多次运行 Document/semantic 指纹稳定性验证。
- SQLite v8 group-first lineage、SCIP producer signature 证据与线性有界歧义存储。
- SQLite v9 V7 pair-first 旧账 dry-run/显式幂等压缩、manifest 审计与 legacy group 禁止重新物化。
- SQLite v10 append-only Provider 稳定性资格；已失败或已过期的资格阻止普通 index 偶然提交。
- SQLite v15 人工 lineage pair materialization request 审计、幂等重放与 request ID 碰撞拒绝。
- SQLite v16 Evidence invalidation outcome；确定性源码漂移记为 `stale`，无法验证源码指纹记为
  `unknown`，事件保存 Source 观察与精确 head 身份，两者都失去硬门禁资格且不能共享同一幂等事件身份。
- 严格只读的 `database stats`，以及默认 dry-run、独占维护锁、WAL checkpoint、完整逻辑清单、
  `VACUUM INTO`、外部恢复日志、默认备份和原子替换组成的 `database compact` 物理维护协议。
- Claude Code 独立 direct adapter、exec-form 用户级 Hook 安装器、漂移保护、精确卸载与真实子进程
  fixture。
- Prime Agent 独立 direct adapter、全局 TypeScript Extension 原子安装、稳定 launcher JSON 桥接、
  无 LLM/API key 加载 fixture、漂移保护和 adapter-specific doctor。

### Security

- Hook 集成使用精确 handler 哈希检测漂移，不覆盖未知用户配置。
- 安装清单、项目注册表和 Hook 配置使用操作系统文件锁、原子替换与写前哈希校验。
- Provider 只执行显式信任的仓库外绝对文件；拒绝 shell shim、仓库命令、索引期间源码变化与非普通输出。
- partial/unverifiable Provider 输出在 SQLite mutation 前拒绝；禁止以多次不完整运行的并集制造语义快照。
- 数据库物理压缩要求显式人工确认和幂等 request ID；空间不足、busy WAL、清单/哈希漂移、未完成或
  失败恢复日志都会 fail-closed，普通运行不能越过维护窗口；报告明确区分协作式外部写保护、进程崩溃
  原子替换与平台相关的突然断电目录项持久性，恢复只清理本次操作新增的原子临时文件。
- Execute/GitOperation/Unknown PostTool 不再依赖命令文本猜测副作用；当前 Git Source 指纹与 fresh
  Evidence 不一致时精确标 stale，无法验证时标 unknown。所有权限消费者另行计算 recorded + 当前
  Source 的 effective freshness；Provider promotion 也拒绝 TOCTOU 漂移并事务性降级其它不兼容 heads。
- 离线 SCIP、过期快照、未确认/歧义 lineage、local symbol 和漂移 Provider 永不获得 hard gate；
  基础设施故障按 advisory fail-open，人工 lineage/锚点变更要求 `--human-confirmed`。
- 所有 Evidence/Provider 外部执行均受进程树生命周期隔离：Windows 使用 Job Object，Unix 使用
  可用的 cgroup/process-group；根进程退出或超时后必须先清空子孙进程。该边界不伪装成网络、
  文件系统或权限沙箱。
- Prime Extension 固化机器稳定 launcher 路径并以 `shell:false` 启动；不订阅 `agent_end`，不把
  heartbeat、goal、schedule 或 autonomous loop 伪装成 Stop continuation。
