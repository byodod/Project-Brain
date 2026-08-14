# ADR-0019：分层 Evidence Plane 与独立 ArtifactGraph

日期：2026-08-14

## 状态

Accepted

## 背景

源码 diff 与 SymbolGraph 只能证明代码层事实。外部框架、工具链与运行环境还有只有真实 Provider
执行后才能确认的关系、构建产物与运行行为。把这些事实塞进 SymbolGraph，或把任意工具缓存目录
当作权威来源，都会混淆证据身份、新鲜度与阻断权限。

## 决策

1. Project Brain 将项目事实分为 `source`、`semantic`、`engine`、`build`、`runtime` 五个 Evidence
   Plane。每一层拥有自己的 provider、contract、source fingerprint 与 snapshot fingerprint。
2. 引擎资源、场景、构建产物与运行场景进入独立 ArtifactGraph；现有 SymbolGraph 继续只表达语言
   符号及其语义边。两张图只能通过显式的 provider evidence 连接，不共享 ID 或暗示同一身份。
3. 下游快照显式记录它实际消费的 upstream snapshot。当前源码或任一上游 fingerprint 改变时，
   该证据立即为 stale；缺少当前指纹时为 unknown，而不是假装 fresh。
4. 只有 `deterministic + complete + fresh + error` finding 才具备进入 hard-block 判定的资格。
   heuristic、partial、stale、unknown 只能注入上下文或升级，不能独立阻断。
5. Artifact ID 必须绑定 `project_key + provider_id + provider_key`；边只能指向同一快照中已声明的
   artifact。未知协议版本、跨项目节点、悬空边与 fingerprint 漂移均 fail-closed 拒绝导入。
6. 工具生成缓存不是事实源。Provider 必须通过锁定 executable 和显式输入合同生成新的 Evidence
   Snapshot；缓存仅可作为可删除的执行中间态，不能直接导入为权威快照。

## 验证

- 相同 ArtifactGraph 输入顺序产生相同 snapshot fingerprint。
- semantic upstream 指纹变化会使 engine snapshot 变为 stale。
- heuristic、partial 或 stale finding 均不能获得 hard-block 资格。
- 悬空边与跨项目 Artifact 被协议验证拒绝。

## 后果

- 外部 Provider 可以在不污染 SymbolGraph 的前提下记录框架或工具链私有事实。
- Build 与 Runtime Provider 可复用相同协议，但仍保留各自的新鲜度和权限边界。
- PostTool 只需使受影响的下游证据过期；Stop reconcile 再按成本层级刷新 engine/build/runtime，
  不需要每次 PreTool 都启动完整引擎。
