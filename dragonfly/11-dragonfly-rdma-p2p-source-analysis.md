# Dragonfly RDMA/P2P Transport 源码分析与 URMA 映射

> 分析日期：2026-08-18  
> Dragonfly 根仓库：`/home/yuan/workspace/dev/Dragonfly2`  
> 根仓库基线：`main`，`2acbb8b6`，与 `origin/main` 一致  
> main 固定的 client gitlink：`017575a58d0abad6b3b274142fa470d47d8db327`  
> 工作目录 client：本地分支 `rdma-p2p-pr1945`，`1ccc7d10`  
> URMA lab：`tcp-urma-file-transfer`，`84db7a5`

## 0. 先给结论：main 中没有完整 RDMA 实现

- [源码确认] Dragonfly 根仓库 `main` 固定的 `client` 提交是 `017575a5`，而当前工作目录中的 `client` 被切到了本地 `rdma-p2p-pr1945` / `1ccc7d10`。根仓库 `git status` 因此显示 `M client`。
- [源码确认] 对 main 固定提交全量检索 `rdma / ibverbs / libibverbs / efa / queue pair / completion queue / ibv_* / rdma_*`，只找到：
  1. 两个 CI 镜像安装 `infiniband-diags`、`ibverbs-utils`；
  2. `piece_collector.rs` 中一条“若 protocol 是 rdma，用 IP 交换 IBVerbs QP endpoint”的注释。
- [源码确认] main 固定提交没有 `rdma` Cargo feature、RDMA client/server、verbs/libfabric 依赖、MR 注册、QP/CQ 创建、WR post 或 completion 处理代码。`DownloaderFactory` 只接受 `tcp` 和 `quic`。
- [源码确认] 根仓库中除 `client` 工作目录以外也没有 RDMA transport 实现。
- [未找到] main 中没有 RDMA 官方设计文档、配置项、测试或性能数据。
- [待验证] `piece_collector.rs` 的注释是历史接口预留、未合入方案残留还是未来方向，main 源码本身不能回答。

因此，本报告必须分成两层：

1. **main 已实现事实**：生产数据路径仍是 TCP/QUIC；不能回答 main 的 QP/CQ/MR 参数，因为对象不存在。
2. **本地候选分支分析**：工作目录确实包含一套完整的 libfabric 实现，可用于评估 URMA 映射，但不能称为“Dragonfly main 官方实现”。下文所有候选实现结论均标为 `[候选分支源码确认]`。

`client/docs/rdma-p2p.md` 也只存在于候选提交，不在 main。因此它属于 `[候选分支文档确认]`，不是 `[官方文档确认]`。

## 1. 搜索结果与仓库归属

### 1.1 main 的仓库结论

| 仓库/组件 | main 中 RDMA 状态 | 证据 |
| --- | --- | --- |
| Dragonfly 根仓库（Go manager/scheduler 等） | [源码确认] 未实现 | 排除 `client` 后定向检索无命中 |
| `client` 子模块 | [源码确认] 只有注释预留，无实现 | `piece_collector.rs` 注释；Cargo 无 RDMA feature |
| `dfdaemon` | [源码确认] 是 `dragonfly-client` crate 中的 binary，不是独立仓库；main 不启动 RDMA server | `dragonfly-client/Cargo.toml` 的 `[[bin]] name = "dfdaemon"` |
| 其他 Dragonfly 仓库 | [未找到] 当前源码树和 submodule 清单中未见承载 RDMA 的其他仓库 | 根仓库 submodule 仅 client、charts、console |

### 1.2 本地候选分支的仓库结论

| 仓库/组件 | 候选分支中的作用 |
| --- | --- |
| Dragonfly 根仓库 | [候选分支源码确认] 无 Go 侧 RDMA 修改；scheduler/manager 协议不变 |
| `client/dragonfly-client` | [候选分支源码确认] Piece 选择、RDMA downloader、TCP fallback、dfdaemon 启动 server |
| `client/dragonfly-client-storage` | [候选分支源码确认] libfabric shim、Fabric/MR/CQ、rendezvous、client/server、直接落盘 |
| `client/dragonfly-client-config` | [候选分支源码确认] RDMA provider、port、window、MR budget、timeout 等配置 |
| libfabric / EFA / ibverbs | [候选分支源码确认] 外部运行/链接依赖；Dragonfly 不直接调用 ibverbs。C shim 调 libfabric，构建脚本可能链接 `libefa.so.1`、`libibverbs.so.1` |

## 2. main 当前 TCP/QUIC 数据路径

### 2.1 总体路径

```text
Scheduler 返回 Parent 元数据
        |
        v
PieceCollector: CollectedParent
  - parent id / host
  - download_ip
  - download_tcp_port / download_quic_port
        |
        v
Task 并发调度（concurrent_piece_count，main 默认 8）
        |
        v
Piece::download_from_parent(task_id, piece number, parent)
        |
        +-- TCPDownloader --> TCPClient --> 每 piece 一个 TCP connection
        |
        `-- QUICDownloader --> QUICClient --> QUIC connection / bidirectional stream
                          |
                          v
               Vortex DownloadPiece(task_id, piece_number)
                          |
                          v
Parent TCPServerHandler / QUICServerHandler
  -> piece_id(task_id, number)
  -> Storage::get_piece
  -> Storage::upload_piece
  -> Content::read_piece
  -> RangeReader
                          |
                          v
         PieceContent metadata + piece byte stream
                          |
                          v
Child Storage::download_piece_from_parent_finished
  -> Content::write_piece_from_stream
  -> CRC32 + pwritev/file write
  -> digest 校验
  -> metadata.download_piece_finished
```

简化成用户要求的视图：

```text
Child Piece Downloader
        |
        | Vortex request + byte stream
        v
TCP connection / QUIC bidirectional stream
        |
        v
Parent Piece Server
        |
        v
Storage metadata -> RangeReader -> task content file/page cache
```

### 2.2 一个 Child 请求 Parent Piece

#### Child 如何发起

- [源码确认] `Task` 从 `CollectedPiece.parents` 选择一个 `CollectedParent`，把 `task_id`、piece `number`、预期 `length` 和 parent 交给 `Piece::download_from_parent`。
- [源码确认] parent 地址来自 `download_ip + download_tcp_port` 或 `download_ip + download_quic_port`；缺失时退到 `parent.host.ip + host.port` 的 TCP 路径。
- [源码确认] TCP/QUIC wire request 是 Vortex `DownloadPiece::new(task_id, number)`。请求内没有本地 buffer 地址、MR key 或 remote key。
- [源码确认] `host_id` 出现在 Downloader trait 参数中，但 TCP/QUIC 实现没有把它编码进这个 piece request。

#### Parent 如何响应

- [源码确认] server 解码 `task_id` 和 `piece_number`，通过 `Storage::piece_id(task_id, number)` 生成 piece id。
- [源码确认] `handle_piece` 读取 piece metadata，执行 upload bandwidth limiter，再用 `Storage::upload_piece` 取得 `RangeReader`。
- [源码确认] Parent 先返回 `PieceContent` 元数据（number、offset、length、digest、parent_id 等），随后在同一 TCP 流或 QUIC 双向流发送 piece bytes。
- [源码确认] Child 将返回的 byte stream 交给 Storage，边收边 CRC32、写 task content，最后比较 Parent digest 并更新 piece metadata。

### 2.3 Piece 如何切分

- [源码确认] main 的 Piece 长度范围是 4 MiB 到 64 MiB；按文件长度优化时，目标不超过约 500 pieces，取 2 的幂并夹在上述范围内；也支持固定 piece length。
- [源码确认] `number` 决定逻辑 piece，通常 `offset = number * piece_length`，末 piece 缩短。
- [源码确认] TCP/QUIC 传输内部的 `Bytes`/socket chunk 不是新的 Dragonfly Piece；Piece 协议仍以一个完整 piece 为请求与校验单位。

### 2.4 main buffer 与 zero-copy

- [源码确认] Parent `RangeReader` 使用 content buffer pool 的可复用 `BytesMut` 做 positional read；Linux TCP server 的路径还可使用 `sendfile`。
- [源码确认] Child TCP 由 `ReaderStream` 产出 `Bytes`，QUIC 直接交出 quinn reassembly chunk；`write_range_from_stream` 对 chunks 做 CRC32 并批量 `pwritev`。
- [源码确认] main 没有注册内存、MR、rkey、RDMA zero-copy。
- [源码确认] “stream 不再额外 copy”只说明 Rust 用户态 chunk ownership 的优化，不能等同于 NIC→storage 的 RDMA zero-copy。

## 3. 候选分支 RDMA 后的数据路径

### 3.1 总体路径

```text
Child Piece::download_from_parent
        |
        | 先连 Parent 已知 TCP piece port：Discover / Capability
        v
RDMADownloader（共享 downloader Fabric）
        |
        | 新 TCP rendezvous connection：Request / Ready / RecvPosted / Done/Error
        v
Child 预 post tagged RECV 到 registered receive windows
        |
        | FI_EP_RDM + FI_TAGGED + FI_SEND/FI_RECV
        v
Parent RDMAServer（共享 server Fabric）
        |
        | RangeReader 或 mmap -> copy/fill registered send ring
        | post tagged SEND，等待 send completion
        v
Child receive completion
        |
        | completed registered window（无中间用户态 bounce）
        v
CRC32 与 pwrite 并行读取同一 registered window
        |
        v
task content file / page cache -> piece metadata finished
```

失败路径：

```text
任意 discovery / rendezvous / fabric / storage write 错误
        |
        v
该 piece 重置 download metadata
        |
        v
Parent 的普通 TCP piece server 重新下载完整 piece
```

### 3.2 RDMA 替换了哪一层

结论：**主要是 B. Downloader transport 层，同时新增 Parent upload transport；不是 HTTP、Piece 语义或 Storage 的整体替换。**

| 选项 | 结论 | 源码依据 |
| --- | --- | --- |
| A. HTTP 层 | [候选分支源码确认] 否 | peer piece request 使用 Vortex/TCP/QUIC 或 RDMA rendezvous，不是 HTTP；源站 backend HTTP 不变 |
| B. Downloader transport 层 | [候选分支源码确认] 是，主要替换点 | `Piece::download_from_parent` 在同一个 piece lifecycle 中优先调用 `RDMADownloader`，失败回到 `TCPDownloader` |
| C. Piece 协议层 | [候选分支源码确认] 不替换 Piece 语义；wire framing 有变化 | task_id、piece number、offset、length、digest 和整 piece 校验保持；Vortex wire request 被 TCP rendezvous frames + tagged chunks 取代 |
| D. Storage 层 | [候选分支源码确认] 否 | Storage metadata、piece id、content file、CRC32 和 finished 状态仍复用；仅新增 RDMA window 直写入口和可选 mmap upload |
| E. 其他 | [候选分支源码确认] Parent Piece Server 数据面也被旁路 | 新 `RDMAServer` 承担 bulk upload；普通 TCP server 保留 discovery 和 fallback |

一个重要细节：普通 Piece 的 RDMA 路径为了保留 registered window，绕开通用 `PieceContentStream` finish path，直接调用 `download_piece_from_parent_finished_rdma`。Persistent/PersistentCache 仍可通过 `AsyncRead` adapter 进入旧接口，因此“无 bounce”结论不能无条件扩展到所有 piece namespace。

## 4. 候选分支单 Piece 协议细节

### 4.1 Discovery 与 request

1. [候选分支源码确认] Child 只知道 scheduler/collector 给出的 Parent TCP piece 地址。
2. [候选分支源码确认] Child 在普通 TCP piece port 发送 `Frame::Discover`；Parent 的 TCP server peek discriminator，并返回当前 `RdmaAdvertisement { provider, fabric_tag, rdma_port }`。成功结果缓存 60 秒。
3. [候选分支源码确认] Child 再连 Parent 的 RDMA rendezvous TCP port（默认 4007），发送 `Frame::Request(PieceRequest)`：
   - `kind`；
   - `task_id`；
   - `piece_number`；
   - local provider + fabric tag；
   - Child provider-opaque endpoint blob；
   - transfer base tag；
   - Child chunk size；
   - Child max inflight chunks。
4. [候选分支源码确认] request 没有 rkey、remote virtual address 或 MR descriptor。

### 4.2 Parent response

1. [候选分支源码确认] Parent 校验 provider 与非空 fabric tag 完全一致。
2. [候选分支源码确认] Parent 用 `task_id + piece_number` 查询原 Storage metadata，协商 `min(client, server, provider max)` 的 chunk size 与 inflight count。
3. [候选分支源码确认] Parent resolve Child endpoint，取得 libfabric address-vector destination。
4. [候选分支源码确认] Parent 打开原有 `RangeReader`；若启用 `mmapContent` 且 piece 在磁盘，则可 mmap piece。两者最终都要填充 registered send window。
5. [候选分支源码确认] Parent 返回 `Ready(offset, length, digest, server_endpoint, chunk_size, max_inflight_chunks)`。

### 4.3 数据阶段

```text
Child                                              Parent
  | Request(task_id, piece_no, endpoint, tag...) --> |
  | <-- Ready(offset, length, digest, negotiated...) |
  |                                                   |
  | post RECV(tag+i) into registered window           |
  | RecvPosted(start_chunk, count) -----------------> |
  |                     registered send ring <- file/mmap copy
  | <-- tagged SEND chunks -------------------------- |
  | poll CQ; wait all RECV completions in window      | poll CQ; wait SEND completions
  | publish completed registered window to Storage    |
  | prepost next window (pipeline depth up to 2)      |
  |                                                   |
  | <-- Done ----------------------------------------- |
  | CRC32 + pwrite directly from each completed window|
  | compare digest; mark piece finished               |
```

### 4.4 操作类型确认

- [候选分支源码确认] **使用 SEND/RECV（更准确地说 libfabric tagged send/receive：`fi_tsend` / `fi_trecv`）。**
- [候选分支源码确认] **不使用 RDMA READ。**
- [候选分支源码确认] **不使用 RDMA WRITE。**
- [候选分支源码确认] **不是 READ/WRITE 混合模式。**
- [候选分支源码确认] 控制面 TCP + 数据面 tagged SEND/RECV 是“双平面混合”，但不能把它叫做 RDMA verb READ/WRITE 混合。

Checkbox：

- [x] SEND/RECV
- [ ] RDMA READ
- [ ] RDMA WRITE
- [ ] one-sided 混合模式

### 4.5 RECV 如何提前 post、completion 如何匹配

- [候选分支源码确认] Child 按 window 为每个 chunk 调 `Fabric::post_recv(buffer, local_offset, len, base_tag + chunk_index)`。
- [候选分支源码确认] post 完一个 window 后才通过 TCP 发 `RecvPosted(start_chunk, count)`；Parent 不收到该 frame 不允许 SEND。
- [候选分支源码确认] 每个 transfer 从全局 tag counter 分配一个大小 4096 的互斥 tag range；每个 chunk 用 `base_tag + chunk_index`，从而隔离并发 piece。
- [候选分支源码确认] 每个 post 有独立 context block。`pending<context_address, PendingOp>` 持有 oneshot completion sender 和 buffer `Arc`；CQ progress thread 用 completion context 查表并唤醒等待任务。
- [候选分支源码确认] receiver 最多保持两个 posted windows；一个 window 内最多 `max_inflight_chunks` 个 RECV。MR budget 不足时降为单 window，而不是持有已有 buffer 阻塞等待第二个。

## 5. QP、CQ、线程与并发模型

### 5.1 “一个 Peer 有几个 QP”不能按 ibverbs RC 回答

- [候选分支源码确认] 实现请求的是一个共享 `FI_EP_RDM` endpoint，不直接创建/管理 `ibv_qp`。
- [候选分支源码确认] EFA 与 verbs 都通过 libfabric；provider 如何映射到底层 QP 属于 provider 内部。
- [未找到] 源码没有可证明的“每 peer QP 数量”。因此不能写成“一 peer 一 QP”。
- [候选分支源码确认] 两个 peers 之间也没有专属连接型 endpoint；并发 transfer 共享 endpoint，以 destination address + tags 区分。

若一个 dfdaemon 同时 serve 和 download RDMA：

| 资源 | 数量/作用域 |
| --- | --- |
| libfabric endpoint | [候选分支源码确认] downloader role 一个共享 Fabric；server role 一个共享 Fabric，二者分开，因此进程内最多至少可见两个 role endpoint |
| CQ | [候选分支源码确认] 每个 Fabric 一个 CQ，同时 bind transmit + receive |
| progress worker | [候选分支源码确认] 每个 Fabric 一个名为 `rdma-progress` 的 OS thread |
| transfer task | [候选分支源码确认] 每个 RDMA piece 一个 Tokio rendezvous/receive 或 server handler task |
| TCP rendezvous connection | [候选分支源码确认] 每 piece 一个；discovery 另有短连接但结果缓存 60 秒 |
| per-peer QP | [未找到] libfabric provider 内部，不可由本源码确认 |

### 5.2 Piece 并发映射

- [源码确认] Dragonfly task 层用 semaphore + `JoinSet` 并发下载 pieces，main 默认 `concurrent_piece_count=8`，配置可改。
- [候选分支源码确认] server 另以 `maxConcurrentTransfers` 控制并发 rendezvous，默认 64。
- [候选分支源码确认] 一个 piece 对应一个 RDMA request，但通常对应多个 SEND WR 和多个 RECV WR；`chunk_count = ceil(piece.length/chunk_size)`。
- [候选分支源码确认] 一个 window 是 `chunk_size * max_inflight_chunks` 上限；默认 4 MiB × 16。Dragonfly 默认最大 piece 64 MiB 时通常正好最多 16 chunks/一个 window，但固定 piece 或 provider限制仍可能产生多 window。
- [候选分支源码确认] Parent send ring 是一或两个 windows；两个 ring half 时可在一半 SEND 的同时填另一半。
- [候选分支源码确认] Child receive window pipeline depth 固定为 2；channel capacity 也是 2。
- [候选分支源码确认] CQ batch size 固定 32。
- [候选分支源码确认] CQ polling：无 pending 时 sleep 100 µs；有 pending 时先 yield，连续 64 次后 sleep 10 µs；不是 event-driven CQ。
- [候选分支源码确认] queue full (`FI_EAGAIN`) 时每 200 µs 重试，单次 post 最长 5 秒。
- [候选分支源码确认] 没有“一个 piece 一个 WR”；也没有跨 piece 的显式 WR batch post API。多个 async transfer 并发向共享 endpoint post，completion 在全局 CQ 批量 reap。

## 6. Buffer / MR / completion 生命周期

### 6.1 发送端

```text
task content file / page cache
        |
        +-- 默认：RangeReader reusable buffer
        |             |
        |             `-- read_exact -> registered send window（copy）
        |
        `-- mmapContent：MappedPiece
                      |
                      `-- copy_from_slice -> registered send window（仍有 copy）
                                      |
                                      v
                         fi_mr_reg / local descriptor
                                      |
                                      v
                        tagged SEND WRs (one per chunk)
                                      |
                                      v
                           shared CQ completion
                                      |
                                      v
                ring half 可 refill / buffer 可回池复用
```

- [候选分支源码确认] `mmapContent` 去掉的是 `AsyncRead` 中间 reader buffer，不是 mmap pages 直接作为 SEND MR；`PieceSource::fill` 仍 `copy_from_slice` 到 registered ring。
- [候选分支源码确认] 单 ring 时必须等本 window 所有 send completions 后再 refill；双 ring 时只写与正在 SEND 的 half 不相交、且该 half 上次 SEND 已完成的区域。

### 6.2 接收端

```text
registered receive window
        |
        | prepost tagged RECV WRs
        v
NIC/libfabric receive
        |
        v
shared CQ：该 window 所有 chunk completions 均已 reap
        |
        v
ReceivedWindow（仍持有 registered lease）
        |
        +-- CRC32 直接读 window
        `-- pwrite 直接读 window
               （两者并行）
        |
        v
drop lease -> best-fit MR pool -> 后续 transfer 复用
```

### 6.3 四个明确回答

1. **是否使用 registered memory？**  
   [候选分支源码确认] 是。每个新 stable `Vec<u8>` 尝试 `fi_mr_reg(..., FI_SEND | FI_RECV)`，保存 local descriptor；MR pool 受 `maxRegisteredBytes` 限制。若 provider 宣告 local MR required，注册失败即失败；不要求 MR 的 provider 可降级为空 descriptor。因此更精确的表述是“transport 有统一的 MR/稳定 buffer 模型，硬件 provider 路径要求注册成功”。

2. **是否 zero-copy？**  
   [候选分支源码确认] **不是端到端 zero-copy。** Parent 至少把文件/mmap 内容 copy 到 registered send ring；Child NIC 直接落 registered window，CRC32 与 `pwrite` 不再经过第二个用户态 staging buffer，这是“接收侧无中间 bounce”。`pwrite` 到 page cache 也不能表述为 NIC 直接写磁盘。

3. **buffer 什么时候释放？**  
   [候选分支源码确认] post 时 `PendingOp` 持有 buffer `Arc`；CQ progress reap completion 后才移除。发送 buffer在所有相应 SEND completion 后可 refill；接收 window 在所有 RECV completion 后发布，Storage 完成 digest+pwrite并 drop `ReceivedWindow` 后回 pool。Fabric shutdown 释放 idle MR；无法证明 DMA 停止的异常路径宁可 quarantine/leak 也不交还 allocator。

4. **completion 前允许复用吗？**  
   [候选分支源码确认] 不允许。源码通过 pending `Arc`、slot/ring half 时序和 completion wait 维持此不变量。

## 7. 涉及文件、作用和调用关系

### 7.1 main 已确认文件

文件：Piece downloader 抽象  
路径：`client/dragonfly-client/src/resource/piece_downloader.rs`（main gitlink `017575a5`）  
作用：[源码确认] transport factory 与 TCP/QUIC downloader；证明 main 只支持 `tcp`、`quic`。  
关键函数：`DownloaderFactory::new`、`TCPDownloader::download_piece`、`QUICDownloader::download_piece`。  
调用关系：`Piece::download_from_parent -> Downloader -> TCPClient/QUICClient`。

文件：Piece lifecycle  
路径：`client/dragonfly-client/src/resource/piece.rs`  
作用：[源码确认] piece 切分、parent transport 选择、Storage finish。  
关键函数：`calculate_piece_length`、`download_from_parent`。  
调用关系：`Task -> Piece::download_from_parent -> Downloader -> Storage::download_piece_from_parent_finished`。

文件：Parent 地址收集  
路径：`client/dragonfly-client/src/resource/piece_collector.rs`  
作用：[源码确认] 从 scheduler stream 收集 piece 与 parent 地址；含唯一 RDMA 预留注释，但无行为。  
关键类型/函数：`CollectedParent`、`PieceCollector::run`。  
调用关系：scheduler response -> `CollectedPiece.parents` -> Task parent selector。

文件：TCP/QUIC client  
路径：`client/dragonfly-client-storage/src/client/tcp.rs`、`client/dragonfly-client-storage/src/client/quic.rs`  
作用：[源码确认] 编码 Vortex request、连接 Parent、解析 PieceContent metadata、返回 byte stream。  
关键函数：`handle_download_piece`、`connect_and_write_request`、`content_stream`。

文件：TCP/QUIC server  
路径：`client/dragonfly-client-storage/src/server/tcp.rs`、`client/dragonfly-client-storage/src/server/quic.rs`  
作用：[源码确认] 解码 request、查 Storage、发送 metadata 与 content stream。  
关键函数：`handle`/`handle_stream`、`handle_piece`、`write_stream`。

文件：Storage facade 与 content I/O  
路径：`client/dragonfly-client-storage/src/lib.rs`、`content_linux.rs`、`io.rs`  
作用：[源码确认] piece metadata lifecycle、RangeReader、stream write、CRC32、pwritev。  
关键函数：`upload_piece`、`download_piece_from_parent_finished`、`Content::read_piece`、`write_piece_from_stream`、`write_range_from_stream`。

### 7.2 只存在于本地候选分支的文件

文件：libfabric C shim  
路径：`client/dragonfly-client-storage/src/rdma/shim.c`  
作用：[候选分支源码确认] 打开 `FI_EP_RDM` endpoint、AV、单 CQ、MR，封装 `fi_tsend/fi_trecv/fi_cq_read/fi_cancel`。  
关键函数：`dfrdma_open`、`dfrdma_mr_reg`、`dfrdma_tsend`、`dfrdma_trecv`、`dfrdma_cq_read_batch`。  
调用关系：Rust `Fabric` FFI -> shim -> libfabric provider -> EFA/verbs 等。

文件：Fabric 安全封装  
路径：`client/dragonfly-client-storage/src/rdma/fabric.rs`  
作用：[候选分支源码确认] endpoint/CQ progress、MR pool、tag 分配、post/wait、completion-context 匹配、取消与 quarantine。  
关键函数：`Fabric::new`、`acquire_buffer`、`post_recv`、`post_send`、`wait`、`progress_loop`。  
调用关系：RDMA client/server -> `Fabric` -> shim；progress thread -> pending map -> async waiter。

文件：TCP rendezvous wire protocol  
路径：`client/dragonfly-client-storage/src/rdma/rendezvous.rs`  
作用：[候选分支源码确认] capability、PieceRequest/Ready、RecvPosted、Done/Error framing。  
关键类型/函数：`PieceRequest`、`PieceReady`、`Frame`、`write_frame`、`read_frame`。

文件：RDMA Child client  
路径：`client/dragonfly-client-storage/src/client/rdma.rs`  
作用：[候选分支源码确认] discovery、request、prepost receive windows、completion wait、registered window stream。  
关键函数：`discover`、`RDMAClient::handle_download`、`receive_stream`、`RDMAStreamReader::next_window`。  
调用关系：`RDMADownloader -> RDMAClient -> receive_stream -> Fabric::post_recv/wait -> RDMAStreamReader`。

文件：RDMA Parent server  
路径：`client/dragonfly-client-storage/src/server/rdma.rs`  
作用：[候选分支源码确认] rendezvous listener、admission、piece lookup、registered send ring、tagged SEND。  
关键函数：`RDMAServer::run`、`RDMAServerHandler::handle_piece`、`open_piece_source`、`PieceSource::fill`。  
调用关系：dfdaemon -> `RDMAServer` -> Storage/Content -> `Fabric::post_send/wait`。

文件：Dragonfly downloader adapter  
路径：`client/dragonfly-client/src/resource/piece_downloader.rs`  
作用：[候选分支源码确认] lazy shared Fabric、capability cache、parent penalty、RDMAClient adapter。  
关键函数：`RDMADownloader::fabric`、`advertisement`、`client`、`download_piece_stream`。  
调用关系：Piece -> RDMADownloader -> discovery/client；错误回到 Piece 的 TCP fallback。

文件：Piece/Storage 直写接入  
路径：`client/dragonfly-client/src/resource/piece.rs`、`client/dragonfly-client-storage/src/lib.rs`、`content_linux.rs`  
作用：[候选分支源码确认] regular piece RDMA fast path、registered windows 直接 CRC32+pwrite、失败重启 piece metadata。  
关键函数：`download_piece_from_parent_over_rdma`、`download_piece_from_parent_finished_rdma`、`write_piece_from_rdma_stream`。

文件：dfdaemon 启动与 discovery 接入  
路径：`client/dragonfly-client/src/bin/dfdaemon/main.rs`、`client/dragonfly-client-storage/src/server/tcp.rs`  
作用：[候选分支源码确认] 启动可选 RDMAServer；在现有 TCP piece port 发布 live capability。  
关键函数：`main` server initialization、`with_rdma_capabilities`、`is_rdma_discovery`、`handle_rdma_discovery`。

文件：配置和构建  
路径：`client/dragonfly-client-config/src/dfdaemon.rs`、`dragonfly-client-storage/build.rs`、两个 crate 的 `Cargo.toml`  
作用：[候选分支源码确认] feature gate、provider/MR/window/timeout 配置、libfabric C/link detection。  
关键类型/函数：`RdmaServer`、`RdmaProvider`、`validate_rdma_server`、build script main。

## 8. Dragonfly 候选 RDMA 与 URMA lab 映射

先强调边界：下表左侧是**非 main 候选实现**，右侧是当前 URMA lab；不是“生产 Dragonfly 已对接 URMA”。

| Dragonfly 候选 RDMA | URMA lab | 映射评价 |
| --- | --- | --- |
| QP/endpoint：共享 `FI_EP_RDM`，无 per-peer QP | `UrmaJetty`：每端一个 RC duplex Jetty，OOB exchange/import/bind | [源码确认] 都是数据面 endpoint 抽象；连接模型不同，不能把一个 Jetty 简单说成候选方案的一条 QP |
| CQ：每 Fabric 一个 transmit+recv CQ | 两个 polling JFC（send/recv） | [源码确认] completion 语义相近，资源拓扑不同 |
| MR：best-fit pooled `PinnedBuf`，动态按 window 注册且有全局 byte budget | 一个 local-only registered Segment，静态切 TX/RX slots | [源码确认] 都提供注册内存与稳定地址；分配/复用模型不同 |
| WR：每 chunk 一个 `fi_tsend/fi_trecv` + operation context | `post_send/post_recv` 返回 `WrHandle`，`user_ctx` 编码 connection/generation/op/slot | [源码确认] 可一一映射到 SEND/RECV work request；tag 与 user_ctx 的职责不同 |
| completion：全局 progress OS thread batch=32，context->oneshot | 调用线程同步 poll JFC，batch=16，user_ctx->outstanding/slot | [源码确认] 都在 CQE 后释放 slot/buffer；调度模型不同 |
| buffer：window/ring，logical length，完成后 lease 回池 | fixed slot；RX CQE 时复制成 owned `Vec` 后立即 release/repost | [源码确认] URMA 当前是 copy mode；候选 regular piece RX window 可保持 lease 到 storage write 完成 |
| connection：每 role 一个共享 RDM endpoint，多 peer/transfer 复用 tags；每 piece TCP rendezvous | 单连接、单 RC Jetty pair、单请求；TCP OOB handshake | [源码确认] URMA 尚无共享 multi-peer transport/session manager |
| piece request：TCP `PieceRequest(task_id,piece_number,endpoint,tag,window)`；数据另走 fabric | URMA `IntegrationMessageV3::Request(task_id,piece_number)` 本身通过 SEND；OOB 只交换 Jetty/control barrier | [源码确认] 业务字段近似；控制面位置和 flow control 不同 |
| flow control：TCP `RecvPosted(window)` | OOB READY + 预投 RX credit；steady state replenish credit | [源码确认] 都遵守 receive-before-send；URMA 尚无 per-window RecvPosted frame |
| 并发 identity：transfer tag range + op context | 单 request_id + message sequence + pointer-free user_ctx | [源码确认] 当前可匹配单请求；尚不能隔离多并发 piece/session |

### 8.1 当前 URMA 已达到的部分

- [源码确认] RC duplex Jetty、shared JFR、SEND/RECV、registered Segment、双 polling JFC、CQ polling、`user_ctx` completion routing。
- [源码确认] bounded SEND pipeline 已实现；window 受 TX/RX slots、JFC depth、Jetty depth、provider max message size约束。
- [源码确认] Child 会在 READY 前预投 `min(2*window, rx slots, remaining messages)` 个 RECV，完成后先补 credit 再做 sink I/O。
- [源码确认] TX slot 在 SEND CQE 前保持 `SendPosted`，不能复用。
- [源码确认] RX CQE 后从 registered Segment copy 到 owned `Vec`，随后 slot release/repost；这是明确的 copy mode。
- [源码确认] lab 已从最早“单 Data stop-and-wait”进展到多 Data bounded pipeline，但仍是单 connection、单 outstanding request。

### 8.2 距离候选 Dragonfly-like transport 的差距

| 差距 | 当前状态 | 与候选实现的关系 |
| --- | --- | --- |
| Dragonfly crates/API 接入 | [源码确认] 无 | 尚未接 `Piece::download_from_parent`、Storage metadata、Parent server |
| 多 peer / connection management | [源码确认] 单 Jetty pair | 候选是 role-shared endpoint + peer address cache；URMA 至少需能管理多个 peer session，但具体拓扑待 URMA/provider验证 |
| 多 piece 并发 | [源码确认] 单 request | 缺 request lifecycle table、并发 identity、admission、per-piece error/fallback |
| Dragonfly parent discovery | [源码确认] 仅 lab TCP OOB | 缺利用 `CollectedParent` 地址的 capability discovery/compatibility/fallback |
| 完整 piece metadata | [源码确认] demo Request/Metadata/Data/End 有 task_id、piece_number、offset/length/digest | 与候选语义接近，但还没有 Dragonfly真实 piece id、parent id、namespace、Storage state transition |
| RX lease / no-bounce storage path | [源码确认] RX CQE 立即 copy 到 `Vec` | 与候选 regular Piece 的 registered window直写有一层明显差距 |
| MR pool/budget | [源码确认] 单静态 Segment + fixed slots | 缺动态/共享 byte budget、best-fit复用、并发 admission；是否照搬候选方案不应预设 |
| completion worker | [源码确认] 业务调用线程同步 poll | 缺可被多并发 request共享的 progress/routing执行模型 |
| failure retirement/cancel | [源码确认] 有 drain/shutdown和 outstanding保护 | 缺候选的 per-op timeout/cancel、endpoint retirement、无法确认DMA停止时 quarantine |
| per-piece TCP fallback | [源码确认] 无 Dragonfly fallback | 候选的可用性核心；接入时必须能在 RDMA失败后安全重启完整 piece |
| server admission/backoff/cache | [源码确认] 无生产层机制 | 缺 max concurrent transfer、capability TTL、parent backoff、busy区别处理 |
| mmap/read path | [源码确认] benchmark file read到临时 buffer再编码/拷入TX slot | 尚未复用 Dragonfly RangeReader 或 mmap；仍非 zero-copy |
| 真实并发/稳定性验证 | [实验确认] M3 real-provider 已验证；B2 W=1/W=2 64 MiB曾通过，当前文档把 B2正式矩阵标为 awaiting environment validation | 不能据此宣称生产并发、长稳或 Dragonfly workload 已验证 |

## Dragonfly RDMA实现总结

### 1. 修改范围

- [源码确认] **main：没有 RDMA 实现可总结为已合入修改。** 只有 client 的一条 QP endpoint 注释预留。
- [候选分支源码确认] 候选实现修改集中在 Rust client：Piece、dfdaemon、storage client/server/content、config/build；manager、scheduler和Go根仓库协议不改。

### 2. 数据路径

- [源码确认] main 是 Vortex request + TCP/QUIC byte stream +原 Storage。
- [候选分支源码确认] 候选是 TCP discovery/rendezvous 控制面 + libfabric tagged SEND/RECV bulk data面 +原 Storage；失败逐 piece 回 TCP。

### 3. RDMA模型

- [候选分支源码确认] libfabric `FI_EP_RDM`、two-sided tagged SEND/RECV；无 READ/WRITE、无 rkey/address exchange、无 direct ibverbs QP管理。
- [待验证] 底层 EFA/verbs provider实际创建多少硬件队列/QP，应用源码不可见。

### 4. 并发模型

- [候选分支源码确认] role级共享 endpoint/CQ/progress thread；piece级 TCP rendezvous task；piece内多个 chunk WR；tag range隔离；receive双window、send一/双ring；CQ batch=32。
- [源码确认] Dragonfly上层已并发调多个 pieces；候选server另有transfer admission。

### 5. Buffer模型

- [候选分支源码确认] bounded registered-buffer pool；post期间由 pending `Arc`保活；completion前绝不复用。Parent文件/mmap copy进send ring；Child NIC落registered window并从原window CRC32+pwrite。
- [候选分支源码确认] 这是“接收侧去 bounce”，不是端到端zero-copy。

### 6. 性能收益来源

- [候选分支文档确认] 候选文档给出的目标是避免单 TCP flow未充分利用EFA/RDMA fabric，并以多chunk inflight、共享endpoint、registered window和收写重叠提高吞吐。
- [候选分支源码确认] 可直接从代码确认的机制包括：多WR window、双window receive、双ring send、CQ batch、MR复用、CRC32与pwrite并行、可选mmap减少一个reader中间层。
- [待验证] 候选文档中的EFA Gbps数字不属于main官方结果，本次未在相同硬件复测，不能作为当前Dragonfly main或URMA性能结论。

## 对URMA接入建议

以下是按当前已有阶段做的**接入优先级归纳**，不是设计一套新协议；每一级只指出要补齐的既有Dragonfly/候选语义。

### Level 0：当前 demo

- [源码确认] 保持 standalone：单 RC duplex Jetty、shared JFR、SEND/RECV、registered Segment、CQ polling、bounded SEND pipeline、copy mode、单连接、单请求。
- [源码确认] 当前最有价值的成果是 correctness 基线：prepost RECV、TX completion前禁复用、user_ctx路由、outstanding drain、CRC32/length验证。
- [优先] 先完成当前B2/B4真实provider矩阵与长传输稳定验证；未验证项不能用架构设计替代。

### Level 1：Dragonfly-like transport

优先级从高到低：

1. [建议，依据源码差距] 把现有 Request/Metadata/Data/End 与真实 Dragonfly `task_id + piece_number + parent address + offset/length/digest` 对齐，并保留整piece CRC/length校验。
2. [建议，依据源码差距] 建立“一个 piece 请求 -> 多 Data SEND/RECV WR -> End/completion”的 transport接口和明确生命周期；继续使用已验证 SEND/RECV，不优先引入 READ/WRITE。
3. [建议，依据源码差距] 接入真实 Parent storage reader与Child storage writer，但第一阶段可继续 copy mode；先证明 fallback前不会有未完成写和completion前不会复用。
4. [建议，依据源码差距] 加入逐piece失败返回与TCP fallback语义；这是候选方案保持Dragonfly可用性的关键，不是性能优化。
5. [建议，依据源码差距] 再扩展同一peer的多piece outstanding、request_id/sequence隔离和bounded admission；随后才扩展multi-peer。
6. [建议，依据源码差距] 将同步poll/outstanding routing提升为可服务多request的共享completion路径，并保留现有user_ctx与slot状态不变量。

### Level 2：生产级接入

- [建议，依据候选源码] capability discovery、版本/可达域兼容、父节点失败backoff、busy与broken区分、逐piece TCP fallback。
- [建议，依据候选源码] MR总量预算、并发admission、buffer pool复用和运行时诊断；避免按每piece无界注册。
- [建议，依据候选源码] timeout/cancel、CQ fatal错误、endpoint retirement、shutdown/drain和DMA安全策略需要在真实UMDK/URMA语义上逐项验证，不能直接照搬libfabric结论。
- [建议，依据源码差距] 在copy mode生产语义稳定后，再评估RX registered-buffer lease直写。READ/WRITE、remote Segment、UBS Memory仍不应因“RDMA”名称而提前引入。
- [待验证] URMA最终是一peer一Jetty、连接池还是共享endpoint式拓扑，必须以UMDK能力、并发实验和Dragonfly故障域要求决定；候选libfabric模型不能自动推出答案。

### 最优先事项

1. [建议] 完成当前SEND/RECV pipeline真实provider稳定验证。
2. [建议] 做真实Dragonfly piece语义和Storage copy-mode闭环。
3. [建议] 做逐piece error + TCP fallback与安全重启。
4. [建议] 再做多piece并发与共享completion routing。
5. [建议] 最后才是registered RX lease/no-bounce、MR池优化和更激进的数据路径。

## 9. 已确认 / 未找到 / 需要进一步验证清单

### 已确认

- [源码确认] main没有完整RDMA实现，当前生产transport是TCP/QUIC。
- [源码确认] main的替换边界只能从Downloader abstraction推导未来接点，不能声称已有RDMA QP/CQ/MR。
- [候选分支源码确认] 本地候选完整实现是libfabric two-sided tagged SEND/RECV，控制面TCP，数据面fabric。
- [候选分支源码确认] 候选替换Downloader/Parent upload transport，不替换Piece语义和Storage体系。
- [候选分支源码确认] 候选使用registered buffers，但不是端到端zero-copy。
- [源码确认] URMA lab在transport primitives与SEND/RECV pipeline上有直接映射，但仍是single connection/single request/copy mode。

### 未找到

- [未找到] main中的RDMA client/server、Cargo feature、配置、测试、官方文档、性能结果。
- [未找到] main或候选源码可见的每peer QP数量。
- [未找到] 候选方案中的RDMA READ/WRITE、rkey交换、remote address暴露。
- [未找到] URMA lab中的Dragonfly crate集成、multi-peer、multi-piece concurrent request、TCP fallback、registered RX lease。

### 需要进一步验证

- [待验证] 本地候选提交的上游PR/合入状态；本报告只确认它不在当前根仓库main固定gitlink。
- [待验证] 候选分支文档的EFA性能数据，需要相同版本、硬件、配置复测。
- [待验证] libfabric provider内部的QP/queue实现细节。
- [待验证] URMA在多Jetty/多peer、长稳pipeline、cancel/teardown、CQ共享和MR budget压力下的真实provider行为。
- [待验证] URMA registered RX lease直写Storage是否能在UMDK所有权和Dragonfly取消/fallback语义下安全成立。

## 10. 复核命令（只读）

```bash
# 根仓库main与gitlink
git -C /home/yuan/workspace/dev/Dragonfly2 branch --show-current
git -C /home/yuan/workspace/dev/Dragonfly2 ls-tree HEAD client

# main固定client提交中定向搜索
git -C /home/yuan/workspace/dev/Dragonfly2/client grep -n -i \
  -E '\b(rdma|ibverbs|libibverbs|efa|queue[_ -]?pair|completion[_ -]?queue|ibv_[a-z_]+|rdma_[a-z_]+)\b' \
  017575a58d0abad6b3b274142fa470d47d8db327

# 查看main固定版本的Downloader factory
git -C /home/yuan/workspace/dev/Dragonfly2/client show \
  017575a58d0abad6b3b274142fa470d47d8db327:dragonfly-client/src/resource/piece_downloader.rs

# 候选分支相对main固定client提交的修改范围
git -C /home/yuan/workspace/dev/Dragonfly2/client diff --stat \
  017575a58d0abad6b3b274142fa470d47d8db327..1ccc7d1048d213dd1237dfe0978a06ac2bde5c1c
```
