# ADR-0037：Production Qualification Control Plane v1

日期：2026-08-14

## 状态

Accepted

## 背景

单元测试、CI 编译和 adapter fixture 只能分别证明局部实现。Project Brain 还需要回答一个更严格的
部署问题：当前这组二进制字节、协议合同、数据库 schema 与运行平台，是否真实通过了项目隔离、
at-least-once 重放、并发交错、Provider 漂移、Stop 防循环和长会话稳定性边界。

这不是项目事实，也不是 Source/Semantic/Engine/Build/Test/Runtime 的第七个 Evidence Plane；若把
“控制面自测通过”写入项目 `brain.db` 或赋予规则权限，就会混淆治理对象与治理者自身的资格。

## 决策

1. 增加固定 `control-plane-v1` 套件，包含且只包含七项版本化用例：
   Q1 adapter contract compatibility、Q2 project isolation、Q3 replay/idempotency、
   Q4 concurrent interleaving、Q5 provider drift、Q6 Stop-loop boundedness、
   Q7 long-session stability。
2. Q4 使用固定种子交错 32 个 session、每 session 100 个 operation，共 6,400 个 pre/post event，
   并验证全部 3,200 个 operation 的因果对；Q7 写入 10,000 个事件、重开数据库核对零丢失，并比较
   首末四分位 p95。
3. 资格 target 精确绑定当前 executable SHA-256、Project Brain version、Hook/config/database contract
   manifest、OS 与 architecture。任一项改变都需要新资格；运行期间 executable target 或项目 Source
   上下文变化只能得到 `Inconclusive`。
4. 最终状态只有 `Qualified`、`Failed`、`Inconclusive`。中断的 `running`、失败和不确定结果都不等于
   Qualified；不提供 skip、ignore 或 force-qualified 开关。
5. 结果写入机器级 `<ProjectBrainData>/state/qualification.sqlite` schema v1。request ID 与 target hash
   组成幂等请求；同 ID 不同 target 拒绝。run 只允许从 `running` 原子收口一次，case 记录不可修改或
   删除，终态报告保存并复验 SHA-256。
6. `qualification run/status/show` 是显式 CLI；普通 Hook 不读取资格账本，资格本身永远不产生项目
   hard block。`doctor --require-qualified` 才要求当前 target 存在精确 Qualified 证明；普通 doctor
   将缺失资格保持为 warning。
7. release matrix 在打包前对每个平台的最终 release binary 运行完整资格套件。资格失败即不产生正式
   发布制品。

## 验证

- storage tests 覆盖同 event ID 相同载荷收敛、不同载荷串行/并发碰撞拒绝，以及 v17→v18 精确哈希迁移。
- qualification tests 覆盖七项套件、精确 request replay、request collision、终态/用例不可变和当前
  target status。
- CLI 黑盒运行必须输出七项逐项证明并以 Qualified/非 Qualified 映射成功/失败退出码。
- workspace test、strict Clippy 与四平台 release qualification 是合并/发布门禁。

## 后果

- “能编译”与“该二进制已经通过生产控制面资格”成为两个可机器区分的事实。
- 资格运行会产生有意的 SQLite 压力和几十秒执行时间，因此只在显式命令与 release 构建执行，不进入
  每个 Hook 热路径。
- 机器账本是部署证明而非远程认证或防恶意管理员篡改的签名系统；哈希与不可变 trigger 用于检测意外
  漂移，不能替代操作系统权限或远程 attestation。
