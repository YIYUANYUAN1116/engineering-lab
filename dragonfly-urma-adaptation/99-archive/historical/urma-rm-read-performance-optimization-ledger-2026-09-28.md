# Dragonfly URMA RM READ 性能优化台账

> 历史文档（2026-09-28 归档）：旧独立 RM READ 优化台账已完整汇入 [真实 Provider 主台账](../../04-performance/history/real-provider-performance-ledger.md) 第 16 节；当前完成度见 [RM READ status](../../03-implementation/rm-read/status.md)。原技术内容保留，按原文日期阅读。

建立：2026-09-28。前 50 批实现与验证记录已归档至[历史档案](../../03-implementation/rm-read/worklog-2026-09.md)；当前实现和安全约束见[状态索引](../../03-implementation/rm-read/status.md)与[设计文档](../../02-architecture/dragonfly-urma-rm-read-design-and-roadmap.md)。本台账从性能问题和可复现证据出发，不按代码提交次数编号。

## 口径与门禁

每次实验必须写明：run ID、Dragonfly/B7/UMDK 提交或构建标识、Parent/Child 节点及内核/uburma/ubcore/liburma、设备/EID/TP、source backing、文件系统、绑核、Piece 大小、CC、READ WR 大小、pwrite cap、预热与样本数。至少检查完整性、成功/尝试数、fallback 和 retained owner，再比较性能。

分别报告完整 dfget aggregate、`toREAD`、READ→首 Piece、首末 Piece span、tail、READ envelope、pwrite envelope。单片阶段统计不相加当作批次墙钟：READ 与 pwrite、CRC 与 pwrite 均可能重叠。跨进程时间戳不可直接相减。低频埋点优先使用现有每 Piece 结束事件中的聚合字段，不增加逐 WR/逐窗口日志。

比较 SEND/RECV 时使用其独立分支构建，在**相同机器、拓扑和 workload**下采集；既有单机 RTP、16 MiB/CC32 的约 7.57 GiB/s 与双机 CTP、32 MiB/CC16 READ 结果不是严格 A/B。

## 冻结基线

| run / 条件 | aggregate | 可解释的阶段事实 |
|---|---:|---|
| read-src-013，source shim 副本、绑核、32 MiB/CC16 | 3965.9 MiB/s | Parent register p50 24.42 ms；copy 3.84 ms；Child offer p50 25.99 ms。 |
| read-src-014，direct mmap、同 case | 5391.5 MiB/s | Parent register p50 1.29 ms；copy 0；Child offer p50 2.25 ms。 |
| read-src-016，direct mmap、双机 CTP、32 MiB/CC16 | 5766.8 MiB/s | READ envelope 69.33 ms；pwrite envelope 86.31 ms；重叠 52.43 ms；31/32 次 pwrite 在最后 CQE 前启动。 |
| read-cc4/8/16-001，32 MiB Piece | 6028.0 / 6231.8 / 5579.0 MiB/s | 高 CC 改善 READ envelope，但写入 envelope 到 CC8 已约 80 ms；aggregate 含不同前导。 |
| read-pwr4/8/16-001，CC16 | 5961.1 / 5554.6 / 6136.7 MiB/s | 限流改变单片等待与写入耗时分配，未显著缩短约 81–85 ms 批次 pwrite envelope。 |
| read-batch32/8/4-001，CC16 | 5893.9 / 5399.4 / 6182.1 MiB/s | 每 Piece 1/4/8 WR，owner 批量提交成立；READ envelope 约 65–68 ms，未呈随 WR 数改善的趋势。 |

以上来自不同轮次，不能跨行直接计算优化百分比。原始命令、样本和勘误见[历史档案](../../03-implementation/rm-read/worklog-2026-09.md)第四十至五十批。

## 当前测量准备（2026-09-28）

READ 分支工作区已增加低频字段：Child 每 Piece 的 `progress_query_count/progress_query_ns/post_command_ns/poll_sleep_ns`，以及 READ lease 的 `pwrite_start_ns/pwrite_end_ns/writeback_start_ns`（相对该 Piece Storage 阶段开始）。字段附在已有完成事件上，不增加逐 WR 或逐窗口日志。离线测试 316 passed、1 个真机测试 ignored；这些新字段尚无真机结果，现有 B7 汇总脚本尚未展示它们。代码仍在本地工作区，正式实验须记录实际提交标识。

本分支的旧 SEND/RECV 函数不参与 READ bulk data；已撤销误加在该函数上的对照埋点。SEND/RECV 基线必须由其实际运行的分支另行测量。

## 优化问题与下一实验

### P1：确认目标栈上的 source 注册成本

- 已知：196 的 file-backed 注册近似 45.15 µs/MiB、截距约 0.006 ms；198 的 file-backed 探针有约 17.7 ms/call 固定等待。两机驱动构建不同，不能从一台外推。
- 补证：198 同进程/同 context 交替测 file 与 anon；给 register/unregister 前后加 syscall marker，定位慢调用；记录真实模块路径、哈希、srcversion 以及 liburma 哈希。
- 产品 A/B：若 198 式栈属于目标环境，让 198 实际担任 Parent，对比 direct file-backed 与 **exact-Piece** anonymous staging。保持 32 MiB Piece、CC16、相同绑核和注册预算；比较 copy、register、offer、READ envelope、pwrite 与完整 aggregate。
- 判定：若 staging 的复制加匿名注册明显低于 file-backed 注册且完整下载改善，再考虑显式 `sourceBackingMode`；否则保持 direct，并调查/统一驱动栈。不得用 task 级 Segment 绕过 exact-Piece 授权。

### P2：解释 READ 与 SEND/RECV 的产品差距

- 在相同双机拓扑、同一文件系统和相同 Piece/CC 上，分别用 READ 与旧 SEND/RECV 分支测正常 CRC+写入路径；固定样本数和预热。
- READ 侧看 Parent source register/offer、Child READ 查询/post/轮询、CQE 至写入、writeback、recycle 和批次重叠；SEND/RECV 侧须采集对应窗口就绪与写入时间，不能从本 READ-only 分支的遗留函数取基线。
- 若 READ 数据面比 SEND/RECV 慢，再单独看 provider/CTP 和 JFS occupancy；若主要是写入尾部，比较实际 Storage 写入策略；若差距集中在前导，不把它归咎于 READ opcode。

### P3：优化选择门槛

当前不继续缩小 READ WR，也不因 196 数据引入 Parent source registered-slot pool。pwrite cap4/8/16 没有证明吞吐收益。任何新方案先指出要消除的**具体阶段**、预计节省的墙钟、会增加的复制/内存/授权成本，再用相同 case A/B；通过完整性和 owner 退休门禁后才讨论默认值。

## 新记录模板

### YYYY-MM-DD：问题 / 假设

- 版本与拓扑：
- 唯一变量与对照 run ID：
- workload、预热、样本、日志级别：
- 功能门：成功/尝试、CRC、fallback、retained owner：
- 完整 dfget 与分阶段墙钟：
- Parent source 与 Child READ/Storage 分解：
- 结论：证实、否定或证据不足；仅适用的栈/条件：
- 下一步与停止条件：
