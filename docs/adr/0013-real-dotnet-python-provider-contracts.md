# ADR-0013：真实 .NET/Python Provider 契约与完整包固定

## 状态

Accepted，2026-08-14。

## 背景

合成 SCIP fixture 只能验证消费者，不能证明真实 producer、Windows 路径、固定 argv 与机器级 Runner
能够共同工作。对最小 C# 与 Python 仓库执行真实资格验证后，发现四个此前被合成测试遮蔽的问题：

1. Windows `Path::canonicalize()` 产生的 `\\?\` verbatim path 被直接传给 scip-dotnet 0.2.14，
   后者把它当 URI 解析并以 `UriFormatException` 退出。
2. scip-python 0.6.6 使用 `--cwd` 与 `--output`；旧 adapter 仍传位置参数目录，实际索引的是 Runner
   临时目录，形成零 Document 的空索引。
3. SCIP 协议允许空 occurrence range；scip-python 用 `[0,0,0,0]` 表示模块定义，而消费者错误拒绝。
4. 只固定 `node.exe + index.js` 不足以证明 scip-python 的实际代码未漂移，因为薄入口会加载
   同包 `dist/` 下的传递 bundle。

此外，官方 `@sourcegraph/scip-python` 0.6.6 在原生 Windows 启动时会把 `path.sep == "\\"`
直接传给 `RegExp`，产生无效正则。该缺陷在 2026-08-14 的上游 main 仍存在；Project Brain 不应
静默下载或修改 producer。

## 决定

1. Project Brain 内部仍使用 canonical path 做边界判断；交给外部 producer 的 argv 与 Node script
   path 去除 Windows verbatim 前缀，UNC 路径转换为等价 `\\server\share` 形式。
2. scip-python 固定 argv 改为 `index --cwd <root> --project-name <project_key> --output <index.scip>`。
3. SCIP importer 接受有序的空半开范围；仍拒绝负数、倒序、越界与非 UTF-8 边界。
4. scip-python 的 Node launcher 模式必须指向 `@sourcegraph/scip-python` package.json 声明的 bin
   入口。绑定时递归遍历包目录，拒绝符号链接与特殊文件，在 20,000 文件/512 MiB 上限内按规范
   相对路径、文件长度与内容哈希生成确定性 manifest。每次 trust check 与执行前重新计算；任何
   bundle 漂移都要求显式 `--replace`。不带 script 的原生 executable 仍按单文件哈希契约处理。
5. 含 package manifest 的 registration ID 使用 v2 域分隔；无 manifest 的既有 Rust/.NET 绑定继续
   使用 v1 算法，避免无关资格失效。读取绑定时重新推导 registration ID，拒绝被篡改的 ID。
6. Project Brain 不内置 scip-python Windows 补丁。真实 Windows 验证可绑定外部、明确审计且整包
   固定的修订版；报告必须说明它不等同于官方原包支持。
7. SQLite schema v11 修正 source attestation 身份：唯一键纳入 trust、registration、executable 与
   artifact。相同语义快照由新绑定重跑时，即使图不变也必须追加新证明；最新证明不能继续指向旧
   registration。V10 历史证明按原 sequence 无损迁移。

## 证据

- scip-dotnet 0.2.14：两次运行均覆盖 1/1 C# 源文件，Document 数 4，完整 Document manifest 与
  semantic snapshot fingerprint 一致；正式 index 导入 3 个定义。
- scip-python 0.6.6 审计修订版：两次运行均覆盖 2/2 Python 源文件，Document 数 2，SCIP artifact、
  Document manifest 与 semantic snapshot fingerprint 一致；正式 index 导入 6 个定义。
- 未修订的官方 scip-python 0.6.6 在同一 Windows 机器的 `--version` 阶段稳定复现
  `Invalid regular expression`，因此不能标记为 `stable_complete`。

这些证据来自隔离、已提交的最小 Git fixture 与独立机器安装根，不写入 Project Brain 自举数据库。

## 后果

- .NET 的真实 Windows Runner 链路已具备端到端证据，不再只依赖合成 SCIP。
- Python 的 Project Brain adapter/consumer 链路已具备端到端证据，但官方 producer 的原生 Windows
  可用性仍受上游缺陷阻塞；支持声明必须区分两者。
- scip-python 绑定与验证会额外读取并哈希整个包，绑定/doctor 延迟增加，但消除了薄入口之外的
  未固定执行代码。
- 真实 producer 升级、包文件变化或补丁变化会生成新的 registration ID，并强制重新资格验证。
