# B7 真实 Provider 性能验证台账

更新时间：2026-09-03。

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

## 5. 并发阶段前的基线与待验证假设

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

上述内容是进入并发阶段前形成的假设；真实 fan-out 验证结果及已确认结论见第 6 节。

## 6. Fan-out 并发 lane 与 TX admission 验证

以下结果来自 node1 parent 向 node2 多 child daemon 的 fan-out correctness/performance 测试。
吞吐为各 measured batch 的 aggregate throughput；Gbps 按 `MiB/s * 8 * 2^20 / 10^9` 换算。
`required pressure`、`optional pressure`、`optional -> ring1`、session retire 和 TCP fallback
均来自 correctness run 的完整证据集合。

| case | pipeline | TX 预算 | 结果 | aggregate MiB/s | Gbps | Jain | completion skew | required pressure | optional pressure | optional -> ring1 | session retire | TCP fallback | 备注 |
|---|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| `fanout-post1-in32-l2` | 2 | TX8 | PASS | **3568.80** | **29.94** | 0.99887 | 36.2 ms | — | — | — | 0 | 0 | 2 lane 基线，多 lane 扩展成立 |
| `fanout-post1-in32-l4-pipe1-tx8` | 1 | TX8 | PASS | **6251.35** | **52.44** | 0.99872 | 57.08 ms | 0 | 0 | 0 | 0 | 0 | 4 lane、单 ring；TX8 正好容纳 4 个 required window |
| `fanout-post1-in32-l4-pipe2-tx16` | 2 | TX16 | PASS | **5197.70** | **43.60** | 0.99945 | 46.86 ms | 0 | 0 | 0 | 0 | 0 | 4 lane、双 ring，有足够 TX 预算 |
| `fanout-post1-in32-l4`（旧实现） | 2 | TX8 | FAIL | **3338.45** | **28.01** | 0.98038 | 235.0 ms | **45** | **34** | 34 | churn | **3453** | required/optional 争抢；BUSY 被当作 transport fault，导致 lane churn 和大量 TCP fallback |
| `pipe2+TX8 transient BUSY`（tracing 未修） | 2 | TX8 | FAIL | **316.22** | **2.65** | 0.95812 | 7793.8 ms | 0 | 0 | 429 | **7** | **1254** | transient BUSY 避免了原 admission churn，但 tracing panic 导致 reset/EOF 和 session failure |
| `pipe2+TX8 tracingfix` | 2 | TX8 | tools FAIL | **6225.16** | **52.22** | 0.99667 | 89.90 ms | **1445** | **1414** | 1414 | 0 | **1445** | lane 不再 retire；BUSY 只使当前 Piece fallback TCP，transport 稳定但并非纯 URMA |
| `pipe2+TX8 admissionfix` | 2 | TX8 | **PASS** | **4948.56** | **41.51** | **0.99903** | **52.04 ms** | **0** | **3956** | **1492** | **0** | **0** | 当前 correctness 基准：required admission 有保障，optional 不足时退化为 ring1，纯 URMA 跑通 |

### 6.1 数据支持的结论

1. **多 lane transport concurrency 已成立。** 2 lane 和 4 lane correctness case 均能在无 session
   retire、无 TCP fallback 的条件下完成，Jain 指数均高于 0.998。
2. **pipe2 + TX8 的原始失败是 TX admission 策略问题，不是 lane 绑定问题。** 旧实现中 required 与
   optional window 直接竞争固定 TX pool；required 失败后的 terminal failure policy 又放大为 lane churn
   和 3453 次 TCP fallback。
3. **transient BUSY 只修复了故障分类，没有构成纯 URMA admission。** tracing 修复后的 6225.16 MiB/s
   结果仍包含 1445 次 TCP fallback，因此不得作为 URMA transport 吞吐基线。
4. **required-first admission 已通过 correctness 验证。** admissionfix 将 required pressure、session
   retire 和 TCP fallback 全部降为 0；TX8 不足以维持所有 lane 的双 ring 时，optional window 主动让位，
   Piece 使用 ring1 继续走 URMA。
5. **`optional pressure` 不等于传输失败。** admissionfix 中 3956 次 optional pressure 和 1492 次
   ring1 降级是受控背压证据；该 case 仍为纯 URMA PASS。
6. **当前 pipe2 + TX8 correctness/performance 基准为 4948.56 MiB/s（41.51 Gbps）。** 相比
   pipe1 + TX8 的 6251.35 MiB/s，吞吐下降约 20.8%，反映低 TX 预算下的 admission 等待和 ring1
   降级成本；不能用包含 TCP fallback 的 tracingfix 数字评价 URMA admissionfix 的性能回退。

## 7. Fan-in 并发 lane 与 RX admission 验证

以下结果来自多个 node2 child server 同时向 node1 parent downloader 提供内容的 fan-in 测试。共享
RX pool 位于 parent process；每个 child 拥有独立 TX pool。case 均使用 `post1-in32`、pipeline depth 2、
1 个 warmup batch 和 3 个 measured batch，每个 task 为 1 GiB。

`required pressure` 表示 required RX window 等待至 transfer timeout 后仍无法获得资源；
`required wait count` 和 `required wait` 则统计最终成功但曾等待 lease 回收的 required admission。
因此 `required pressure = 0` 与 `required wait count > 0` 并不矛盾。

| case | lanes | RX 预算 | 结果 | aggregate MiB/s | Gbps | Jain | completion skew | required pressure | optional pressure | required wait count | required wait total | required wait mean | session retire | TCP fallback |
|---|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `b73-fanin-l2-pipe2-rx16-001` | 2 | RX16 | PASS | **4033.38** | **33.83** | 0.99443 | 68.05 ms | 0 | 52 | 0 | 0 ms | 0 ms | 0 | 0 |
| `b73-fanin-l2-pipe2-rx8-001` | 2 | RX8 | PASS | **2889.96** | **24.24** | 0.99743 | 61.24 ms | 0 | 1294 | 817 | 1562.55 ms | 1.913 ms | 0 | 0 |
| `b73-fanin-l4-pipe2-rx16-001` | 4 | RX16 | PASS | **3874.43** | **32.50** | 0.95845 | 443.54 ms | 0 | 1810 | 934 | 1755.23 ms | 1.879 ms | 0 | 0 |
| `b73-fanin-l4-pipe2-rx8-001` | 4 | RX8 | PASS | **3255.36** | **27.31** | 0.98897 | 285.56 ms | 0 | 3552 | 2588 | 6907.41 ms | 2.669 ms | 0 | 0 |

### 7.1 RX admissionfix 重复性

在上述预算矩阵前，`fanin-post1-in32-l2 + pipeline2` 使用默认 RX32 预算连续复跑 5 次：

- 5/5 PASS，required pressure、session retire、TCP fallback 和 previous transfer failure 均为 0；
- aggregate throughput 均值为 3654.88 MiB/s，中位数 3713.86 MiB/s，范围为
  3198.93--4315.47 MiB/s；
- Jain 均值为 0.99592，completion skew 均值为 60.12 ms；
- optional pressure 每轮为 2--456，合计 961；所有 shortage 均对应 optional 单窗口降级；
- 该组运行早于 required-wait 指标加入，因此不能从 `required pressure = 0` 反推 required wait count 为 0。

### 7.2 数据支持的结论

1. **RX required-first admission 在当前矩阵中通过 correctness 验证。** 四个 case 全部为纯 URMA
   PASS；即使 RX8 下出现 2588 次 required wait，也没有 required timeout、lane churn、session retire
   或 TCP fallback。
2. **RX16 足以覆盖 2 lane 的本轮 required 工作集，但不足以让 4 lane 始终无等待。** 2 lane RX16
   的 required wait 为 0，仅有 52 次 optional 降级；4 lane RX16 出现 934 次 required wait 和
   1810 次 optional 降级。registered lease 跨 Piece 的 Storage 消费周期存活，因此不能只按静态
   `lanes * pipeline depth * window size` 判断运行时是否无压力。
3. **RX8 已进入明显的预算受限区。** 相比同 lane 数的 RX16，RX8 在 2 lane 下吞吐下降 28.35%，
   在 4 lane 下下降 15.98%；4 lane RX8 的 required wait 总时长为 6.91 s，单次均值 2.669 ms。
4. **受控等待没有被放大为 transport failure。** 所有 case 的 required pressure、session retire、
   TCP fallback 均为 0；optional pressure 只使 receive pipeline 降级到单窗口。
5. **增加 lane 数没有在 RX16 下提高 aggregate throughput。** RX16 从 2 lane 增加到 4 lane 后，
   aggregate throughput 下降 3.94%，Jain 从 0.99443 降至 0.95845，completion skew 从 68.05 ms
   增至 443.54 ms。当前瓶颈已经进入共享 RX/Storage/主机资源，而不是缺少 transport lane。
6. **RX8 下增加 lane 可以隐藏部分等待，但显著放大争用。** 4 lane 相比 2 lane 的 aggregate
   throughput 提升 12.64%，同时 required wait count 增至 3.17 倍、总等待时间增至 4.42 倍，
   completion skew 增至 4.66 倍；这不是更充足的 RX admission，而是更多并发对等待的覆盖。
7. **矩阵的性能排序仍需重复样本确认。** 当前四个预算点各只有一次 run；它们足以确认 correctness
   和 RX8/RX16 压力级别，但吞吐、公平性和 skew 的精确差异应至少再复跑 3 次后再作为稳定基线。

## 8. 单 lane 并发 Piece 验证（B8.4）

`b84-piece-c4-shutdownfix-001` 使用一个 Parent、一个 Child 和一条 persistent lane，同时启动 4 个
独立 1 GiB task。每个 batch 从 Parent server 日志配对
`start upload piece content over urma` 与 `urma piece finished on peer lane`，transfer identity 使用
`(lane_id, transfer_id)`；只按 `transfer_id` 关联会在新 lane 从 1 重新分配后产生假 duplicate。

以下 aggregate 按三个 measured batch 的总字节数除以总 makespan 计算；Jain 和 completion skew 为
三批均值。warmup 不参与汇总。

| run | task concurrency | lanes | max active transfers | max active tasks | 结果 | aggregate MiB/s | Gbps | mean Jain | mean completion skew |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|
| `b84-piece-c4-shutdownfix-001` | 4 | 1 | **16** | **4** | **PASS** | **6157.46** | **51.65** | **0.999892** | **16.45 ms** |

三个 measured batch 的明细为：

| batch | makespan | aggregate MiB/s | Jain | completion skew | Piece start/completion | lane | overlap |
|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 647.81 ms | 6322.80 | 0.999828 | 20.99 ms | 619 / 619 | 1 | PASS |
| 2 | 664.85 ms | 6160.79 | 0.999948 | 12.74 ms | 599 / 599 | 1 | PASS |
| 3 | 682.96 ms | 5997.38 | 0.999901 | 15.63 ms | 595 / 595 | 1 | PASS |

每批均满足：`laneCount=1`、`maxActiveTaskCount=4`、`maxActiveTransfers=16`、全部 task 有 Piece start、
全部 transfer start/finish 配对，且 missing、duplicate、unfinished 均为 0。最终 manifest 为
`state=passed`、`error=null`，Parent/Child 都由 owner gate 正常停止。

第一轮 `b84-piece-c4-001` 的数据路径同样通过，但 Child 先退出时 Parent 记录
`URMA incoming transfer queue is closed`，旧 shutdown analyzer 只放行 `early eof`，因此误报
`stop-failed`。工具现仅在 shutdown offset 之后把这两类事件计为受控 peer close，并继续硬失败其他 CQE、
completion、protocol、digest、Jetty 或 panic 错误；`shutdownfix` rerun 已验证该修复。

### 8.1 数据支持的结论与边界

1. **同一 persistent lane 的 Piece 生命周期并发成立。** 4 个 task 同时活跃，server 侧最多同时存在
   16 个 transfer；这不是上层 task 并发后在 lane 上串行排队。
2. **transfer identity 必须是 `(lane_id, transfer_id)`。** `transfer_id` 是 lane-local，并会在新 lane
   上从 1 重新分配；工具已按复合 identity 统计 active、duplicate、finish 和 unfinished。
3. **本轮 fairness 和完成偏斜稳定。** 三批 Jain 均高于 0.9998，平均 completion skew 为 16.45 ms；
   aggregate 范围为 5997.38--6322.80 MiB/s。
4. **本轮没有宣称同 lane native data window 并发。** manifest 明确记录
   `nativeRxWindowConcurrencyClaimed=false`。当前 shared-JFR safety gate 仍限制同 lane 同时只有一个
   native RX window outstanding；已证明的是 control/rendezvous、Storage 准备和 Piece lifecycle 并发。
5. **下一 gate 是 completion 可验证的 native window concurrency。** 删除 safety gate 前必须先建立
   sender 提供的 transfer/chunk identity（例如经过真实 provider 验证的 64-bit `SEND_IMM`），或实现等价的
   lane-wide ordered send-ticket；不能仅依赖 RC SEND 消费 shared JFR 的 FIFO 顺序推断 Piece identity。

## 9. SEND_IMM 真实 Provider 冒烟

在 `udmac0d1e2` 上使用 UMDK `urma_perftest` 的 immediate-data 模式，对单 Jetty、RC、DUPLEX 路径完成
SEND 带宽和时延冒烟：

| test | bytes | iterations | 关键配置 | 结果 |
|---|---:|---:|---|---|
| `URMA_SEND BandWidth` | 65536 | 50000 | JFC depth 4096、JFS depth 128、CQ moderation 100 | peak **69444.85 MB/s**；average **67405.55 MB/s**；**1.078489 Mpps** |
| `URMA_SEND Latency` | 64 | 10000 | JFC depth 512、JFS depth 1 | min **2.03 us**；median **2.16 us**；average **2.18 us**；p99 **2.41 us**；p99.9 **5.47 us**；max **8.00 us** |

这两项证明真实 UDMA provider 的 RC `SEND_IMM` opcode 路径能够建连、传输并完成 CQE，且没有出现明显性能
异常。它们不证明 receive CQE 中 64-bit immediate 的 high/low 32 位均被准确保留，也不证明 immediate 与
payload、local receive `user_ctx` 的对应关系。最终 correctness gate 由 `tcp-urma-file-transfer` 分支新增的
`send_imm_probe` 承担；在该探针双向通过前，不删除 Dragonfly shared-JFR safety gate。

### 9.1 RC SEND_IMM 64-bit correctness probe

`tcp-urma-file-transfer` 分支的 `send_imm_probe` 已在真实 provider 上完成 64 和 256 message 两个点。
本轮方向均为 node2 `90.91.177.157` Child/SEND sender 到 node1 `90.91.177.158`
Parent/RECV receiver：

| messages | Child send retired | Parent receive CQE | distinct immediate | distinct RX slots | payload binding | CQE errors | 结果 |
|---:|---:|---:|---:|---:|---|---:|---|
| 64 | 64 | 64 | 64 | 64 | PASS | 0 | **PASS** |
| 256 | 256 | 256 | 256 | 256 | PASS | 0 | **PASS** |

两个点均报告 `transportMode=RC`，sender opcode 为 `SEND_IMM`，receiver opcode 为
`SEND_WITH_IMM`，`full64BitIdentity=true`，所有 WR 正常完成并由 `Ready -> Closed` 退出。
探针包含 high/low 32 位均非零的固定值和四个交错 transfer namespace，因此本轮已经证明：

1. 该方向的 receive CQE 能完整保留 64-bit immediate；
2. local RX slot 与 sender-provided identity 能够独立取得；
3. payload 可以按 immediate 精确路由，且无 duplicate、missing、unknown 或 slot reuse；
4. 256 条同时预投递的 shared-JFR receive WR 和 SEND_IMM completion 能完整 drain。

node2 重启导致反向 node1 sender 到 node2 receiver 暂未执行。该缺口不阻塞 Dragonfly 的
feature-gated/shadow 接入，但在反向通过前，不能把 `SEND_IMM` 标为双向 provider validation 完成，也不能
据此删除 shared-JFR safety gate 或宣称 native RX window concurrency 已验证。

## 10. 单 lane native RX window 与并发 Piece（B8.6）

2026-09-03 在一个 Parent、一个 Child、一条 persistent lane 上完成 `post1-in32` 的 native RX window
并发 correctness 验证。每个 fast run 使用一个 measured batch；C2 每批传输 2 GiB，C4 每批传输 4 GiB。
runner 同时验证：

- Parent server 的 Piece start/finish 使用 `(lane_id, transfer_id)` 完整配对；
- Child 的 native RX admission/release 使用
  `(lane_id, transfer_id, window_start_chunk)` 完整配对；
- 同 lane 至少两个不同 transfer 的 native RX window 同时 outstanding；
- SEND_IMM window 与 Piece 汇总一致，允许 provider 保持同 transfer 匹配，因此
  `crossTransferChunkCount` 只作为观测值，不作为并发必要条件；
- 无 BUSY/reject、session retirement、TCP fallback 或 previous transfer failure。

修复后的四个 fast correctness 点如下。前三组粘贴摘要未附 manifest run ID，故暂以重复序号登记；最后一组
明确为 `b86-fast-c4-002`，归档 manifest 后应回填其余 ID。

| run | task concurrency | 结果 | aggregate MiB/s | Gbps | Jain | completion skew | TX required pressure | TX optional pressure | RX required pressure | RX optional pressure | required RX wait count | required RX wait total | retire | TCP fallback |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| C2 fast repeat 1 | 2 | **PASS** | **6542.06** | **54.88** | 0.999981 | 2.76 ms | 0 | 512 | 0 | 167 | 0 | 0 ms | 0 | 0 |
| C2 fast repeat 2 | 2 | **PASS** | **6355.50** | **53.31** | 1.000000 | 0.12 ms | 0 | 511 | 0 | 134 | 0 | 0 ms | 0 | 0 |
| C4 fast repeat 1 | 4 | **PASS** | **8162.41** | **68.47** | 0.999789 | 17.72 ms | 0 | 759 | 0 | 115 | 3 | 4.70 ms | 0 | 0 |
| `b86-fast-c4-002` | 4 | **PASS** | **7200.44** | **60.40** | 0.999888 | 15.34 ms | 0 | 696 | 0 | 91 | 28 | 104.18 ms | 0 | 0 |

两次 C2 aggregate 均值为 6448.78 MiB/s，范围为 6355.50--6542.06 MiB/s；两次 C4 aggregate 均值为
7681.42 MiB/s，范围为 7200.44--8162.41 MiB/s。C4 均值比 C2 高 19.12%，但 C4 两次结果相差
11.79%，且每个 fast run 只有一个 measured batch，因此这些数字是 correctness/performance smoke，尚不能
替代 warmup + 三批以上 measured 数据形成的稳定性能基线。

### 10.1 Aggregate queue-depth 根因与修复

B8.6 首先暴露了两处“单 Piece window 深度”和“lane native queue depth”混用：

1. `b86-fast-001` 中 Child 将 `maxInflightChunks=32` 同时用作单 window 大小和 JFR `recv_depth`。
   一窗正好耗尽 32 个 RECV WR，1024 个 window 全部串行，表现为
   `maxActiveWindows=1`、`maxActiveTransfers=1`、512 次 optional RX 单窗降级。修复后，单 window
   仍为 32 chunks，而默认 RX32 预算下 aggregate native JFR depth 为 512，并由 per-lane semaphore
   持有 credit 至对应 receive CQE 完成。
2. `b86-fast-002` 在 RX 并发打开后，Parent 仍将单 window 的 32 chunks 用作 JFS `send_depth`。
   多 transfer 同时 SEND 时真实 provider 返回
   `post_jetty_send_imm_wr failed with status -12`（`ENOMEM`），随后共享 lane 关闭、Child 收到 connection
   reset，aggregate 仅 68.07 MiB/s。修复后，单 TX window 仍为 32 chunks，而默认 TX8 预算下
   aggregate native JFS depth 为 128，并由 per-lane semaphore 持有 credit 至全部 SEND CQE 完成。

两侧 aggregate depth 均取 registered slot 数、provider JFR/JFS capability 和配置并发需求的最小值；
required window 等待 native credit，optional pipeline window 在预算不足时受控退化。provider 在 admission
成功后若仍返回 `ENOMEM`，仍按 transport invariant 破坏 fail closed，不能降级成普通 Piece BUSY。

### 10.2 数据支持的结论与边界

1. **同 lane native RX window concurrency 已成立。** 四个修复后 fast run 均通过 admission 生命周期、
   SEND_IMM routing 和 Piece overlap gate，不再只是上层 Piece/control 生命周期并发。
2. **JFR/JFS aggregate admission 消除了 provider overflow。** 四组均无 native post error、session retire
   和 TCP fallback；`required pressure=0` 表明 required TX/RX window 最终都取得资源。
3. **optional pressure 是受控 pipeline 退化。** TX optional pressure 为 511--759，RX optional pressure为
   91--167，但没有被放大为 lane failure或 TCP fallback。日志行计数只覆盖特定降级消息，不要求与
   Prometheus optional pressure counter 一一相等。
4. **C4 已进入可见的 RX required 等待区。** 两次 C4 分别出现 3 次/4.70 ms 和 28 次/104.18 ms
   required RX wait；均未超时，但第二次吞吐比第一次低 11.79%，后续稳定性能测试必须同时采集 wait 指标。
5. **B8.4 的旧边界已经被 B8.6 取代。** 第 8.1 节记录的
   `nativeRxWindowConcurrencyClaimed=false` 只描述当时的 `b84-piece-c4-shutdownfix-001`，不得继续作为
   当前实现能力判断。

### 10.3 三批次稳定基线

在 fast correctness 点通过后，以 1 次 warmup + 3 个 measured batch 复测 C2/C4。两组均通过完整的
Piece lifecycle、native RX admission 和 SEND_IMM routing gate；全部 18 GiB measured data 无
BUSY/reject、session retirement、TCP fallback 或 previous-transfer failure。

| run | task concurrency | measured data | 结果 | aggregate MiB/s | Gbps | Jain | mean completion skew | TX required / optional pressure | RX required / optional pressure | required RX wait count / total | TX ring1 fallback | RX window1 fallback |
|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `b86-native-rx-stable-c2-001` | 2 | 6 GiB | **PASS** | **6494.90** | **54.48** | 0.999525 | 11.93 ms | 0 / 2038 | 0 / 559 | 0 / 0 ms | 25 | 559 |
| `b86-native-rx-stable-c4-002` | 4 | 12 GiB | **PASS** | **8335.11** | **69.92** | 0.999738 | 18.76 ms | 0 / 2892 | 0 / 446 | 6 / 111.20 ms | 0 | 446 |

C4 aggregate throughput 比 C2 高 28.33%，同时保持 Jain fairness 大于 0.9995。C4 的 6 次 required RX
wait 合计 111.20 ms，平均每次 18.53 ms，但没有 required pressure、timeout 或 fallback；这说明 RX
required admission 已经成为可观测的延迟来源，尚未构成 correctness 或 transport stability 问题。
两组大量 optional pressure 和 RX 单窗退化说明第二 pipeline window 经常拿不到预算；它们是后续资源配置
和吞吐优化的直接观测项，不能误报为传输失败。

`b86-native-rx-stable-c4-001` 的内容传输和 transport diagnostics 正常，但 runner 在 Parent 最后一个
finish 日志落盘前截取 batch 尾边界，误报一个 unfinished transfer；修复为在下一 batch 启动前有界等待
完整 lifecycle 后，`c4-002` 正式通过。因此 `c4-001` 不作为 PASS 基线登记。

## 11. TCP 单任务 Piece concurrency 基线

2026-09-03 在 25 Gbps 网卡环境完成 TCP 对照测试。这里的 `CC` 是单个 `dfget` 内的
`download.concurrentPieceCount`，不是并发 `dfget` 数量；所以 manifest 的 task concurrency 均为 1。
每个 case 使用 1 GiB 文件、2 次 warmup 和 5 次 measured task，其他参数固定为 `post1-pipe2-in16`。

| run | Piece concurrency | 结果 | measured data | aggregate MiB/s | Gbps | 25 Gbps 利用率 |
|---|---:|---|---:|---:|---:|---:|
| `tcp-piece-cc1-001` | 1 | **PASS** | 5 GiB | **1581.77** | **13.27** | 53.08% |
| `tcp-piece-cc2-001` | 2 | **PASS** | 5 GiB | **2286.19** | **19.18** | 76.72% |
| `tcp-piece-cc4-001` | 4 | **PASS** | 5 GiB | **2530.39** | **21.23** | 84.91% |
| `tcp-piece-cc8-001` | 8 | **PASS** | 5 GiB | **2552.23** | **21.41** | 85.64% |
| `tcp-piece-cc16-001` | 16 | **PASS** | 5 GiB | **2553.02** | **21.42** | 85.67% |
| `tcp-piece-cc32-001` | 32 | **PASS** | 5 GiB | **2543.42** | **21.34** | 85.34% |

由 CC1 到 CC4，aggregate throughput 从 1581.77 MiB/s 增长到 2530.39 MiB/s，增幅 59.97%；
CC4 到 CC16 只再增长 0.89%，CC32 相对 CC16 下降 0.38%。因此当前 TCP 路径在 CC4 后已经进入平台区，
CC8--CC16 可视为该环境下的有效饱和点。25 Gbps 的理论单向带宽为 2980.23 MiB/s，最佳实测
2553.02 MiB/s（21.42 Gbps），线速利用率为 85.67%。

对应 URMA 对照见第 12 节。不得将 B8.6 的 C2/C4 多 `dfget` 同 lane 并发结果与本表直接作倍率比较。

## 12. URMA 单任务 Piece concurrency 基线

2026-09-03 按第 11 节相同口径完成 URMA CC1/2/4/8/16/32 对照：单个 `dfget`、1 GiB 文件、
`post1-pipe2-in16`，每个 case 使用 2 次 warmup 和 5 次 measured task。manifest 的 task concurrency
仍为 1；CC 只表示 `download.concurrentPieceCount`。

> 本节 12.1--12.4 是接收端改用 vectored positional write 之前的历史基线。12.5 记录 `pwritev`
> 优化后的首个正式点；后续 concurrency 曲线必须单独登记，不与本表混合覆盖。

| run | Piece concurrency | 结果 | measured data | aggregate MiB/s | Gbps | 同 CC TCP MiB/s | 相对 TCP |
|---|---:|---|---:|---:|---:|---:|---:|
| `urma-piece-cc1-001` | 1 | **PASS** | 5 GiB | **1670.79** | **14.02** | 1581.77 | +5.63% |
| `urma-piece-cc2-001` | 2 | **PASS** | 5 GiB | **2568.31** | **21.54** | 2286.19 | +12.34% |
| `urma-piece-cc4-001` | 4 | **PASS** | 5 GiB | **3997.11** | **33.53** | 2530.39 | +57.96% |
| `urma-piece-cc8-001` | 8 | **PASS** | 5 GiB | **5159.93** | **43.28** | 2552.23 | +102.17% |
| `urma-piece-cc16-001` | 16 | **PASS** | 5 GiB | **4765.83** | **39.98** | 2553.02 | +86.67% |
| `urma-piece-cc32-001` | 32 | **PASS** | 5 GiB | **5497.07** | **46.11** | 2543.42 | +116.13% |

### 12.1 数据支持的结论与边界

1. **URMA 的并发扩展明显强于 TCP。** CC1 到 CC8 依次增长 53.72%、55.63% 和 29.09%；CC8 已达到
   同 CC TCP 的 2.02 倍，CC32 达到 2.16 倍。
2. **当前最佳单任务点为 CC32。** 5497.07 MiB/s（46.11 Gbps）已经超过 25 Gbps TCP 网卡线速，
   因此 25 Gbps 只能作为 Ethernet/TCP 基线，不能作为 `udmac0d1e2` URMA fabric 的带宽上限。
3. **曲线尚不能解释为链路饱和。** CC8 到 CC16 下降 7.64%，CC16 到 CC32 又增长 15.34%；这种非单调
   变化更符合 admission、Piece 调度或 run-level 波动，而不是平滑进入固定链路上限。
4. **默认 TX8 曾是首要待验证边界。** `in16` 下一个 required window 为 1 MiB，8 MiB TX pool 理论上只能
   同时容纳 8 个 required window；后续正交结果已排除它是主要吞吐瓶颈，见 12.2 节。
5. **本轮原始 sweep 的 diagnostics 尚未取得。** 用户提供的摘要中 diagnostics 为 `null`，不能据此宣称
   pressure、wait、ring/window 退化或 fallback 均为零。后续正交 run 已通过
   `.result.urmaDiagnostics` 完整采集这些字段。

### 12.2 TX、pipeline 与 transfer cap 正交结果

后续 case 固定单 `dfget`、1 GiB、`in16`，每组 2 次 warmup + 3 次 measured task；所有 run 均为纯
URMA PASS，无 required pressure、session retirement、TCP fallback 或 previous-transfer failure。

| 配置 | aggregate MiB/s | Gbps | 关键诊断/结论 |
|---|---:|---:|---|
| CC8 / post1 / pipe2 / TX16 | 4418.19 | 37.06 | TX optional 314，RX optional 404，无 required wait |
| CC16 / post1 / pipe2 / TX16 | 4640.07 | 38.92 | TX optional 901，130 次 ring1；RX optional 476 |
| CC16 / post1 / pipe2 / TX32 | 4662.01 | 39.11 | TX optional 降至 420、ring1 清零，但吞吐仅比 TX16 高 0.47% |
| CC8 / post8 / pipe2 / TX16 | 5127.73 | 43.01 | 比同配置 post1 高 16.06% |
| CC16 / post8 / pipe2 / TX32 | 4903.91 | 41.14 | 比同配置 post1 高 5.19% |
| CC16 / post1 / pipe1 / TX16 | 4594.55 | 38.54 | 所有 optional pressure 清零；仅比 pipe2 低 0.98% |
| CC32 / post8 / pipe1 / MCT16 / TX32 | 4954.88 | 41.56 | 所有 pressure 为零 |
| CC32 / post8 / pipe1 / MCT32 / TX32 | 4574.61 | 38.37 | 所有 pressure 为零；比 MCT16 低 7.67% |

这些数据排除了 registered TX pool、第二 pipeline ring 和 MCT16 cap 是当前主要瓶颈。TX32 虽能消除
ring1 降级，但未转化为吞吐；pipe1 在高 Piece concurrency 下已能依靠 transfer 间并行保持数据面忙碌；
MCT32 只增加软件竞争。post8 在 CC8/CC16 的局部对照中有收益，因此继续做严格 post-list sweep。

### 12.3 CC32 post-list 单变量 sweep

固定 CC32、pipe1、in16、MCT16、TX32、RX32，只改变 `postListSize`。每组为 2 次 warmup + 3 次
measured task，全部 PASS，且 TX/RX required/optional pressure、admission wait、ring/window 降级、
BUSY、session retirement 和 TCP fallback 均为零。

| run | post-list | aggregate MiB/s | Gbps | min / median / max MiB/s | 相对 post1 |
|---|---:|---:|---:|---:|---:|
| `urma-piece-cc32-post1-001` | 1 | **5409.58** | **45.38** | 5311.40 / 5407.14 / 5513.98 | 基准 |
| `urma-piece-cc32-post4-001` | 4 | **5379.34** | **45.13** | 5323.96 / 5391.72 / 5423.29 | -0.56% |
| `urma-piece-cc32-post8-001` | 8 | **4684.68** | **39.30** | 4225.96 / 4944.68 / 4962.42 | -13.40% |
| `urma-piece-cc32-post16-001` | 16 | **5023.97** | **42.14** | 4943.68 / 4979.04 / 5154.20 | -7.13% |

post1 是本轮最快点，post4 与其只差 0.56%；post8 的 min--max 范围达到 736.46 MiB/s，明显比其他点
抖动，并导致 aggregate 比 post1 低 13.40%。post16 相比 post8 恢复 7.24%，但仍比 post1 低 7.13%。
因此在 CC32/pipe1 多 transfer 已充分并行时，WR list batching 没有稳定收益，不能将 post8 直接设为全局
默认值。它与 12.2 节 CC8/CC16 的正收益共同说明 post-list 效果依赖并发形态；下一步应转向
transport-only 与 CRC32+write 分层基准以及 CPU/completion profiling，而不是继续扩大 post-list。

UMDK 64 KiB SEND microbenchmark 的平均值为 67405.55 MB/s，而本组最佳 E2E 约为 5764 MB/s，仅相当于
裸 SEND 数值的约 8.55%。该比例只用于说明仍有优化空间，不能作为等价效率指标：Dragonfly E2E 额外包含
Piece 调度、控制协议、CRC32、文件读写和完成物化等开销。

### 12.4 Piece 大小与 demo 参数对齐实验（`pwritev` 前）

固定单个 `dfget`、1 GiB 文件、post1、pipe2、in16、Piece concurrency 16，每组使用 1 次 warmup +
3 次 measured task；只改变 Piece 大小。所有 case 均 PASS，且无 BUSY、session retirement、TCP fallback
或 previous-transfer failure。

| run | Piece 大小 | aggregate MiB/s | Gbps | min / median / max MiB/s | TX req/opt | RX req/opt | TX ring1 / RX window1 |
|---|---:|---:|---:|---:|---:|---:|---:|
| `urma-piece-4mib-cc16-001` | 4 MiB | **4802.31** | **40.28** | 4688.00 / 4776.37 / 4949.88 | 0 / 1022 | 0 / 427 | 78 / 427 |
| `urma-piece-16mib-cc16-001` | 16 MiB | **5716.08** | **47.93** | 5596.63 / 5747.77 / 5808.03 | 0 / 235 | 0 / 122 | 12 / 122 |
| `urma-piece32-001` | 32 MiB | **5761.32** | **48.33** | 5586.34 / 5790.86 / 5916.46 | 0 / 109 | 0 / 67 | 14 / 67 |
| `urma-piece-64mib-cc16-001` | 64 MiB | **4963.44** | **41.63** | 4863.88 / 4869.73 / 5168.69 | 0 / 64 | 0 / 41 | 15 / 41 |
| `urma-piece64-budget-001` | 64 MiB，扩大 TX/RX budget | **4544.09** | **38.12** | 4214.38 / 4698.78 / 4759.78 | 0 / 33 | 0 / 47 | 0 / 47 |

4 MiB 增至 16 MiB 后 aggregate throughput 提升 19.03%；16 MiB 增至 32 MiB 只再提升 0.79%，已进入
平台区。64 MiB 不仅没有继续提升，反而相对 32 MiB 下降 13.85%；扩大 budget 并消除 TX ring1 后仍下降，
因此该回退不能归因于 TX registered budget。当前工程默认候选保留 16 MiB：它与 32 MiB 性能基本相同，
但内存占用、尾延迟和调度粒度更稳妥；不再继续扩大 Piece。

另做一组接近 demo 传输窗口的对齐实验：`urma-piece16-cc4-in64-001` 固定 16 MiB Piece、CC4、in64、
pipe2、post1、MCT4、TX32/RX32。该 case PASS，aggregate 为 **5508.36 MiB/s（46.21 Gbps）**，
min/median/max 为 5477.04/5514.65/5533.70 MiB/s；TX required/optional pressure 为 0/13，RX 为
0/76，无 required wait、transport failure 或 fallback。其 native send depth 为 256，native receive depth
为 512。虽然单 Piece 的 send/wait 和 total 时间更短，但因活跃 Piece 数从 16 降到 4，aggregate 比
CC16/in16 低 3.63%；扩大单 Piece window 不能替代 Piece 间并发。

### 12.5 接收端 `pwritev` 优化与新基线

接收端原实现对每个 64 KiB span 单独调用 positional write。优化后，同一 receive window 的 spans 通过
一次 `pwritev` 完整写入，正确处理 partial write、`EINTR`、零进展和 offset overflow；CRC32 并行以及
window lease/recycle 生命周期保持不变。16 MiB Piece、in16 时，隐含 write syscall 数由每 Piece
`16 windows x 16 spans = 256` 次降为 `16` 次，即减少 **16 倍**。

使用与 12.4 的 16 MiB 基线相同参数（单 `dfget`、CC16、in16、post1、pipe2、1 warmup + 3 measured）
运行 `urma-piece16-cc16-pwritev-001`：

| 写路径 | aggregate MiB/s | Gbps | min / median / max MiB/s | 相对旧路径 |
|---|---:|---:|---:|---:|
| 逐 64 KiB positional write | 5716.08 | 47.93 | 5596.63 / 5747.77 / 5808.03 | 基准 |
| receive-window `pwritev` | **7494.17** | **62.87** | 7438.46 / 7494.31 / 7550.58 | **+31.11%** |

新 case 为纯 URMA PASS：TX required/optional pressure 为 0/236，RX 为 0/164，TX ring1 14 次、RX
单窗 164 次；required RX wait、BUSY、session retirement、TCP fallback 和 previous-transfer failure 均为零。
Child 样本均显示 `receive_window_count=16`、`pwrite_calls=16`；`pwrite_ns` 约 8.38--9.41 ms，旧路径约
11.64--12.74 ms，下降约 27%；storage total 约 11.79--12.49 ms，旧路径约 15.20--17.25 ms，下降约
25%。吞吐样本极差约 1.5%，收益稳定。

该点说明接收端小粒度写 syscall 是此前明确的软件瓶颈，`pwritev` 结果从本节起作为新的 URMA E2E 基线。
其 62.87 Gbps 高于 demo 8 GiB mmap file-to-file 的约 55.10 Gbps，但两者的文件大小、并发形态和计时边界
不同，只能说明 Dragonfly Piece 路径已进入同一性能量级，不能直接宣称实现效率超过 demo。下一步按
CC1/2/4/8/16/32 重跑完整单 `dfget` concurrency 曲线，确认峰值、平台点以及 CC16/CC32 的稳定性。

### 12.6 `pwritev` 后单任务 Piece concurrency 曲线

2026-09-03 使用 `pwritev` 接收写路径重跑完整曲线。固定单个 `dfget`、1 GiB 文件、16 MiB Piece、
post1、pipe2、in16；每个 case 使用 1 次 warmup + 3 次 measured task。manifest task concurrency 均为
1，表中的 CC 仍仅表示 `download.concurrentPieceCount`。所有 case 均 PASS；无 required TX/RX
pressure、required RX wait、BUSY/reject、session retirement、TCP fallback 或 previous-transfer failure。

| run | Piece concurrency | aggregate MiB/s | Gbps | 相对前一点 | 同 CC TCP MiB/s | 相对 TCP | TX opt / ring1 | RX opt / window1 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `urma-piece-16mib-cc1-post1-pipe2-001` | 1 | **2223.98** | **18.66** | — | 1581.77 | +40.60% | 0 / 0 | 0 / 0 |
| `urma-piece-16mib-cc2-post1-pipe2-001` | 2 | **3290.99** | **27.61** | +47.98% | 2286.19 | +43.95% | 6 / 0 | 32 / 32 |
| `urma-piece-16mib-cc4-post1-pipe2-001` | 4 | **5021.63** | **42.12** | +52.59% | 2530.39 | +98.45% | 16 / 0 | 176 / 176 |
| `urma-piece-16mib-cc8-post1-pipe2-001` | 8 | **7100.04** | **59.56** | +41.39% | 2552.23 | +178.19% | 255 / 204 | 176 / 176 |
| `urma-piece-16mib-cc16-post1-pipe2-001` | 16 | **7570.72** | **63.51** | +6.63% | 2553.02 | **+196.54%** | 230 / 14 | 152 / 152 |
| `urma-piece-16mib-cc32-post1-pipe2-001` | 32 | **6762.88** | **56.73** | **-10.67%** | 2543.42 | +165.90% | 174 / 7 | 106 / 106 |

曲线从 CC1 到 CC8 保持明显扩展，CC8 到 CC16 仅再增长 6.63%，CC32 则回退 10.67%。因此当前性能
峰值和推荐配置为 **CC16：7570.72 MiB/s（63.51 Gbps）**；如优先控制资源消耗，可选择 CC8，吞吐为
峰值的 93.78%。CC16 相对同 CC 的 25 Gbps TCP 基线达到 **2.97 倍**。该跨 transport 对照使用相同
单任务/1 GiB/CC 口径，但 TCP 历史基线没有登记固定 16 MiB Piece，因此客户材料中应同时披露这一边界。

新 CC16 相对 12.4 中相同 16 MiB Piece 的旧写路径 5716.08 MiB/s 提升 **32.45%**；相对 12.5 的
首个 `pwritev` 单点 7494.17 MiB/s 高 1.02%，说明优化收益可以复现。CC32 在 optional pressure 更低且无
required wait/failure 的情况下仍下降，排除 registered budget 和 transport fault 是主要原因。如果该组仍受
`maxConcurrentTransfers=16` 限制，则 CC32 不会增加 lane 内有效 transfer 并发，只会增加 Piece 调度、
文件写入和 CPU 竞争；应将其作为过并发点，而不是继续扩大 CC。

各组首条 Piece 样本的 `pwrite_ns` 从 CC1/2/4 的约 1.31/1.36/1.67 ms，增至 CC8/16/32 的约
7.33/7.09/13.77 ms；CC32 的方向性证据与文件写竞争假设一致。但这些只是每组首条日志，不代表总体
分位数。下一步应对 CC8/16/32 全部 Piece 的 `rx_window_wait_ns`、`digest_ns`、`pwrite_ns` 和
`storage_total_ns` 汇总 p50/p95/p99，并结合 CPU/completion profiling 定位平台与回退来源。

### 12.7 Provider `max_msg_size` 与可配置 chunk-size 议题

`[设计议题登记，2026-09-03]` 当前 URMA process pool 在启动时注册一个连续 Segment，并固定按
64 KiB slot 切分；一条 SEND_IMM 使用一个 slot，因此 provider 的 `max_msg_size` 即使大于 64 KiB，
当前数据面仍只能发送至多 64 KiB。`postListSize` 增加的是一次提交的 WR 数，不改变单 WR payload。

这里不能将 RDMA 的 `maxRegisteredBytes=512MiB` 解释成“预注册一个 512 MiB MR”。经
`rdma-p2p-pr1945` 源码确认，RDMA 将该值作为 active + idle cache 的全局注册预算：pool 初始为空，
`acquire_buffer(len)` best-fit 复用或按需注册可变长 `PinnedBuf`。默认 4 MiB chunk × 16 inflight
形成 64 MiB window，发送端双 ring 最多可形成一个 128 MiB staging buffer；多个 MR 的总量才受
512 MiB ceiling 约束。

URMA 后续采用 hybrid 路线：对齐 RDMA 的显式 `chunkSize` 配置和两端/provider 协商，不照搬动态
MR cache；保留 process 级连续预注册 Segment，将物理 slot size 改成 Runtime 级可配置值，同时用独立
logical `windowSize` 控制每 transfer 的内存占用。第一轮真实 provider sweep 固定 window bytes，测试
64 KiB/256 KiB/1 MiB/4 MiB chunk；继续记录 throughput、WR/CQE 数、TX/RX pressure、required wait、
CPU 和 storage timing。该议题尚未实现，现有 64 KiB 测试数据和默认值保持有效。

### 12.8 每 lane CC8 的多 lane fan-out 与 process admission 宽限

2026-09-03 使用同一物理 Child host 上的隔离 daemon 运行 fan-out 饱和曲线。固定 1 GiB 文件、
16 MiB Piece、每 lane CC8、post1、pipe2、in16；每组 1 次 warmup + 3 次 measured batch。该拓扑用于
验证一个 Parent process 的多 lane 扩展，不代表多个物理 Child 节点。

| run | lanes | aggregate MiB/s | Gbps | 相对前一点 | Jain | completion skew | 结果 |
|---|---:|---:|---:|---:|---:|---:|---|
| `fanout-piece16-cc8-l1-tx64-001` | 1 | **7212.60** | **60.50** | — | 1.00000 | 0 ms | PASS |
| `fanout-piece16-cc8-l2-tx64-001` | 2 | **10737.82** | **90.08** | +48.88% | 0.99436 | 25.68 ms | PASS |
| `fanout-piece16-cc8-l4-tx64-003` | 4 | **13648.16** | **114.49** | +27.10% | 0.99904 | 21.18 ms | PASS |

L4 相对 L1 提升 89.23%，但 L2 到 L4 的边际收益已下降，开始接近共享 Fabric、completion、CPU 或
Storage 路径的平台。L4 使用 MCT40、TX64 MiB + RX32 MiB；TX required pressure 为 0，optional
pressure 为 334，只有 2 次 single-ring 退化，且无 BUSY/reject、session retirement、TCP fallback 或
previous-transfer failure。

L4 在修复前使用瞬时 `try_acquire` 判断 process transfer admission，即使每个 Child 的最大活跃 Piece
均为 8，Parent 仍会在 persistent-lane Piece 收尾交接期间成批返回 BUSY。修复后保留 MCT 硬上限，满载
时先有界等待 10 ms，超时才返回 transfer-local BUSY。本次共 1024 个含 warmup transfer，其中 82 次
等待后成功（8.01%）；mean/p50/p95/p99/max 分别为 1.615/1.318/3.981/4.574/4.574 ms，最大值仅占
10 ms 门限的 45.74%。这证明问题是可被短宽限吸收的服务端 permit 释放交接，不是持续过载或 permit
泄漏。

L8 首轮 `fanout-piece16-cc8-l8-tx128-001` 进一步暴露 permit 释放位置仍晚于数据面完成：330 个 Piece
虽在 10 ms 宽限内取得 admission，但 p95/p99/max 已达 9.313/10.322/10.908 ms，另有 21 个 Piece
超时收到 BUSY 并 fallback TCP。该轮 aggregate 为 12731.73 MiB/s（106.80 Gbps），但包含 fallback，
不得作为纯 URMA 基线。服务端随后将 process permit 改为在全部 SEND WR 完成且 TX leases 回收后、发送
`Piece Done` 前释放；错误、超时和取消路径仍由 RAII guard 释放。该顺序保证释放时数据面资源已归还，
同时让 Client 在收到 terminal frame 并启动替代 Piece 前即可观察到空闲 permit。

修复后的 L8/TX128 与 TX budget 对照如下。固定 8 lanes、每 lane CC8、MCT80、16 MiB Piece、post1、
pipe2、in16、RX32 MiB；每个 run 为 1 次 warmup + 3 个 measured batch。所有列出的 run 均为纯 URMA
PASS，process admission wait、BUSY/reject、session retirement、TCP fallback 和 previous-transfer failure
均为 0。

| run | TX / 总注册预算 | aggregate MiB/s | Gbps | Jain | completion skew | TX optional pressure | TX ring1 fallback |
|---|---|---:|---:|---:|---:|---:|---:|
| `fanout-piece16-cc8-l8-tx128-002` | 128 / 160 MiB | **16053.62** | **134.67** | 0.99900 | 40.91 ms | 1564 | 122 |
| `fanout-piece16-cc8-l8-tx128-005` | 128 / 160 MiB | **15954.09** | **133.83** | 0.99951 | 30.33 ms | 1429 | 148 |
| `fanout-piece16-cc8-l8-tx160-001` | 160 / 192 MiB | **11667.94** | **97.88** | 0.99926 | 50.86 ms | 1670 | 156 |
| `fanout-piece16-cc8-l8-tx160-003` | 160 / 192 MiB | **13479.93** | **113.08** | 0.99911 | 38.28 ms | 1747 | 137 |

两次 TX128 的均值为 16003.86 MiB/s（134.25 Gbps），两次只相差 0.62%；相对 L4 提升 17.26%。
TX160 均值为 12573.93 MiB/s（105.48 Gbps），比 TX128 均值低 21.43%，且 TX160 两次相差
15.53%。增加预算没有降低 optional pressure，ring1 次数也未与吞吐同向变化，因此 TX budget 和双 ring
可用率不是该点的主吞吐瓶颈；当前推荐保留 TX128。

这里一个满载 transfer 的双 ring 工作集为 `2 * 16 * 64 KiB = 2 MiB`，8 lanes * CC8 共 64 个
活跃 transfer，理论总量正好为 128 MiB。`optional pressure` 还包括 optional lease 主动让位给 required
waiter，不能等同于物理 pool 耗尽；上述 run 的 `txBufferUnavailableLines` 均为 0。代码审查同时发现
当前 TX window allocator 每次申请会扫描整个 TX slot 区、复制全部 slot state，并对每个选中 slot 执行
`free_tx.retain()`；TX slots 从 2048 增至 2560 会放大串行 allocator bookkeeping。该路径是下一轮应先
计时验证的候选瓶颈，尚不能只凭本组 E2E 数据认定为唯一根因。

测试卫生方面，连续未 cleanup 的 run 曾出现吞吐逐轮下降，执行清理后 TX128 恢复至 15954.09 MiB/s。
runner 的 origin artifact 是公共 seed 的 hard link，不能仅凭删除 `/var/www/dragonfly` 链接就认定释放了
对应文件数据或 page cache；未清理的 `/var/lib/dragonfly-b7/<run>`、主机负载和冷热缓存均可能参与。
后续正式对照必须在每轮保存本地 evidence 后执行 manifest-owned `cleanup --execute`，并交替运行 A/B；
本台账不把未严格控制的连续下降归因于单一目录。

### 12.9 每 lane CC1 的 lane 扩展曲线

同日补录第一组 fan-out 数据。固定 1 GiB 文件、16 MiB Piece、每 lane CC1、post1、pipe2、in16、
MCT8、TX16 MiB + RX32 MiB；每组 1 次 warmup + 3 次 measured batch。L1 使用普通 queue topology，
L2/L4/L8 使用同一物理 Child host 上的隔离 daemon，因此该组证明的是一个 Parent process 面向多个
lane/daemon 的扩展，不代表多物理节点。

| run | lanes | aggregate MiB/s | Gbps | 相对前一点 | Jain | completion skew | manifest 状态 |
|---|---:|---:|---:|---:|---:|---:|---|
| `fanout-piece16-cc1-l1-001` | 1 | **2080.34** | **17.45** | — | 1.00000 | 0 ms | passed |
| `fanout-piece16-cc1-l2-001` | 2 | **2806.45** | **23.54** | +34.90% | 0.97132 | 212.80 ms | cleaned（结果保留） |
| `fanout-piece16-cc1-l4-001` | 4 | **4405.17** | **36.95** | +56.97% | 0.99934 | 56.21 ms | passed |
| `fanout-piece16-cc1-l8-001` | 8 | **6417.47** | **53.83** | +45.68% | 0.99998 | 17.68 ms | passed |

L8 相对 L1 达到 3.08 倍（+208.48%），说明在每 lane 只有一个 Piece 时，增加 lane 能持续填充共享
URMA Fabric；但扩展不是线性的，8 倍 lane 只得到约 3.08 倍吞吐。L2 的 Jain 和 completion skew 明显
弱于其余点，属于该组的离群稳定性信号，后续若用于客户材料应至少复跑一次 L2。

该组还可与 12.8 的 L1/CC8 做一个“总 Piece concurrency 均为 8”的方向性对照：L8/CC1 为
6417.47 MiB/s，L1/CC8 为 7212.60 MiB/s，前者低 11.02%。两组的 lane/daemon 数和 TX budget 不同，
不是严格 A/B；但结果支持当前判断：在总 Piece concurrency 已足够时，优先在持久单 lane 内复用并发
Piece，比单纯增加 lane 更高效；多 lane 的主要价值是跨独立 peer/daemon 扩展总并发和总吞吐。

本次命令对 L2/L4/L8 查询了 `.result.urmaDiagnostics`，而 fan-out diagnostics 实际位于
`.result.fanoutDiagnostics`，所以输出中的 `null` 不表示相关计数为零。L2 当前 manifest 已进入
`cleaned` 生命周期状态。表中保留其 transfer summary，但在未补取/确认 `fanoutValidation` 和
`fanoutDiagnostics` 前，不额外宣称该点具有零 fallback/零 retirement 证据。
