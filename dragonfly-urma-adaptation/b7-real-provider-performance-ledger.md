# B7 真实 Provider 性能验证台账

更新时间：2026-09-01。

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
