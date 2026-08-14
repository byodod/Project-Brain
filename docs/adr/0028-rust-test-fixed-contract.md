# ADR-0028：Rust Test 绑定 Build head 并固定离线 Cargo 合同

## 状态

Accepted

## 背景

`cargo test` 不只是运行测试二进制；它还会编译 `cfg(test)` 代码、执行 build.rs 与 proc macro，并可能通过测试代码访问机器资源。因此 Rust Test 不能复用 CompilerOnly 权限，也不能接受仓库提供的 command、args、environment 或 runner。另一方面，稳定版 libtest 的默认结果是文本摘要，无法可靠区分断言失败、显式 panic 与 harness 异常；把所有非零退出直接提升为确定性规则违规会违反 EvidenceFinding 与 Rule Effect 分离原则。

## 决策

1. 新增 `cargo-test.<profile>` Test Provider v1。它要求指定 `cargo-build.<build_profile>` 当前 head，并验证 Build 为 fresh、complete、deterministic、无 finding，Source fingerprint 与当前工作树一致，cargo executable SHA-256 一致。
2. Build Snapshot 为每种 Build adapter 增加规范 `build_target` artifact。Rust Test 要求该 artifact 的内容身份精确等于当前项目相对 `Cargo.toml`；旧 Build head 缺少此绑定时必须重跑 Build，不能猜测 target。
3. adapter 固定执行：

   ```text
   cargo test
     --manifest-path <PROJECT_ROOT>/Cargo.toml
     --workspace
     --all-targets
     --frozen
     --target-dir <MACHINE_SCRATCH>/target
   ```

   同时设置 `CARGO_NET_OFFLINE=true`、`CARGO_INCREMENTAL=0`，清空环境后只保留机器 allowlist。仓库不能注入 feature、package、filter、runner、shell、network、install 或额外参数。
4. 本合同属于 `repository_test_code`。调用方必须分别确认本机 cargo executable 与仓库测试代码；Project Brain 当前不是通用 OS 沙箱。
5. Provider 只解析有界 UTF-8 输出中的完整稳定版 libtest `test result:` 计数，并聚合多个 test binary。状态区分 passed、failed、crashed、timed_out、no_tests、provider_failed；coverage 区分 covered、partial、empty、unknown，ignored/filtered 测试产生 partial。
6. libtest 文本 failure 无法证明是声明断言而不是 panic/harness 异常，因此 `rust_test_failed` 保持 advisory。无摘要、输出截断、超时和非断言进程失败也不能获得 deterministic_violation 权限。未知 finding 仍不产生隐式 effect。
7. 执行前后重算工作树 fingerprint，并重新校验 cargo executable SHA-256；任何源码或工具漂移都丢弃结果，不提交 Evidence。

## 验证

- 单元测试覆盖多 harness 汇总、NoTests 与 failure advisory 分类。
- 全 workspace fmt、clippy `-D warnings` 与 tests 作为提交门禁。
- 真实 Provider 验收先生成 `cargo-build.workspace-debug`，再运行 `cargo-test.workspace-tests`；重复相同输入必须得到相同 Test fingerprint 与 `snapshot_inserted=false`。

## 后果

- Rust Test 有了独立、可审计的固定合同，并真实依赖 Build Evidence，而不是把任意仓库脚本包装成测试。
- v1 可以可信报告完整 cargo/libtest 观测，但不会把文本失败冒充结构化断言。若未来需要 hard-block eligible 的 Rust 断言，必须新增 adapter-owned 结构化结果协议并提升 contract version。
- Python Test Provider 已由 ADR-0029 定义独立 manifest/bootstrap 合同。
