# ADR-0035：受治理外部执行必须先进入进程树容器

- 状态：Accepted
- 日期：2026-08-14

## 背景

ADR-0008 的第一版 Runner 在 Unix 启动独立 process group，Windows 则在超时后调用
`taskkill /T`。这只能表达“发现超时后尝试递归终止”，不能证明进程从启动起就受约束，也不能覆盖
根进程正常退出但子孙进程继续运行的情况。若子孙继承 stdout/stderr 管道，等待输出 EOF 甚至会把
清理推迟到子孙自行退出以后；此时再校验产物会把并发写入窗口误当成已经关闭。

## 决策

1. Semantic、Engine、Build、Test、Runtime 的所有外部执行继续统一通过一个入口，但底层改为
   `processkit = 3.3.2` 的 `ProcessGroup`。Windows 使用 Job Object，并以 suspended spawn、加入 Job、
   resume 的顺序消除 spawn 后再绑定的竞争窗口；Unix 使用可用的 cgroup/process-group 机制。
2. 进程树容器创建或受控 spawn 失败时拒绝执行，不能静默退回普通 `std::process::Command`、shell、
   `taskkill` 或只终止根 PID。
3. stdout/stderr 在后台持续排空。每个流仍只在内存保留前 1 MiB，但对完整原始字节流累计长度并计算
   SHA-256；输出上限不能阻塞子进程，也不能把截断后的摘要冒充完整摘要。
4. 单独等待根进程退出，不等待继承管道的子孙先关闭。根进程无论自然退出、非零退出、信号退出或
   超时，必须先以零宽限清空并确认整个容器，再等待输出泵完成并返回结果。
5. 只有进程树清空、输出收尾和退出状态一致性都通过后，上层才可以继续验证 SCIP、TRX、runtime
   result 或 RuntimeArtifactBundle。任何清理失败都 fail-closed 为基础设施错误。
6. `processkit` 与 Tokio 作为精确版本依赖进入锁文件；Project Brain 自身继续 `unsafe_code = forbid`，
   平台原生句柄操作封装在经过版本固定的依赖边界内。

## 验收不变量

- 根进程确认子孙已启动后自然退出，API 返回后子孙不能继续写逃逸标记。
- 根进程确认子孙已启动后超时，API 返回后子孙不能继续写逃逸标记。
- 两个证明都直接递归启动当前 Rust 测试程序，不依赖 shell、`taskkill` 或平台专属脚本。
- 输出超过 1 MiB 时，保留字节有界、`total_bytes` 覆盖完整流、SHA-256 与完整输入一致。
- 上述测试必须在 Windows、Linux、Intel macOS 与 Apple Silicon macOS CI 上通过。

## 安全边界

这是进程生命周期与进程树收敛合同，不是通用 OS 沙箱。它不阻止已信任工具访问网络、用户可访问
文件、注册表或其他系统资源；Unix process-group fallback 也不能阻止恶意子进程主动 `setsid` 逃逸。
因此仓库测试代码、build script、proc macro、语言 indexer 与运行时仍是独立信任面。后续若加入
网络、文件系统、令牌或权限隔离，必须建立新的显式合同，不能把本 ADR 的 Job Object/process-group
证明扩大解释为完整沙箱。

## 后果

- 删除 Windows `taskkill /T` 与 Unix `/bin/kill` 的事后补救路径。
- 正常退出与超时共享同一条确定性清理顺序，产物校验不再先于子孙进程收敛。
- 依赖体积与 async runtime 增加，但换来 race-free Windows Job Object 和跨平台、可回归的进程树合同。
