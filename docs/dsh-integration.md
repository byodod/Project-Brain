# dsh 接入、远程安装与验收

本文给出 dsh 的独立安装与验收路径。dsh 接入证明、Codex 接入证明和 Project Brain 的包来源证明是
三件不同的事，不能互相替代。

## 前置条件

先确认目标 dsh CLI 和其 profile 包管理器可用：

```text
dsh --version
pnpm --version
```

dsh 的 `plugin --profile <name> add/remove` 会调用 pnpm。Project Brain 不安装或修改 dsh、Node.js、
Corepack 或 pnpm；缺少这些前置工具时会保留现状并明确失败。

Windows 上，Project Brain 会按 PATH 目录顺序查找 `dsh.exe`、`dsh.cmd`、`dsh.bat`。如需固定某个
dsh 安装，可将 `PROJECT_BRAIN_DSH_EXECUTABLE` 设置为精确文件路径。不要把 PowerShell 的
`dsh.ps1` 解析结果当作原生进程一定能够启动的证明。

## 验证 npm 远程来源

来源验收应在空目录中安装明确版本，并直接调用该目录的 npm shim。不要调用 PATH 中可能已经存在的
`project-brain`，也不要用源码构建或仓库内 `target/release` 替代 npm 包：

```text
npm install --prefix <empty-directory> @byodod/project-brain@<version>
<empty-directory>/node_modules/.bin/project-brain --version
<empty-directory>/node_modules/.bin/project-brain install
```

需要更强证据时，继续记录 `npm view @byodod/project-brain@<version> dist.integrity`、实际 tarball
SHA-256，以及 `node_modules/@byodod/project-brain/vendor/<target>/` 中原生二进制的 SHA-256。
这些证据只证明包来源；它们不证明 dsh lifecycle 已接入。

## 安装 dsh profile Plugin

profile 必须显式选择。例如当前 dsh preset 为 `cordis`：

```text
project-brain --dsh-profile cordis install-hooks dsh
project-brain --dsh-profile cordis doctor dsh
project-brain capabilities dsh
```

不要为验证 dsh 而安装 Codex Hook。只有同一仓库确实还使用 Codex 时，才独立执行
`project-brain install-hooks codex`。

## 合格证据

dsh 接入至少需要同时证明：

1. 指定 profile 的 `@project-brain/dsh-plugin` 依赖和 bundle 声明存在；
2. `doctor dsh` 中 `adapter_hooks` 通过，目标 bundle 与机器级 launcher 哈希匹配；
3. `capabilities dsh` 只声明已经实现的能力；
4. 真实或隔离 fixture 覆盖 `agent/pre-step`、`tools/pre-execute`、`tools/post-execute`、
   `agent/turn-stopping` 四个治理边界；
5. 工具前 deny、工具后 context 和 Stop continuation 的实际返回值符合协议。

`codex-probe`、Codex app-server 会话或 Codex Hook 审计都不能充当上述 dsh 证据。若项目另有 Codex
验收，应单独标记并设置确定性超时，避免它阻塞 dsh 安装判断。

## 已观察到的安装问题

| 问题 | 分类 | 处理 |
|---|---|---|
| 在错误工作目录执行源码构建 | 操作问题 | 来源验收不允许源码构建；必须从隔离 npm 目录调用 shim |
| 把本地源码 0.2.2 安装当作 npm 远程安装 | 证据混淆 | 分开记录 npm 包来源、机器级 install 和 dsh lifecycle 三组证据 |
| dsh profile 初始化时找不到 pnpm | 前置条件 | 先由用户安装或启用 pnpm，再重试；Project Brain 不擅自安装工具链 |
| Windows PATH 中只有 npm 的 `dsh.cmd`，裸 `dsh` 启动失败 | Project Brain 缺陷 | 自动发现 `.exe/.cmd/.bat`；仍可用精确环境变量覆盖 |
| `doctor` 因语义 Provider 未绑定而显示 degraded | 配置状态 | 单独检查 `adapter_hooks`；Provider 绑定属于语言证据配置，不等同于 dsh Hook 失败 |
| Codex probe 长时间无进展 | 外部验收问题 | 不计入 dsh 结果；Codex 验收必须设置超时并独立记录 |

