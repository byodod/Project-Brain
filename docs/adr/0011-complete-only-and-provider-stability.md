# ADR 0011：只有完整且可重复的 Provider 输出可成为语义事实

## 状态

Accepted

## 背景

真实 `rust-analyzer scip` 在同一 Cargo workspace、同一源码上曾输出不同的 Document 集合。进程
exit 0、SCIP 可解析，甚至多次运行的 Document 并集看似完整，都不能证明这些 Document 来自同一个
一致的语义世界。ADR 0010 让 partial 可见，但仍允许其进入 semantic snapshot；这会让局部语义被
误用为 latest baseline。

## 决策

1. 新 SCIP 导入只有 `coverage.status == complete` 才能调用 store transaction。`partial` 与
   `unverifiable` 在任何 SQLite mutation 前失败。
2. Provider 稳定性验证默认重复 5 次，固定源码指纹、registration ID/revision 与 executable
   SHA-256，同时比较完整 Document path set 和完整 provider-neutral semantic snapshot fingerprint。
3. 稳定性验证只写机器级 Provider audit，不提交 semantic snapshot，不产生 lineage，不移动 latest。
4. 重试只用于诊断。禁止把多次不完整 workspace 输出取 union，也不以相同 Document count 代替
   path set 相等。
5. 单次 workspace index 仍是默认快速路径。Package shard 只可能作为后续显式 fallback；在每个
   shard 的 source partition、coverage 与冲突规则可证明以前，不进入默认 contract。

## 后果

- Provider 成功但漏文件不再污染最新语义基线。
- 不稳定 Provider 会以可重放的运行、二进制和源码证据显式失败。
- 某些语义 Producer 天生不为无 occurrence 文件输出 Document；这类项目需要后续 Required/Optional/
  Excluded source expectation，而不能放松 complete-only 门禁。
