# ADR-0038：最终只内建四个 Coding Agent Hook Adapter

日期：2026-08-15

## 状态

Accepted

## 背景

Project Brain 的价值来自强制经过的生命周期控制面，而不是不断扩大 Agent 名单。每新增一个 adapter，
都必须长期维护官方事件、阻断语义、身份稳定性、安装漂移和真实 fixture。范围不固定会稀释治理可信度。

## 决策

1. 最终内建范围固定为 Codex、Pi、OpenCode、dsh；
2. 四者共享内部 Hook 协议（最初为 v1，当前由 ADR-0040 升级为 v2），但保留独立 identity、版本、幂等域和原生输出；
3. Codex 使用用户级 hooks 中由 Project Brain 自有并精确哈希的 groups（不是企业 managed hooks）；
   Pi/OpenCode 使用用户级 Extension/Plugin 独占文件；dsh 使用显式 profile 官方 plugin bundle 命令；
4. Pre-tool 内部故障一律失败关闭；其它事件以 degraded feedback 暴露；
5. capability matrix 只报告已证明的行为：Pi continuation 为 emulated，OpenCode 为 unsupported，Codex/dsh 为 supported；
6. 安装、重复安装、漂移拒绝、强制卸载和 doctor 必须有黑盒 fixture；
7. 其它 Agent 不保留半实现入口、别名、配置字段或文档承诺。

## 验证

- 四个 `capabilities` 命令；
- 同一受保护删除动作在四个 direct adapter 中均阻断；
- Pi/OpenCode 的 Project Brain-owned file 幂等与漂移安全；
- dsh profile bundle 安装、doctor、卸载；
- release 自检在四个平台执行全部四个 capability 查询；
- 源码和当前文档不存在已移除 adapter 的分支。

## 后果

Project Brain 可以把测试和兼容性预算集中到四条真实接入链。未来若要改变最终范围，必须新 ADR 明确
替代本决策，并先提供官方协议证据和完整黑盒 fixture。

Codex plugin 可以提供更强的目录所有权边界，但当前 plugin CLI 依赖 marketplace 安装，且 Codex IDE
extension 不支持 plugins；用户级 command hooks 覆盖面更完整。因此 v1 保留官方 `hooks.json` 接入，
明确要求 Codex 自身 trust，并用 Project Brain 清单与逐 handler hash 提供可逆所有权。待 plugin 在目标
宿主中具备统一安装/运行覆盖后，再以新 ADR 评估迁移。
