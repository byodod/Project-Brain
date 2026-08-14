# ADR-0036：Prime Agent Extension Provisioning v1

日期：2026-08-14

## 状态

Accepted

## 背景

本决策核对 Prime Agent 官方仓库提交 `9f9501146e869466acaca66dac49cff857b7b4f9`（coding-agent
package `0.7.2`）。其 Extension loader 自动发现用户级 `~/.prime/agent/extensions/` 与项目级
`.prime/agent/extensions/`。它支持直接 `.ts/.js` 文件、子目录 `index.ts/index.js`，以及带 Prime
package 声明的目录。Project Brain 是机器治理基础设施，不能把可执行桥接器默认写进项目目录，也
不能让仓库选择 launcher。

正式事件中 `tool_call` 可返回 `{block:true, reason}`，`before_agent_start` 可注入 custom message，
`tool_result` 可修改结果；`agent_end` 没有 Stop veto/continuation 合同。heartbeat、goal 与 schedule
是 runtime 唤醒或目标机制，不是 Hook 生命周期事件。

## 决策

1. `install-hooks prime-agent` 只管理全局
   `<prime-home>/extensions/project-brain/index.ts`。默认 prime home 为
   `PRIME_AGENT_CODING_AGENT_DIR`，否则 `~/.prime/agent`；`--prime-home` 只用于显式机器部署与测试。
2. 首次安装先原子创建专属 `project-brain` 目录，以目录存在性声明所有权；任何无 Project Brain
   manifest 的既有目录均视为漂移，绝不覆盖。目录内只允许托管 `index.ts`。
3. 机器 integration manifest 绑定 schema、integration version、API contract、目标路径与 SHA-256、
   稳定 launcher 路径与 SHA-256，以及精确事件集。相同二次安装 NO-OP；任何文件、成员或 manifest
   漂移默认拒绝。卸载只删除精确托管目标，漂移必须显式 `--force`。
4. Extension 以 Node `spawn` 直接执行绝对 launcher，参数为独立 argv，固定 `shell:false`；stdin、
   stdout、stderr 与运行时间均有界。repo 配置不能覆盖 launcher。
5. 只订阅 `session_start`、`input`、`before_agent_start`、`tool_call`、`tool_result`。session/intent
   context 在 `before_agent_start` 注入；tool veto fail-closed；post-tool 只追加反馈，不覆盖原结果。
6. 不订阅 `agent_end`，不调用 `sendMessage` 伪造继续，也不映射 heartbeat、goal、schedule 或
   autonomous loop。`continue_after_stop` 继续声明 unsupported。
7. Prime doctor 验证专属目录、冲突文件、Extension/launcher hash、当前机器稳定 launcher 路径、
   项目外路径和 capability roundtrip。测试环境存在 Node 22 时，直接加载实际 `.ts`，用伪
   ExtensionAPI 注册事件并通过真实 launcher 验证 hard block；不需要 LLM 或 API key。

## 验证

- CLI 黑盒测试覆盖首次安装、二次 NO-OP、无 manifest 路径碰撞、Extension 漂移拒绝、doctor
  降级、漂移卸载拒绝、精确卸载和用户其他 Extension 保留。
- Node 22 fixture 加载实际生成的 TypeScript 模块，核对五个事件且不存在 `agent_end`，并经真实
  launcher 对受保护路径取得 `block=true`。
- workspace test、strict Clippy 与 Windows/Linux/Intel macOS/Apple Silicon macOS CI 作为合并门禁。

## 后果

- Prime Agent 与 Codex/Claude Code 达到用户级 provisioning 对称，但能力模型仍保持不对称。
- doctor 的 `extension_contract_and_launcher_verified` 只声明 Extension 合同和 launcher 往返已验证，
  不宣称 Prime 的模型、网络、daemon 或完整交互会话已验证。
- Extension bridge 是生命周期接入，不是网络、文件系统或权限沙箱。

## 依据

- Prime Agent repository：<https://github.com/PrimeIntellect-ai/prime-agent>（访问：2026-08-14）
- Extension documentation：<https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/docs/extensions.md>（访问：2026-08-14）
- Loader source：<https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/extensions/loader.ts>（访问：2026-08-14）
- Event types：<https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/extensions/types.ts>（访问：2026-08-14）
