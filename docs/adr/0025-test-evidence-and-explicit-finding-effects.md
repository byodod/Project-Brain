# ADR-0025：Test 是独立 Evidence Plane，Finding 只能经显式映射产生治理 Effect

## 状态

Accepted

## 背景

Build、Test 与 Runtime 回答不同问题。Build 证明固定构建合同的结果，Test 证明声明的验证条件，Runtime
证明应用在某个场景中的实际执行。把测试塞进 Runtime profile 会混淆覆盖率、失败类型和上游依赖。
同时，Provider 输出的 `error` 可能来自断言、基础设施、超时或解析不确定性；若所有 error finding
自动阻断，确定性控制面会退化为不可审计的启发式门禁。

## 决策

1. Evidence 链扩展为 `Source → Semantic/Engine/Build → Test → Runtime`。Test 是独立 plane，可显式
   引用 Source、Semantic、Engine 与 Build；Runtime 可引用 Test。源码修改会把 Test 与其他下游 head
   一并标为 stale。
2. `EvidenceFinding` 增加 `authority`：默认 `advisory`，只有 Provider 能证明合同内确定性违规时才标记
   `deterministic_violation`。旧快照缺少该字段时仍按 advisory 读取，且旧 advisory fingerprint 不变。
3. `finding_can_hard_block` 同时要求：显式映射、deterministic Provider、complete coverage、fresh head、
   error severity 与 deterministic violation。缺少任何一项都只能反馈，不能阻断。
4. 仓库配置使用 `finding_effect_mappings` 精确绑定 plane、provider ID、provider contract version 与
   finding code。未知 finding 没有隐式 effect。Block/Escalate 映射仍必须是 hard，且 authority 只能来自
   explicit user、repository rule 或 accepted decision。
5. Stop 将符合资格的 Block/Escalate 映射合并到现有对账结果。ledger 不可用、head 缺失、contract
   漂移或证据 stale 时 fail-open，并留下 warning。
6. 继续复用通用 `evidence_snapshots/attestations/heads/staleness_events` 账本，不新增 Test 专用快照表。
   SQLite schema v14 仅扩展 `evidence_snapshots.plane` 检查约束以接受 `test`，迁移保留既有快照、head、
   attestation 与外键关系。
7. 本 ADR 只建立 Test plane 与治理映射核心。具体 .NET、Rust、Python、Godot Test Provider 必须另行
   实现 adapter-owned 固定合同；仓库不得提供任意 command、shell、args、restore、network 或 export。

## 验证

- 协议测试证明 Test 接受声明的四类上游，而 Build 不能反向引用 Test。
- 权限测试证明没有显式映射、advisory finding、heuristic/partial/stale evidence 均不能 hard block。
- Stop 测试证明 fresh + complete + deterministic violation 在显式映射后可 ContinueWork，标记 stale
  后自动降级为 advisory。
- schema v13→v14 迁移测试保留既有 Engine head，并能在同一数据库提交 fresh Test head。

## 后果

- Project Brain 可以表达测试覆盖和测试失败，而不冒充 Runtime 正确性。
- Provider 负责报告事实，仓库规则负责赋予 effect；二者分离后可以审计“为什么被阻断”。
- 后续 ADR 已实现 .NET 固定 Test、Godot structured scenario 与 Rust offline/frozen Test Provider；
  Python Test adapter 仍需单独定义，不能由 Python Build validation 冒充。
