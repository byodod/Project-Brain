# Changelog

本项目采用 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 的结构，并遵循语义化版本。

## [Unreleased]

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
- 严格只读的 `database stats`，以及默认 dry-run、独占维护锁、WAL checkpoint、完整逻辑清单、
  `VACUUM INTO`、外部恢复日志、默认备份和原子替换组成的 `database compact` 物理维护协议。

### Security

- Hook 集成使用精确 handler 哈希检测漂移，不覆盖未知用户配置。
- 安装清单、项目注册表和 Hook 配置使用操作系统文件锁、原子替换与写前哈希校验。
- Provider 只执行显式信任的仓库外绝对文件；拒绝 shell shim、仓库命令、索引期间源码变化与非普通输出。
- partial/unverifiable Provider 输出在 SQLite mutation 前拒绝；禁止以多次不完整运行的并集制造语义快照。
- 数据库物理压缩要求显式人工确认和幂等 request ID；空间不足、busy WAL、清单/哈希漂移、未完成或
  失败恢复日志都会 fail-closed，普通运行不能越过维护窗口；报告明确区分协作式外部写保护、进程崩溃
  原子替换与平台相关的突然断电目录项持久性，恢复只清理本次操作新增的原子临时文件。
- 离线 SCIP、过期快照、未确认/歧义 lineage、local symbol 和漂移 Provider 永不获得 hard gate；
  基础设施故障按 advisory fail-open，人工 lineage/锚点变更要求 `--human-confirmed`。
