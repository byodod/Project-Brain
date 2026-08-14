# ADR-0033：V7 lineage 逻辑压缩必须绑定已批准计划

日期：2026-08-14

## 状态

Accepted

## 背景

真实自举数据库包含 225,867 条 lineage candidate 和同量 evidence，其中 225,866 条来自 V7
pair-first 歧义笛卡尔积。V9 已能只读证明完整 group，并在单个事务中写审计后删除冗余 pair；但原
apply 只要求 request ID 与人工确认，不携带 dry-run manifest。预演后若新增裁决、证据或候选，apply
会重新计算并直接执行另一份当前计划，而不是拒绝已经过期的人工批准。

此外，candidate ID 是全库主键，而 decision 同时保存 project_key。正常 API 会维持两者同项目，损坏
或旧数据却可能形成跨项目引用。逻辑删除不能把这种异常默认为普通“受保护记录”。

## 决策

1. 逻辑压缩协议升级为 operation version 2；group 表示算法仍为 version 1，不改写既有事实。
2. dry-run 的 `compaction_manifest_hash` 覆盖 operation version、project_key、总候选数、可压缩/受保护
   分类、受保护 candidate 身份摘要、目标 group/member 数、oversized 数，以及每个 group 的成员、
   candidate 和 evidence 摘要。
3. apply 必须显式提供 `--approved-manifest-hash`。在任何 group、审计或删除写入前重新构建当前计划；
   hash 不同返回 plan-stale，整个事务回滚。
4. request hash 绑定 project_key、operation version 与 approved manifest。同一 request ID 只能重放
   完全相同的批准计划；换 hash 必须报幂等冲突。
5. apply 使用 Project Brain 独占协作维护锁和 SQLite `BEGIN IMMEDIATE`。外部不遵守锁协议的进程仍由
   SQLite 事务隔离；竞争写入只能导致 busy/回滚，不能部分删除。
6. preview 和 apply 都检查与目标项目相连的 decision/candidate 引用。任一端 project_key 不同即视为
   integrity violation，不进行压缩。
7. candidate/evidence 删除数必须分别与计划精确相等。逻辑压缩仍不执行 checkpoint、VACUUM 或物理
   文件替换；空间回收继续由独立 `database compact` 协议处理。

## 验证

- preview 后改变 candidate state，旧 manifest apply 返回 plan-stale，候选、group 与审计不变。
- 仅新增一个不可压缩的受保护候选也会改变 manifest，不能绕过完整分类审批。
- 同 request ID 换 approved manifest 返回 idempotency conflict。
- decision project_key 与 candidate project_key 不一致时 preview/apply 都 fail-closed。
- 正常完整笛卡尔积仍能原子压缩、精确计数并幂等重放。

## 后果

- 首次真实 225k 逻辑压缩需要执行新的 dry-run，并把其 hash 原样带入 apply；旧预演输出不能授权新版
  删除。
- 协作锁会暂时阻止其它 Project Brain 进程打开数据库，这是大规模逻辑迁移所需的明确维护窗口。
- 本 ADR 不建立通用按年龄删除 Evidence 的 retention 框架；人工裁决、人工 materialization、额外
  evidence 与所有引用闭包继续永久保留。
