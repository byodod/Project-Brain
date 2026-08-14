# ADR-0034：lineage 逻辑删除前必须完成全库 Online Backup

日期：2026-08-14

## 状态

Accepted

## 背景

ADR-0033 已把 V7 legacy lineage 压缩绑定到人工批准的精确 deletion plan，但真实自举数据库首次执行仍会
删除约 22.5 万条 candidate/evidence。逻辑资格证明、事务原子性和后续物理压缩彼此独立；即使删除计划
正确，也必须在误操作、程序缺陷或事后审计时提供完整的删除前恢复点。

SQLite WAL 模式下，裸 `.db` 文件不一定包含最新已提交页面。先 checkpoint 再复制会改变在线状态，并在
checkpoint 与加锁之间产生竞态；`VACUUM INTO` 又会把逻辑备份和物理重写混为一个协议。允许
`--no-backup` 则会让最高风险的首次迁移失去不可绕过的恢复保证。

## 决策

1. `lineage compact-legacy-proposals --apply` 必须创建删除前全库备份；不提供跳过备份的参数。
2. apply 取得项目级独占协作锁后，在主连接 A 上开启 `BEGIN IMMEDIATE`，重新计算并验证批准计划，再生成
   删除前全库逻辑清单。到此为止只允许读取，不得写 group、run、审计或删除任何行。
3. 连接 B 以只读方式打开同一源库，调用 SQLite Online Backup API 写入机器级备份目录。A 阻止新写者，
   B 读取同一个已提交状态；该流程不执行 WAL checkpoint，不复制裸数据库文件，也不调用 `VACUUM`。
4. 备份根目录固定为项目工作树之外的
   `<install-root>/state/backups/lineage-compaction/`。database instance ID 持久化在 metadata；backup ID
   绑定 instance ID、request ID、批准计划 hash、删除前逻辑清单和备份协议版本。短目录名只用于避免
   Windows 路径长度问题，完整身份仍由 backup ID 承担。
5. Online Backup 先写确定性 `.partial.sqlite3`。只允许删除本次身份精确生成的普通 partial 文件；最终
   `.sqlite3` 通过同目录 hard-link 的 create-new 语义发布，禁止覆盖既有目标。已存在最终文件只有在
   quick check、外键检查和全库逻辑清单全部匹配时才可重用。
6. 备份必须在发布前和发布后各验证一次。源事务与备份的全库逻辑清单必须相等，`quick_check` 必须为
   `ok`，外键违规必须为零。备份文件 SHA-256 记录制品 provenance，但不与 WAL 模式源 `.db` 的裸文件
   SHA-256 比较。
7. 只有备份验证成功后，A 才能在同一事务写 group/member、compaction run/group 审计并执行精确计数的
   evidence/candidate 删除。schema v17 的 run 审计绑定 backup ID、相对路径、备份 SHA-256、删除前和
   备份逻辑清单；随后一次提交。存储触发器拒绝缺少任一证明或前后逻辑清单不相等的 operation v2+
   run，防止绕过 Rust API 写入伪成功审计。备份失败时事务回滚且没有压缩 DML。
8. 同 request 的事务在备份发布后崩溃，可以按确定性路径复验并重用最终备份；最终备份不会自动删除。
   同 request 成功重放先重新验证原备份的路径边界、文件 SHA-256、逻辑清单和完整性，再返回已提交
   报告且不创建第二份备份；备份缺失或漂移时 fail-closed。
9. dry-run 只读取备份根最近的现有祖先并报告空间，不创建目录或文件。空间预算在备份卷和源 WAL 卷上
   分别保留 `1.2 × 当前数据库大小 + 64 MiB`；同卷时要求两份预算之和。

## 验证

- 非空 WAL 下 Online Backup 成功，备份前后 WAL 字节数不变，证明流程没有 checkpoint。
- A 持有 immediate 事务时第二写者失败，B 仍能完成只读备份。
- 备份和源事务全库逻辑清单相同，且另一个 project_key 的符号数据完整保留。
- 冲突最终文件不被覆盖，候选、evidence、group 和 compaction run 都不改变。
- 备份完成后事务回滚，再用相同 request apply 会复验并重用备份。
- CLI 拒绝 lineage 的 `--no-backup`，并拒绝把机器级备份根放入项目工作树。
- v16→v17 迁移保留旧 compaction run；旧行的备份证明为空，新 apply 行必须完整。

## 后果

- 首次真实 225k 逻辑压缩需要额外的机器级磁盘空间和一次全库读取，但获得了可独立恢复、可审计的删除
  前状态。
- 备份是逻辑删除协议的一部分，不替代 `database compact` 的 crash-safe 物理文件维护协议；后者仍可
  独立选择是否保留物理替换前备份。
- 标准库无法对所有文件系统证明突然断电后的目录项持久性；本协议承诺进程级不覆盖发布与内容复验，
  不夸大为通用 OS 沙箱或跨平台断电 write-through。
