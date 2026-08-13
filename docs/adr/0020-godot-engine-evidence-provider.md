# ADR-0020：Godot Engine Evidence Provider 使用真实引擎双快照探针

日期：2026-08-14

## 状态

Accepted

## 背景

文本解析只能看到 `.tscn/.tres` 的声明，不能证明 UID 实际可解析、导入产物可用、resource 能被
锁定版本的 Godot 加载，或主场景/autoload 在当前 ProjectSettings 下成立。直接导入 `.godot/`
又会把可删除缓存误当成项目权威。

## 决策

1. CLI 增加 `evidence godot`，只接受用户显式信任的机器绝对 executable；拒绝仓库内 binary、
   Windows command shim 和运行期间 SHA-256 漂移。
2. 先验证 executable 是 Godot 4 且提供 `--headless`、`--script`、`--import`，再执行真实 import。
   本 Provider 永远不构造或调用 export argv。
3. Project Brain 在机器临时目录写入固定 GDScript probe，并把 HOME、APPDATA、LOCALAPPDATA 与 XDG
   数据目录隔离到本次临时目录，避免污染仓库或依赖用户 editor 配置。
4. Probe 在加载前后各采集一次 project hash、main scene、autoload、UID、resource dependencies 与
   文件 hash；两份状态除 load result 外必须完全相同。Rust 随后再次读取所有权威文件核对 hash。
5. `.godot/`、`.git/` 不参与扫描。任何指向 `.godot/` 的依赖都产生错误 finding，绝不进入
   ArtifactGraph。
6. v1 首次交付只输出经过验证的 Engine Evidence Snapshot，不在本 ADR 内定义持久化。后续
   [ADR-0021](0021-evidence-ledger-and-hook-staleness.md) 已用独立 schema migration 和重放测试增加
   SQLite ledger 与 Hook staleness 传播。

## 验证

- 合成 fixture 覆盖 main scene、script dependency、missing dependency、source drift 与 cache
  reference。
- Windows 上使用 Godot 4.6 Mono 官方 console binary 对 Orca 游戏项目连续运行两次：两次均
  import/probe exit 0、全部声明 resource load 成功、error_count=0，source 与 snapshot fingerprint
  完全相同。
- 隔离环境修复后，项目根不再产生 `Godot/` editor 配置目录。

## 后果

- Project Brain 首次拥有超出源码/SCIP 的真实引擎事实；其后续治理闭环由 ADR-0021 定义。
- 引擎执行会正常重建 `.godot/` cache；该目录仍是可删除派生物，不构成 evidence authority。
- Build 与 Runtime Plane 后续可以引用本 Engine Snapshot，而不是把“编译通过”和“场景可加载”
  混成同一事实。

## 依据

- Godot 4.6 command line：<https://docs.godotengine.org/en/4.6/tutorials/editor/command_line_tutorial.html>
- Godot 4.6 ResourceLoader：<https://docs.godotengine.org/en/4.6/classes/class_resourceloader.html>
- Godot ResourceUID：<https://docs.godotengine.org/en/stable/classes/class_resourceuid.html>
- Godot FileAccess：<https://docs.godotengine.org/en/stable/classes/class_fileaccess.html>
- Godot data paths：<https://docs.godotengine.org/en/4.6/tutorials/io/data_paths.html>

以上页面访问于 2026-08-14。
