# ADR-0006：Semantic Lineage Ledger 与显式裁决

- 状态：Accepted
- 日期：2026-08-13

## 背景

Semantic provider 能提供比语法 fallback 更好的 symbol key，但 raw SCIP symbol、名称、路径和高
置信匹配仍不足以证明两个快照中的节点是同一演化实体。若候选算法直接复用旧 ID、恢复 tombstone
或让规则自动跟随，一次误判会不可逆地污染项目历史。

## 决策

1. Snapshot、symbol observation、candidate、evidence 和 decision 是彼此分离的事实层。
2. Candidate generator 只比较同 project、provider profile/contract、language 的相邻快照中
   removed/inserted symbol；local 与稳定 ID 不生成 lineage。
3. Candidate 初始永远为 `proposed`。Ambiguity 使用 `ambiguity_group_id`，不是生命周期状态。
4. 生命周期为 `proposed / confirmed / rejected / superseded / invalidated`。人工可用新的显式请求
   reconsider rejected；已确认关系的替代必须原子 supersede old + confirm new。
5. Candidate endpoint unique key 保证重跑幂等；算法 ID/版本、输入摘要和结构化证据进入独立
   append-only evidence 表，不改变人工状态。
6. Decision append-only。`request_id + request_hash` 处理 at-least-once 提交，revision CAS 与
   partial unique indexes 处理并发和一对一冲突，不自动选 winner。
7. 新 snapshot 不会使旧 lineage 自动失效。`invalidated` 只表示 endpoint、snapshot fingerprint、
   store integrity 或 provider contract 的结构性失效。
8. Lineage 永不修改 SymbolNode ID、provider key、tombstone、历史 snapshot 或项目规则。

## 结果

- 算法可以升级并积累证据，而不会重置 rejection、confirmation 或 supersession。
- 用户裁决可重放、可审计，并在并发竞争中最多产生一个 confirmed predecessor/successor。
- 历史 lineage chain 必须逐段显式确认，不从 `S1 -> S2` 自动推导 `S1 -> S3`。
- 跨项目、跨语言和跨 provider equivalence 仍被拒绝，未来需要独立模型。

## 验收不变量

- 同 endpoints 重跑只存在一条 candidate；同 evidence 不重复，新算法版本可追加 evidence。
- 高置信和唯一匹配不会自动 confirmed。
- 相同 request/payload 幂等，相同 request/不同 payload 冲突。
- reject 后显式 reconsider 保留两条 decision；generator 不恢复 state。
- competing confirm 只有一个成功；替代确认在一个事务内完成。
- v4 升级到 v5 保留 project-scoped symbol graph，并创建空 lineage ledger。
- confirm/reject 前后 symbol/tombstone/snapshot 数据不变。
