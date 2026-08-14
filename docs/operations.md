# 运维、资格与发布

本文覆盖 Project Brain 的项目数据库、审计、Production Qualification、开发门禁和发布流程。

## 1. 数据库与审计

本地 `.project-brain/brain.db` 是项目级审计与派生状态。它按 `project_key` 保存 Adapter 事件、Evidence、
符号快照和 lineage，不应作为普通缓存随意删除。

常用命令：

```text
project-brain audit --limit 20
project-brain database stats
project-brain database compact
project-brain database compact \
  --apply \
  --request-id maintenance-001 \
  --human-confirmed
```

数据库迁移严格前向，未知版本拒绝。大型 lineage 清理必须经过预览、显式批准、项目外 SQLite Online
Backup 和重放验证，不会由 Hook 自动执行。

`database compact` 默认为 dry-run；只有同时提供 `--apply`、`--request-id` 和 `--human-confirmed` 才会
进入实际维护路径。执行前应检查空间、维护锁、WAL 状态和备份目标。完整安全依据见
[ADR-0031](adr/0031-crash-safe-database-compaction.md) 与
[ADR-0034](adr/0034-mandatory-online-backup-before-lineage-deletion.md)。

## 2. Production Qualification

Qualification 在机器级隔离状态中验证当前二进制与控制面合同，不写项目 `brain.db`：

```text
project-brain qualification run --request-id local-qualification-001
project-brain qualification status
project-brain doctor codex --require-qualified
```

固定 Q1-Q7 套件验证：

- 四 Adapter 合同与能力声明；
- 项目隔离；
- 并发重放与幂等；
- 并发 session/operation 因果关联；
- Provider executable 漂移拒绝；
- Stop 续轮有界；
- 10,000 事件长会话重启零丢失与延迟稳定性。

资格 target 绑定二进制 SHA-256、版本、协议合同、数据库 schema、操作系统和架构。任一项变化都需要重新
运行资格；`Failed`、`Inconclusive` 或中断的记录都不能充当 `Qualified`。

主分支 CI 与 Release workflow 会在每个平台构建最终二进制后，以独立进程执行完整资格套件。详细设计
见 [ADR-0037](adr/0037-production-qualification-control-plane.md)。

## 3. 本地开发门禁

项目要求 Rust 1.92。构建 release CLI：

```text
cargo build --release --locked -p project-brain
```

提交前运行：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

## 4. GitHub CI 与 Release

主分支 CI 在以下目标上测试、构建并执行 Production Qualification：

- `x86_64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

推送与 workspace 版本一致的 `vX.Y.Z` 标签后，Release workflow 会：

1. 验证 tag、格式、测试和 Clippy；
2. 在四个平台分别构建 release binary；
3. 运行四 Adapter 能力自检与完整 Production Qualification；
4. 打包二进制、README、CHANGELOG、顶层双许可说明和两份完整许可证正文；
5. 从四份原生归档组装并 smoke-test `@byodod/project-brain` npm tarball；
6. 生成包含四份原生归档和一份 npm tarball 的 `SHA256SUMS`；
7. 发布非草稿、非预发布的 GitHub Release；
8. 已启用 Trusted Publisher 时，通过 OIDC 自动发布 npm 包。

维护者的逐项发布清单见 [RELEASING.md](RELEASING.md)，首次 npm 身份引导见
[npm 分发](npm-distribution.md)。
