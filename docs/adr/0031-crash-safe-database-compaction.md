# ADR-0031：可恢复、逻辑等价的数据库物理压缩

## 状态

Accepted

## 背景

V7 pair-first lineage 曾为歧义 group 物化笛卡尔积。V9 已能在保留 decision/evidence 审计的前提下，
把满足严格资格的旧 candidate/evidence 压缩成 group/member，但 SQLite 删除页面只进入 freelist，不会
自动缩小 `brain.db`。现场自举库约 522 MB，包含 225,867 个 candidate 和同量 evidence；在逻辑压缩
之前 freelist 为 0。直接运行 `VACUUM` 既不能证明哪些记录可删，也缺少并发、崩溃、幂等和替换验证。

## 决策

1. 逻辑压缩与物理压缩保持两个显式命令。物理维护永不删除 lineage 记录，也不能替代 V9 的资格证明。
2. `database stats` 使用 `SQLITE_OPEN_READ_ONLY + query_only` 和一致性读取事务；不调用普通 store
   初始化，不迁移 schema、不设置 journal mode、不 checkpoint。
3. `database compact` 默认 dry-run。预演读取页面/freelist/关键行数，遍历 schema 和所有表内容生成
   顺序稳定的 SHA-256 逻辑清单，计算数据库文件 SHA-256 和磁盘预算。`--full-check` 额外执行
   `PRAGMA integrity_check`；quick check 和 foreign-key check 始终执行。
4. apply 必须同时提供 `--apply`、非空 request ID 与 `--human-confirmed`。参数摘要绑定 project key、
   full-check 与备份策略；同 request/同参数恢复或重放，同 request/不同参数 fail-closed。
5. 所有普通 Project Brain 数据库访问持有共享维护锁；apply 等待有界时间取得独占锁。未完成或失败的
   外部操作日志会阻止普通 Hook/CLI，但仍允许只读 stats 和使用相同 request ID 恢复。
6. 在创建候选前先检查可用空间。原子替换需要额外候选副本和原子写临时副本；默认备份还需要一份
   原库，因此以数据库大小的 2 倍或 3 倍加当前 WAL 大小，再乘 1.2 安全余量估算。
7. 独占窗口内先执行 `PRAGMA wal_checkpoint(TRUNCATE)`；busy 非零时拒绝。非 WAL 数据库返回的 -1
   帧数显式记为 unavailable。原子提交前再次 checkpoint、复核源逻辑/文件身份；所有本进程 SQLite
   handle 关闭且 WAL 为 0 后，才在紧邻 commit 的窗口清理 WAL/SHM sidecar。
8. SQLite 以 `VACUUM INTO` 在数据库同目录生成确定命名候选。源库与候选库必须具有相同当前 schema、
   schema 对象清单、全部用户表及 `sqlite_sequence` 的逐值逻辑清单、零 foreign-key violation，且
   quick/full integrity 检查通过。候选生成期间源文件或逻辑清单漂移即拒绝切换。
9. 默认在成功 checkpoint 后以原子文件写保留原库备份；`--no-backup` 只接受显式选择。已验证候选再流式写入目标的
   同目录原子临时文件，刷新缓冲、同步临时文件并原子替换目标；Windows 使用 replace-existing，Unix
   使用同文件系统 rename。标准库不能提供统一的目录项 write-through 证明，因此报告明确标注突然断电
   时目录项持久性取决于平台，不把进程崩溃原子性夸大成跨平台断电事务。提交前后均复核源、现目标和
   最终目标 SHA-256，不采用 delete-then-rename 窗口。Windows
   sharing violation/permission busy 使用 50/100/250/500/1000 ms 有界退避；仍失败保持 `verified`，不回退
   或破坏源库。操作日志先保存替换前已存在的精确命名原子临时文件基线；失败或崩溃恢复只清理由本次
   替换新增的临时文件，不触碰基线文件。
10. 操作日志位于被替换数据库之外，状态为 `running / verified / swapped / completed / failed`。
    `verified` 表示候选与未漂移源均通过，`swapped` 只表示文件替换完成，`completed` 还要求重开后的最终
    逻辑/文件验证通过。completed request 重放会重新验证当前库，并区分“仍是当时目标”和“此后合法演进”，
    不会把历史成功报告冒充当前数据库快照。
    日志原子写入并保存源/目标统计、逻辑清单、文件哈希、WAL 结果和确定性文件名。恢复同时比较逻辑
    清单和文件哈希；因为物理压缩前后逻辑清单理应相同，不能只靠逻辑清单判断是否已经 swap。当前
    文件既不匹配源也不匹配目标时，只有默认备份仍精确匹配源哈希才允许原子还原并重建候选；否则停机。
11. 日志里的数据库、候选和备份文件名必须从数据库名与 request SHA-256 重新派生，禁止信任可篡改
    日志提供的任意路径。临时清理也必须验证目标仍在数据库目录内。
12. 维护锁是 Project Brain 进程间的协作协议，不是通用 OS 沙箱。绕过工具直接打开 SQLite 的外部
    writer 不在证明范围内；因此 journal/report 固定记录 `external_writer_protection=cooperative_only`，
    文档和输出不得声称阻止任意进程写库。
13. journal/report 固定记录
    `replacement_durability=temp_file_synced_atomic_replace;power_loss_directory_durability_platform_dependent`，
    将可验证的临时文件同步与原子替换，同平台相关的突然断电目录项持久性明确分开。

## 验证

- 只读 stats/完整逻辑检查前后数据库字节不变。
- 含删除空洞的文件库经 `VACUUM INTO` 后，schema、总行数和完整逻辑清单完全相同，文件不增大。
- 共享锁阻止独占维护，释放后独占锁可取得。
- apply 后数据库可重开；相同请求幂等重放，不同参数碰撞拒绝。
- fixture 模拟 swap 前失败日志、已验证候选和原库备份，可用同 request 恢复；模拟 swap 后日志未完成，
  通过目标文件哈希识别并完成；模拟 swap 后目标损坏时，从精确源哈希备份原子恢复再重建。失败/未完成
  日志在恢复前阻止普通运行。
- 真实 522 MB 自举库 dry-run 在 25.4 秒内完成，quick/foreign-key 检查为 ok/0，生成完整逻辑清单，
  报告约 1.88 GB 默认备份空间预算，且没有执行 apply。

## 后果

- 大库 dry-run 会顺序读取并哈希全部逻辑内容，延迟高于 `database stats`；这是切换前证明成本，快速
  运维观察应使用 stats。
- 默认备份和原子流式替换需要额外磁盘空间，但避免在源库上原地改写，也保留首次生产维护的回退物。
- completed 日志和可选备份会保留在 `.project-brain` 且由 `brain.db*` 忽略；后续若加入保留策略，
  必须是新的显式、可审计维护协议，不能静默删除。
