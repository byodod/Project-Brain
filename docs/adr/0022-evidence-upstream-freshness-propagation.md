# ADR-0022：Evidence freshness 沿显式 upstream 引用传播

日期：2026-08-14

## 状态

Accepted；第 2 条的 Source 排除已由 ADR-0032 supersede

## 背景

Engine、Build 与 Runtime Provider 共用 Evidence ledger 后，仅把 Engine head 标为 stale 会留下错误的
fresh 下游。另一方面，Semantic 通常与 Build 平行，不能因为某个 SCIP Provider 不可用就无条件让
Build 失效。一次 Hook 事件也可能同时影响多个 plane，而 v12 事件表只能表达单个 plane。

## 决策

1. SQLite schema 升级为 v13；`evidence_staleness_events` 增加规范排序的 `planes_json`，旧 v12 行按
   原 `plane` 无损回填。一个 project + event ID 对应一个不可变的多 plane 事件。
2. 明确的 Create/Modify/Delete `PostToolUse` 保守失效现有 Semantic、Engine、Build、Runtime heads。
   Source Plane 表示声明本身，不作为这次 downstream 验证结果失效的目标。
3. 应用 Snapshot 时只检查它显式声明的 upstream `(plane, provider_id, fingerprint)`：缺失为 unknown，
   fingerprint 不一致或上游 stale 为 stale，上游 unknown 为 unknown；没有声明的 Semantic 不影响 Build。
4. head 变化后对当前 fresh heads 做固定点传播，使 Engine → Build → Runtime 等依赖链在同一事务失效。
5. 失效不会自动恢复。真实重跑某个 Provider 只能重新计算该 Provider 自己的 head；旧下游必须各自重跑。
6. stale/unknown 继续没有独立硬阻断权限；基础设施失败不等于项目违规。

## 验证

- 缺失 Engine upstream 的 Build Snapshot 保存为 unknown，而不是 fresh。
- Engine、Build、Runtime 链可全部 fresh；新 Engine fingerprint 会让后两者传递变 stale。
- 重跑 Engine 不恢复 Build/Runtime；重跑 Build 后 Runtime 仍 stale，直到 Runtime 自己重跑。
- 一个 Hook event 可幂等失效多个 plane，并保持项目隔离与事件碰撞检测。
- v12 staleness 历史升级到 v13 后保留原行并得到单元素 `planes_json`。

## 后果

- Build/Runtime Provider 可以安全接入同一 ledger，不会把旧验证误报为当前 fresh。
- 传播由显式 EvidenceReference 驱动，而不是由 plane 名称猜依赖。
- shell/git 修改不能靠命令文本猜测；[ADR-0032](0032-post-tool-source-fingerprint-reconciliation.md)
  已以 live Source fingerprint、effective freshness 与精确 head reconciliation 完成该权限边界，并将
  Source Plane 纳入显式 mutation 失效目标。
