# ADR-0016：按适配器选择 Doctor

## 状态

Accepted；由 ADR-0038 扩展到最终四适配器。

## 背景

不同 Agent 的配置根、安装目标、生命周期合同和续轮能力不同。固定检查单一配置会让一个 Agent 的健康状态错误代表另一个 Agent。

## 决策

1. `doctor [codex|pi|opencode|dsh]` 显式选择 adapter，省略时默认 Codex；
2. 每个 adapter 使用独立 home、manifest、目标 hash 和 launcher fixture；
3. 输出通用字段 `adapter`、`adapter_hooks`、`adapter_trust_state`；
4. dsh doctor 必须显式给出 profile，并验证 profile package dependency 与 bundle 声明；
5. 一个 adapter 的有效安装不得掩盖另一个 adapter 的缺失或漂移；
6. Doctor 同时验证项目注册、Provider 状态、数据库和可选 Qualification。

## 验证

- 四适配器 capability/安装目标 fixture；
- PI/opencode 漂移拒绝与强制卸载边界；
- dsh profile bundle 安装、doctor、卸载黑盒测试；
- 未注册项目、错误 profile 和损坏 target 返回非零。
