# B7 真实 Provider 性能验证台账

更新时间：2026-08-31。

本文记录 Dragonfly URMA Phase B 在真实 provider、node1 parent / node2 child 环境中的性能实验。
它只登记已经取得的实验数据、统计口径和由数据支持的结论；并发、多 peer、故障和 shutdown 结果在完成
前不得从本台账中的单流结果外推。

## 1. 当前有效基线

### 1.1 环境与负载

- 拓扑：node1（`90.91.177.158`）作为 parent，node2（`90.91.177.157`）作为 child；
- 负载：1 GiB 文件，256 个 4 MiB Piece，每个 Piece 使用 64 KiB URMA chunk；
- Dragonfly 路径：parent mmap source，child registered RX window direct-write + CRC32；
- Session：同一 parent 使用一条 persistent lane，同 lane Piece 顺序传输；
- 每个 performance case：2 个 warmup task + 5 个 measured task，每轮使用唯一 tag；
- child 使用 `--disable-back-to-source`，parent 在 child 启动前完成全部 task preheat；
- output 位于对应 Dragonfly storage 文件系统内，完成态 task 通过 hard link 物化，不包含跨文件系统
  1 GiB copy；
- E2E throughput 按 measured child task 的总字节数除以总 dfget wall time计算；
- task timing 分为 `dfget start -> first Piece completion`、`first -> last Piece completion` 和
  `last Piece completion -> dfget exit` 三段。

已知 manifest run ID：

- `post1-in32`：`b7-inflight32-002`；
- `post8-in32`：`b7-batch32-002`。

其余四组由同一轮汇总结果登记；原始 run ID 未随汇总一起记录，后续若从服务器归档 manifest，应回填。

### 1.2 E2E 与任务级时间分解

以下是当前有效的单 parent / 单 child / 单 lane 基线。时间单位为 ms，吞吐单位为 MiB/s。

| case | aggregate MiB/s | E2E | startup | Piece span | tail |
|---|---:|---:|---:|---:|---:|
| `post1-in16` | 1604.14 | 638.35 | 26.84 | 600.73 | 10.78 |
| `post1-in32` | 1958.40 | 522.88 | 26.48 | 486.70 | 9.70 |
| `post1-in64` | 1686.71 | 607.10 | 25.76 | 571.72 | 9.62 |
| `post8-in16` | 1418.58 | 721.85 | 26.40 | 686.44 | 9.01 |
| `post8-in32` | 2047.45 | 500.13 | 26.25 | 465.29 | 8.59 |
| `post8-in64` | **2410.47** | **424.81** | **25.14** | **390.04** | 9.64 |

有效结果表明 output-copy 修复后，startup 约为 25--27 ms、tail 约为 9--11 ms；不同参数的主要差异
集中在 Piece span。`post8-in64` 中 Piece span 占 E2E 约 91.8%，startup + tail 只占约 8.2%。

### 1.3 Parent TX 分解

单位为每 Piece 的均值 ms；`windows`、`ring` 和 `overlap` 为每 Piece 均值。

| case | windows | ring | overlap | fill | send | recv-post | credit | WR post | CQE | Piece total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `post1-in16` | 4.0 | 2.0 | 3.0 | 0.992 | 1.767 | 0.529 | 0.395 | 0.184 | 0.656 | 2.247 |
| `post1-in32` | 2.0 | 2.0 | 1.0 | 0.918 | 1.130 | 0.466 | 0.059 | 0.117 | 0.486 | 1.819 |
| `post1-in64` | 1.0 | 1.0 | 0.0 | 1.055 | 0.866 | 0.110 | 0.033 | 0.081 | 0.641 | 2.085 |
| `post8-in16` | 4.0 | 2.0 | 3.0 | 1.053 | 2.018 | 0.557 | 0.433 | 0.167 | 0.858 | 2.519 |
| `post8-in32` | 2.0 | 2.0 | 1.0 | 0.946 | 0.965 | 0.486 | 0.054 | 0.094 | 0.327 | 1.662 |
| `post8-in64` | 1.0 | 1.0 | 0.0 | **0.808** | **0.474** | **0.103** | **0.017** | **0.034** | **0.318** | **1.411** |

这些字段的计时范围存在 overlap，不能相加后当作 `Piece total`。它们用于定位等待来源，而不是互斥的
CPU attribution。

### 1.4 Child RX/Storage 分解

单位为每 Piece 的均值 ms。

| case | RX window wait | digest | pwrite | recycle | Storage total |
|---|---:|---:|---:|---:|---:|
| `post1-in16` | 1.521 | 0.412 | 0.405 | 0.195 | 1.985 |
| `post1-in32` | 0.938 | 0.412 | 0.625 | 0.064 | 1.568 |
| `post1-in64` | 0.821 | 0.413 | 0.412 | 0.027 | 1.358 |
| `post8-in16` | 1.784 | 0.414 | 0.409 | 0.185 | 2.223 |
| `post8-in32` | 0.843 | 0.411 | 0.510 | 0.079 | 1.372 |
| `post8-in64` | **0.453** | 0.409 | 0.402 | **0.019** | **0.940** |

digest/write 与 receive pipeline 存在并行，子项同样不能简单相加。`post8-in64` 的 Storage total 明显
低于 TX Piece total 和 Piece completion gap，当前单流更接近 TX 侧限制。

### 1.5 Piece completion gap

单位为 ms。

| case | mean | p50 | p95 | max |
|---|---:|---:|---:|---:|
| `post1-in16` | 2.349 | 2.350 | 2.541 | 2.636 |
| `post1-in32` | 1.926 | 1.926 | 2.125 | 2.259 |
| `post1-in64` | 2.172 | 2.213 | 2.393 | 2.529 |
| `post8-in16` | 2.629 | 2.602 | 2.931 | 3.140 |
| `post8-in32` | 1.813 | 1.817 | 2.215 | 2.382 |
| `post8-in64` | **1.504** | **1.501** | **1.752** | **1.933** |

## 2. 统计口径限制

E2E 和 `taskTimingSummary` 明确只统计 5 个 measured task。当前 TX/RX/gap 汇总脚本读取整轮 daemon
日志；此前观测到 TX/RX 样本数为 `1792 = 7 * 256`、gap 样本数为 `1785 = 7 * 255`，因此这些内部
分解包含 2 个 warmup 和 5 个 measured task。

这不影响当前参数趋势判断，但内部表与 measured-only E2E 不是完全相同的样本集合。进入并发阶段前，
分析脚本应按 measured task ID 过滤 daemon 日志，warmup 只保留原始证据、不参与汇总。

当前还没有登记：

- parent/child CPU 利用率与核分布；
- NIC/UB 链路速率和端口计数器；
- page-cache 冷热状态的严格控制；
- Dragonfly TCP/RDMA 同口径数据；
- 多 lane aggregate throughput 和公平性。

因此本轮不能宣称达到链路线速，也不能直接与 demo 的 standalone file benchmark 数字比较。

## 3. 已作废的跨文件系统 output-copy 数据

早期 runner 将 Dragonfly storage 放在 `/var/lib/dragonfly-b7/...`，将 dfget output 放在
`/tmp/dragonfly-urma-b7/...`。真机 `st_dev` 分别为 64768 和 47，hard link 不可能成功，dfget 在所有
Piece 完成后又执行一次完整文件 copy。

`b7-batch32-001` 的 sample-001 时间线为：

```text
14:13:03.311179  最后一个 URMA Piece 完成
14:13:03.311276  scheduler 确认 256 Pieces 完成
14:13:03.758104  1 GiB output copy 完成
```

最后 Piece 到 copy 完成耗时 446.925 ms。该 run 的五次 task timing 为：

```text
startup mean       25.992 ms
Piece span mean   550.155 ms
tail mean         458.071 ms
E2E mean         1034.218 ms
aggregate          990.12 MiB/s
```

工具提交 `2e1890f` 将每个角色的 output 移到其 storage 文件系统。修复后的
`b7-batch32-002` 明确记录 hard-link success，tail 降至 8.595 ms，aggregate 提升到
2047.45 MiB/s。

以下旧 E2E 数字均混入跨文件系统的 1 GiB copy，只作为问题发现历史保留，不得用于当前性能基线或参数
排序：

| 旧 case | aggregate MiB/s | 状态 |
|---|---:|---|
| baseline/post1-in16 | 854.19 | 作废：包含 output copy |
| post1-in32 | 992.71 | 作废：包含 output copy |
| post1-in64 | 862.35 | 作废：包含 output copy |
| post8-in16 | 905.79 | 作废：包含 output copy |
| post8-in32 | 990.12 | 作废：包含 output copy |

`b7-smoke-002` 的 988.61 MiB/s 同样不作为性能基线；该轮价值是确认 1 GiB 内容一致性和 URMA 正常
路径。早期 selected-evidence 汇总还曾混入 preheat/双方日志，人工按角色核对后 parent 正常 Piece 数为
256；后续工具已修复 evidence 作用域和 warmup/preheat 顺序。

## 4. 当前数据支持的结论

1. **当前单流最优为 `post8-in64`。** E2E aggregate 为 2410.47 MiB/s，mean Piece gap 为
   1.504 ms。
2. **`postListSize` 与 inflight/window 形态强耦合。** post8 相对 post1 在 in16 下退化约 11.6%，
   在 in32 下提升约 4.5%，在 in64 下提升约 42.9%，不能把 post8 视为所有窗口形态的固定最优值。
3. **post1-in64 的退化与 ring overlap 消失一致。** in32 为两个 window、ring=2；in64 的 4 MiB
   Piece 恰为一个 window，ring=1，TX fill 与 SEND 无跨 window overlap。
4. **post8-in64 显著压低单窗口发送开销。** 相比 post1-in64，其 TX Piece total 从 2.085 ms 降到
   1.411 ms，CQE wait 从 0.641 ms 降到 0.318 ms，mean gap 从 2.172 ms 降到 1.504 ms。
5. **当前单流优化重点在稳态 TX。** `post8-in64` 的 TX fill 为 0.808 ms，占 TX Piece total 的主要
   部分；RX Storage total 为 0.940 ms，低于 TX Piece total 和 completion gap。
6. **不继续把单流细调作为并发前置。** post4/16/32 sweep 可在并发结果表明仍由 batching 限制时再做；
   当前应进入同 lane 排队、TX fan-out 和 RX fan-in 验证。

## 5. 并发阶段的基线与待验证假设

并发测试保留两个单流对照：

- 控制组：`post1-in32`，1958.40 MiB/s；
- 当前优化候选：`post8-in64`，2410.47 MiB/s。

默认 slot size 为 64 KiB，TX 预算为 8 MiB（128 slots），RX 预算为 32 MiB（512 slots）。
`post8-in64` 的一个 4 MiB Piece 首窗口需要64个 slots，因此数据产生以下待验证假设：

- 一个 parent process 在默认 TX 分区下最多同时满足两个 in64 required TX window；第三条 active lane
  可能收到 `BufferUnavailable`/BUSY 并触发 fallback；
- 一个 child process 在默认 RX 分区下最多同时满足八个 in64 required RX window；
- 同一 child、同一 parent 的多个 task 仍由一个 `SessionSlot` 顺序执行，只能验证排队，不能代表多 lane
  transport concurrency；
- fan-out（一个 parent、多 child daemon）用于验证共享 TX pool/JFC/owner fairness；fan-in（多 parent
  daemon、一个 child）用于验证共享 RX pool、Storage 和 digest 并发。

上述内容均为下一阶段假设，不得在真实并发数据取得前标记为 PASS 或生产容量结论。
