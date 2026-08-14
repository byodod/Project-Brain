# ADR-0008：机器级语义 Provider Runner

- 状态：Accepted
- 日期：2026-08-13

## 背景

仓库已经能显式声明 Rust、.NET、Python 的 SCIP profile，并能导入已有 `.scip`。如果直接把
executable、参数或环境放进仓库配置，Project Brain 会把不可信仓库提升为机器命令执行源；如果只
在文档里要求用户自行运行 producer，又无法让 `doctor`、provenance、并发和失败语义形成闭环。

## 决策

1. 仓库仍只保存 `project_key`、language roots 与 semantic provider profile；机器路径只进入
   `<ProjectBrainData>/state/providers.json`，唯一键为 `project_key + profile_id`。
2. `provider bind` 要求仓库外的绝对普通文件和显式 `--trust-local-executable`。注册固定 program、
   可选 Node entrypoint 的规范路径与 SHA-256，并执行固定 `--version` probe。内容变化只能通过
   `--replace` 建立新 revision，不能静默刷新。
3. 第一版自动 Runner 只支持 rust-analyzer、scip-dotnet、scip-python。argv 由 adapter 构造，
   不接受 repo command/args/environment，不调用 shell；Windows `.cmd/.bat` shim 被拒绝。
4. scip-python 的 Windows 安全入口是原生 `node.exe + hash-pinned JS entrypoint`。两者都必须位于
   仓库外。自定义 producer 仍可通过 `index-scip` 手工导入，但不能自动执行。
5. Provider stdin 关闭；父环境清空后只传递明确白名单，并从 `PATH` 排除当前仓库。stdout/stderr
   持续排空、全量哈希，内存中最多保留各 1 MiB，不作为协议或 Agent 上下文。
6. 每个 `project_key + profile_id + worktree` 使用 OS 文件锁。输出只允许机器临时目录中的普通
   `index.scip`；链接、空文件和超限文件拒绝。成功或失败均写机器级有界 JSONL audit。
7. 外部进程前后计算 Git 已跟踪与未忽略未跟踪文件的内容指纹；SCIP 完整解析后、SQLite 提交前
   再次核对。任何变化都丢弃输出，不产生 semantic snapshot。
8. 外部进程期间不持有 SQLite 写事务。先完成 process、artifact、SCIP、profile、root 与源码指纹
   校验，再记录 `semantic_commit_prepared` audit，最后执行现有 semantic snapshot 事务。
9. `doctor` 要求每个已配置 profile 都有当前项目的有效机器绑定，并检查 producer、路径与哈希；
   缺失或 drift 返回 degraded，不自动执行或重新信任 Provider。

## 失败语义

Provider 的 fail-closed 表示“不产生 semantic truth”，不是“阻止 Agent 开发”。未绑定、哈希漂移、
超时、非零退出、非法输出、profile/provenance 不符或源码变化都不提交快照。未来 Hook 使用这些
数据时，Provider 不可用必须 fail-open 为警告/审计，不能伪装成规则违规或制造 Stop 循环。

## 安全边界

Runner 保证 Project Brain 自身不执行仓库声明的命令；它不是通用 OS sandbox。受信任 indexer
可能调用 Cargo、proc macro、build script、.NET 工具或 Python 环境。scip-dotnet adapter 默认加入
`--skip-dotnet-restore`，但用户仍需把语言工具链视为独立信任面。Windows 当前使用 `taskkill /T`
终止进程树，Unix 使用独立 process group；更强的 Job Object/sandbox 是后续隔离层。该终止策略
已由 [ADR-0035](0035-external-process-tree-containment.md) 的启动时进程树容器取代。

## 验收不变量

- 相对路径、仓库内 executable/entrypoint、Windows shell shim 和缺少显式 trust flag 均拒绝。
- 同项目不同 profile 隔离；不同 project_key 不共享绑定。
- executable/entrypoint 漂移时 `doctor` 和执行都失败，且不会自动更新 SHA。
- 路径带空格仍作为单一 argv；project_key 不能注入参数。
- stdout/stderr 超限仍被持续排空，全量字节数和 SHA 保留，内存 capture 有界。
- 源码在执行或解析期间改变时，不提交 semantic snapshot。
- 成功产物有 artifact SHA、program SHA、probe version、registration revision、source fingerprints；
  临时 `.scip` 在导入后清理。
