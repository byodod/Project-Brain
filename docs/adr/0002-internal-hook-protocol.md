# ADR-0002：内部 Hook 协议与能力模型

## 状态

Accepted；四适配器最终范围由 ADR-0038 固定。

## 背景

Codex、PI、opencode、dsh 的生命周期事件、身份字段和控制能力不对称。直接让规则内核理解 vendor JSON 会把同一治理语义复制四次，并诱发能力伪装。

## 决策

1. 采用 `InternalHookEvent v1` 与 `InternalHookOutcome v1`；
2. Adapter 只负责原生协议与内部协议映射；
3. adapter identity、version、event/session/operation 命名空间完全隔离；
4. 能力使用 `supported|unsupported` 明确表达；
5. 无原生 continuation 契约的 Agent 不通过普通消息模拟 Stop 续轮；
6. 未知协议版本、缺失身份或同 event ID 不同 payload 全部拒绝。

## 结果

- 规则内核只实现一次；
- 同一事件可跨 Agent 重放比较，但不会跨域去重；
- 新能力必须先有真实原生契约和 fixture，不能为矩阵对称而加入。

## 验证

- 每个 adapter 的 pre-tool block 黑盒 fixture；
- capability matrix 精确断言；
- 相同 event 幂等、碰撞拒绝与 adapter 域隔离测试；
- 不支持 continuation 的 adapter 明确返回 unsupported。
