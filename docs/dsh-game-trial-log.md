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

## 第 2 轮：已终止

DSH session：`session-da2f387e-1d49-49f1-a6b9-5b60b58f1fe0`

### 已验证事实

- 新会话收到原始用户目标、项目键和 `PB-TRIAL-001` 外部工具硬门禁。
- DSH 没有再次调用 Orca、computer-use、Codex、Claude 或其它外部 Agent。
- DSH 用约 6 分 47 秒完成首次整体规划，随后创建 `game-core.js`、`game.js`、`sound.js`、`index.html`、`style.css` 和三份测试脚本。
- DSH 在测试前自审发现 Boss 配置缺失等问题并主动修改。

### 触发终止的问题

DSH 把三套测试串行放入一个 shell 调用，其中首个 `node test/run-tests.js` 持续超过 90 秒仍未结束。监督方确认对应 Node 子进程一直存活；停止 DSH 后子进程才被清理。

这说明 DSH 创建的长时模拟缺少明确执行上限。按故障即重置协议，第 2 轮立即终止，未读取或修改游戏实现来帮助其修复。

### 第 3 轮前置修复

- 清空第 2 轮全部游戏和测试文件。
- 原始用户目标明确规定：任何单次自动测试必须在 30 秒内结束；超过 30 秒必须主动终止并如实报告，不能用无上限模拟阻塞开发。
- Project Brain 项目规则同步加入这一执行预算提示，使其成为每个新 session 可见的仓库级约束。

## 第 3 轮：已终止

DSH session：`session-fe8a1885-39d6-41c0-b3a2-6fc5e8331841`

### 已验证事实

- 新会话收到同一非技术游戏目标、`PB-TRIAL-001` 外部工具硬门禁和 `PB-TRIAL-002` 单次测试上限。
- DSH 自主选择 Godot 4.6，并加载 `cli-anything-godot` skill 检查引擎、导出模板和 CLI 环境。
- 监督方在会话开始前和停止后分别确认，试验仓库根目录都只有 `.git` 与 `.project-brain`；DSH 尚未创建游戏文件，也没有遗留 Godot 或 .NET 子进程。

### 触发终止的问题

DSH 在一个 `pwsh` 工具调用中依次查询工作目录、Node 版本和 `D:\node-v22.22.0-win-x64` 目录，并用 `Write-Host "---"` 作为分隔。PowerShell 延迟格式化对象表格，最终把两个目录的对象合并到分隔线之后的同一张表中。DSH 因此把来自 Node 安装目录的 `node_modules`、`CHANGELOG.md`、`claude`、`claude.cmd` 和 `claude.ps1` 错误归因到试验仓库。

DSH 随后执行排除 `.git` 与 `.project-brain` 的递归查询，结果为空，但没有明确撤销先前判断。监督方在约 1 分 07 秒时停止会话。该轮没有进入游戏实现阶段。

### 试验协议纠正

用户指出：由监督方在每轮失败后替 DSH 向 Project Brain 增加规则，只能证明监督方会补写提示，不能检验编程 Agent 是否会主动建立和维护项目约束。

因此，第 1 至第 3 轮保留为历史证据，但不再作为后续基线。监督方放弃尚未提交的 `PB-TRIAL-003`，并在第 4 轮前归档旧试验仓库、以同一路径创建真正的空仓库。新协议如下：

- 不预置 `.project-brain`、Project Brain 规则、测试时限或外部工具禁令。
- 只向 DSH 提供非技术游戏目标，并要求它自行使用 Project Brain 管理项目；是否初始化 Project Brain、写入什么规则、何时修订规则，全部由 DSH 决定。
- 监督方只做旁路观察、审计和事实记录；只修复 Project Brain 源码或接入缺陷，不再替 DSH 编写项目规则。
- DSH 是否能发现、初始化并正确使用 Project Brain，本身属于能力试验结果。
