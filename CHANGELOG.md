# Changelog

本项目采用 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 的结构，并遵循语义化版本。

## [Unreleased]

### Added

- 确定性四态规则引擎、项目级 Hook 协议和 Codex 生命周期适配。
- Git Change Envelope、Rust changed-symbol 分析、Provider-neutral 符号图和离线 SCIP 导入。
- Project-scoped semantic lineage ledger 与 SQLite 审计、迁移和幂等重放。
- 显式 Rust、.NET、Python 项目初始化模板。
- 跨平台机器安装、版本化 payload、原子回滚、项目注册、用户级 Codex dispatcher 和 `doctor`。

### Security

- Hook 集成使用精确 handler 哈希检测漂移，不覆盖未知用户配置。
- 安装清单、项目注册表和 Hook 配置使用操作系统文件锁、原子替换与写前哈希校验。
