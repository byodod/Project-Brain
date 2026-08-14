# ADR-0012：Group-first lineage 与 SCIP 签名证据

## 状态

Accepted，2026-08-13。

## 背景

真实自举数据库在只有三个 semantic snapshot 时增长到 521,822,208 bytes。表级审计证明，
225,867 条 `semantic_lineage_candidates` 和同量 evidence 占据绝大多数页面；其中一次快照转换
生成 182,019 条全部歧义的 pair。

进一步检查发现，这些 pair 的定义指纹全部等于 `SHA256("<symbol>")`。SCIP definition occurrence
通常只覆盖名称 token；旧 importer 抽取该 token 后又把名称替换成 `<symbol>`，使大量互不相关定义
落入同一个 fingerprint bucket。旧生成器随后物化 `removed × inserted` 的完整笛卡尔积。

## 决定

1. SCIP lineage evidence 优先使用 producer 提供的非空 `signature_documentation.text`。定义 token
   不再冒充定义正文；签名缺失时生成不可跨节点匹配的占位指纹。
2. Lineage 算法升级为 `project-brain-lineage-groups` version 2。先按 project、profile、contract、
   language、相邻 snapshot、symbol kind 和定义指纹形成 group。
3. 只有 1×1 group 自动生成一个 `proposed` candidate；任何歧义 group 自动生成零 pair。
4. 普通歧义 group 完整保存 from/to member set 与摘要。单侧超过 4096 members 时保存
   `summary_only`，禁止直接物化。
5. 用户可以显式从非超大 group 选择一个 from/to member pair；该操作仍只创建 `proposed`，确认
继续使用原有 `--human-confirmed`、request ID、revision CAS 与一对一唯一约束；pair materialization
自身也必须由 request ID 提供幂等重放与碰撞检测，不能只依赖 endpoint 唯一键。
6. SQLite schema v8 新增 group、member、generation-run 与 candidate origin；V7 candidate 保持原样，
   不在 schema migration 中删除或重新解释。
7. 旧笛卡尔积行只允许由后续显式 compact 命令处理：先 dry-run，证明 group 可完整重建、实际 pair
   等于笛卡尔积、状态仍为 proposed、且没有 decision 引用，才能压缩。VACUUM 不能替代逻辑压缩。
8. SQLite schema v9 落地 `compact-legacy-proposals`：默认 dry-run；apply 需要 request ID 与
   `--human-confirmed`。每个可压缩 group 必须只有 version 1 证据且没有额外 evidence，按 kind 拆分后
   精确覆盖 from×to。事务先保存 group/member、candidate/evidence manifest hash 和 append-only run
   审计，再删除对应旧行。由失真 token 指纹恢复的 group 只作历史审计，不允许重新物化候选。

## 后果

- 自动 candidate 数不超过 1×1 group 数；group member row 数对 removed+inserted 线性有界。
- 歧义空间不会被“只保存前 N 个 pair”偏向性截断，人工仍可选择任意已保存 member pair。
- producer 没有签名时减少 lineage recall，但不会制造虚假 rename/move 证据，符合 fail-closed 原则。
- 旧数据库物理体积不会因迁移自动下降；逻辑压缩与物理 VACUUM 必须分别审计和显式执行。
