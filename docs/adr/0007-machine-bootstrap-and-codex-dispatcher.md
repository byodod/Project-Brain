# ADR-0007：机器级安装、项目注册与 Codex 用户级 Dispatcher

- 状态：Accepted
- 日期：2026-08-13

## 背景

真实 Windows / Orca / Godot C# 项目验证暴露了两个生产问题：`project-brain` 未必在
`PATH` 中；把开发机绝对 exe 路径写入仓库 `.codex/hooks.json` 会污染可移植配置并导致每台
机器的 Hook 定义不同。Codex 同时加载用户级与项目级 Hook，非托管命令 Hook 又按精确定义
哈希要求审核，因此稳定的机器级命令路径比项目内绝对路径更合适。

## 决策

1. `init` 只创建仓库级项目身份、规则、Change Envelope 和显式语言/provider profile；仓库配置
   不保存 Project Brain 或外部 provider 的可执行路径。
2. `install` 创建机器级稳定 launcher、版本化 payload 和安装清单。Hook 只引用稳定 launcher，
   不依赖 `PATH`；版本目录不可原地覆盖，版本升级不改变 Hook command。`rollback` 只原子交换
   `current / previous` 指针。
3. `bootstrap` 只把已经初始化的项目根与其已提交 `project_key` 注册到本机；不得从 `cwd` 推导
   或生成新的项目身份。
4. Codex 生产接入安装到用户级 `~/.codex/hooks.json`。Dispatcher 先用 Hook stdin 的 `cwd`
   查本机注册表；未注册项目静默 NO-OP，注册匹配后再验证仓库 `project_key` 并进入 Runtime。
5. `install-hooks codex` 为五个事件追加独立 matcher group，保留未知顶层字段、已有事件和已有
   matcher group。管理归属与 handler hash 只记录在机器级 integration manifest，不扩展 Codex JSON。
6. 重复安装必须 NO-OP；检测到缺失、重复或被修改的受管 handler 时返回 Integration Drift，不
   猜测归属、不追加第二份。
7. `uninstall-hooks codex` 根据 manifest 精确 hash 只删除 Project Brain handler，并保留安装后
   用户新增的 Hook。发生漂移时默认拒绝，`--force` 只作为显式恢复操作。
8. Hook JSON、注册表和安装状态使用同目录跨平台原子替换；写入前后都执行内容 hash CAS，避免
   覆盖并发修改。解析失败时零变更。
9. 用户级 dispatcher 输入上限为 1 MiB；无法解析的输入不能定位项目，因此静默 NO-OP，不能
   对陌生仓库执行项目逻辑。
10. `doctor` 只确认安装、注册、Hook 定义和本地状态；Codex 的 Hook 信任状态没有正式机器接口，
    必须报告 `not_programmatically_verifiable`，不能声称已经受信任。

## 默认布局

```text
Windows: %LOCALAPPDATA%\ProjectBrain
Linux:   ${XDG_DATA_HOME:-~/.local/share}/project-brain
macOS:   ~/Library/Application Support/ProjectBrain

bin/project-brain(.exe)          # 稳定 launcher
versions/<version>/...           # 不可变 payload
state/install.json
state/projects.json
state/integrations/codex.json
```

`--install-root` 与 `--codex-home` 只用于便携安装、测试和管理员部署，不写入仓库配置。

## 验收不变量

- 带空格的 Windows 安装根通过 `commandWindows` 正确引用；POSIX command 使用单引号转义。
- 安装、bootstrap、Hook 安装、dispatcher、doctor 和卸载由真实 CLI 端到端测试覆盖。
- 已有用户 Stop Hook 在安装和卸载后保持字节语义不变，Project Brain handler 始终恰好一份。
- 未注册项目的 dispatcher 输出为空且退出成功；注册项目仍可同步拒绝删除 Brain 配置。
- `.project-brain/config.json` 不包含当前机器 install root 或 provider executable path。
- malformed `hooks.json` 不被替换；卸载不恢复旧文件快照，不覆盖安装后的用户编辑。

## 依据

- Codex Hooks：<https://learn.chatgpt.com/docs/hooks>（访问：2026-08-13）
- 原“决策记忆设计方案”聊天的安装/bootstrap/Hook 契约（2026-08-13）
