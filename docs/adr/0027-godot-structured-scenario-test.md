# ADR-0027：Godot Scenario Test 使用精确 Build CAS 与结构化断言

## 状态

Accepted

## 背景

Godot headless 主场景“退出码为零”只能证明进程结束，不能证明玩法闭环、存档往返或资源约束成立。
直接把任意场景和参数塞进 Runtime 又会开放 repository-controlled argv、混淆 Runtime 与 Test Plane，
并可能在测试时重新构建出与 Build Evidence 不同的字节。

## 决策

1. `godot-scenario-test.<profile>` 只消费指定 `dotnet-build.<build_profile>` 的 fresh、complete、
   deterministic、无 finding head。当前 Source、build_target、主程序集绑定、CAS manifest 与项目身份
   必须一致；Build 必须精确引用一个匹配当前 Godot executable SHA-256 的 fresh Engine head。
2. Provider 从 Git Source manifest 物理复制项目，拒绝 link/reparse，并把 CAS 精确物化到 staged
   `.godot/mono/temp/bin/Debug`。先使用固定 argv import，再使用固定 argv 运行一个仓库内 `.tscn`。
   不接受自定义参数、shell、`--script`、restore、build 或 export。
3. 场景属于 `repository_test_code`，必须与机器 executable 分别显式信任。HOME、APPDATA、XDG 与临时
   路径被重定向到本次 scratch；这不是 OS 沙箱，仓库测试代码仍可能访问网络或用户可访问资源。
4. 场景必须写出固定 `.project-brain-test-result-v1.json`。schema 只允许 scenario_id、status 和有界
   assertion 列表；profile、ID 唯一性、字段、大小、UTF-8 与 status/assertion 一致性均由 Rust adapter
   验证。空 assertion 是 NoTests，不是 Pass。
5. 合法结构化失败断言产生 `godot_scenario_assertion_failed`，authority 为
   `deterministic_violation`。import/runtime diagnostic、缺失或非法结果、进程崩溃、超时和输出截断是
   advisory/provider failure，不能伪装成断言违规。即使是 deterministic_violation，也只有仓库 hard
   rule 精确映射 plane/provider/contract/finding code 后才可阻断。
6. import 前不得预先生成保留结果文件；执行前后重新校验 staged Source、完整 CAS、权威 worktree 和
   executable。任一 TOCTOU 漂移使整次结果拒绝提交。

## 验证

- 单元测试覆盖严格 JSON shape、未知字段、重复 assertion ID、矛盾 status 和 argv 路径脱敏。
- 全 workspace test、fmt 与 `clippy -D warnings` 作为提交门禁。
- 真实 Godot 4.6 C# 项目先生成 Engine
  `sha256_aa84d637ecb89ff7b85d0c12025ffebd0de38d66018f42ac1da90c834652ef4e` 与 Build
  `sha256_6d01e91261180f52f9f29d21bc0a54bd2c76b70c7b1bc83f7eb6c4359a5980b8`，再从 CAS 运行
  `Tests/ProjectBrainScenario.tscn`；结构化 Test Snapshot 为
  `sha256_74c3b2f8084d025e11c3ac59df983827d166a48beb07bc9b5e015d9932caf621`，1/1 assertion passed、
  coverage complete、无 finding。相同输入第二次运行得到
  同一 fingerprint 且 `snapshot_inserted=false`。全程未执行 export。

## 后果

- Project Brain 能把“场景明确声明的失败断言”和“引擎/环境/测试框架失败”分开，避免把所有红灯直接
  升格为硬门禁。
- 测试运行的是 Build Evidence 的精确字节，不依赖仓库当前 bin/obj，也不会二次构建。
- v1 只覆盖 Godot C# + `.tscn` 结构化场景；GDScript-only Build、物理输入设备与 GUI 像素证据仍是
  不同合同，不能由本 Provider 冒充。
