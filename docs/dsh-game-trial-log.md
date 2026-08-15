# DSH 真实游戏开发试验记录

本文件只记录可复验事实。DSH 自身完成的证据、Project Brain 审计证据与监督方独立浏览器证据必须分开，禁止互相冒充。

## 试验协议

- 试验仓库：`E:\Github\Test\project-brain-dsh-game-trial`
- Project Brain 项目键：`pb_061173452eb9c6dde86768c0042af879`
- Agent：DSH `web` profile
- 发现 Project Brain、DSH 接入或能力归属问题时，立即停止当前 DSH 轮次；修复并验证后清空 DSH 产物，使用新会话从空仓库重测。
- 监督方不修改或诊断游戏代码，只负责下达非技术目标、观察、审计、运行验收与修复 Project Brain。

## 第 1 轮：已终止

DSH session：`session-4820a524-96f2-4aee-b62e-ce366b1d3a70`

### DSH 自身完成的事实

- 创建了零外部依赖的原生 HTML/CSS/JavaScript Canvas 游戏。
- 形成两个本地里程碑提交：`ce64d9a`（规则引擎与测试）和 `33e9ecb`（可玩垂直切片）。
- Node 测试从初始失败推进到 41/41；后续自审添加静态集成测试后达到 47/47。
- 启动本地静态服务器并验证资源返回 200。
- 自审发现音频解锁时机、开始键重复触发及前期平衡过难等问题，并在终止前继续调整。

### 监督方独立证据

- `http://127.0.0.1:4173/` 可打开真实开始画面和 Canvas 战斗画面。
- 开始、暂停冻结、失败后重新开始均通过真实浏览器操作验证。
- 浏览器控制台错误/警告读取结果为空；源码静态检查未发现外部网络引用。
- 这些证据不计入 DSH 自身浏览器能力。

### 触发终止的问题

DSH 没有可确认的内置浏览器工具，却调用外部 `Orca CLI / computer-use`，启动 Orca runtime、创建标签页并执行 `orca eval`、`orca screenshot`。Project Brain 审计保存了完整 `pwsh` 命令及结果。该行为不能被报告为“DSH 自身完成浏览器验收”。

第 1 轮因此立即终止。未完成的游戏平衡调整不再继续，也不进入下一轮基线。

### 同轮发现并修复的 Project Brain 问题

1. DSH 提案使用绝对路径而 Git delta 使用仓库相对路径，导致同一文件被误判为提案外变更；现已统一归一为项目相对路径。
2. `repair_required` 曾阻止 `git status`/`git diff` 等只读检查，使 Agent 无法诊断和修复；现已只允许保守的只读检查，仍拒绝变更命令。
3. 动态上下文建议提交 claims，但 DSH 环境中的 `project-brain` 不在 PATH；现会同时注入稳定 launcher 的绝对路径。
4. `audit` 输出接入提前关闭的管道时会触发 Rust stdout panic；现捕获 stdout broken-pipe，并有独立 CLI 回归测试。

## 第 2 轮前置修复

- 清空第 1 轮 DSH 游戏文件和提交，恢复到只含 Project Brain 控制面的空仓库基线。
- 为试验项目增加有明确仓库规则权限的硬门禁：DSH shell 不得调用 Orca、computer-use、Codex、Claude 等外部浏览器或外部 Agent 工具。
- 构建、测试并重新安装修复后的 Project Brain DSH plugin 后，创建全新 DSH 会话从头开发。
