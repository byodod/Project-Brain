# ADR-0023：Build Evidence 使用固定合同与独立仓库代码信任

## 状态

Accepted

## 背景

Semantic、Engine 与 Build 证明不同事实。`dotnet build` 和 `cargo build` 即使使用固定 argv，仍可能
执行仓库控制的 MSBuild task、build.rs 或 proc macro；把 executable 已锁定误当成“不执行仓库代码”
会形成错误安全边界。Python 的语法有效性又不需要 import 或执行项目模块。

Build 还必须区分“完整观测到构建失败”和“机器工具链无法完成合同”。前者可以成为确定性 error
finding，后者只能 advisory。对于 Godot C#，编译事实必须引用已实际验证的 Engine 快照，而不是仅凭
文件扩展名猜测项目类型。

## 决策

1. Build 是独立 Evidence Plane。仓库配置不能提供 command、args、shell、script 或环境变量。
2. v1 只提供三个内置 adapter：
   - `.NET`：单个 `.csproj`，Debug、no-restore、no-incremental、disable-build-servers；
   - Rust：workspace、all-targets、frozen、机器 scratch target；
   - Python：isolated mode 下逐文件 `compile()`，不 import、不 exec。
3. `.NET` 与 Rust 的 execution class 为 `RepositoryBuildCode`，要求
   `trust_local_executable + trust_repository_build_code`；Python 为 `CompilerOnly`。
4. executable 必须是仓库外绝对普通文件并固定 SHA-256。运行环境清空后只恢复 adapter allowlist；
   HOME、临时目录、.NET CLI home 与输出目录指向机器 scratch。
5. `.NET --no-restore` 只复制白名单 NuGet restore metadata 到 scratch；不复制旧 DLL 或生成源码。
   NuGet package cache 是显式机器输入，仓库不能改写其位置。
6. ArtifactSet 递归记录最终输出普通文件的相对路径、大小与 SHA-256，并限制文件数和总大小；链接或
   越界路径拒绝。`.NET obj` 中含 scratch 绝对路径的 cache 不进入权威产物清单。
7. 运行前后 worktree 指纹或 executable hash 漂移时丢弃结果。
8. `coverage=complete` 不代表成功。项目构建错误为 complete error finding；工具链/准备状态不可用为
   partial warning finding。失败证据先持久化，CLI 再返回非零。
9. Godot C# 可要求唯一 fresh+complete+deterministic Engine head，并把该 fingerprint 写入 upstream。

## 验证

- Godot 4.6 C# 项目：真实 Engine upstream 后，隔离 `.NET 9.0.308` Debug build 得到 0 warning、
  0 error；连续构建的最终 `bin/Debug` 文件哈希一致，只有 4 个 obj cache 因 scratch 路径变化而不同，
  因此这些 cache 被排除。项目仓库没有新增跟踪修改，也未执行 export。
- 本仓库 Rust workspace：全新临时 target、`--workspace --all-targets --frozen` 成功，记录 2996 个
  产物条目，完成后 scratch 删除。
- Python 工具目录：isolated compile validation 成功，合同显示 `compiler_only + validation_only`。
- 缺少 linker、SDK 或 assets 的 fixture/真实失败均归类为 unavailable，不提升为项目违规。

## 后果

- Runtime Provider 可引用一份明确成功、无 error finding 的 Build Snapshot，而不会把 complete 误读为成功。
- 构建不会隐式下载依赖、运行测试、启动应用或执行引擎 export。
- v1 `.NET` 暂不接受多项目 solution；多项目必须先定义逐项目 restore/output 隔离与聚合身份。
