# Dragonfly URMA RM + READ 整体方案与后续工作

更新时间：2026-09-11。

## 0. 文档目的

本文基于当前 `urma-read-prototype` 分支，整理 Dragonfly P2P 使用 URMA Reliable Message（RM）和
one-sided READ 的整体方案、代码改造边界、资源与故障生命周期、后续工作包以及验证门禁。

本文的首轮目标方案是一个纯 RM + READ bulk data backend。整 Piece buffer 是首轮实现选择，
不是已经由性能数据证明的最终最优结构：

- 不以 8 KiB 为协议分流阈值；8 KiB 只作为已有性能建议的背景信息；
- 不要求在本分支保留 URMA SEND/RECV bulk data 路径；
- TCP 继续承担请求、metadata、Segment 交换、完成确认和错误等控制面；
- 首轮一个 Piece 对应一个 remote-readable Segment 和一个本地 Piece-sized registered buffer；
- `Piece length <= max_read_size` 时使用一条 READ WR；
- `Piece length > max_read_size` 时只切分 READ WR，不切分 Piece Segment 或本地 Piece buffer；
- 全部 READ CQE 成功并完成 terminal control gate 后，才把完整 registered Piece lease 交给 Storage；
- 任一 READ、控制协议或完整性错误仍按整 Piece 失败处理，复用 Dragonfly 的 reset 和 TCP fallback。

## 1. 前置材料与证据边界

开始设计前已完整阅读：

- `rdma-urma-upload-download-path-comparison.md`；
- `b7-real-provider-performance-ledger.md`；
- `urma-rm-for-dragonfly-p2p-evaluation.md`。

并对照了：

- 当前 Dragonfly `urma-read-prototype` / `urma-rm-prototype` 源码；
- `/home/yuan/workspace/dev/client` 的 Dragonfly RDMA 候选实现；
- `/home/yuan/workspace/dev/urma-transport-lab` 的 `tcp-urma-file-transfer` 分支；
- `/home/yuan/workspace/cloud-native/umdk` 的 API、User Guide、sample 和 UDMA provider 源码。

本文区分以下证据等级：

- `[源码确认]`：由当前 Dragonfly 或 UMDK 源码直接确认；
- `[既有实验]`：由已有 RC/RM/SEND 或 B7 台账确认；
- `[设计决定]`：本方案选定的目标结构；
- `[待验证]`：必须通过目标 provider 跨节点实验后才能确认。

当前可以确认：

1. `[源码确认]` UMDK 提供 READ/WRITE opcode、remote Segment register/export/import/unimport 和 access token；
2. `[源码确认]` READ 的 remote source 只能有一个 SGE；本地 destination 可以使用 SGL，但受设备
   `max_jfs_sge` 和具体 provider 限制；
3. `[源码确认]` 当前 Dragonfly Segment 只使用 `URMA_ACCESS_LOCAL_ONLY`，FFI 只封装 SEND/RECV；
4. `[源码确认]` 当前 wire 只交换 Jetty descriptor，没有 Segment descriptor、token 或
   `max_read_size`；
5. `[源码确认]` 当前 RM branch 已有 process-wide shared RM endpoint、PeerTarget、owner thread、
   Piece multiplexing、registered lease、Storage CRC32/pwritev 和整 Piece TCP fallback；
6. `[待验证]` 目标设备上 RM + READ 的跨节点正确性、最大 READ、ordering、错误、revoke 和 shutdown
   语义尚不能只根据 API 推断；
7. `[待验证]` READ 是否提高 Dragonfly E2E 吞吐，必须和当前 RM + SEND/RECV 在相同 workload 下 A/B。

阶段判断：技术上具备实现基础，适合做独立 READ backend；尚不应标记为 production-ready。

### 1.1 2026-09-11 设计复核补充

本次补充来自文档与本地源码复核，没有新增真机验证结论：

- `[源码确认]` 当前 `udma_u_unimport_seg()` 清理本地 imported wrapper，不承担 outstanding READ drain；
- `[源码确认]` 当前 `udma_u_ungrant_seg()` 对 `ummu_ungrant()` 失败只记录日志；外层
  `udma_u_unregister_seg()` 在该路径仍可返回 `URMA_SUCCESS`。因此 API 成功返回不能单独证明撤权完成；
- `[源码确认]` 当前 `MappedPiece` 只持有 mmap；`Storage::map_upload_piece()` 在 mmap 返回前就结束
  upload metadata 计数，不能直接充当 remote Segment 整个存活期的 Storage 保活凭据；
- `[源码确认]` 当前 shared RM Jetty 创建仍绑定 JFR；READ 不消耗 bulk RECV WR 不等于可以删除 JFR 对象。

复核定位：

- `/home/yuan/workspace/cloud-native/umdk/src/urma/hw/udma/udma_u_segment.c`：
  `udma_u_ungrant_seg`、`udma_u_unregister_seg`、`udma_u_unimport_seg`；
- `dragonfly-client-storage/src/content.rs`：`MappedPiece`；
- `dragonfly-client-storage/src/lib.rs`：`map_upload_piece`；
- `dragonfly-client-storage/src/server/urma.rs`：上传 limiter 与 Piece 生命周期；
- `dragonfly-client-storage/src/urma/ffi/shim.c`：shared RM Jetty/JFR 创建。

### 1.2 8 KiB 与 256M 建议的适用边界

`[外部建议，待目标 provider 验证]` 当前收到的信息是 RM 小消息适合 SEND/RECV，大消息适合
READ/WRITE，分界约为 8 KiB，READ/WRITE 最大可达“256M”。这不是本分支已经确认的设备能力或
通用协议常量：

- 应回填建议对应的设备、provider/驱动/UMDK 版本、RTP/CTP profile、方向和测试条件；
- “256M”究竟是 256 MB 还是 256 MiB，以及是否指单 WR payload，必须确认，wire 一律使用明确字节数；
- 恰好 8 KiB 的策略尚未定义，R0/R6 应覆盖阈值两侧及边界，不能从建议直接推出默认策略；
- READ、WRITE 和 SEND 的 limit 分别查询，不以 `max_msg_size` 替代 `max_read_size`，不硬编码 256M；
- 纯 READ 是当前实验分支的范围选择，不表示已证明小 Piece/尾 Piece 使用 READ 更优。

运行时以有效 capability 和配置取最小值；capability 缺失、为零或 profile 不支持时关闭该 READ attempt，
不得猜测最大值。阈值和上限的实测结果登记到 provider ledger 后再决定后续策略。

## 2. 核心设计决定

### 2.1 连接与数据模型

保留当前 RM branch 的 process-wide shared endpoint：

```text
process UrmaFabric
  ├─ shared RM Jetty / JFS
  ├─ shared send JFC
  ├─ provider-required JFR / recv JFC（若沿用当前 Jetty 形态）
  ├─ PeerTarget registry
  │    ├─ peer A -> imported Jetty / generation
  │    └─ peer B -> imported Jetty / generation
  ├─ imported remote Segment registry
  ├─ Piece-sized local READ buffer pool
  └─ READ operation/completion router
```

每个 peer 仍通过 TCP control session 使用同一个 RM endpoint。每个 Piece 是一个独立 logical transfer，
但不创建独立 Jetty/JFR。READ 不需要为 bulk data post RECV 或维持 SEND credit；当前 Jetty API/
provider 路径仍需要 JFR 对象，其最小 depth、recv JFC 和关闭依赖由 R0/R1 验证。不能把删除 bulk
RECV 逻辑等同于删除 native JFR；也不在首轮同时迁移到另一种 JFS-only endpoint 形态。

### 2.2 首轮以 Piece 为 Segment、buffer 和完成聚合边界

一个正常 Piece 使用：

```text
Parent: one Piece mmap + one exported read-only Segment
Child:  one imported Segment + one Piece-sized local registered lease
```

`max_read_size` 只决定一个 Piece 需要多少条 READ WR：

```text
effective_max_read_size = min(
    local device max_read_size,
    negotiated peer/protocol limit,
    configured max_read_size
)

read_wr_count = ceil(piece_length / effective_max_read_size)
```

例如 Piece 为 64 MiB、有效 `max_read_size` 为 16 MiB：

```text
READ 0: remote Piece [ 0, 16 MiB) -> local Piece [ 0, 16 MiB)
READ 1: remote Piece [16, 32 MiB) -> local Piece [16, 32 MiB)
READ 2: remote Piece [32, 48 MiB) -> local Piece [32, 48 MiB)
READ 3: remote Piece [48, 64 MiB) -> local Piece [48, 64 MiB)
```

首轮每个 Piece attempt 只进行一次 mmap/register、SegmentOffer、import/unimport 和 ReadDone。
不因为 `max_read_size` 切片而额外拆分 remote Segment，以免把 token、registration 和回收成本按 WR
数量放大。

整 Piece local buffer 是 correctness baseline，不是最终结构承诺。它会使同 Piece 的 Storage 消费
等待全部 READ 与 Done，失去原 receive-window 路径的 Piece 内传输/写入重叠，并扩大并发工作集。
若 R6 证明内存或流水线损失不可接受，允许保留一个 remote Piece Segment，改用多个有界 local READ
window；这不要求增加 remote Segment/token。该演进必须另行定义 window 交付、最终 Done gate、
consumer-held lease 预算及整 Piece reset，首轮不实现。

### 2.3 纯 READ bulk path

本分支不把 SEND/RECV 作为 bulk data 的兼容模式。初始协商只允许：

```text
data_plane = RM_READ
```

如果任一端不支持 RM READ、无法 mmap/register/import、Piece 超出本地地址或预算能力，或者 provider
返回不支持，则本次 URMA Piece attempt 失败并回退 TCP。不要在同一个 Piece 的一部分已经 READ 后切换
到 SEND/RECV。

这项决定不影响 TCP control plane；Request、SegmentOffer、ReadDone、Done 和 Error 仍走 TCP。

### 2.4 READ pull，而不是 WRITE push

Child 是 Piece 消费方，由 Child 发起 READ，原因是：

- 与 Dragonfly 下载方控制接收内存、并发和 Storage 背压的语义一致；
- Parent 不再为每个 Chunk post SEND，也不消耗 Child JFR/RQE；
- 直接注册 Parent 的只读 Piece mmap 时，有机会消除当前
  `file mmap -> CPU copy -> registered TX -> NIC` 中的 source-fill copy；
- 暴露只读 Piece 范围的风险小于向动态 P2P peer 暴露 writable RX memory；
- WRITE 不自然消除 Parent source fill，并额外引入 Target 通知和迟到 DMA 覆盖新 generation 的风险。

WRITE 可留作未来独立实验，但不属于本分支的第一阶段范围。

## 3. 目标端到端流程

### 3.1 正常流程

```text
Child / READ initiator                         Parent / memory target
       | PieceRequest(task, kind, number, READ cap)      |
       |------------------------------------------------>|
       |                                                 | lookup/validate metadata
       | PieceMetadata(transfer, length, digest)          |
       |<------------------------------------------------|
       | acquire whole-Piece local byte/buffer permits    |
       | BufferReady(transfer, accepted length)          |
       |------------------------------------------------>|
       |                                                 | bounded process/source admission
       |                                                 | upload limiter.acquire(length)
       |                                                 | acquire immutable Storage lease
       |                                                 | mmap; allocate token/generation
       |                                                 | register READ Segment
       | SegmentOffer(metadata identity, descriptor,     |
       |              token, generation, read limit)     |
       |<------------------------------------------------|
       | validate metadata identity/ranges; import_seg    |
       | post READ slices under shared JFS admission      |
       | reap and validate all accepted READ WRs          |
       | stop new post; unimport after drain              |
       | ReadDone(transfer, generation, total length)     |
       |------------------------------------------------>|
       |                                                 | verified revoke/unregister
       |                                                 | release mmap/Storage lease
       |                                                 | release process/source permits
       | Done(transfer, generation)                      |
       |<------------------------------------------------|
       | publish complete registered Piece lease         |
       | CRC32 + positional write; join workers          |
       | metadata finish; recycle local lease/permits     |
```

`PieceMetadata -> BufferReady` 是首轮采用的显式 byte-admission barrier：Parent 在 Child 取得本地
buffer 前不发布可 READ 的 descriptor，也不提前注册大量 source。它不是旧 `RecvPosted`，不涉及 RQE。
若将来利用已有可信 metadata 合并交互，必须保留相同预算保证，并校验最终 Offer 的 length/digest/identity。
等待 BufferReady 和等待 Parent admission 都必须有界；超时进入第 8.4 节取消状态机。尚未取得 source
permit 的 metadata/BufferReady 请求也受独立 pending-request 数量上限约束，避免把注册压力转为无界控制状态。

### 3.2 为什么保留 ReadDone 和 Done

READ completion 是 initiator 本地可见事件。Parent 没有对称的 data CQE，不能在发出 SegmentOffer 后
自行判断 remote READ 已完成。

- `ReadDone`：Child 声明全部 READ CQE 已收敛，Parent 可以进入 Segment revoke/unregister；
- `Done`：Parent 声明该 transfer 的 remote Segment 生命周期已经关闭；
- Child 只有收到匹配的 `Done` 后，才把最终 Piece lease 发布给 Storage。

这延续当前路径“最终数据必须经过 terminal protocol gate 后才能成功”的约束。ReadDone 是合作方协议
声明，不是 Parent 本地的硬件完成证明；Parent 仍需满足第 5.3 节撤权门禁，才能发送 Done。
普通 Error、Cancel 或 TCP EOF 不能代替 ReadDone，也不能授权 Parent 立即释放 source。

### 3.3 Storage 路径

目标 RX 路径为：

```text
NIC READ DMA
-> contiguous registered Piece lease
-> terminal Done validation
-> CRC32 and positional write in parallel
-> recycle Piece lease
```

如果 Piece lease 是单个连续 span，Storage 可使用一次 positional write，而不需要为 64 KiB spans 组织
大 iovec。现有 registered-window CRC32、write worker join、partial fallback reset 和 digest 校验逻辑应
尽量复用。

### 3.4 业务限流、指标与 permit 交接

READ 模式继续保留现有 Dragonfly 上传限流和三类 Piece 的业务指标：

- Parent 在 SegmentOffer 发布前取得整 Piece 上传带宽额度并完成 source/process admission；额度等待
  不得占住无期限 remote-readable Segment。该 limiter 控制授权速率，不声称能约束异常 peer 重复 READ；
- 每次已接纳 upload attempt 的 started 与 finished/failed 必须一一对应；收到 Request 不等于成功上传；
- 正常成功指标在匹配 ReadDone、source 安全回收且 Done 发送成功后结算；Done 发送失败结算 failed
  并记录 control failure，不能先计 finished 再重复计 failed。上传端成功不代表 Child 后续 CRC/write
  或 metadata commit 成功；
- traffic 指标应注明是正常完成的逻辑 Piece bytes，不能当作 NIC 实际流量。Parent 没有逐 READ CQE，
  失败 attempt 的实际已读字节不可凭空推断；Child 可另记已成功完成 READ 的字节数；
- Parent source/process permit 在安全撤权、资源归还后且发送 Done 前释放，延续 B7 的 permit 交接顺序；
- Child active byte permit 保持到 consumer workers join 且 lease recycle，不能在 READ CQE 完成时提前释放。

## 4. Buffer 与内存注册设计

### 4.1 当前 fixed slot pool 不适合作为最终 READ allocator

当前池按 64 KiB slot 服务 SEND/RECV：

- RX slot FIFO 分配，回收后不保证一个 Piece 所需的地址连续；
- 默认总注册预算 40 MiB、TX 8 MiB、RX 32 MiB，无法容纳最大 64 MiB Piece；
- slot identity 与 WR 一一对应，适合 per-message completion，不适合一条 READ 覆盖整个 Piece；
- READ 虽允许本地 SGL，但一条大 READ 会受 `max_jfs_sge` 限制，并保留大量 slot bookkeeping。

### 4.2 目标：byte-budgeted Piece buffer pool

推荐增加 READ 专用 allocator：

```text
ReadBufferPool
  ├─ byte semaphore / maxRegisteredReadBytes
  ├─ active Piece-sized registered buffers
  ├─ idle best-fit buffers or bounded size classes
  └─ quarantine for uncertain outstanding DMA
```

第一版可以采用动态 Piece-sized aligned allocation + register，先保证语义正确；取得 registration 延迟
和并发数据后，再决定使用：

- best-fit registered buffer cache；
- 4/8/16/32/64 MiB size classes；
- 一个大预注册 arena 上的连续 range allocator。

不要在没有实验数据前同时实现复杂 arena compaction。无论采用哪一种底层，Dragonfly-facing contract
都应是：

```text
acquire_piece_buffer(length) -> exclusive contiguous RegisteredPieceLease
```

### 4.3 内存预算

Piece-sized READ 会改变内存工作集。自动 Piece 通常为 4--64 MiB；在 Piece CC16 下，64 MiB Piece 的
理论 active RX 数据可达到 1 GiB。因此必须增加独立预算，例如：

```text
maxRegisteredReadBytes
maxConcurrentReadBytes
maxConcurrentReadTransfers
maxCachedReadBytes
```

registered ceiling 与 active concurrency 应分开：idle registered cache 可以复用，但不能挤占 active
transfer 的保底空间。

required admission 必须有界等待或返回 transfer-local BUSY；无法容纳单 Piece 的硬预算应立即拒绝，
不能等待永远不可能取得的额度。普通内存压力不得破坏 shared RM endpoint。

Parent 必须另设 source export 预算，不能只限制 Child local buffer：

```text
maxExportedReadBytes / maxExportedReadSegments
perPeerExportedReadBytes / perPeerExportedReadSegments
maxQuarantinedReadBytes / maxQuarantinedReadSegments
```

这些是预算维度建议，最终配置命名可合并，但必须具有以下不变量：

| 资源 | 计费区间 | 释放条件 |
|---|---|---|
| Child active buffer bytes | acquire 至 READ、等 Done、consumer-held | CRC/write workers join 且 lease recycle；异常需先 drain |
| Child registered bytes | register 至 active/idle/quarantine | 已证明安全的 unregister；idle cache 仍计费 |
| Parent exported bytes/Segment entries | source registration reservation 至 export/等 ReadDone/revoke | 已证明撤权且 native registration 关闭 |
| Parent Storage backing lease | 建立不可变内容租约至远端授权关闭 | revoke 完成后释放；quarantine 时继续持有 |
| 两侧 quarantine bytes/entries | 无法确认 DMA/撤权的资源转入隔离 | 仅在取得安全证据后回收；不因 transfer 失败而退还预算 |

process 总注册预算应覆盖本进程同时作为 Parent/Child 的 active、cached、exported 和 quarantined
资源；注册额度按实际 allocation/registration 大小计，不只按 payload。相同文件页被重复注册时，还要
单独记录逻辑授权字节、registration 数和实际 pinned pages，不能假设按唯一 Piece 自动去重。

idle cache 在新 required 请求受压时有界驱逐；隔离资源不可参与驱逐或 best-fit 复用。quarantine 上限
是停止接纳的触发线，不是强制释放不安全内存的理由；达到触发线时停止相关 peer 新工作，必要时关闭
本进程 READ admission 并回退 TCP。已经隔离的内存必须持续计费，直到安全回收或经验证的进程退出。

所有双向 peer 都使用一致的资源申请顺序：先验证 metadata，Child 取得完整 local budget，再通知 Parent
取得 source/process budget，最后 import/post；等待内存或控制消息时不占用 JFS WR permits。per-peer
配额与 required-first 调度防止单个慢 peer 长期占据整个 process budget。

这个申请顺序本身不能消除双向下载的跨进程资源循环。首轮在 process ceiling 内为 local destination
与 exported source 设置各自硬分区/保底，不能让 Child buffer 吃光 Parent source 的进展空间；暂不做
共享 overflow。每侧均限制 per-peer bytes/transfers，并在无法满足保底工作集时显式降低有效并发。
首轮不实现 idle cache 时 `maxCachedReadBytes=0`；idle 驱逐规则是后续启用 cache 的前置约束。

### 4.4 Parent mmap Segment

Parent 需要新增 external-memory Segment wrapper：

```text
MappedPiece owner
  -> register existing VA/length with remote READ access
  -> export Segment descriptor
  -> hold mmap + native Segment until revoke/unregister
```

它与当前“shim 自己 posix_memalign、register、unregister、free”的 Segment owner不同，必须明确：

- mmap backing owner 先创建、最后释放；
- native Segment 先 unregister，之后才能 drop mmap；
- register 失败不影响现有 Storage 文件；
- Piece 文件 truncate、unlink、GC 和 task eviction 必须与 Segment lease 协调；
- page alignment、offset alignment、pin/non-pin 和 cache coherency 以目标 provider 实验为准。

建议一个 Piece 一个最小授权 Segment，而不是给 peer 暴露整个 task 文件或整个 storage。

需引入明确的 `ExportedPieceLease` owner，同时持有：

```text
immutable Piece identity/range + Storage backing lease
  + mmap owner + native Segment + token/generation + export-budget permits
```

当前 `MappedPiece` 只拥有 mmap；`map_upload_piece()` 在返回 mmap 前就调用相应的
`upload_*_finished`。READ 接入必须增加覆盖整个 export 生命周期的 Storage 保活/不可变内容租约，
不能把上述短期 metadata 计数当作已经具备的 GC gate。

- mmap 通常能在文件 unlink 后继续引用原 backing；不要把 unlink 与 truncate/覆盖视为同一种风险；
- truncate、修改已授权范围、回收并重用同一内容对象必须与 active export 互斥；只保留文件描述符不够；
- source lease 在 lookup/metadata 到 mmap/register 之间也要消除 TOCTOU，Offer 必须指向同一已完成内容；
- generation/quarantine 生命周期跨越普通 attempt guard；取消 future 不得顺带 drop mmap 或退还 export budget；
- 非页对齐 Piece offset/length、tail page 和相邻 Piece 共页必须探测实际授权粒度；若 provider 必须扩大
  授权到 Piece 外，则 exact-Piece 模式不可直接启用。首轮回退 TCP，后续可另评独立 staging Segment。

file mapping 的 `WillNeed`/预取建议不应记作“页面已经全部驻留”。pin、预取、page fault 和真正的
source lifetime 分别测量；首轮不以 non-pin 隐含绕过预算或撤权要求。

## 5. Segment descriptor 与安全模型

### 5.1 SegmentOffer 必需字段

wire DTO 不直接传递 liburma pointer，至少包含：

```text
version
transfer_id
peer_generation
segment_generation
piece_offset
piece_length
remote_ubva: eid + uasid + va
segment_length
access flags
token policy / token id metadata
token value
effective_max_read_size
provider-owned opaque descriptor bytes（若当前 provider ABI 需要）
```

具体 DTO 应由 C shim 从 `urma_get_seg_ctx()` 得到的公开/opaque结构复制或序列化，Rust application 层不
解释裸 UMDK结构。

### 5.2 最小权限

Segment 只授予 remote READ：

- 不授予 WRITE、ATOMIC；
- token 不进入普通日志、metrics、tracing field 或错误消息；
- token/descriptor 绑定 peer、transfer、Piece范围和 generation；
- import 前检查 Segment 长度和权限；
- 每条 READ post 前同时验证 remote offset、local offset 和 length；
- completion 后再次验证 slice identity、length和总覆盖范围。

上述 peer/transfer/generation 绑定首先是本地协议与 registry 校验，不能自动等同于硬件权限。必须明确
token 是 bearer capability 还是 provider 能限制到指定 peer：额外 peer 持有同一 descriptor/token 后的
访问行为属于 R1/R5 验证项。若无法提供要求的 peer 隔离，必须限制部署信任边界或保持该 profile 关闭。

当前 `fabricTag` 只是 reachability domain，不是 peer身份认证。READ production gate 必须明确现有
Dragonfly Piece 请求授权是否足够，或者是否需要额外 peer身份/会话绑定。

### 5.3 旧 token 与撤销

软件 generation 只能拒绝 stale control/CQE，不能阻止持有旧 token 的远端硬件继续访问。因此必须验证：

1. `unregister_seg` 是否同步撤销远端访问；
2. outstanding remote READ 存在时 unregister 是等待、失败，还是立即返回；
3. peer crash、TCP断开或 Child owner thread退出后如何证明不会再有 READ；
4. token_id 重用前是否存在 provider要求的 drain 或 grace period；
5. 无法证明安全时是否必须 quarantine mmap/Segment至进程结束。

以上任一项未闭环前，不得在错误路径立即复用相同地址和 token generation。

当前本地 UDMA 源码还给出两个必须进入 probe 的具体边界：

- `udma_u_unimport_seg()` 只是清理本地 wrapper/token 副本并 free，不 drain 已提交的 READ。
  必须先停止 post、等待全部 accepted WR 退休，再 unimport；不能反过来用 unimport 充当取消 WR；
- `udma_u_ungrant_seg()` 中 `ummu_ungrant()` 失败只记日志，外层 unregister 仍可能返回成功。
  因此不能仅凭返回码把“native wrapper 释放”登记为“硬件授权已撤销”。

R0/R1 必须验证撤权后旧 descriptor/token READ 被拒绝、outstanding READ 与撤权竞争、底层撤权失败
注入、地址/token 重用后旧授权不能访问新内容。需要厂商语义说明、可观测错误传播和真机行为共同形成
可执行门禁；若当前 provider 不能可靠区分撤权失败，不能只靠多跑几次正常 unregister 放行 production。

quarantine 必须持有仍需保护的 backing、预算及尚有效的 native owner。native wrapper 已被 provider
释放时不能保留悬空 handle，更不能重试调用已释放对象；也不能假设重建 Fabric 就能撤销旧授权。
具体升级路径和进程退出后的访问终止语义纳入 provider gate。

## 6. READ WR、切片和 completion

### 6.1 Slice 生成规则

对 `piece_length` 和 `effective_max_read_size`：

```text
for offset in (0..piece_length).step_by(effective_max_read_size):
    length = min(effective_max_read_size, piece_length - offset)
    READ remote_base + offset -> local_base + offset, length
```

硬约束：

- `effective_max_read_size > 0`；
- offset/length 加法无溢出；
- 所有 slices 精确、不重叠地覆盖 `[0, piece_length)`；
- 单条 SGE length 符合 UMDK `u32` 和 provider limit；
- remote SGE 数固定为 1；
- 初版 local destination 固定为一个连续 SGE；
- Piece length 必须可由当前进程地址空间和配置预算表示。

### 6.2 Post 与 admission

多 slice Piece 可以 linked post，但不能无界占用 shared JFS：

```text
shared JFS permits
  + per-peer outstanding READ permits
  + per-transfer outstanding slice permits
  + active registered-byte permit
```

第一版建议按 provider `max_jfs_depth` 有界 post 全部 slices；如果 Piece 被切成很多 slices，则使用滑动
窗口。一个 peer 不得长时间占满 shared JFS，使其他 PeerTarget 饥饿。

linked post 必须精确处理 provider 接受的 prefix：先登记 operation owner，post 返回后只 commit
实际 accepted WR；未提交 suffix 回滚 WR permits，已提交 prefix 保持 buffer/import/target 引用直到
其 CQE 或已验证 flush 退休。不能按计划 slice 总数等待 CQE，也不能因 post 返回失败立即释放整 Piece。
取消开始后禁止补 post；同一 transfer 的所有在途 post command 必须由 owner 串行收敛。

### 6.3 Completion identity

当前 `WrToken` 只表达 Send/Recv和单 SlotId。READ 需要独立的 logical operation identity，例如：

```text
[peer target id][peer generation][READ][operation id]

operation registry:
operation id -> {
    transfer_id,
    segment_generation,
    slice_index,
    local_offset,
    remote_offset,
    expected_length,
    PieceLease owner/reference
}
```

不要强行把 Piece-sized READ operation塞进现有 64 KiB SlotId语义。

一个 Piece 只有在以下条件全部满足时完成：

```text
posted slices == planned slices
successful CQEs == posted slices
completed byte coverage == Piece length
no duplicate/missing/overlap/out-of-range completion
all outstanding WR owners retired
```

### 6.4 Selective completion

第一版所有 READ WR 都应 `complete_enable=1`，建立明确的 correctness baseline。只有目标 provider
ordering、partial post、error CQE 和 shutdown全部验证后，才能研究 completion moderation/frontier
retirement。

RM shared JFS 下不能假定不同 PeerTarget 的完成全局有序。若未来做 selective completion，必须先证明
ordering 是 shared JFS全局、per target，还是 per TP，并按相应粒度维护 frontier。

## 7. Control protocol

建议将当前 DFUR wire升级一个版本，新增或扩展：

```text
Capability:
  data_plane = RM_READ
  max_read_size
  max_jfs_sge
  segment_descriptor_version

Request:
  transfer_id
  existing Piece identity
  requested data_plane = RM_READ

PieceMetadata:
  transfer_id
  PieceMetadata(length, digest, identity)

BufferReady:
  transfer_id
  accepted_length
  metadata identity

SegmentOffer:
  transfer_id
  PieceMetadata
  SegmentDescriptor
  segment_generation
  effective_max_read_size

ReadDone:
  transfer_id
  segment_generation
  completed_length
  read_wr_count

Done:
  transfer_id
  segment_generation

Cancel:
  transfer_id
  segment_generation（Offer 尚未发出时为空）
  reason

CancelDrained:
  transfer_id
  segment_generation（若已有 Offer）
  accepted_wr_count / retired_wr_count

Cancelled:
  transfer_id
  segment_generation（若已有 Offer）

Error:
  transfer_id
  error scope/code/message
```

原 `RecvPosted` 不属于 READ bulk path，可以从该分支的 active协议中删除。若考虑未来 wire兼容，不应让
旧 peer 把 READ frame误解成 SEND/RECV frame；版本不匹配直接走 TCP fallback。BufferReady 仅确认 Child
已保留完整 buffer，不授予额外数据访问权限。

Cancel 是停止新工作的请求，不是远端 DMA 已结束的证明；CancelDrained 是 Child 在全部 accepted WR
退休并关闭 import 后的声明；Cancelled 是 Parent 安全关闭对应 export 后的 terminal response。
三者与成功 ReadDone/Done 分开。所有 frame 按 session/peer generation、transfer 和 segment generation
校验；metadata/BufferReady 阶段尚无 segment generation，不能用任意零值匹配已有 export。

同一 transfer 的 Cancel、Offer 和 terminal frame 由状态机串行裁决；Cancel 后迟到的 Offer 不得启动
READ，只进入取消清理。已完成的 terminal 可用有界 tombstone 处理同 identity 的重发，不能重复释放
资源或计指标；未知/不匹配 generation fail closed。

取消发生时尚未看到 Offer 的 Child 可以发送无 segment generation 的 CancelDrained，但 Parent 若已
发布 Offer，不能用它清理该 export：Child 必须消费迟到 Offer，再回复匹配 generation 的 CancelDrained。
控制 writer 必须保留同 transfer 的发布顺序，Parent 不能在已发布 Offer 后发无 generation 的 Cancelled。

## 8. 正常与异常生命周期

### 8.1 Parent source Segment

```text
lookup/validate Piece metadata
-> wait BufferReady under bounded deadline
-> acquire source/process budget and upload limiter
-> acquire immutable Storage backing lease
-> mmap exact Piece
-> register remote-readable Segment
-> export descriptor / SegmentOffer
-> wait ReadDone
-> stop accepting activity for generation
-> revoke/unregister
-> drop mmap and Storage lease
-> release source/process permits
-> Done
```

### 8.2 Child imported Segment

```text
receive SegmentOffer
-> validate descriptor
-> import_seg
-> post READ slices
-> reap all READ CQEs
-> unimport_seg
-> ReadDone
-> wait Done
```

首轮固定顺序为 stop post -> reap all accepted WRs -> unimport -> ReadDone。当前 UDMA unimport
不提供 drain，先后不可随意交换。Parent 收到 ReadDone 后仍执行独立撤权门禁，不能仅依赖 Child 声明。
import 或 unimport 失败的分类需结合 accepted WR 和 native handle 状态，不得一律当作已清理。

### 8.3 Child Piece lease

```text
acquire registered Piece buffer
-> READ posted
-> READ completed
-> wait Done
-> publish immutable lease
-> CRC/write workers join
-> recycle/cache/unregister
```

任何 READ outstanding 时都不能释放或重新分配 Piece buffer。

### 8.4 取消、超时与 fallback 状态机

| 取消时点 | Child 行为 | Parent 行为 |
|---|---|---|
| Offer 前，含本地预算失败 | Cancel；确认无 READ 后 CancelDrained，未注册资源正常归还 | 停止准备；如准备已并发完成则关闭未发布 source；安全清理后 Cancelled |
| Offer 后、尚无 accepted WR | 停止 post；关闭已有 import；CancelDrained | 不能凭 Cancel 释放；等待 drained 声明并执行撤权，再 Cancelled |
| partial post 或任意 READ outstanding | Cancel；停止 suffix/new post；保留全部 DMA owner；reap accepted prefix 后 unimport、CancelDrained | 保持 source/Storage lease；满足撤权门禁后清理并 Cancelled |
| 全部 READ 成功但尚未发 ReadDone | owner 裁决取消或成功路径，只选一种 terminal 流程 | 按匹配的取消/成功状态机处理 |
| ReadDone 已发送，等待 Done 时超时/断链 | 本地 DMA 已 drain，可在确认没有 consumer worker 后安全回收本地 buffer；attempt 失败 | 按自己已掌握的状态完成撤权；不能把连接关闭当作撤权证据 |
| 无法得到 drain 或可靠撤权证据 | 相关 local buffer/import/target 保留或隔离；不复用 | source/backing 保留或隔离；达到预算触发线停止 admission |

CancelDrained 也只是合作方声明；Parent 的安全回收仍要求 provider 撤权证据。TCP EOF/peer crash
没有这个声明时走独立 fault gate，不能伪造 ReadDone 或 CancelDrained。显式 unregister/unimport 的错误
保留 native owner 状态；RAII Drop 只触发 owner 清理流程，不允许异步 DMA 尚未收敛时直接 free。

为 metadata/BufferReady、source admission、Offer、READ drain 和 terminal wait 设置明确的有界 deadline，
并受整 Piece deadline 约束。普通 Piece timeout 不等于 drain deadline 到期后可以释放内存。

TCP fallback 继续整 Piece reset/重下。READ 失败后必须先证明旧 attempt 不会再向 Storage 发布 lease、
写文件或提交 metadata；已提交 blocking workers 必须 join。无法立即 drain 的 DMA 若只指向隔离的独立
buffer，可在 owner 接管且交付路径封闭后开始 TCP 重下，但不得复用该 buffer 或相关未安全回收资源。

### 8.5 故障分类

| 范围 | 示例 | 处理 |
|---|---|---|
| transfer-local | mmap/register/import失败、Piece预算不足、digest mismatch | 失败当前 Piece；shared endpoint保持可用；TCP fallback |
| peer-local | descriptor/token异常、peer超时、重复或越权ReadDone | 停止该 PeerTarget新工作；drain/unimport相关资源；其他peer继续 |
| fabric/device | shared JFS/JFC错误、无法drain、provider invariant破坏 | 停止全局admission；退休并重建整个Fabric；TCP fallback |

首版应保守分类：无法确认 outstanding READ是否收敛时，不把 buffer、Segment或mmap返回普通 allocator。

## 9. 当前代码改造面

### 9.1 FFI / shim

主要文件：

- `dragonfly-client-storage/src/urma/ffi/shim.{c,h}`；
- `dragonfly-client-storage/src/urma/ffi/mod.rs`。

需要增加：

- capability DTO：`max_read_size`、`max_write_size`；
- external VA Segment registration；
- configurable access/token policy；
- Segment descriptor export/free；
- remote Segment import/unimport；
- READ WR及linked READ WR post；
- local/remote range validation；
- Segment/imported Segment/outstanding WR计数和关闭门禁；
- pointer-free Rust DTO，raw UMDK对象不得越过 FFI owner边界。

### 9.2 Native ownership / Runtime / Fabric

主要文件：

- `runtime.rs`；
- `fabric.rs`；
- `lane.rs`；
- `target.rs`；
- `completion.rs`。

需要增加：

- shared endpoint READ能力和 JFS depth校验；
- `RemoteSegmentId` / generation registry；
- per-PeerTarget imported Segment registry；
- READ command、operation owner和completion router；
- Piece byte/JFS/per-peer admission；
- transfer-local drain、peer-local segment cleanup和Fabric shutdown审计。

### 9.3 Buffer

主要文件：`buffer.rs`。

需要新增 Piece-sized contiguous `RegisteredPieceLease`。不要让 READ lease复用当前“一个 message一个
SlotId”的状态机。可以共享底层 registration helper、byte budget、recycle notifier和quarantine策略。

### 9.4 Wire / Session

主要文件：

- `rendezvous.rs`；
- `control.rs`；
- `session.rs`；
- `transfer.rs`。

需要：

- wire版本升级；
- capability扩展；
- PieceMetadata/BufferReady、SegmentOffer/ReadDone/Done 和 Cancel/CancelDrained/Cancelled 状态机；
- READ slice规划和聚合；
- 删除active RecvPosted/SEND credit依赖；
- transfer drop/cancel/timeout时触发精确drain。

### 9.5 Client / Server adapter

主要文件：

- `client/urma.rs`；
- `server/urma.rs`；
- Dragonfly `piece_downloader.rs` 和三类 Piece调用点。

Parent adapter 负责 metadata、BufferReady gate、上传限流、source/process admission、Storage backing
lease、source Segment 和成功/取消清理；Child adapter 负责整 Piece buffer admission、READ、drain、
registered lease 交付和 Storage finish。三类 Piece 仍使用相同 READ backend；任一阶段失败后按第 8.4
节封闭旧 attempt，再复用现有整 Piece reset 和 TCP fallback。业务 started/finished/failed/traffic
指标必须与资源 permit 分开记账，不能用 mmap 创建完成替代上传完成。

## 10. 后续工作包

### R0：设计与 provider capability基线

目标：冻结最小协议和取得目标设备事实。

- 保存 `urma_admin show`、sysfs `max_read_size/max_write_size/max_msg_size`；
- 使用同一 UMDK build验证 RM/RTP和RM/CTP READ；
- 长度覆盖 4 KiB、8 KiB 前后及恰好 8 KiB、64 KiB、1 MiB、16 MiB、64 MiB 和实际 capability 边界；
- 明确“256M”单位和适用 profile；实际单 WR 上限的前一字节、等于上限、超过上限分别验证；
- 明确 READ 路径仍需的 Jetty/JFR/JFC 对象和最小 depth，不因没有 RECV WR 删除依赖；
- 单节点和跨节点分别执行；
- 验证READ completion、错误权限、越界、peer exit和unregister行为；
- 输出provider capability ledger。

验收：选定目标 RM profile 下跨节点 READ 正确，最大长度、completion 和撤权错误的可观测性明确。
RTP/CTP 分别登记支持或不支持，只放行通过 gate 的 profile。当前 ungrant 错误传播缺口需有可执行的
解决或禁用策略；不能仅以普通 unregister 返回成功通过门禁，否则停止 Dragonfly 接入。

### R1：独立 Segment + READ probe

建议在 `urma-transport-lab` 新建独立分支/二进制，不修改现有M4 SEND基线。

- external buffer register/export/import/unimport/unregister；
- read-only token；
- 单 READ 和超 `max_read_size` 多 slice READ；
- CRC32/SHA-256验证；
- timeout、partial post、错误CQE、peer crash和shutdown；
- 撤权后旧 descriptor/token READ、撤权失败注入、地址/token 重用；
- 非页对齐 Piece、tail page、相邻 Piece 共页和越界授权；
- 第三个 peer 持有同一 descriptor/token 时的访问边界；
- 分别验证 unimport 与 outstanding drain、unregister 与硬件撤权，覆盖进程退出后的授权终止；
- 记录 register/import/unregister latency、pinned bytes 和隔离资源，验证隔离触发线与停止 admission。

验收：正常、tail、超限切片、故障和 shutdown 矩阵全部闭环；所有 Segment/WR 有明确 owner 和账目。
正常路径零泄漏，无法安全释放的故障路径必须显式隔离、持续计费且有停止 admission 的界限。
未证明撤权、授权范围或重用安全的 profile 不进入 Dragonfly 集成。

### R2：Dragonfly FFI与native foundation

- 扩展capability DTO；
- 新增Segment/imported Segment RAII wrapper；
- 新增READ WR和completion；
- 增加operation registry、partial-post和close guard；
- mock/unit测试覆盖bounds、token、generation、slice、drain。

验收：native层可以在不接Storage的情况下完成一个Piece-sized内存READ，feature-on编译/测试通过。

### R3：Piece buffer与READ Session

- 实现byte-budgeted contiguous Piece buffer；
- DFUR READ wire、BufferReady byte barrier 和成功/取消状态机；
- 一Piece一Segment/lease；
- 超 `max_read_size` 自动切片；
- shared JFS、per-peer、per-transfer admission；
- terminal ReadDone/Done 与 Cancel/CancelDrained/Cancelled gate；
- Child/Parent 双向预算与进展保底、consumer-held 计费和 quarantine admission 触发线；首轮 idle cache 为零；
- partial-post prefix、取消与迟到 Offer/CQE/terminal 的状态机验证。

验收：内存到内存的单Piece、tail、4/16/64 MiB、并发Piece和多peer均无串包或生命周期错误。

### R4：file mmap + Dragonfly三类Piece接入

- 将现有 `MappedPiece` 扩展为可被owner thread安全注册的backing owner；
- Parent exact-Piece read-only Segment；
- Child lease接入现有registered Storage finish；
- Piece、persistent Piece和persistent-cache Piece；
- mmap/register失败、BUSY和中途READ失败的整Piece TCP fallback；
- `ExportedPieceLease` 覆盖 Storage 不可变内容、mmap、Segment、token 和预算；
- GC/unlink/truncate/覆盖与 active Segment 的明确互斥和保活规则；
- limiter 在 Offer 前完成，业务指标单次结算，Parent permit 安全回收后、Done 前释放；
- 三类 Piece 的 metadata-to-mmap TOCTOU、限流和失败清理测试。

验收：三类Piece normal/tail/fallback/content一致性通过，正常路径无source-to-TX copy。

### R5：fault、shutdown与安全门禁

- Child在SegmentOffer后退出；
- Parent在READ outstanding时退出；
- TCP control断开；
- 一个slice错误、其余slice仍outstanding；
- stale/replayed SegmentOffer或ReadDone；
- token错误、越界READ、重复import/unimport；
- 一个坏peer不影响其他PeerTarget；
- daemon shutdown with outstanding READ；
- GC/truncate/覆盖与 active remote Segment 竞争；
- BufferReady 前取消、Offer/Cancel 交错、partial-post 后取消、ReadDone 后 Done 丢失；
- quarantine 满额后停止 admission、健康 peer 进展及 TCP fallback 与隔离 DMA 并行；
- 复验底层 ungrant 失败、旧 token/地址重用、跨 peer 授权和非页对齐范围门禁。

验收：所有可回收资源都有provider证据；无法证明的路径明确quarantine；无迟到DMA访问已复用内存。

### R6：性能与是否继续决策

固定相同B7 workload比较：

```text
RM SEND/RECV baseline
RM READ from registered staging buffer
RM READ from file mmap
TCP baseline
```

变量至少包含：

- 小 Piece/尾 Piece：4 KiB、8 KiB 边界两侧与边界点；常规 Piece：4/16/32/64 MiB；
- Piece CC1/2/4/8/16/32；
- lane/peer 1/2/4/8；
- READ size：1 MiB、16 MiB、64 MiB、provider 实际 max；上限附近单 WR 与超限切片分开登记；
- warm/cold page cache；
- controlled cleanup、CPU/NUMA 和相同 process 注册预算；
- 两种 backend 实际 active/consumer-held/exported/registered 工作集与预算导致的有效并发；
- 固定总预算的部署对照，以及预算充足时固定有效并发的诊断对照，分别报告而不混合。

指标包括：

- Dragonfly E2E throughput、Piece p50/p95/p99、首个 READ 完成至首次 Storage write 的延迟；
- Piece 内 READ/Storage 重叠损失、跨 Piece 重叠、峰值内存与预算等待；
- PieceMetadata/BufferReady、source admission/limiter、Offer 和 terminal control 的分段耗时；
- Parent/Child CPU、memory bandwidth、context switch/page fault；
- register/import/unregister p50/p95/p99；
- JFS post、CQE/s、owner queue wait、poll batch；
- active/cached/quarantined registered bytes；
- pinned pages、Segment/import table entry和GC等待；
- TCP fallback、peer retirement和Fabric rebuild。

Go条件：correctness/fault全通过，file-mmap READ相对SEND/RECV在目标场景取得稳定E2E或CPU收益，且
registration/pin/GC 成本可控。不能仅凭 WR/CQE 数降低判定收益；若整 Piece buffer 的内存或流水线
代价不可接受，可另立“单 remote Segment + 有界 local READ window”实验，不将首轮 buffer 形态冻结为
最终架构。

No-go条件：provider跨节点语义不稳定、Segment无法安全撤销、page pin/registration成本抵消收益，或
Piece-sized buffer使目标并发不可接受。No-go时保留RM + SEND/RECV并继续优化Chunk、CQ moderation和
owner/completion路径。

## 11. 建议的第一轮实现范围

第一轮不要同时实现缓存、selective completion和复杂allocator。建议严格限制为：

```text
transport mode             RM/RTP（若provider gate确认）
bulk opcode                READ
source                     anonymous aligned memory，之后再file mmap
remote Segment             one per Piece
local destination          one contiguous registered Piece buffer
Piece concurrency          1，之后逐步放开
READ completion            every WR signaled
max_read_size slicing      yes
token                      one read-only token per Segment/generation
fallback                   whole Piece TCP fallback
```

最小实现顺序：

1. capability `max_read_size`；
2. external Segment register/export/import生命周期；
3. 单条 READ；
4. 超限slice聚合；
5. Piece-sized local lease；
6. byte-admission barrier、SegmentOffer/ReadDone/Done 与取消状态机；
7. 内存Piece端到端；
8. file mmap；
9. 逐步开放并发并完成扩展 fault 矩阵；
10. correctness/fault gate 通过后进行 performance。

基础 fault、drain、撤权和超预算停止策略随第 2--7 步实现，不推迟到 file mmap 或性能阶段。
R0/R1 是进入上述 Dragonfly 实现顺序的前置，不因 CC1 或匿名内存而豁免生命周期门禁。

## 12. 明确不在首轮范围内

- 按8 KiB切换SEND/RECV与READ；
- WRITE/WRITE_IMM production path；
- 一个Piece内部动态切换opcode；
- 暴露整个task文件、整个storage或整个registered pool；
- direct-register任意尚未完成/可能被修改的文件区域；
- READ selective completion；
- 跨PeerTarget ordering假设；
- 无界registered buffer cache；
- 没有drain证据时复用旧Segment地址或token；
- 以裸READ perftest数字替代Dragonfly E2E结论。

## 13. 当前结论

纯 RM + READ 与当前 shared RM endpoint 并不冲突。首轮用于建立 correctness baseline 的数据边界是：

```text
one Piece
  = one exact read-only remote Segment
  = one contiguous local registered Piece lease
  = one or ceil(Piece/max_read_size) READ WRs
  = one aggregated Piece completion
```

这一方案有机会减少原 SEND/RECV 的 per-chunk WR、CQE、bulk receive 和 credit bookkeeping；
shared JFR 对象是否仍需保留取决于选定 endpoint/provider。直接注册 file mmap 还有机会消除 Parent
source-fill copy，但收益需扣除 registration、pin、额外控制交互和整 Piece 等待带来的成本。
一个 remote Piece Segment 与一个完整 local Piece buffer 不必永久绑定，是否演进为有界 local windows
由 R6 的内存和流水线数据决定。

真正的难点不在READ opcode，而在Piece-sized注册内存预算、external mmap Segment所有权、token授权、
ReadDone/revoke、peer crash和GC/shutdown。后续应先完成provider与独立Segment probe，再进入Dragonfly
Session和Storage接入，不能跳过生命周期门禁直接做性能路径。
