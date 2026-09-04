# Phase B：URMA production 性能数据路径

更新时间：2026-08-31。

## 0. 当前实施状态

截至 2026-08-31，B1-B6 已完成代码实现。B5/B6 在当前开发环境完成格式、metadata 和 diff 静态检查；
该环境缺少 `protoc`，且 vendored OpenSSL 构建缺少 Perl，因此仍没有在同一环境补齐 feature-on
编译/单测。真实 provider 节点已运行包含 B5/B6 的 validation binary：B4 mmap direct-fill correctness
见 0.1/0.2，B5 post-list 与 B6 inflight 的六组单流矩阵见 B7 性能台账。fault、budget pressure、
多 peer 和 outstanding shutdown 尚未执行，因此 B1-B6 不整体标记为真机 PASS。

B5 已接入 linked SEND/RECV post-list、partial-post 前缀记账和现有 CQ batch/fair owner 调度；B6 已接入
进程级 registered-byte ceiling、固定 TX 保底/RX 余量、可配置 pipeline depth、第二窗口 non-blocking
申请与 budget-pressure 指标。2026-08-30 correctness review 后又补齐 shared-JFC lane retirement：Jetty
及其 owned shared JFR 一并进入 ERROR，发送侧 `WR_FLUSH_ERR_DONE` 按 native `local_id` 路由；只有普通
WR 已全部退休且 flush-done 已到达才删除 Jetty/JFR。2026-08-31 已完成 B5/B6 单 parent、单 child、
单 lane 的真实 provider 参数矩阵：修复 benchmark output 跨文件系统 copy 后，当前最优
`post8-in64` 达到 2410.47 MiB/s；完整口径、六组参数和作废数据见
[B7 真实 Provider 性能验证台账](./b7-real-provider-performance-ledger.md)。该结果不覆盖多 peer、budget
pressure、公平性、fault 或 outstanding shutdown，因此 B1-B6 仍不整体标记为真机 PASS。

### B1：registered window lease 基础已落地

- registered Segment 的 CPU backing 可通过受边界约束的 span 访问，native handle 仍严格留在 owner
  thread；没有把裸指针或 native object 无依据地声明为跨线程安全；
- RX 使用只读 lease，TX 使用独占 direct-fill lease；pool/lease identity、slot generation 和
  `user_ctx` generation 校验共同阻止 wrong-pool recycle、double recycle、迟到 CQE 和迟到 recycle
  命中已复用 slot；
- lease 显式 recycle 和 `Drop` 保底都只向 owner thread 发送 urgent command，不从 consumer thread
  直接调用 native API；
- close/shutdown 会检查 active lease；无法安全收敛时隔离 backing，不能先注销 Segment 后留下悬空
  view；
- 已覆盖 generation、tail span、direct fill、wrong pool、double recycle、active close、Drop urgent
  recycle 和 `Send/Sync` 边界测试。

### B2：registered RX completion 与双窗口预投递已落地

- 一个逻辑 RX window 的全部 slot 会先原子保留，成功后才逐 WR post；预算不足返回可分类的
  `BufferUnavailable`，不会留下半个 window 已 post 的状态；
- CQ completion 直接归并为只读 `RegisteredRxWindowLease`，保留多 slot part、精确有效长度和尾
  slot，不再生成 `ReceivedChunk(Vec<u8>)`；
- Session 最多预投递两个 window。pipeline permit 从 pending、completed 一直持有到 consumer 释放
  lease，第三个 window 不能越过背压；第二个 window 资源不足时安全退化为深度 1；
- `RecvPosted` 只在完整 window post 成功后发送；lease 释放后由 owner recycle，后续 repost/credit
  才能继续推进；现有 sequence/length/Done、整 Piece timeout、drop/error gate 保持不变；
- transport-neutral `Downloader` 兼容入口仍会把完整 registered lease 聚合复制一次到 `Vec`，然后
  显式 recycle；B3 production Piece 路径不再走这个兼容入口。

### B3：Storage direct-write 与 digest overlap 已落地

- `UrmaStreamReader`/`UrmaReceivedWindow` 把 immutable multi-span registered lease 交给 Storage，
  `piece.rs` 不接触 Fabric、lane、slot 或 CQ 类型；
- normal、persistent、persistent-cache 三类 Piece 均接入 URMA 专用 completion path；每个 window
  直接 positional write，CRC32 与 write 在两个 blocking worker 上并行读取同一 lease；
- 两个 worker 全部 join 后才显式 recycle；write/digest error 也先等待已提交工作收敛，drop/error
  保底仍通过 owner urgent command 回收；
- expected length 和 positional offset 使用硬边界/溢出检查，过长 window 不得覆盖后续 Piece，短流
  也不会提交 metadata；
- Storage/digest/transfer 失败继续按 Piece 类型 reset partial metadata，再从 TCP 整块重下；最终 window
  仍在 Done 验证后才发布；
- production RX 当前为 `NIC DMA -> registered spans -> pwrite/digest`，额外 userspace staging copy
  为 0。只有通用 `Downloader` 兼容调用仍保留一次 lease -> `Bytes` copy。

### B4：TX direct-fill、mmap 与双窗口 ring 已落地

`[状态：代码完成，纯测试/编译通过；mmap 生产路径与 ring=2 send∥fill overlap 真机 correctness PASS（2026-08-29，见 0.1/0.2），ring=1 退化注入/持久类 Piece 场景真机待验]`

- server 不再创建 owned window，也不再逐 chunk `.to_vec()`；`MappedPiece` 或 `RangeReader` 直接填充
  exclusive `TxWindowLease` 的逐 slot span，生产路径已删除普通 Vec -> registered Segment copy API；
- 一个逻辑 window 固定为一个 message 对应一个 TX slot。即使协商 chunk 小于 slot size，也不会让多个
  outstanding SEND 共享同一个 slot/user_ctx；尾 window 可在不增长 lease 的前提下缩短 span 和 chunk 数；
- `CompletionRouter` 为整窗建立共享 completion state；每个 SEND CQE 只把自己的 slot 从
  `SendPosted` 恢复为 `LeasedTx`，全部 CQE 完成后才把 exclusive lease 返回 Session，错误/timeout 时也
  不会在 provider 仍引用 slot 时 recycle；
- server 使用两个独立 lease 实现 ring：`send(current)` 与 `fill(next)` 并行；第二个 lease 因 TX pool
  压力或有界等待超时时退化为 ring=1，单窗必须等全部 CQE 后才 refill；
- 默认 TX 分区为 128 slots，`pipelineDepth=2` 时协商 SEND window 进一步限到 64 chunks，为第二个
  lease 保留形成 ring 的空间；B6 已将该分区改为 byte 配置，shared overflow/严格公平仍留待数据决定；
- 新增 `storage.server.urma.mmapContent`，启用后 normal/persistent/persistent-cache 完成态 Piece 均先尝试
  `map_upload_piece`；cache-resident 或 mmap 失败仍走原有 upload `RangeReader`；
- TX 当前为 `mmap/RangeReader -> registered TX spans -> NIC DMA`，只保留一次必要 source-fill copy。

当前验证结果：

```text
cargo fmt --all -- --check                                      PASS
cargo check -p dragonfly-client-storage                         PASS
cargo check -p dragonfly-client --features urma                 PASS
cargo test -p dragonfly-client-storage --features urma urma::   43 passed / 0 failed
cargo test -p dragonfly-client-storage --features urma test_write_urma_stream
                                                                  2 passed / 0 failed
cargo test -p dragonfly-client-storage --features urma --lib    155 passed / 0 failed
cargo test -p dragonfly-client --features urma --lib            62 passed / 0 failed
```

上述 URMA 测试使用本地 UMDK build tree，未启动真实 provider。

### 0.1 B4 真机验证记录（2026-08-29，node1 parent / node2 child）

首次真实 provider 跨节点验证：B4 mmap direct-fill TX 生产路径 correctness PASS。

- 场景：1 GiB 任务经 URMA 从 parent（node1）下载到 child（node2），
  parent 开启 `storage.server.urma.mmapContent`；
- mmap 命中：初始并发 8 个 Piece（0-7）全部输出
  `URMA upload using mmap content`（server/urma.rs），grep 全程无
  `URMA mmap unavailable; falling back to reader`，即 mmap direct-fill
  命中率 8/8、reader fallback 为 0；
- lane 复用：全部请求来自同一 `remote_address`（90.91.177.157:45222），
  persistent lane 顺序复用，符合 B4 设计；
- 内容一致性：source（/var/www/dragonfly/input-1g.bin）、parent 侧输出
  （/tmp/parent-b4-mmap1.bin）、child 侧输出（/tmp/child-b4-mmap1.bin）
  三端 SHA-256 均为
  `49bc20df15e412a64472421e13fe86ff1c5165e18b2afccf160d4dc19fe68a14`；
- 同日落地配套观测：
  - 修复连接级 `#[instrument]` span 的 `task_id`/`piece_id` 字段随
    `record()` 逐 Piece 累积导致日志行膨胀的问题；改为每 Piece 独立
    `info_span!("urma_piece", task_id, piece_id)`；
  - finished 日志新增 TX ring 观测字段：`tx_source`（mmap|reader）、
    `tx_windows`、`tx_ring_depth`（仅当存在 send∥fill 重叠才记 2）、
    `tx_overlap_windows`、`tx_second_lease_fallback`、`tx_fill_ns`、
    `tx_send_wait_ns`；
- 结构性发现：默认参数下 Piece=4 MiB 且窗口=64 chunks×64 KiB=4 MiB，
  每个 Piece 恰为单窗口，`tx_windows=1`，双 ring 结构性不会触发。真机
  验证 ring=2 需 Piece 长度大于 TX 窗口（如调小 `maxInflightChunks` 至
  8 使窗口为 512 KiB，4 MiB Piece 共 8 个窗口）；
- 本轮仍未覆盖（保持 B7 债务）：ring=2/overlap 真机证明（依赖上述参数
  调整）、ring=1 退化注入、persistent/persistent-cache Piece mmap 场景、
  fault/shutdown、B2/B3 RX 路径真机 correctness、吞吐与 copy 口径的
  B7 同口径测量。

当时结论：B4 mmap 生产路径的真机 correctness 已可标记 PASS；B1-B4 整体
不据此整体标记真机 PASS，其余验证项仍按 B7/runbook 执行。该记录形成后已继续完成
B5 post/CQ/credit 批处理和 B6 预算/退化代码；当前状态以第 0 节和第 8 节为准。

### 0.2 B4 双 ring（ring=2）真机验证记录（2026-08-29，node1 parent / node2 child）

按 0.1 的结构性发现调整参数（减小 TX 窗口使 Piece 多窗口化）后，双 ring
真机验证 PASS。

- 场景：1 GiB 任务，parent 开启 `mmapContent`，TX 窗口缩为 8 chunks，
  4 MiB Piece 分 8 个窗口，双 ring 结构性生效；
- ring 深度：多个 Piece 的 finished 日志一致显示
  `tx_windows=8 tx_ring_depth=2 tx_overlap_windows=7
  tx_second_lease_fallback=false`——8 个窗口中 7 个发生 send∥fill
  重叠（最后一个窗口无 next 可填，结构性少 1），零退化；
- 第二个 lease 获取：每个多窗口 Piece 均输出
  `URMA TX double ring enabled tx_ring_depth=2 window_chunks=8`，
  全程无 `URMA TX second lease unavailable`；
- overlap 有效性：`tx_fill_ns≈0.69ms` 远小于
  `tx_send_wait_ns≈2.37ms`，即 send 等待 CQE 期间 fill(next) 已完成，
  TX 数据填充被完全隐藏在发送等待内，符合 ring 设计预期；
- mmap：本轮 Piece 全部命中 `URMA upload using mmap content`；
- 内容一致性：source（/var/www/dragonfly/input-1g.bin）与 parent 侧输出
  （/tmp/parent-b4-ring2-1.bin）SHA-256 均为
  `49bc20df15e412a64472421e13fe86ff1c5165e18b2afccf160d4dc19fe68a14`，
  与 0.1 的 child 端基准一致；node2 侧错误审计
  （fallback/cqe/completion/digest/protocol/generation/double recycle/
  wrong-pool/transfer failed）grep 全部为空；
- span 修复生效：日志行只包含单个 `piece_id`/`remote_address`，无字段
  累积。

结论修订：B4 的 ring=2 send∥fill overlap 真机 correctness PASS。
B4 剩余真机债务：ring=1 退化注入（second lease 强制失败场景）、
persistent/persistent-cache Piece mmap 场景、fault/shutdown、
B2/B3 RX 路径真机 correctness、B7 同口径吞吐测量。

## 1. 结论与范围

Phase B 不再以“能完成一个 Piece”为目标，而是把 Phase A 的 copy-mode 数据路径收敛成
适合 Dragonfly production 的有界流水线。

`[设计决定]` 当前 RDMA production path 中与 transport 性能直接相关的能力，原则上都纳入
URMA Phase B：

- registered buffer lease 与全局注册内存预算；
- RX completed window 零 staging-copy 交付；
- RX 双 window 预投递；
- Storage 直接从 registered RX window `pwrite`；
- digest、写盘和下一窗口接收重叠；
- TX registered 双 window ring；
- Storage/mmap 填充下一 TX window 与当前 window SEND 重叠；
- 多 outstanding WR、receive credit、批量 post 和 CQ batch；
- 并发 admission、背压、取消、异常回收和 shutdown drain；
- 能定位 source-fill、credit、CQ、Storage 和 CPU 瓶颈的指标。

“对齐 RDMA”指对齐能力和业务边界，不要求照搬 libfabric 的类型名或内部接口。URMA 仍由
现有 `Fabric -> Session -> client/server adapter` 承担 provider、Jetty、WR/CQE 和 lane 生命周期。

demo 的作用是提供已验证的 URMA 实现依据。不会迁移 benchmark CLI、独立文件协议或固定负载
技巧，也不会重新建立 demo 的 application 层。

Phase B 的目标 copy 口径是：

```text
下载 RX：NIC DMA -> registered RX window -> pwrite/digest
          0 次额外 userspace staging copy

上传 TX：mmap/RangeReader -> registered TX window -> NIC DMA
          1 次必要的 source-fill copy
```

RDMA 当前的 mmap 上传也仍需把 mmap 内容复制到 registered send window；因此 Phase B 不把它
误称为 TX zero-copy。直接注册文件映射、URMA READ/WRITE、remote Segment 和 UBS Memory 不属于
本阶段承诺，除非后续数据证明上述 SEND/RECV 路径仍无法达到目标。

## 2. Phase A 历史基线与当前过渡状态

### 2.1 下载路径存在两次 staging copy

```text
provider DMA
-> registered RX slot
-> SegmentHandle::read -> ReceivedChunk(Vec)       copy 1
-> receive_next_window::extend_from_slice -> Vec  copy 2
-> Bytes::from(Vec)                               ownership move，无 copy
-> generic PieceContentStream
-> Storage
```

`[源码确认]` copy 1 位于 `urma/buffer.rs::complete_recv`，copy 2 位于
`urma/session.rs::receive_next_window`。当前设计易于保证 slot 及时回收和 Rust ownership，但会增加
内存带宽、allocator 和大 Piece CPU 成本。

`[B2 已实现]` 上述 `SegmentHandle::read -> ReceivedChunk(Vec)` 路径已经删除。CQ 现在发布
`RegisteredRxWindowLease`；兼容 `PieceContentStream` 的入口只在完整 window 上做一次聚合 copy，随后
显式 recycle lease。

`[B3 已实现]` Dragonfly production Piece 路径现在取得 `UrmaStreamReader`，Storage 直接遍历 lease 的
registered spans 做 positional write 和 CRC32，不再经过兼容 `PieceContentStream`，因此 production
RX 已达到 0 次额外 userspace staging copy。兼容 copy 只为 transport-neutral trait 调用保留。

### 2.2 上传路径也存在两次 staging copy

```text
Storage RangeReader -> owned window
-> chunk slice.to_vec()              copy 1
-> registered TX slot write/memcpy   copy 2
-> provider DMA
```

`[源码确认]` copy 1 位于 `urma/session.rs::send_next_window`，copy 2 位于
`urma/buffer.rs::write_tx`。而且当前一个 window 完成后才读取/发送下一个 window，Storage source-fill
与 NIC SEND 没有形成稳定重叠。

### 2.3 当前架构应保留的部分

- process-level Runtime/JFC/registered Segment；
- owner thread 统一执行 native 调用和 CQ polling；
- per-peer RC Jetty/lane 与 persistent Session；
- `RecvPosted` receive-ready barrier 和 negotiated transfer shape；
- per-operation completion route、timeout、abort、drain/reap；
- Dragonfly Storage、limiter、metrics、digest、fallback 和 Piece metadata 语义。

Phase B 是替换 Fabric/Session 内的数据所有权与窗口调度，不推翻这些边界。

## 3. 依据与生产取舍

| 能力 | RDMA 当前实现 | URMA demo `tcp-urma-file-transfer` | Phase B 决定 |
|---|---|---|---|
| registered buffer lease | `PooledBuf`/`ReceivedWindow` | `RegisteredRxWindowLease` | 必做 |
| 注册内存总预算/等待 | best-fit pool、`acquire/try_acquire` | 固定 slot pool | 必做，URMA 可继续固定 slot，但预算必须全局有界 |
| RX direct window | CQ 完成后直接发布 lease | lease 覆盖 completed RX slot run | 必做 |
| RX 双窗口 | `RECEIVE_PIPELINE_DEPTH=2` | 大 receive credit + window recycling | 必做，深度可配置且受预算约束 |
| Storage direct write | registered bytes 直接 `pwrite` | worker 直接 `pwrite` lease | 必做 |
| digest/write 并行 | 每 window 并行 digest + write | async sink + CRC workers | 必做能力；不照搬 benchmark worker CLI |
| TX 双窗口 ring | fill next 与 send current 并行 | registered TX ring/batch | 必做 |
| mmap source | `MappedPiece` + `WillNeed/Sequential` | file mmap source | 必做，cache-resident Piece 保留 reader fallback |
| 多 outstanding SEND/RECV | per-window 多 op + 双窗口 | window 64、显式 credit | 必做；具体深度由配置/capability决定 |
| batch post/CQ | libfabric post + completion loop | linked WR post-list、CQ batch | 必做 URMA-native 优化，但先保证错误能逐 WR 路由 |
| credit 返回 | `RecvPosted` window | repost 后批量返回 remote credit | 必做语义；只有 lease 释放并 repost 后才能返还 credit |
| fixed-TX 重复 payload | 无 | memory benchmark fast path | benchmark-only，不进 production |
| warmup messages | 无 | benchmark 稳定性开关 | benchmark-only，不进 Piece 协议 |
| external CRC 参数 | metadata 已有 digest | 避免 benchmark 预扫描 | benchmark-only；production 继续使用 Piece metadata digest |
| benchmark JSON/CLI | 无 | 独立 harness | 不迁移；只复用观测字段和实验证据 |

demo 的真实 provider 结果证明了这些机制组合有价值，但不能直接作为 Dragonfly 性能承诺：

- memory fixed-TX：16 GiB、window 64、post-list 16，537.90 Gbit/s；
- file-to-file：8 GiB、window 64、post-list 16、CRC workers 4，三轮平均 55.10 Gbit/s；
- 50 GiB 未由 benchmark 预热的文件路径只有 29.79 Gbit/s，`tx_fill` 占 wall time 99.01%。

`[实验确认]` 最后一项尤其说明 mmap 本身不会消除 registered TX source-fill；Phase B 必须同时做
双窗口 overlap 和 source-fill 指标，不能只增加 WR 深度。

### 3.1 URMA slot pool 是否对齐 RDMA buffer pool

`[设计决定，2026-08-29]` 当前不把 URMA fixed slot pool 重写成 RDMA best-fit `PooledBuf`。两者需要
对齐 production 能力与资源语义，不要求对齐 allocator 结构。

当前模型差异：

| 维度 | RDMA | URMA |
|---|---|---|
| 注册方式 | 按需分配/注册 `PinnedBuf`，完成后进入 best-fit cache | Runtime 启动时注册一个连续 Segment |
| 分配粒度 | 任意 logical length，复用最小可容纳 buffer | 固定 slot；当前默认 64 KiB |
| 预算 | `max_registered_bytes` 共享 byte budget，支持 wait/try-acquire | process byte ceiling + 固定 TX/RX 分区；默认 40 MiB，其中 TX 8 MiB（128 slots）、RX 32 MiB（512 slots） |
| window 布局 | 通常一个连续 `PooledBuf` | 多 slot 组成 multi-span logical window |
| 回收 | 最后一个 operation/reader owner 释放后直接返回本地 pool | consumer 通过 urgent owner command 校验 lease/generation 后回收 |
| 资源不足 | 等待预算或 non-blocking 失败 | `BufferUnavailable`；第二 window 安全退化为 pipeline depth 1 |

#### 3.1.1 RDMA `maxRegisteredBytes` 语义澄清与 URMA chunk-size 适配

`[源码确认，2026-09-03]` RDMA 的默认 `maxRegisteredBytes=512MiB` 是 active + idle cached
registered buffer 的**总预算**，不是启动时分配并注册一个连续 512 MiB MR。RDMA `BufferPool` 初始为空；
`acquire_buffer(len)` 优先 best-fit 复用容量足够的 idle `PinnedBuf`，cache miss 时才按请求的可变长度
分配并调用 `fi_mr_reg`。buffer 归还后继续保留注册并进入 cache，所有 active/cached buffer 通过按
64 KiB 粒度计数的 semaphore 共同受 512 MiB 上限约束。

RDMA 默认 `chunkSize=4MiB`、`maxInflightChunks=16`，所以一个 logical receive window 通常为
64 MiB。接收端可在预算允许时同时持有两个独立 window buffer；发送端可申请一个覆盖双窗口的
128 MiB staging buffer。运行后可能存在多个不同 capacity 的 cached MR，其总量接近 512 MiB，
但不等价于一个 512 MiB contiguous MR。

当前 URMA 则确实在 Runtime 启动时注册一个 `maxRegisteredBytes` 大小的连续 Segment，再按固定
64 KiB slot 切分。其有效单条 SEND/SEND_IMM payload 是 provider `max_msg_size` 与 slot size 的较小值；
因此 provider 支持大于 64 KiB 的消息时，当前实现仍无法利用该能力。`postListSize` 只批量提交多个
64 KiB WR，不会增大单条消息。

后续适配采用以下边界：

1. 配置和 capability negotiation 对齐 RDMA，引入显式 `chunkSize`，取本地配置、两端 provider
   `max_msg_size` 和 peer capability 的最小值；默认先保留 64 KiB 以维持兼容性。
2. 不直接迁移 RDMA 的 per-window variable-length registration/cache；URMA 第一阶段继续使用 process
   级连续预注册 Segment，并将 slot size 改为 Runtime 启动时确定的统一 `chunkSize`。
3. window 容量应与 chunk size 解耦，优先按 `windowSize` 字节数表达，再推导
   `chunksPerWindow = windowSize / negotiatedChunkSize`，避免增大 chunk 时按
   `chunkSize * maxInflightChunks * pipelineDepth * maxConcurrentTransfers * laneCount` 放大注册内存。
4. 第一阶段只支持一个 Runtime/Segment 对应一种物理 slot size；协商得到更小 chunk 时使用 slot 前缀，
   接受一定内部碎片。只有真实 workload 证明 size 分布离散且浪费显著时，再评估 size class 或动态 arena。
5. SEND_IMM identity、slot generation、lane/transfer completion route、lease recycle 和 shutdown/drain
   语义保持不变；chunk-size 改造不能弱化这些已验证的所有权边界。

验证时先固定 logical window bytes，比较 64 KiB、256 KiB、1 MiB 和 4 MiB chunk，以隔离单 WR 大小
带来的 posting/CQE 收益；另设一组 RDMA 默认形态（4 MiB × 16 = 64 MiB window）用于机制对齐，不能
与固定小 window 的结果混为同一变量实验。

URMA 已在 B6 补齐、不要求照搬 RDMA 类型的能力：

- `maxRegisteredBytes` 约束 process 级预注册 Segment 总量，active + idle/预留内存都计入；
- `txRegisteredBytes` 提供固定 TX 保底，RX 使用剩余预算；两端各至少保留一个 64 KiB slot；
- 已持有一个 window 时，第二 window 通过 non-blocking command admission/立即 slot 申请，失败退化为深度 1；
- `pipelineDepth` 将方向 slot 数折算为单 window 上限，默认 2；
- registered bytes 和 required/optional budget pressure 已有低基数 metrics；peer/Piece 维度保留在结构化日志；
- active lease、outstanding WR、credit 与 Segment shutdown 继续通过 close/drain/recycle gate 审计。

尚未实现的是 TX/RX shared overflow、动态 arena/size class，以及严格的跨 peer slot fairness；它们继续受
下述真实 workload 证据门槛约束，不是 B6 当前固定分区方案的隐含承诺。

只有真实 workload 出现以下证据时，才升级 allocator：

1. 一侧 slot 耗尽并频繁 `BufferUnavailable`，另一侧 TX/RX slots 或 registered bytes 长期空闲；
2. negotiated chunk 明显小于 slot size，导致有效 payload/registered bytes 比例过低；
3. multi-span 数使 WR/CQE、CRC slice 遍历、`pwrite` syscall 或 owner recycle queue 成为 CPU 瓶颈；
4. 双窗口经常退化为单窗口，但并非 Storage consumer 慢或 registered-byte ceiling 确实耗尽；
5. Piece/window/message size 分布很散，单一 slot size 无法同时满足内存利用率和 WR 数；
6. 启动时固定 pin 内存违反容器 memlock、低流量节点成本或多 device/runtime 扩展要求。

出现上述证据后的优先演进顺序是：

```text
post/CQ/pwritev batching
-> 调整 slot size
-> TX/RX reserved minimum + shared overflow
-> 多 size-class slot/slab
-> 可增长/可回收的 multi-Segment arena
-> 最后才评估完整 best-fit variable buffer pool
```

如果双窗口稳定、slot 利用率高、TX/RX 没有单边闲置、owner queue 不是瓶颈且 pinned memory 可接受，
fixed slot pool 更简单且热路径没有 MR registration miss，应继续保留。B5/B6 已在现有 slot 模型上完成
批处理和预算/退化最小闭环；是否继续演进 allocator 由 B7 指标决定，不能为了与 RDMA 内部同形而提前重写。

## 4. 目标架构

### 4.1 下载：registered RX window 直到 Storage 消费完成

```text
UrmaClientSession
  -> acquire RX WindowLease A/B
  -> post window A and B receives
  -> send RecvPosted(A), RecvPosted(B)
  -> CQ validates every chunk in A
  -> publish ReceivedWindow(A)
       +-> Storage pwrite(A) ----+
       +-> CRC32(A) -------------+-> release A -> recycle/repost -> credit
  -> CQ/provider concurrently fills B
```

关键不变式：

- window 内所有 chunk CQE 完成且长度/sequence 校验成功后才可发布；
- lease 存活期间 provider、owner thread 和 allocator 都不能改写或释放对应 slot；
- write 与 digest 只持有共享只读视图；两者均结束后才能 recycle；
- consumer drop、timeout、digest/write failure 也必须经 owner thread 回收或 drain；
- shutdown 必须等待/撤销 active lease，不能先注销 Segment；
- 最终 window 仍受现有 Done gate 约束，不能把 peer 的迟到错误隐藏为成功。

业务层需要像 RDMA 一样增加 URMA 专用 reader/Storage completion path。generic
`PieceContentStream<Bytes>` 保留给 TCP/QUIC 和兼容路径，但不能承载零 copy registered lease；强行把
lease 转成 `Bytes` 会重新引入 copy或不安全的生命周期。

### 4.2 上传：registered TX 双窗口 ring

```text
Window A: mmap/RangeReader -> registered A -> post SENDs -> wait CQEs
Window B:                    fill registered B ----------------^

下一轮：
Window B: registered B -> post SENDs
Window A:                 refill A
```

关键不变式：

- server adapter 不再构造完整 owned window 后让 Session 对每 chunk `.to_vec()`；
- source 直接填充一个 exclusive `TxWindowLease`；
- Session 只把 lease 中的 offset/length 提交给 Fabric，不复制 payload；
- 一个 half 上所有 SEND CQE 完成前不得 refill；
- ring=2 只有在 Piece 跨多个 window 且注册内存预算允许时启用，否则安全退化为 ring=1；
- mmap 只用于完成态、磁盘可映射 Piece；cache-resident 或 mmap 失败走 RangeReader direct-fill；
- Piece/transfer limiter 和 metrics 仍在 server adapter，不下沉到 Fabric。

## 5. 工作包与依赖顺序

### B0：冻结 correctness gate 与性能观测

在改变 ownership 前保留 Phase A 的全部 contract/fallback/fault tests，并补齐以下计数：

- TX/RX payload bytes、WR post/CQE/error；
- active/peak TX、RX slots 和 registered bytes；
- source-fill、post、CQ wait、remote-credit wait；
- RX window consumer wait、pwrite、digest、recycle；
- owner command queue depth/等待时间；
- copy-mode fallback 次数（过渡期）。

真实 provider Phase A runbook 必须通过，才能把后续异常归因于 Phase B；环境未就绪时可以继续做
纯测试和编译，但不能把 B0 标记为真机完成。这里不要求先做 Dragonfly TCP/URMA 同口径 benchmark。

### B1：registered window lease 基础

`[状态：代码完成，真机待验]`

1. 将 registered backing 的生命周期与 slot allocator 状态解耦，提供只读 RX lease 和独占 TX lease。
2. 明确 backing 是否可安全跨 owner/Tokio thread 读取；以 UMDK ABI/provider 要求验证 `Send/Sync`，
   不直接给 raw pointer 添加无依据的 unsafe trait。
3. lease drop 不直接调用 native API；通过 owner command 显式 recycle，drop 作为保底通知。
4. pool 记录 active lease，close/shutdown 在 lease 未清空时拒绝注销 Segment。
5. 增加 slot generation，防止迟到 CQE 或迟到 recycle 命中已复用 slot。
6. 单测覆盖 double recycle、wrong pool、partial window、尾 slot、active lease close、abort/drain。

这一工作包只建立安全所有权，不先改变 `piece.rs` 业务链路。

### B2：RX direct window 与双窗口预投递

`[状态：lease/completion/pipeline 代码完成，真机待验]`

1. Fabric 支持把一个连续/逻辑连续 RX window 的多个 slot 作为一个 lease 发布。
2. Session 将 `receive_next_window -> Vec<u8>` 改为 receive window reader/stream。
3. 同时预投递最多两个 window，并允许提前发送两个 `RecvPosted`；深度受 Jetty recv depth、RX slot
   budget、Piece 剩余长度和 consumer 背压共同限制。
4. receive credit 只在 Storage 释放 lease且 slot 确实 repost 后返回。
5. 保留 sequence/length/Done、整 Piece timeout、consumer drop 和 peer error 语义。

B2 消除了 RX slot -> `ReceivedChunk(Vec)` 和 chunk Vec -> aggregate window 的旧两段式路径；在 B2
完成时兼容 adapter 仍有一次 lease -> aggregate window copy。B3 已使 production Piece 路径绕过该
兼容 copy；仅 transport-neutral trait 调用仍保留它。

### B3：Storage direct-write 与 digest overlap

`[状态：代码完成，纯测试/编译通过，真机待验]`

1. 增加 `download_piece_from_parent_finished_urma`，接口形态与 RDMA 专用 completion path 对齐。
2. 每个 registered window 直接 positional write 到目标 Piece range，不经过 generic stream staging。
3. CRC32 与 write 并行读取同一个 immutable lease，同时允许 Fabric 接收下一 window。
4. 对 expected length 做硬边界，禁止恶意/错误 peer 覆盖后续 Piece。
5. blocking pwrite 不可取消；fallback 前必须等待已提交 write 收敛，再 reset partial Piece。
6. normal/persistent/persistent-cache 三条路径保持相同失败重置和 TCP 整块重下语义。

### B4：TX direct-fill、mmap 与双窗口 ring

`[状态：代码完成，纯测试/编译通过，真机待验]`

1. server 从 BufferPool 获取 `TxWindowLease`，Storage source 直接填充 lease。
2. 删除 production 数据路径上的 per-chunk `.to_vec()` 和普通 Vec -> registered slot write。
3. 添加 `MappedPiece` source；normal/persistent/persistent-cache 完成态文件均尝试 mmap，cache-resident
   和失败场景回退 RangeReader。
4. ring=2 时并行 `send(current)` 与 `fill(next)`；ring=1 时严格等 CQE 后再覆盖。
5. 注册预算等待受 timeout/admission 约束，资源不足返回可分类 BUSY/TOO_LARGE，不无限占用 transfer slot。

完成后 TX 只保留 source -> registered window 的一次 fill copy。

### B5：URMA post/CQ/credit 批处理

`[状态：代码完成；单 lane postListSize=1/8 真实 provider 正常路径和性能矩阵已测；partial-post、错误 CQE、flush 与多 peer 待验]`

1. shim/FFI 已增加 linked SEND/RECV WR post-list，批内每个 WR 保留独立 `user_ctx`。
2. UMDK `bad_wr` 被转换为成功提交前缀，Session 只为该前缀消费 slot/credit；未提交后缀可安全回收。
3. lane 按 `postListSize` 分批，且仍由 configured window、Jetty/JFC depth、slot count 和 remote posted
   credit 联合限界；默认值为 1，允许 1..64，避免未校准即改变生产行为。
4. CQ 沿用 batch poll（当前 batch 16）和逐 WR completion route；partial CQ、单 WR error 仍按独立
   `user_ctx` 退休。无有效 `user_ctx` 的 `WR_FLUSH_ERR_DONE` 改按 native Jetty `local_id` 路由，不能
   poison 整个 Fabric，也不能替代真实 `WR_FLUSH_ERR` 对 outstanding WR 的逐条退休。
5. owner 在 CQ 与 command 之间保留公平调度，持续 CQ busy 不得饿死 shutdown/abort/control command。

具体默认值不照搬 demo 的 window 64/post-list 16；先由 capability 限界，再在真实 provider 上校准。

### B6：并发预算、配置与退化路径

`[状态：最小固定分区方案代码完成；inflight=16/32/64 单 lane 真机矩阵已测；budget pressure、跨 peer 公平与并发退化待验]`

- `maxRegisteredBytes` 默认 40 MiB、范围 128 KiB..4 GiB；`txRegisteredBytes` 默认 8 MiB，RX 使用
  剩余预算。Runtime 仍一次注册连续 Segment，并按固定 64 KiB slot 划分，默认保持 TX 128/RX 512。
- `pipelineDepth` 默认 2、范围 1..2；单 window 的方向上限为 `slots / pipelineDepth`，因此默认 TX
  上限 64、RX 上限 256，最终仍受协商和 provider capability 限界。
- 第一 window 属于 required admission，失败返回 BUSY/fallback；第二 TX/RX window 属于 optional，使用
  non-blocking command admission 和立即 slot 申请，失败退化为 depth 1，不创建无界 owned buffer。
- shared process Fabric 会校验预算配置一致性，不能静默创建不同 Segment 形态。
- endpoint/session failure 继续等待 outstanding WR 归零，清 remote credit，并通过 lease book、pool close
  gate、urgent recycle 和 pending completion 路径审计生命周期。
- 新增 `dragonfly_client_urma_registered_bytes{direction="tx|rx"}` 和
  `dragonfly_client_urma_budget_pressure_total{direction,stage}`（stage=`required|optional`）；peer/Piece、
  source-fill、Storage backpressure 使用结构化日志，避免高基数 metric labels。

B6 的协议边界不包含“同一 lane 并发多个 Piece”：同一 parent 的 persistent Session 仍顺序传 Piece；不同
peer 使用独立 lane。TX/RX shared overflow、动态 allocator 和更强跨 peer fairness 留待 B7 数据决定。

### B7：真实 provider 验证与性能验收

2026-08-31 已完成固定 topology 的 1 GiB 单 lane 参数矩阵，当前有效最佳结果为
`post8-in64 = 2410.47 MiB/s`；参见
[B7 真实 Provider 性能验证台账](./b7-real-provider-performance-ledger.md)。这只覆盖下列顺序中的
连续正常路径和单流性能观测，不替代尚未执行的 fault、资源压力和多 peer 项。

验证顺序：

1. 单 Piece correctness：尾 window、非整 chunk、三类 Piece、CRC、Done；
2. 同 Session 连续 Piece：至少 10 次，确认 lease/generation/credit 不泄漏；
3. fault/shutdown：SEND/RECV outstanding、consumer drop、pwrite 失败、timeout、断链；
4. 资源压力：ring=1、ring=2、多 peer、注册预算耗尽和公平进展；
5. memory source：确认 WR/CQ/credit/owner thread 上限；
6. file source：确认 source-fill、mmap fault、pwrite、digest 和 page-cache 影响；
7. 最后再做 Dragonfly TCP/URMA 同口径 benchmark，评估端到端吞吐和两端 CPU。

## 6. 完成标准

Phase B 只有同时满足以下条件才完成：

- production RX 数据路径无 registered -> owned -> aggregate staging copy；
- production TX 无 per-window owned staging 和 per-chunk `.to_vec()`，只有 direct source-fill；
- RX/TX 双窗口在预算允许时真实 overlap，预算不足可正确退化；
- mmap、RangeReader fallback、normal/persistent/cache 三类 Piece 均正确；
- digest/write/receive pipeline 不改变 length、CRC、metadata commit 和 TCP fallback 语义；
- 所有 lease、WR、slot、credit、lane 在成功、失败、取消、shutdown 后归零；
- feature-off、feature-on unit/contract/fault tests 通过；
- 真实 UDMA provider correctness、连续传输、fault/shutdown 验证通过；
- benchmark 能解释瓶颈，不能只有一个总吞吐数字；性能回归条件和环境被记录。

吞吐目标不能直接使用 demo 的 537.90/55.10 Gbit/s 作为 Dragonfly 门槛，因为 Storage、Piece、CPU、
文件系统和计时边界不同。初次真机验收先要求：每引入一个 work package 不降低 correctness，copy count
按目标下降，pipeline 指标证明存在 overlap；最后再基于同环境 Phase A 与 TCP 建立可量化门槛。

## 7. 明确不做

- 迁移 standalone benchmark protocol/CLI/JSON、fixed-TX payload、warmup message；
- 在 Fabric 内复制 Storage、Piece、limiter、fallback 或 metadata；
- 为了和 RDMA 同名而暴露 transport-private `acquire_buffer`；
- 无界 window、无界 channel 或整 Piece Vec；
- 未验证的 raw pointer 跨线程、CQE 前复用 TX、lease 释放前 repost RX；
- 将 URMA READ/WRITE、remote Segment 或 UBS Memory作为 Phase B 前置条件。

## 8. 下一实施点

B1-B6 已形成完整 RX/TX production copy-count、post batching 和固定注册预算/退化路径。下一轮进入 B7，
在同一真实 provider 环境统一验证 B5/B6，并补齐 B1-B4 遗留的尾 window、连续 Piece、故障、budget
pressure、公平进展和 outstanding shutdown 债务。当前机器无法完成的 feature-on 编译/测试也必须先在
具备 `protoc`、Perl 和 UMDK build tree 的环境补跑；其中必须覆盖 Jetty/JFR 双 ERROR、send JFC
flush-done、recv JFC drain 以及多 lane 共享 JFC 的顺序删除。静态检查不得当作真机 correctness 或性能结论。
