# ADR-0029：Python Test 使用物理 Source staging 与 adapter-owned manifest runner

## 状态

Accepted

## 背景

直接运行 pytest 会把 discovery、插件、配置文件、参数和 import side effect 全部交给仓库，无法证明
Project Brain 执行的是固定合同。研究建议使用 isolated Python runner，但 `python -I -m
project_brain_test_runner` 会同时隔离当前目录和用户 site；若 runner 未被机器级安装，模块本身无法可靠装载。
此外，import Python 模块必然执行其顶层代码，不能承诺“无 module side effect”。

## 决策

1. 新增 `python-test.<profile>` Test Provider v1。它必须引用 fresh、complete、deterministic、无 finding 的
   `python-compile.<build_profile>` head，并精确核对 Source fingerprint、Python executable SHA-256 与
   `build_target=source_root`。
2. 仓库只提供 JSON manifest，不提供 command、runner、args、shell、environment、pytest 配置或插件。
   schema v1 只接受 `schema_version` 与有界、无重复、按顺序的 `module/function`。名字必须由 ASCII Python
   标识符组成；module 必须唯一对应 source_root 内属于 Git Source 的 `module.py` 或
   `module/__init__.py`。
3. Project Brain 在 Rust 中先验证 manifest，再把 Git Source 物理复制到机器 scratch。执行前后校验原工作树
   fingerprint、staged manifest 和 Python executable hash；源码或工具漂移时丢弃结果。
4. argv 固定为 `python -I -S -B -X utf8 -c <adapter-bootstrap> <staged-source-root>
   <adapter-manifest> <result>`。bootstrap 只调用清单声明的同步、零参数、模块自有函数；函数必须返回
   None。它不进行 discovery、pip install、plugin loading 或网络操作，但仓库代码不是 OS 沙箱，仍可使用
   Python 能访问的机器资源，因此必须单独显式信任 `repository_test_code`。
5. adapter-owned 结果只记录 module、function 与 passed/assertion_failed/error，不采信仓库消息、traceback
   或自定义结果。字段、顺序、数量和文件大小均严格校验。
6. AssertionError 是显式声明测试函数内的结构化失败，产生
   `python_test_assertion_failed + deterministic_violation`；其他 exception 为 crashed + advisory，runner
   failure、非法/缺失结果、输出截断与超时为 provider uncertainty。任何 violation 仍须 fresh、complete
   Evidence 且精确命中显式 finding mapping 才能 hard block。
7. Test coverage 区分 covered、partial、empty、unknown；本合同逐项尝试全部声明函数，合法完整结果为
   covered，空 manifest 为 empty，provider uncertainty 为 unknown。

## 验证

- 单元测试覆盖 manifest 未知字段与非法标识符、结果身份/顺序、结构化断言权限、异常 advisory、NoTests
  和跨 Provider partial coverage。
- 仓库夹具先运行 `python-compile.fixture-compile`，再运行 `python-test.fixture-tests`；两项显式测试必须
  通过，重复相同输入必须产生相同 fingerprint 与 `snapshot_inserted=false`。
- 全 workspace fmt、clippy `-D warnings` 与 tests 作为提交门禁。

## 后果

- Python 项目有了不依赖 pytest 生态隐式行为的最小固定 Test 合同。
- v1 明确承认 import 会执行仓库顶层代码，并以显式 trust 与 staging/TOCTOU 约束风险，而不伪装成通用
  OS 沙箱。
- 需要 fixture、async test、parameterization、pytest compatibility 或强网络隔离时必须新增版本化合同，
  不能向 v1 注入任意参数。
