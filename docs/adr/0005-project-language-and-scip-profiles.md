# ADR-0005：项目语言与 SCIP Provider Profile

- 状态：Accepted
- 日期：2026-08-13

## 背景

项目可能同时包含 Rust、C#、Visual Basic、Python 和其他语言。SCIP producer 对
`Document.language`、symbol kind、relationship 与 role 的实际输出并不统一：scip-dotnet 使用
`C#`/`Visual Basic`，scip-python 可能留空，rust-analyzer 则不提供可依赖的 implementation/import
关系。如果依赖扩展名或一次索引中的观察结果猜测，会把不确定事实固化为错误身份和能力。

## 决策

1. `project_key` 继续作为最高项目边界；项目配置显式声明开放 language ID 和 roots。
2. 语义 provider 使用独立 profile，包含稳定 ID、格式、实际 producer、Brain contract 版本和
   逐 Document 原始语言映射。
3. 不自动扫描 Cargo、solution、project、pyproject、扩展名或 shebang 生成配置。
4. 空 `Document.language` 只有显式 missing-language mapping 才接受。
5. Provider ID 包含 profile、producer、contract 与原始 contract 摘要，既保留可读性也避免规范化
   碰撞；provider key 再包含规范 language。Producer version 仅记录 provenance，不改变 Brain contract。
6. 能力绑定 producer + language，使用 Supported、Partial、Unsupported、Unknown 四态。
7. V3b 实际执行 rust-analyzer `.scip` 验证；C#/VB 与 Python 使用贴近真实 producer 行为的合成
   fixture，不捆绑或自动运行 scip-dotnet/scip-python。
8. SCIP raw symbol 只是身份输入与 lineage evidence；lineage candidate 仅在同 project、provider、
   language 内比较，不自动复用 ID、修改墓碑或让历史规则跟随。

## 结果

- 一个项目和一个 `.scip` 都可包含多种明确映射的语言。
- Python 空语言不会被路径启发式误分类。
- `scip-rust` wrapper 不会被误认为 producer；Rust profile 只接受 `rust-analyzer`。
- 自定义语言可通过项目 profile 接入，不需要修改核心枚举。
- 能力缺口保持可见，Runtime 不会因单个 fixture 出现关系就宣称完整支持。

## 验收不变量

- 未声明 profile、producer 不符、raw language 未映射、越出 roots 均拒绝导入。
- C# 与 Visual Basic 可在同一 scip-dotnet index 中保留各自规范 language。
- Python 空 language 需要显式 opt-in。
- Provider descriptor version 表示 Brain contract，不等于 producer version。
- 不生成 producer 未明确提供的 call/import/implementation 边。
