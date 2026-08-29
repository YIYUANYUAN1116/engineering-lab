# Phase B：URMA production 性能数据路径

更新时间：2026-08-29。

## 0. 当前实施状态

截至 2026-08-29，B1、B2、B3、B4 已完成代码实现和纯测试/编译验证；Phase B 新数据路径尚未进行
真实 provider 跨节点验证，因此这里不把 B1-B4 标记为真机 PASS。

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

- server 不再创建 owned window，也不再逐 chunk `.to_vec()`；`MappedPiece` 或 `RangeReader` 直接填充
  exclusive `TxWindowLease` 的逐 slot span，生产路径已删除普通 Vec -> registered Segment copy API；
- 一个逻辑 window 固定为一个 message 对应一个 TX slot。即使协商 chunk 小于 slot size，也不会让多个
  outstanding SEND 共享同一个 slot/user_ctx；尾 window 可在不增长 lease 的前提下缩短 span 和 chunk 数；
- `CompletionRouter` 为整窗建立共享 completion state；每个 SEND CQE 只把自己的 slot 从
  `SendPosted` 恢复为 `LeasedTx`，全部 CQE 完成后才把 exclusive lease 返回 Session，错误/timeout 时也
  不会在 provider 仍引用 slot 时 recycle；
- server 使用两个独立 lease 实现 ring：`send(current)` 与 `fill(next)` 并行；第二个 lease 因 TX pool
  压力或有界等待超时时退化为 ring=1，单窗必须等全部 CQE 后才 refill；
- fixed TX pool 默认 128 slots，协商 SEND window 进一步限到半池容量，默认最多 64 chunks，为第二个
  lease 保留形成 ring 的空间；这只是 B4 的固定池限界，不替代 B6 的多 peer fairness/shared overflow；
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

上述 URMA 测试使用本地 UMDK build tree，未启动真实 provider。下一阶段进入 B5 post/CQ/credit
批处理；B1-B4 仍需按 B7/runbook 补真实跨节点验证。

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
| 预算 | `max_registered_bytes` 共享 byte budget，支持 wait/try-acquire | 固定 TX/RX slot 数；当前默认 TX 128、RX 512，共 40 MiB |
| window 布局 | 通常一个连续 `PooledBuf` | 多 slot 组成 multi-span logical window |
| 回收 | 最后一个 operation/reader owner 释放后直接返回本地 pool | consumer 通过 urgent owner command 校验 lease/generation 后回收 |
| 资源不足 | 等待预算或 non-blocking 失败 | `BufferUnavailable`；第二 window 安全退化为 pipeline depth 1 |

URMA 必须补齐、但不要求照搬 RDMA 类型的能力：

- process 级 registered-byte ceiling，active + idle/预留内存都计入；
- 已持有一个 window 时，第二 window 只能 non-blocking acquire，禁止并发死锁；
- 多 peer admission/fairness，以及 TX/RX 最小保留和共享 overflow 策略；
- slot/byte active、idle、等待、分配失败、双窗口降级和 owner recycle latency 指标；
- active lease、outstanding WR 与 Segment shutdown 的可审计生命周期。

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
fixed slot pool 更简单且热路径没有 MR registration miss，应继续保留。B4/B5 先在现有 slot 模型上完成；
B6 根据上述指标决定是否演进 allocator，不能为了与 RDMA 内部同形而提前重写。

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

1. 在 shim/FFI 增加最小的 linked WR/post-list API，批内每个 WR 保留独立 `user_ctx`。
2. CQ polling 使用 provider 支持的 batch，正确处理 partial post、partial CQ、单 WR error 和 flush。
3. 将 configured window、post-list、Jetty depth、JFC depth、slot count 和 remote posted credit 联合限界。
4. credit update 可批量发送，但只有已 repost 的 RX 数量才可计入；保留最后 control/Done 所需 credit。
5. owner thread 调度不能因持续 CQ busy polling 饿死 shutdown/abort/control command。

具体默认值不照搬 demo 的 window 64/post-list 16；先由 capability 限界，再在真实 provider 上校准。

### B6：并发预算、配置与退化路径

- process 全局 `max_registered_bytes`，并区分 TX/RX 保底或公平策略；
- per-transfer max window、pipeline depth、post-list、mmap 开关；
- `try_acquire` 避免持有一个 window 再阻塞等第二个造成并发死锁；
- 内存不足时从双 window 退化成单 window，不退回无界 owned buffer；
- endpoint/session failure 退休时，所有 WR、lease、credit 和 pending operation 可审计归零；
- metrics 能按 peer/Piece 识别 budget pressure、source-fill 和 Storage backpressure。

### B7：真实 provider 验证与性能验收

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

B1-B4 已形成完整 RX/TX production copy-count 路径：RX direct positional write/digest，TX direct-fill、
mmap/RangeReader fallback 和双 lease overlap。下一轮进入 B5 post/CQ/credit 批处理；同时保留
B1-B4 的真实 provider correctness、尾 window、连续 Piece、故障和 outstanding shutdown 验证债务，
不得把纯测试结果当作真机性能结论。
