# ADR-0039：框架特有能力使用外部 Evidence Provider 进程协议

日期：2026-08-15

## 状态

Accepted

## 背景

核心需要支持 .NET、Python、Rust 及其它项目，但不能为每种框架增加类型、CLI 分支和运行逻辑。
动态库会把 ABI、panic、allocator 与版本漂移带入治理进程；当前引入 WASI 又会扩大跨平台实现成本。

## 决策

1. 采用独立进程 JSON stdin/stdout `Provider Process Protocol v1`；
2. Provider 必须先通过 `describe` 声明稳定 ID、版本、合同版本和 Evidence plane 能力；
3. 机器绑定固定 `project_key + profile_id`、descriptor hash、executable SHA-256、authority ceiling 和 revision；
4. 绑定必须显式 `--trust-local-executable`，不同绑定必须显式 `--replace`；
5. `run` 只接收核心解析并复制到机器私有 staging 的声明输入，以及 opaque config；
6. 响应是 Evidence candidate，不是最终 Snapshot；核心验证身份、版本、payload hash、ArtifactGraph、
   input manifest 和 Source TOCTOU 后重新构造并事务化保存；
7. Provider 不能声明最终 finding authority。确定性 claim 受机器 authority ceiling 与仓库规则双重限制；
8. executable 漂移、崩溃、超时、截断、非法 JSON、错误 schema 或身份不匹配都不产生 trusted head；
9. Provider 私有 payload 不被核心解析，只作为内容寻址 artifact 保存。

## 验证

- 协议 crate 覆盖版本、身份、payload hash、成功/失败响应约束；
- `reference_provider` example 提供最小可执行实现；
- CLI 黑盒 fixture 验证 bind、isolated staging、run、持久化和声明输入修改后的 stale；
- 启发式 ceiling 无法把 deterministic claim 提升为 hard authority；
- executable 与 Source 运行中漂移时结果丢弃。

## 后果

核心保持框架中立；具体框架可作为独立仓库或安装包演进。v1 containment 不是通用恶意代码沙箱，
因此只能运行用户明确信任且哈希固定的机器 executable。
