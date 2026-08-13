# ADR-0021：Evidence 使用不可变快照、轻量证明和 Hook staleness head

日期：2026-08-14

## 状态

Accepted

## 背景

Godot Provider 已能产生确定性 Engine Evidence Snapshot，但只打印 JSON 无法让后续 Agent 生命周期
判断其是否仍对应当前源码。若每次运行都重复保存完整 ArtifactGraph，又会重演 semantic observation
历史造成的数据库膨胀。Provider 自己直接 block 还会绕过仓库规则的权限模型。

## 决策

1. SQLite schema v12 新增 `evidence_snapshots`、`evidence_attestations`、`evidence_heads` 和
   `evidence_staleness_events`；所有身份键都显式包含 `project_key`。
2. 完整 Snapshot 按 project、plane、provider、fingerprint 不可变去重；每次真实运行只追加一条轻量
   attestation。应用快照、追加证明和把 head 恢复 fresh 在同一事务完成。
3. staleness event 使用 project + event ID 幂等；同 ID 同内容重放，同 ID 不同内容拒绝。它只修改
   当前 head，不改写历史 Snapshot。
4. Codex、Claude Code 与 Prime Agent 复用同一内部 `PostToolUse` 逻辑。明确的 Create/Modify/Delete
   工具结束后，即使 vendor 报告失败或未知也按可能部分写入处理，把 Engine head 标为 stale。
5. stale 状态会在 Session、Intent、PreTool 和 Stop 中作为上下文或 warning 出现，但不会自行 block。
   重新运行受信任 Provider 才能恢复 fresh。
6. finding 的 hard-block 资格仍要求 deterministic + complete + fresh + error；资格之外还必须由仓库
   规则显式授予 authority、strength 与 effect。Provider finding 本身不是规则。

## 验证

- 相同 Snapshot 连续应用只产生一份完整 JSON 和多条递增 attestation。
- staleness 事件重复投递幂等；同 ID 内容碰撞拒绝；重新应用 Snapshot 清除 stale 状态。
- Codex PostToolUse 集成测试证明 Write 后 head 变 stale、hook 返回 warning、下一 Session 注入 stale。
- schema v1-v11 迁移测试全部升级到 v12，并保留既有数据契约。

## 后果

- Engine Evidence 成为可查询、可重放且不会因相同运行重复膨胀的项目状态。
- 当前只对结构化 Create/Modify/Delete 保守失效；shell/git 的真实变更检测仍需基于变更前后源码清单，
  不能靠命令文本猜测。
- Build 与 Runtime Provider 可复用同一 ledger；跨 plane upstream 传播仍需在对应 Provider 落地时加入。
