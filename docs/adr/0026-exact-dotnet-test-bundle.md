# ADR-0026：.NET Test 从 Build CAS 运行精确程序集，不重新进入构建

## 状态

Accepted

## 背景

`dotnet test --no-build --no-restore` 仍会进入 MSBuild 项目求值，并依赖当前 `obj`、target framework 与
restore metadata。Project Brain 的 Build 在机器 scratch 中完成后会删除这些中间态；若 Test 再读取
仓库当前 `obj`，便无法证明运行的是 Build Evidence 已验证的同一组字节。

## 决策

1. .NET Build CAS manifest 增加规范 `build_target`。旧 bundle 没有该字段时仍可供既有 Runtime 使用，
   但不能成为 Test v1 的输入。
2. `.NET Test v1` 要求指定 fresh、complete、deterministic、无 finding 的
   `dotnet-build.<build_profile>` head，并核对当前 Source fingerprint、Build provider、target、dotnet
   executable SHA-256 与 bundle 内明确存在的测试程序集。
3. Provider 将 CAS bundle 物化到机器临时目录，然后固定执行：

   ```text
   dotnet vstest <exact-bundle-assembly>
     --Logger:trx
     --ResultsDirectory:<machine-scratch>
     --nologo
   ```

   不执行 build、restore、项目脚本、shell 或 export，也不接受自定义 runner 参数。
4. 测试程序集本身属于 `repository_test_code`，必须独立显式信任。环境被清空并把 HOME、APPDATA、
   DOTNET_CLI_HOME、NuGet cache、TEMP/TMP 指向本次 scratch；这不是 OS 沙箱，测试代码仍可能访问网络
   或机器上用户可访问的其他资源。
5. TRX 文件数、大小、UTF-8 与 Counters 结构有固定边界。状态区分 passed、failed、crashed、timed_out、
   no_tests、provider_failed；覆盖区分 covered、partial、empty、unknown。NoTests 不是 Pass。
6. TRX v1 汇总不能可靠区分断言失败与测试代码/环境异常，因此 `dotnet_test_failed` 虽为 error，authority
   仍是 advisory，不能经 mapping 获得 hard-block 资格。后续只有 adapter-owned 的结构化断言协议才能
   产生 `deterministic_violation`。
7. 超时会终止进程树并保存 partial Test Evidence；输出截断、无 TRX 或非法 TRX 保存
   `provider_failed`，而不是静默丢失或伪装成项目失败。

## 验证

- 单元测试覆盖 TRX pass/no-tests/failure 计数、路径逃逸和 profile 边界。
- 真实 .NET 9 受控夹具先生成 fresh Build bundle，再从 CAS 精确物化并执行：1/1 通过得到
  `status=passed, coverage=covered`。
- 同一夹具改为失败断言后得到 0/1 通过、`status=failed`，Evidence 持久化后 CLI 返回非零；finding
  保持 advisory。
- 无测试 class library 得到 `status=no_tests, coverage=empty`，不是 pass，且同样持久化。

## 后果

- Test 运行字节与 Build Evidence 一致，不依赖仓库 `bin/obj` 或第二次构建。
- .NET Test 的 TRX 边界保持不变；Godot scenario 由 ADR-0027 定义独立结构化合同，Rust Test 由
  ADR-0028 固定离线 Cargo 合同，Python Test 由 ADR-0029 固定 manifest/bootstrap 合同。
- 当前通用 CAS 类型沿用历史 `RuntimeArtifactBundle` 名称；协议上它已承担受控 execution bundle。后续
  若改名必须保持 manifest 与既有 Runtime 的向后兼容。
