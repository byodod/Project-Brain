# ADR-0024：Runtime 使用机器级 CAS 与隔离项目镜像绑定精确 Build 字节

## 状态

Accepted

## 背景

只在 Build Evidence 中记录文件哈希还不足以执行 Runtime。Build 的 machine scratch 会被删除，仓库
中的 Godot `.godot/mono/temp/bin/Debug` 又可能来自其他构建。若 Runtime 再执行一次 `dotnet build`，
它观测的是一次新的执行，不能证明运行了原 Build 验证过的字节。Godot C# loader 仍从项目数据目录的
固定位置加载程序集，当前没有稳定的命令行参数把运行程序集任意重定向到历史输出目录。

## 决策

1. 成功的 Godot C# Build 在 scratch 删除前，将最终输出闭包逐文件提升到机器级 content-addressed
   store。object key 是文件 SHA-256；写入使用 machine lock、临时文件、sync、原子 commit 与提交后
   重哈希。
2. Build 生成规范排序的 `RuntimeArtifactBundle v1`，包含项目、Build provider、Source fingerprint、
   ArtifactManifest fingerprint、总字节数、全部 `(relative_path,size,sha256)`，以及从
   `project.godot [dotnet] project/assembly_name` 得到的主程序集路径和 SHA-256。
3. bundle manifest 自身按规范 JSON 字节取得 content fingerprint，并以该 fingerprint 原子存储。
   Evidence ArtifactNode 绑定这些完整字节，不写入机器绝对路径。
4. 历史 Build Evidence 不因本机 object 被清理而改写。Present、Evicted、Corrupt 是独立机器状态；
   Runtime 准备阶段必须重读 manifest 并校验每个 object。不可用时拒绝运行，不自动重建。
   Build 成功但 CAS 提升失败时仍保存 Build 观测，标记 `incomplete + runtime_bundle_unavailable`，CLI
   返回非零；它不成为可运行 Build。
5. Runtime v1 将从权威 Source manifest 建立机器私有 staged project，拒绝链接、junction/reparse、旧
   `.godot`、`bin`、`obj` 与 Project Brain 状态；随后把 bundle 精确物化到
   `.godot/mono/temp/bin/Debug/`。
6. 物化使用物理复制而不是 hardlink；复制后重哈希，并在 import 前后、Runtime 前后重复验证。Godot
   import 若改写 bundle 中任一文件，准备失败。
7. Runtime 使用锁定 Godot executable 与固定 argv。禁止 `--build-solutions`、restore、build、test、
   `--script` 和全部 export/release 参数；v1 运行项目声明的 main scene，不接受仓库自定义 argv。
8. staging、run journal、日志和 `user://` 位于 machine private run root。若权威源码已有
   `override.cfg`，v1 拒绝合并。清理只能在数据库记录、run marker 与精确 run root 三者匹配时执行。

## 验证

- 真实 Godot 4.6 C# 项目的 5 个最终文件、6,181,663 字节已提升到独立测试 CAS；bundle fingerprint
  为 `sha256_2c90a21c720f69a9c2311e3e4d1694d5e6b68ff3daae7497ec32d05b98be6b06`。
- `game.dll` 在 Build manifest、CAS object 与主程序集 attestation 中均为
  `02941be7557c283808e3930775393728e368b06e9b7fbb8d71fc56090e007409`。
- 单元测试覆盖成功提升、完整重校验、主程序集缺失拒绝、规范 manifest 身份；全 workspace test 与
  `clippy -D warnings` 通过。

## 后果

- Runtime 能证明实际加载候选与先前 Build Evidence 是同一组字节，而不是“同样源码又构建了一次”。
- CAS 会消耗机器空间；自动 GC 在 pin、quota、grace 与 crash recovery 合同实现前保持禁用。
- 当前只为 Godot C# Build 创建 Runtime bundle；Rust/Python Build Evidence 不伪装成 Godot 可运行包。
- 隔离 staged Runtime Provider 尚需按本 ADR 后半部分实现，CAS 完成不等于 Runtime 闭环已完成。
