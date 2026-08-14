# ADR-0032：不透明工具结束后以 Source 指纹对账 Evidence authority

日期：2026-08-14

## 状态

Accepted

## 背景

Evidence head 保存产生快照时的 `source_fingerprint`，但原 PostTool 逻辑只对明确
Create/Modify/Delete 失效。Bash/shell 的代码生成、脚本写入或 Git switch/checkout 被归类为
Execute/GitOperation，可能改变源码却让旧 Build/Test/Runtime Evidence 继续显示 fresh。这样旧 finding
仍可能参与硬门禁，属于当前 authority correctness 漏洞；retention 只能决定历史保留多久，不能修复
“当前还能否相信”。

## 决策

1. 结构化 Create/Modify/Delete 直接失效全部 Source-bound Evidence（Source、Semantic、Engine、Build、
   Test、Runtime）；失败或 unknown status 也可能部分写入，因此不因 vendor 状态跳过。
2. Execute、GitOperation 与 Unknown 在 PostTool 重新计算 `git::worktree_fingerprint`，逐一与当前 non-stale
   heads 的 `source_fingerprint` 比较。只把不一致的 fresh/unknown heads 原子标记为 stale，匹配的当前
   provider 不因其它 provider 旧账被误伤。
3. Git、路径或文件读取使当前指纹无法计算时，不能保留 hard authority。SQLite v16 将 fresh heads 标为
   unknown，并在失效事件中保存 `freshness=unknown`、实际 head 身份与 Source 观察；已证明不一致保存
   `freshness=stale`。同 event ID 不能在两种观察间重放。stale 是比 unknown 更强的已知事实；后续验证
   不可用不得把 stale 覆盖回 unknown。
4. 不解析 shell 命令文本，不维护危险命令名单，不在 PreTool 猜测副作用，也不要求 P0 持久化前后
   manifest。当前 fingerprint 足以裁决旧 Evidence 是否仍对应当前 Git Source；精确路径差异是后续
   explain/impact 能力。
5. 自动对账是单调降权：fresh→stale、fresh→unknown、unknown→stale；不会因为源码后来恰好切回旧
   fingerprint 自动恢复 stale/unknown。只有重新运行对应的受信任 Provider 才能恢复 fresh。
6. changed paths 仅作为审计辅助：可从当前 HEAD diff 获得时记录，无法获得时允许为空；authority 判断
   只依赖完整 Source fingerprint，而不是这份路径样本。
7. `evidence_heads.freshness` 被定义为 persisted freshness，而不是权限结论。Session/Intent/PreTool/Stop、
   Finding hard gate、`--require-engine` 与 `evidence status` 必须现场取得当前 Source 指纹并计算
   effective freshness。只有 persisted fresh + 当前指纹可验证 + snapshot Source 相同，才具备 fresh
   authority；因此即使 PostTool 漏报，旧 head 也不能硬阻断。
8. 生产 CLI 提升 Provider head 前再次取得当前 Source 指纹。若 run 结果绑定的指纹已经漂移，则在任何
   snapshot/attestation/head 写入前拒绝；若相同，则在单个 SQLite 事务内提升新 head、stale 同项目其它
   不同 Source 指纹的 fresh heads、记录精确 transition 身份，并传播显式 upstream 失效。未验证的通用
   snapshot apply 不作为公开生产 API。

## 验证

- 不修改 Git Source 的 shell command 保持匹配 Evidence fresh。
- 不透明 generator 修改源码后，即使命令文本没有任何已知写入关键字，PostTool 仍把 head 标 stale。
- Source 指纹无法计算时标 unknown；unknown 与 stale 使用同一 event ID 会触发幂等碰撞。
- persisted fresh 但当前 Source 不同或不可验证的 finding 永远不能 hard gate；`--require-engine` 同样拒绝。
- 延迟 Provider 结果与当前 Source 不同会在写入前拒绝；成功 promotion 会精确 stale 不兼容的其它 heads。
- unknown 再次匹配不会恢复 fresh，unknown 后确认不匹配会加强为 stale。
- 已有 Write/Create/Delete 失效、跨 plane 原子更新、重复事件和 Provider 重新证明 fresh 测试继续通过。

## 后果

- shell/Git/新未知工具及漏报 Hook 都不再形成“旧 Evidence 仍有 hard authority”的权限窗口。
- 不透明 PostTool 需要遍历 Git Source 计算指纹，延迟与仓库大小相关；这是 authority 正确性的确定性成本。
- 精准到 profile/path 的影响传播、manifest delta 与 retention 仍是后续独立协议，不能削弱本阶段的
  保守失效保证。
