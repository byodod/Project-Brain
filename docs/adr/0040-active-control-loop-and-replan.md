# ADR-0040：主动控制循环、重规划与实际 Diff 纠偏

状态：Accepted

## 背景

只在真实 USER 输入到达时注入一次项目上下文，无法覆盖 Agent 的连续模型步骤、工具选择、文件写入、
compact/resume 和 subagent。把所有角色文本都当作新用户意图又会篡改目标权威并造成上下文膨胀。
工具前的意图也不能替代工具后的实际 Source 变化。

## 决策

1. Internal Hook Protocol 升级为 v2，增加 `context_requested`、session lineage、`ProposedChange`、
   有界工具结果摘要和 `replan` gate。
2. Runtime 持久化 lifecycle、goal、project-context revision 与 outstanding hold。每个可用 model pre-step
   请求上下文，但只有 revision、epoch 或 hold 变化时真正交付。
3. 原始 USER 目标是高权限事实；ASSISTANT、CONTEXT、TOOL、COMPACTED 和 SUBAGENT 不会被重新解释为
   USER。compact/resume 使交付回执失效；subagent 保留 parent 与 delegation depth。
4. `effect=require_review` 与 `block` 使用相同配置权限门槛：仅 hard 且 authority 属于 explicit_user、
   repository_rule、accepted_decision。低权限语义判断只能 advisory。
5. 上游没有原生 replan 时，Adapter 必须拒绝当前 mutation，把纠偏状态交付到下一模型步骤；上下文交付
   回执不是写入许可，只有相同 proposal digest 的重新提案才可解除 replan。
6. PreTool 放行时保存结构化提案和 Source baseline。PostTool 使用持久化提案计算 ObservedChange；额外路径
   形成 `repair_required`，暂停无关写入并阻止 Stop，直到修复写入的实际 delta 不再外溢。
7. Agent 可追加 GoalInterpretation、CompatibilityAssessment、VerificationClaim，但 claim ledger 为
   append-only、低权限，没有删除、豁免、接受决策或标记已实现入口。完成状态只接受实际 diff、Evidence
   与 Stop gate。
8. 任意 shell 的预期影响可能不透明；此时先保存 whole-Source baseline，PostTool 以实际 delta 纠偏。
   缺失提案或基线进入 `verify_required`，不能静默当作成功。

## 后果

- dsh 能在没有新 USER 输入的自运行步骤继续获得按需项目控制上下文。
- 提示词只负责让模型看见状态；权限、幂等、提案匹配、hold 和完成判断仍由确定性 Runtime 控制。
- Codex、Pi、OpenCode 没有已证明的 pre-model seam，因此 capability 必须如实为 unsupported；它们仍保留
  工具前 gate、工具后反馈和各自 Stop 能力。
- 并行 mutation 若无法证明单一 operation 对实际 delta 的独占归属，必须降级为验证/修复状态；不能把
  并发变化错误归因给一个提案。

## 验证

- 规则测试证明 `require_review` 的权限与优先级；
- 状态存储测试证明项目隔离、revision、rehydration、append-only claim 与幂等冲突；
- Adapter 测试证明首次 replan、下一 step 交付、同提案重试；
- Source 测试证明提案外路径形成 repair hold 并阻止 Stop；
- DSH 安装 fixture 验证每步 `context_requested`、完整工具结果和重新安装后的 bundle hash。
