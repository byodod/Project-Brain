# ADR-0030：.NET Build 从项目根解析固定 SDK

## 状态

Accepted

## 背景

`.NET` CLI 根据进程工作目录向上查找 `global.json`。Build Provider v1 虽然把 `.csproj` 作为绝对参数
传给 `dotnet build`，但版本探测与构建都从 machine scratch 启动，因此会绕过仓库根的 `global.json`。
现场项目明确固定 .NET SDK 8.0.410，旧合同却探测并使用机器默认 9.0.308；Evidence 内部一致，却没有
执行项目声明的工具链约束，不能称为可复现构建。

## 决策

1. Build Provider contract 升级为 v2，并在合同中显式记录 `working_directory_policy`。
2. `dotnet --version` 与固定 `dotnet build` 必须使用同一个项目根工作目录，使 `global.json` 同时约束
   版本身份和实际构建。
3. bin、obj、NuGet restore metadata、CLI home 与临时目录仍全部位于 machine scratch；不允许因为
   工作目录变化而把构建产物写回仓库。
4. Build 前后继续比较完整 worktree fingerprint；仓库构建代码若修改 Source，结果必须丢弃。
5. Cargo 与 Python 的工作目录保持 machine scratch；本 ADR 不借机开放仓库 command、args、env、
   restore、network 或 export。

## 验证

- 单元测试固定 `.NET = project_root`、Cargo/Python = `machine_scratch` 的策略映射。
- Darter 现场项目根 `global.json` 固定 8.0.410；v2 版本探测与真实 Build report 必须共同报告 8.0.410，
  不再回落到机器默认 9.0.308。
- 同一固定合同仍产生完整 artifact manifest 与 Runtime CAS bundle，项目工作树不新增构建输出。

## 后果

- provider contract version 变化会使旧 Build head 失去同合同权威；下游 Test/Runtime 必须消费重跑后的
  v2 Build head。
- `global.json` 是 Source fingerprint 的一部分；修改 SDK 约束会同时改变 Source 与 provider version，
  形成可审计的工具链迁移。
