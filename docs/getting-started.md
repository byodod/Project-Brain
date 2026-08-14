# 快速开始与 Agent 接入

本文说明如何安装 Project Brain、初始化仓库，并接入 Codex、Pi、OpenCode 或 dsh。

## 1. 获取与安装

推荐通过 npm 安装官方跨平台包：

```text
npm install --global @byodod/project-brain
project-brain --version
```

临时运行可以使用：

```text
npx @byodod/project-brain --version
```

npm 包直接携带四个平台的原生 Rust CLI，不运行 `postinstall` 下载脚本。也可以从
[GitHub Releases](https://github.com/byodod/Project-Brain/releases) 下载当前平台压缩包，并使用同一
Release 中的 `SHA256SUMS` 校验归档。将 `project-brain` 或 `project-brain.exe` 放到可执行路径后运行：

```text
project-brain install
```

该命令把当前二进制安装到机器级稳定位置，并创建供各 Agent 调用的 launcher。可用
`--install-root` 显式覆盖机器级安装根。

npm 分发支持 Windows x64、Linux glibc x64、macOS Intel x64 和 macOS Apple Silicon arm64；其它平台
应使用源码构建。

## 2. 初始化仓库

在目标仓库根目录运行 `init`。语言 profile 只声明项目事实，不会自动下载 Provider 或工具链：

```text
project-brain init --profile rust
project-brain init --profile dotnet
project-brain init --profile python
```

多语言项目可以一次声明多个 profile：

```text
project-brain init --profile dotnet --profile python
```

初始化会创建 `.project-brain/config.json` 及必要结构。随后注册当前项目：

```text
project-brain bootstrap
```

项目身份由 `project_key` 表达；工作目录、Agent session ID 或 event ID 都不能替代项目身份。

## 3. 安装 Agent 接入

只安装实际需要的接入：

```text
project-brain install-hooks codex
project-brain install-hooks pi
project-brain install-hooks opencode
project-brain --dsh-profile default install-hooks dsh
```

接入位置：

- Codex：`$CODEX_HOME/hooks.json`。Project Brain 为五个生命周期事件追加自身拥有并逐项哈希的 group，
  不覆盖用户 handler；这不是企业 managed hook，仍受 Codex 自身的 hook trust 约束。
- Pi：`$PI_CODING_AGENT_DIR/extensions/project-brain/index.ts`，默认位于
  `~/.pi/agent/extensions/project-brain/index.ts`。Pi 没有正式停止前 veto；`agent_end` 后 follow-up 是最多
  一次的模拟续轮。
- OpenCode：`$OPENCODE_CONFIG_DIR/plugins/project-brain.js`，默认位于
  `~/.config/opencode/plugins/project-brain.js`。
- dsh：通过 `dsh plugin --profile <name> add/remove` 管理指定 profile，不修改其它 profile。

四个接入都调用稳定 launcher 的 `dispatch` 入口。未注册项目返回 NO-OP；已注册项目发生治理或审计错误
时，工具前事件失败关闭。

可用全局参数显式覆盖 Agent 配置根：`--codex-home`、`--pi-home`、`--opencode-home`、`--dsh-home`。

## 4. 检查安装

对每个已安装接入分别运行 doctor：

```text
project-brain doctor codex
project-brain doctor pi
project-brain doctor opencode
project-brain --dsh-profile default doctor dsh
```

查看 Agent 的机器可读能力：

```text
project-brain capabilities codex
project-brain capabilities pi
project-brain capabilities opencode
project-brain capabilities dsh
```

Pi 的 `continue_after_stop` 为 `emulated`，OpenCode 为 `unsupported`；这是上游生命周期边界，不是安装
故障。

## 5. 卸载接入

卸载只删除 Project Brain 管理且哈希匹配的片段。检测到用户修改或内容漂移时默认拒绝，只有确认目标
后才应使用 `--force`：

```text
project-brain uninstall-hooks codex
project-brain uninstall-hooks pi
project-brain uninstall-hooks opencode
project-brain --dsh-profile default uninstall-hooks dsh
```

卸载 Agent 接入不会删除项目的 `.project-brain/brain.db`、审计历史或仓库规则。

## 下一步

- 配置规则和理解阻断权限：[架构说明](architecture.md)
- 接入语言或框架分析：[Provider 与 Evidence](providers.md)
- 数据库维护与资格验证：[运维、资格与发布](operations.md)
