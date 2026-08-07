# URMA Buffer 生命周期与 Dragonfly PieceContentStream 映射

> 研究范围：Dragonfly Child 从 Parent 下载 standard Piece 时，request/response、Piece body `Bytes`、`PieceContentStream` 和 Storage 落盘之间的 buffer 所有权；以及 URMA SEND/RECV 的 JFR receive buffer 如何映射到该路径。本文只分析，不修改源码。
>
> Dragonfly 指定源码：`/home/yuan/workspace/cloud-native/dragonfly/dragonfly`。根仓库 commit `2acbb8b6414939919cfc8474bf0ba4c38ae2c8ba`；实际 `client` 子仓库 commit `c2f7edbfd4df573192beca60ca18790a730740e4`。依赖版本：`bytes 1.12.1`、`tokio-util 0.7.18`、`quinn 0.11.11`、`vortex-protocol 0.1.5`。
>
> URMA 指定源码：`/home/yuan/workspace/cloud-native/umdk` commit `3bfff198329a497ed49e53bd5585c34bcb7c9d88`；重点目录 `src/urma/tools/urma_perftest`，并向下核对 liburma 类型与当前 u-udma provider 的 `user_ctx/completion_len` 路径。
>
> 证据标记严格使用：`[源码确认]`、`[架构推断]`、`[待实验验证]`。

## 1. 结论摘要

1. `[源码确认]` TCP 与 QUIC 在 `Downloader` 之上走同一条链：构造 Vortex `DownloadPiece` request，先读取 response header 和 `PieceContent` metadata，再将剩余 body 适配为 `PieceContentStream<Item = io::Result<Bytes>>`，最后由 Storage 按 chunk 拉取、CRC32、批量 `pwritev`。
2. `[源码确认]` TCP body 的 `Bytes` 在 `tokio_util::io::ReaderStream::poll_next()` 中产生：socket 数据读入 `BytesMut`，随后 `split().freeze()`。`split/freeze` 不复制 payload，但 socket 到用户态 `BytesMut` 存在 read 数据边界。
3. `[源码确认]` QUIC body 的 `Bytes` 由 quinn `RecvStream::read_chunk()` 返回，Dragonfly 直接 move `chunk.bytes`，没有再复制 body；quinn 文档只确认 `read_chunk` 这一层“不复制”，不能据此确定 UDP、解密、重组全过程的 copy 次数。
4. `[源码确认]` Storage 不把 chunk 合并成完整 Piece。`Bytes` 被 move 到 `Vec<Bytes>`，CRC32 只读 payload，`IoSlice` 借用 payload，blocking `pwritev` 完成后 `chunks.clear()` 才释放 `Bytes`。
5. `[源码确认]` 因为 blocking write task 拥有 `Vec<Bytes>`，即使 Piece stream 报错或外层写超时导致 async future 被取消，已提交的 blocking task仍可能继续执行；相应 `Bytes` 的最终释放可能晚于下载 future 返回。
6. `[源码确认]` URMA `urma_jfr_wr_t` 的 SGE 指向已注册 local Segment；当前 u-udma provider post 时保存 `wr.user_ctx`，poll recv CQE 时恢复到 `urma_cr_t.user_ctx`，并返回 `completion_len`。这提供了 CQE 找回 receive slot 和有效字节范围的源码闭环。
7. `[架构推断]` JFR slot 在 recv CQE 到达后只从“设备可写”转为“应用可读”，不能立即 repost。若它被零复制包装为 `Bytes`，必须等最后一个 `Bytes` 引用析构后才能回收/repost，否则 Storage 的 CRC 或 `pwritev` 可能读到被下一条 SEND 覆盖的数据。
8. `[源码确认]` Dragonfly 已存在可复用的所有权范式：`BufferPool::freeze()` 使用 `Bytes::from_owner(PooledChunk)`，`PooledChunk::drop()` 在最后一个 `Bytes` 引用释放时归还 buffer。
9. `[架构推断]` 最小 URMA 映射应为 `UrmaCompletionPoller -> registered BufferPool -> per-Piece bounded channel -> PieceContentStream -> Storage`。slot owner 的 Drop 只把 slot ID 放入回收队列，由 poller/runtime 线程执行 repost。

## 2. 当前 TCP/QUIC：request 到 Storage 的逐步源码映射

### 2.1 上层入口和统一返回契约

```text
Piece::download_from_parent()
  -> TCPDownloader::download_piece() / QUICDownloader::download_piece()
  -> TCPClient::download_piece() / QUICClient::download_piece()
  -> (PieceContentStream, offset, digest)
  -> Storage::download_piece_from_parent_finished()
```

| 步骤 | 源码位置 | 结论 |
|---|---|---|
| 协议选择 | `client/dragonfly-client/src/resource/piece.rs:388-433` | `[源码确认]` 按 `config.download.protocol` 和 Parent IP/port 选择 TCP 或 QUIC；fallback 日志虽称 gRPC，实际仍调用 TCP downloader。 |
| Downloader trait | `client/dragonfly-client/src/resource/piece_downloader.rs:34-66` | `[源码确认]` `download_piece()` 返回 `(PieceContentStream, u64, String)`，后两项是 offset 和 digest。 |
| TCPDownloader | 同文件 `319-345` | `[源码确认]` 从 pool 取得 `TCPClient` wrapper，调用 `entry.client.download_piece()`，成功时直接转交 stream。 |
| QUICDownloader | 同文件 `167-193` | `[源码确认]` 行为与 TCPDownloader 对称。 |
| 交给 Storage | `resource/piece.rs:435-448` | `[源码确认]` stream 以 `&mut stream` 传给 `Storage::download_piece_from_parent_finished()`。 |

`[源码确认]` `TCPClient`/`QUICClient` wrapper 只保存 `Arc<Config>` 和 `addr`，pool 不持有 body buffer，也不持有可复用连接。stream 返回后，`request_guard` 随 downloader 方法返回而释放，body 的后续生命周期独立于 wrapper pool。

### 2.2 Request：创建、持有、发送、释放

TCP 和 QUIC 的 request 构造相同：

```rust
let request: Bytes = Vortex::DownloadPiece(
    Header::new_download_piece(),
    DownloadPiece::new(task_id.to_string(), number),
).into();
```

源码位置：

- TCP：`client/dragonfly-client-storage/src/client/tcp.rs:90-101`；
- QUIC：`client/dragonfly-client-storage/src/client/quic.rs:101-112`；
- Vortex 编码：Cargo registry `vortex-protocol-0.1.5/src/tlv/download_piece.rs:110-118`、`src/lib.rs:378-404`。

request buffer 的实际创建链：

```text
task_id: &str
  -> task_id.to_string()                         // 新 String
  -> DownloadPiece::into()
       -> BytesMut(capacity=68)
       -> extend_from_slice(task_id) + put_u32   // 第一次序列化
       -> freeze() -> value Bytes
  -> Vortex::into()
       -> 新 BytesMut(capacity=header+value)
       -> put header + extend_from_slice(value)  // value 再复制进完整 packet
       -> freeze() -> request Bytes
```

- `[源码确认]` request 编码至少有两次显式 payload 写入：业务字段写入中间 `BytesMut`，随后中间 `value Bytes` 被 `extend_from_slice()` 复制到最终 Vortex packet。
- `[源码确认]` `BytesMut::freeze()` 本身是所有权/表示转换，不复制 payload；`bytes 1.12.1/src/bytes_mut.rs:230-273` 明确称其为 zero cost，并复用底层 Vec/共享分配。
- `[源码确认]` TCP 在 `connect_and_write_request()` 的 `OwnedWriteHalf::write_all(&request)` 和 `flush()` 发送；源码 `tcp.rs:255-300`。
- `[源码确认]` QUIC 在 `connect_and_write_request()` 的 `SendStream::write_all(&request)` 发送；源码 `quic.rs:260-313`。
- `[源码确认]` `write_all()` 只借用 request slice；`connect_and_write_request(request)` 返回时按值参数 `request` 被 drop。它不进入 `PieceContentStream`，也不由 Storage 持有。
- `[待实验验证]` request 从用户 buffer 进入 TCP/UDP/QUIC provider 后的内核、加密和 NIC copy 次数，需要 trace/perf 验证。

### 2.3 Response metadata：创建、接收和复制边界

Parent TCP/QUIC 的 response 前半段相同：

```text
PieceContent::new(...)
  -> PieceContent::into() -> piece_content_bytes
  -> Header::new_piece_content(...).into() -> header_bytes
  -> response BytesMut
       extend_from_slice(header_bytes)
       extend_from_slice(piece_content_bytes)
  -> response.freeze()
  -> write_all(response)
  -> 紧接着发送 Piece body
```

源码：`client/dragonfly-client-storage/src/server/tcp.rs:222-251`、`server/quic.rs:233-269`；Vortex metadata 编码：`vortex-protocol-0.1.5/src/tlv/piece_content.rs:169-178`。

- `[源码确认]` Parent response metadata 经过多个小 buffer：metadata 自身编码为 `Bytes`，Header 编码为另一个 `Bytes`，server 再用 `extend_from_slice()` 把二者复制到最终 response `BytesMut`。
- `[源码确认]` 这些复制只覆盖 header/offset/length/digest 等 metadata，不覆盖随后流式发送的 Piece body。
- `[源码确认]` Linux TCP Parent 的 Piece body 使用 `sendfile_range()`，不在用户态创建 body `Bytes`；QUIC Parent 使用 `copy_buf(RangeReader, SendStream)`。源码：`server/tcp.rs:706-729`、`server/quic.rs:746-764`。

Child response 处理：

```text
read_header(reader)
  -> header BytesMut(HEADER_SIZE)
  -> read_exact
  -> freeze + parse

read_piece_content(reader)
  -> metadata_length BytesMut
  -> read_exact
  -> metadata BytesMut
  -> read_exact
  -> 新 content BytesMut
  -> copy length bytes + metadata bytes
  -> freeze + parse PieceContent

remaining reader
  -> content_stream(reader)
```

- `[源码确认]` TCP 源码为 `client/tcp.rs:101-123, 303-359`；QUIC 源码为 `client/quic.rs:112-133, 316-369`。
- `[源码确认]` response metadata 接收阶段存在 `read_exact()` 填充和一次显式合并复制，但 body 尚未进入这些 buffer。
- `[源码确认]` `PieceContent::metadata()` 返回 `PieceMetadata` clone；`handle_download_piece()` 将 `offset` 和 `digest` 移出该 clone，并把仍位于 body 起点的 reader 变为 stream。

### 2.4 Piece body Bytes -> PieceContentStream

`[源码确认]` 统一类型位于 `client/dragonfly-client-storage/src/client/mod.rs:20-26`：

```rust
pub type PieceContentStream =
    BoxStream<'static, std::io::Result<Bytes>>;
```

#### TCP

```text
OwnedReadHalf
  -> TCPClient::content_stream()
  -> ReaderStream::with_capacity(reader, storage.write_buffer_size)
  -> ReaderStream::poll_next()
       poll_read_buf(reader, &mut BytesMut)
       BytesMut::split()
       freeze()
  -> Result<Bytes>
  -> boxed PieceContentStream
```

- `[源码确认]` Dragonfly adapter 位于 `client/tcp.rs:55-59`。
- `[源码确认]` `ReaderStream` 具体实现在 `tokio-util-0.7.18/src/io/reader_stream.rs:45-120`：持有 `Option<OwnedReadHalf>`、`BytesMut` 和 capacity；成功读取后执行 `this.buf.split().freeze()`。
- `[源码确认]` `BytesMut::split()` 是 O(1) 的浅分割，返回已读区间并让内部 `BytesMut` 保留未使用的尾部 capacity；`bytes-1.12.1/src/bytes_mut.rs:349-378`。
- `[源码确认]` 因而 TCP body 存在 kernel socket -> 用户态 `BytesMut` 的 read 数据传递；`BytesMut -> Bytes` 不发生 payload copy。
- `[源码确认]` 若一次 read 未用满 allocation，ReaderStream 可以继续使用不重叠的尾部 capacity，同时上一个 `Bytes` 持有已读区间；尾部耗尽时 `capacity()==0`，下一次 poll 会 reserve 新空间。
- `[源码确认]` `PieceContentStream` 拥有 ReaderStream，ReaderStream 又拥有 `OwnedReadHalf`。stream EOF、错误或被 drop 时 reader 才随之释放。

#### QUIC

```text
RecvStream
  -> QUICClient::content_stream()
  -> futures::stream::unfold
  -> RecvStream::read_chunk(usize::MAX, ordered=true)
  -> quinn::Chunk { offset, bytes }
  -> move chunk.bytes
  -> Result<Bytes>
  -> boxed PieceContentStream
```

- `[源码确认]` Dragonfly adapter 位于 `client/quic.rs:58-70`。
- `[源码确认]` quinn `read_chunk()` 位于 `quinn-0.11.11/src/recv_stream.rs:159-203`，说明该接口相对 `read` 更高效，因为不复制；chunk boundary 不对应 peer write boundary。
- `[源码确认]` `ordered=true` 保证返回 chunk 的 offset 紧接前次已读位置；Dragonfly没有使用 `Chunk.offset`，只 move `chunk.bytes`。
- `[源码确认]` Dragonfly 从 `Chunk.bytes` 到 stream item 不做 body copy。
- `[待实验验证]` quinn-proto reassembly 之前的 UDP receive、解密、包重组、碎片合并和分配次数不由上述调用点证明。

### 2.5 PieceContentStream -> Storage

```text
Piece::download_from_parent()
  -> Storage::download_piece_from_parent_finished()
  -> handle_downloaded_piece_from_parent_finished()
  -> Content::write_piece_from_stream()
  -> write_range_from_stream()
       stream.try_next().await
       truncate to expected remaining length
       crc32fast::Hasher::update(&chunk)
       batch.push(chunk)
       spawn_blocking(write_all_vectored_at)
       IoSlice borrows each Bytes
       rustix::io::pwritev
       chunks.clear()
  -> compare expected digest
  -> metadata.download_piece_finished()
```

源码位置：

- `resource/piece.rs:435-460`；
- `dragonfly-client-storage/src/lib.rs:835-906`；
- `storage/src/content_linux.rs:268-301`；
- `storage/src/io.rs:327-480`。

- `[源码确认]` Storage 的真实消费点是 `write_range_from_stream()` 中的 `stream.try_next().await`，不是 downloader 返回 stream 的时刻。
- `[源码确认]` chunk 超过 expected remaining length 时，`Bytes::truncate()` 只缩短视图，不复制 payload；达到 expected length 后 Storage 不要求继续读到 transport EOF。
- `[源码确认]` CRC32 直接借用 `&chunk`；`batch.push(chunk)` move 的只是 `Bytes` handle；没有把多个 chunk 合并为一个 Piece buffer。
- `[源码确认]` `write_all_vectored_at()` 创建 `Vec<IoSlice>` 描述符并借用各 `Bytes`，`pwritev` 才把用户 payload写入普通 buffered file。
- `[源码确认]` 当前默认 `storage.write_buffer_size` 实际函数值为 512 KiB；它同时作为 TCP ReaderStream 初始 capacity 和 Storage batch flush threshold。源码：`dragonfly-client-config/src/dfdaemon.rs:268-277, 971-985`。

## 3. Bytes 的精确生命周期

### 3.1 正常路径所有权转移

```text
TCP ReaderStream / quinn reassembly
  owns allocation
      |
      v
Bytes stream item
  owns/refcounts immutable view
      |
      v  move
write_range_from_stream local `chunk`
      |
      +-- borrowed by CRC32
      |
      v  move
Vec<Bytes> `batch`
      |
      v  move into spawn_blocking closure
Vec<Bytes> `chunks`
      |
      +-- borrowed by Vec<IoSlice> during pwritev
      |
      v
pwritev returns -> chunks.clear()
      |
      v
drop each Bytes -> release backing storage / owner
```

- `[源码确认]` `Bytes` 自身由 pointer、length、data 和 vtable 组成，并实现引用计数/owner 驱动的 Clone/Drop；`bytes-1.12.1/src/bytes.rs:101-120, 662-678`。
- `[源码确认]` 正常路径至少允许同时存在两组 body buffer：async 侧正在积累的 `batch`，以及一个 blocking thread 持有的 `in_flight` batch。源码：`storage/src/io.rs:400-468`。
- `[源码确认]` 某个 body `Bytes` 的通常释放点是其所在 blocking batch 的 `pwritev` 完成并执行 `chunks.clear()`，不是 stream yield 时，也不是 CRC32 完成时。
- `[源码确认]` 如果某个 `Bytes` 被 clone，底层 owner 必须等最后一个 clone drop 才释放。当前 Storage 主路径只 move、不 clone body chunk，但 `PieceContentStream` 的公开 item 类型并不禁止其他消费者 clone。

### 3.2 错误、超时和提前结束

- `[源码确认]` stream 在 `try_next().await?` 返回错误时，当前正在积累且尚未提交的 `batch` 随函数退出而 drop。
- `[源码确认]` 已经 `spawn_blocking` 的 write task 拥有其 `Vec<Bytes>`；drop Tokio `JoinHandle` 只 detach task，不会取消正在执行的 blocking closure。因此这些 `Bytes` 仍存活到 closure 返回或错误 unwind。
- `[源码确认]` `Storage::download_piece_from_parent_finished()` 用 `tokio::select!` 实现 `write_piece_timeout`。超时会取消写 future，但不能反向取消已经进入 blocking pool 的 `pwritev` closure。
- `[架构推断]` URMA runtime 在 Piece 报错/超时后不能立即注销 receive Segment 或销毁 BufferPool；必须等所有 stream lease 和 detached blocking write 中的 lease 都归还。
- `[源码确认]` Storage 达到 expected length 后停止 poll stream；剩余 transport reader 随 `PieceContentStream` 离开作用域而 drop。最后一个被截断的 chunk仍由 batch 持有至写完成。
- `[待实验验证]` Tokio blocking pool 排队时，超时后 slot lease 最长滞留时间与磁盘尾延迟相关，需要故障注入测量。

### 3.3 当前路径的 copy 清单

| 区段 | 结论 |
|---|---|
| request 业务字段 -> DownloadPiece buffer | `[源码确认]` `extend_from_slice`/`put_u32`，发生序列化写入。 |
| DownloadPiece value -> 最终 Vortex packet | `[源码确认]` `extend_from_slice(&value)`，发生一次显式复制。 |
| Parent PieceContent/Header -> response packet | `[源码确认]` server `extend_from_slice` 合并，发生 metadata 复制。 |
| Child socket -> TCP ReaderStream BytesMut | `[源码确认]` AsyncRead 填充用户 buffer，存在 kernel/user 数据边界。 |
| TCP BytesMut -> Bytes | `[源码确认]` `split().freeze()`，不复制 payload。 |
| quinn Chunk.bytes -> PieceContentStream | `[源码确认]` move handle，不复制 payload。 |
| Bytes -> CRC32 | `[源码确认]` 只读借用，不复制。 |
| Bytes -> Vec<Bytes> | `[源码确认]` move handle，不复制 payload。 |
| Vec<Bytes> -> IoSlice | `[源码确认]` 建立借用描述符，不复制 payload。 |
| IoSlice -> task file | `[源码确认]` `pwritev` 的用户/内核文件写边界；未使用 `O_DIRECT`。 |
| 固定端到端 memcpy 次数 | `[待实验验证]` 还取决于内核、quinn、文件系统、页缓存、provider 与 NIC，不能只由应用源码给出。 |

## 4. URMA SEND/RECV receive buffer 的源码事实

### 4.1 Buffer 分配、注册与 SGE

`urma_perftest` 的关键链路：

```text
memalign/hugepage -> ctx->local_buf
  -> urma_register_seg() -> ctx->local_tseg
  -> urma_sge_t { addr, len, tseg }
  -> urma_jfr_wr_t { src.sge, user_ctx }
  -> urma_post_jfr_wr() / urma_post_jetty_recv_wr()
```

- `[源码确认]` `perftest_resources.c:851-964` 按 page 对齐分配 `local_buf`，将 buffer 分为 recv/send 区域，并通过 `urma_register_seg()` 注册整个 `buf_len`。
- `[源码确认]` `perftest_run_test.c:1205-1234` 的 `init_jfr_wr()` 设置 receive SGE 的 `addr`、`len` 和 `tseg=ctx->local_tseg[i]`，再设置 `wr->src` 与 `wr->user_ctx`。
- `[源码确认]` `prepare_jfr_wr()` 在 BW 测试前预投 receive WR：simplex 调 `urma_post_jfr_wr()`，duplex 调 `urma_post_jetty_recv_wr()`；源码 `perftest_run_test.c:1270-1339`。
- `[源码确认]` `urma_sge_t`、`urma_jfr_wr_t` 和 `urma_cr_t` 定义位于 `src/urma/lib/urma/core/include/urma_types.h:1026-1041, 1129-1167`。

### 4.2 post -> CQE -> slot 的源码闭环

```text
receive WR.user_ctx
  -> u-udma post_recv_one()
       rq.wrid[wqe_idx] = wr.user_ctx
  -> hardware writes registered SGE and produces CQE
  -> urma_poll_jfc()
  -> u-udma parse recv CQE
       entry_idx -> rq.wrid[] -> cr.user_ctx
       cqe.byte_cnt -> cr.completion_len
  -> application recovers slot/context and valid byte length
```

- `[源码确认]` u-udma `udma_u_jfr.c:445-475` 将 receive SGE 编进 WQE，并把 `wr->user_ctx` 保存到 `jfr->rq.wrid[wqe_idx]`；post 后写 receive doorbell。
- `[源码确认]` u-udma `udma_u_jfc.c:673-689` 根据 recv CQE `entry_idx` 恢复 `cr->user_ctx`，随后释放 provider 内部 WQE index 并推进 queue consumer index。
- `[源码确认]` `udma_u_jfc.c:749-768` 把 CQE `byte_cnt` 写入 `cr->completion_len`，同时填充 send/recv、Jetty、local/remote id 等字段。
- `[源码确认]` `urma_cr_t.completion_len` 的 API 注释就是 “The number of bytes transferred”；`user_ctx` 是对应 WR 的 completion data。
- `[源码确认]` `urma_perftest` 接收循环 `run_once_bw_recv()` poll recv JFC，检查 `cr.status`，用 `cr.user_ctx` 找到 Jetty，然后按消费量 repost；源码 `perftest_run_test.c:2074-2205`。

### 4.3 perftest 不能直接照搬的所有权行为

- `[源码确认]` perftest BW 收到 CQE 后只计数，不把 payload 交给异步下游，所以可以在达到批量阈值后直接 repost 对应 receive WR。
- `[源码确认]` perftest `user_ctx` 默认只编码 Jetty index，不足以标识 Dragonfly 所需的具体 slot、Piece request 和 chunk sequence。
- `[源码确认]` `perftest_mgmt_ub.c:760-802` 展示的是 copy-out 模式：CQE 后按 `completion_len` 从固定 `recv_buf` `memcpy` 到 caller buffer，然后固定 receive buffer 才能安全复用。
- `[架构推断]` Dragonfly 若采用 copy-out 模式，可以在 CQE 后复制为独立 `Bytes` 并立即 repost；若追求 receive-buffer 零复制，则必须延迟 repost 到 `Bytes` 最后引用释放。
- `[待实验验证]` “成功 recv CQE 后设备绝不再访问该 SGE”的精确 memory visibility/cache ordering 保证，需要目标 provider/设备规范和读写 litmus test确认；perftest 以 CQE 后直接读 buffer，但未测试跨线程 Rust owner 场景。

## 5. JFR buffer 到 PieceContentStream 的所有权映射

### 5.1 必须采用的 slot 状态机

```text
Free
  -> Posted                         post_recv(slot SGE)
  -> Completed                      recv CQE(status, user_ctx, completion_len)
  -> StreamOwned                    immutable Bytes lease sent to Piece stream
  -> StorageBatch                   Bytes moved into Vec<Bytes>
  -> BlockingWrite                  pwritev closure owns Bytes
  -> ReclaimQueued                  last Bytes/owner Drop
  -> Free or Posted                 poller processes recycle queue and reposts
```

- `[架构推断]` `Posted` 状态中应用不得修改或释放 slot；它是硬件可写的 receive target。
- `[架构推断]` 成功 CQE 只允许 `Posted -> Completed`，不能直接 `Posted -> Posted`，因为下游尚未读取 payload。
- `[架构推断]` `completion_len` 只决定当前 slot 中有效 slice 的长度；它不提供 Piece ID、chunk sequence、EOF 或 digest。
- `[架构推断]` `StreamOwned` 以后 buffer 必须视为 immutable，这与 `Bytes` API契约一致。
- `[架构推断]` 真正可复用点是最后一个 owner Drop，而非 poller 产生 stream item 的时刻。
- `[待实验验证]` CQE error、flush、peer reset 时 slot 是否可立即 repost或必须先把 Jetty/JFR 置特定状态，需目标环境验证。

### 5.2 两种可实现的 Bytes 映射

#### A. Copy-out 模式

```text
CQE
  -> lookup slot
  -> Bytes::copy_from_slice(slot[..completion_len])
  -> repost slot immediately
  -> send independent Bytes to PieceContentStream
```

- `[源码确认]` `Bytes::copy_from_slice()` 会创建独立分配并复制；`bytes-1.12.1/src/bytes.rs` 提供该 API。
- `[源码确认]` `urma_perftest` 的 UB management receive 使用等价的 `memcpy` copy-out 思路。
- `[架构推断]` 优点是 slot 与 Storage 生命周期解耦、repost 简单；代价是每个 chunk 增加一次用户态 copy。
- `[待实验验证]` copy 成本是否被 CRC/pwritev/磁盘成本掩盖，应以端到端 Piece 落盘测试判断。

#### B. Lease/owner 模式

```text
CQE
  -> lookup completed slot
  -> UrmaRecvLease { pool, slot_id, len, request_id, generation }
  -> Bytes::from_owner(lease)
  -> PieceContentStream
  -> Storage batch / blocking pwritev
  -> last Bytes drop
  -> UrmaRecvLease::drop()
  -> recycle_queue.push(slot_id, generation)
  -> poller validates generation and reposts
```

- `[源码确认]` `bytes 1.12.1` 的 `Bytes::from_owner(T)` 要求 owner 实现 `AsRef<[u8]> + Send + 'static`；最后一个 `Bytes` clone drop 后 owner 才 drop。源码：`bytes/src/bytes.rs:225-280`。
- `[源码确认]` Dragonfly 已有完全相同的回收范式：`dragonfly-client-util/src/buffer_pool/mod.rs:102-134` 中 `BufferPool::freeze()` 构造 `Bytes::from_owner(PooledChunk)`，`PooledChunk::drop()` 将底层 `BytesMut` 归还 pool。
- `[架构推断]` `UrmaRecvLease` 可以复用该范式，但它拥有的应是“已注册大 Segment 中一个 slot 的独占租约”，而不是移动 Segment 本身。
- `[架构推断]` Drop 可能发生在 Tokio runtime、blocking `pwritev` thread 或任意持有最后 clone 的线程；因此 Drop 不应直接调用可能有线程亲和/锁约束的 URMA post API，只应执行非阻塞 recycle enqueue。
- `[架构推断]` `generation` 用于防止取消、延迟 Drop 或 connection reset 后的旧 lease 错误归还已重新分配的 slot。
- `[待实验验证]` liburma/provider 的 post 线程安全、注册内存跨线程可见性，以及 lock-free recycle queue 对 polling latency 的影响。

### 5.3 Piece 路由、顺序和 EOF

- `[源码确认]` `urma_cr_t.user_ctx` 是一个 `uint64_t`，可携带 slot table key，但 perftest 没有定义 Dragonfly request/chunk 协议。
- `[架构推断]` 最安全的 `user_ctx` 是稳定 slot key/generation；Piece request id、chunk sequence、payload type、EOF/total length应放在应用消息 header 或 slot metadata table，不能只依赖裸 pointer。
- `[源码确认]` Dragonfly Storage 按 stream item 到达顺序推进 file offset，不读取 chunk 自带 offset；QUIC 因此显式使用 `ordered=true`。
- `[架构推断]` URMA adapter 必须在 yield 前保证每个 Piece 内有序：要么由单 Jetty/单发送队列的已验证顺序保证，要么按应用 sequence重排。
- `[待实验验证]` 目标 transport mode 下 SEND、recv CQE、多个 JFC/Jetty 的实际 ordering 保证；perftest 计数路径没有验证 payload 顺序。
- `[架构推断]` Piece stream 的结束应由已知 expected length和应用 metadata/EOF 控制。单个 CQE只表示一个 receive WR 完成，不等于 Piece 完成。

## 6. 最小架构：UrmaCompletionPoller -> BufferPool -> PieceContentStream

### 6.1 组件关系

```text
                         recycle_queue <-----------------------------+
                              |                                      |
                              v                                      | Drop(last Bytes)
+---------------------+   +----------------------+   +-------------------------------+
| UrmaCompletionPoller|-->| RegisteredBufferPool |-->| Bytes<UrmaRecvLease owner>     |
| poll recv JFC       |   | slot state/generation|   +-------------------------------+
| validate CQE        |   | registered Segment   |                  |
| route request       |   | post/repost RQE      |                  v
+---------------------+   +----------------------+       bounded per-Piece channel
          |                                                       |
          | error                                                 v
          +------------------------------------------> PieceContentStream
                                                                  |
                                                                  v
                                              CRC -> Vec<Bytes> -> pwritev
```

### 6.2 `RegisteredBufferPool`

- `[架构推断]` pool 启动时分配固定大块内存、注册一个或少量 Segment，再按固定大小切成 N 个 receive slots，避免 per-chunk 注册/注销。
- `[架构推断]` slot table 至少记录 `slot_id`、address、capacity、state、generation、当前 request/connection 和实际 `completion_len`。
- `[架构推断]` pool 是 buffer 所有权的唯一真相源；poller、stream 和 Storage 只持 slot key 或 lease，不直接释放 Segment。
- `[架构推断]` shutdown 顺序必须是停止新 post、关闭 Piece streams、等待 outstanding WR/CQE、等待所有 lease归还、drain recycle queue，最后 unregister Segment/free memory。
- `[待实验验证]` pool/slot 大小、JFR depth、注册粒度、NUMA placement 和最大 pin memory。

### 6.3 `UrmaCompletionPoller`

- `[架构推断]` poller 独占或协调 recv JFC polling，先检查 `urma_poll_jfc()` 返回值，再逐个检查 `cr.status`。
- `[架构推断]` poller 用 `cr.user_ctx` 找 slot，校验 generation/state，并用 `cr.completion_len` 检查不超过 slot capacity。
- `[架构推断]` poller 解析最小应用 header，将 metadata 与 body路由到对应 request；body 形成 lease-backed `Bytes`。
- `[架构推断]` poller 不在 CQE 到达时 repost body slot；它先将 slot 转为 `StreamOwned`，之后只处理 recycle queue 中由 lease Drop 归还的 slot。
- `[架构推断]` poller 不宜阻塞等待某个满的 Tokio channel；可将 completed slot 放入有限 pending 队列或 `try_send`，让有限 slot/JFR credit形成背压。
- `[待实验验证]` busy poll、JFCE event、混合 poll 的 CPU/延迟，以及 poller 与 Tokio runtime 的唤醒开销。

### 6.4 `PieceContentStream`

最小外观仍保持 Dragonfly 现有类型：

```rust
BoxStream<'static, io::Result<Bytes>>
```

- `[架构推断]` 每个在途 Piece 创建 bounded `mpsc` receiver，并包装成 boxed stream；poller持有 sender/route entry。
- `[架构推断]` metadata response到达并解析后，`UrmaDownloader::download_piece()` 即可返回 `(stream, offset, digest)`；body CQE随后异步驱动 stream，与当前 TCP/QUIC “先 metadata、后 body”契约一致。
- `[架构推断]` stream drop 必须注销 route、丢弃尚未交付的 pending lease；这些 lease的 Drop进入 recycle queue。
- `[架构推断]` transport/CQE error应转换为一次 `io::Error` stream item或关闭 stream；Piece 的 retry、digest和 metadata失败语义继续由现有上层处理。
- `[源码确认]` 只要输出满足 `Stream<Item=io::Result<Bytes>> + Unpin`，现有 `Storage::download_piece_from_parent_finished()` 和 `write_range_from_stream()` 无需理解 URMA。

### 6.5 最小正常时序

```text
1. BufferPool checkout Free slot，设置 generation/request，post recv WR
2. Parent SEND 一个 body chunk
3. JFR 写入 slot，recv JFC 产生 CQE
4. Poller: status == SUCCESS
5. Poller: user_ctx -> slot；completion_len -> valid slice
6. Poller: slot Posted -> Completed -> StreamOwned
7. Poller: Bytes::from_owner(UrmaRecvLease) -> Piece channel
8. Storage: try_next -> CRC -> batch -> blocking pwritev
9. pwritev 返回，Vec clear，最后一个 Bytes drop
10. Lease Drop -> recycle_queue(slot_id, generation)
11. Poller 验证并将 slot ReclaimQueued -> Free -> Posted
```

- `[源码确认]` 步骤 3–5 的 transport字段闭环由 perftest、liburma类型和 u-udma provider源码确认。
- `[源码确认]` 步骤 8–9 的 Dragonfly消费/释放路径由 Storage源码确认。
- `[架构推断]` 步骤 1、6–7、10–11 是两侧源码之间必须新增的 ownership adapter。

## 7. 背压、取消和资源安全

### 7.1 背压

- `[架构推断]` 有限 registered slots + bounded Piece channel 是最小背压机制。Storage慢时 lease不释放，可 repost RQE减少，最终耗尽 receive credits，而不是无限分配普通 `Bytes`。
- `[架构推断]` 必须为 control request/metadata保留独立 slot或 credit，避免 body slot全部被慢磁盘占用后控制消息也无法接收。
- `[待实验验证]` JFR 无 RQE 时的 RNR、sender retry、超时和 CQE status，以及恢复后是否继续有序。

### 7.2 Piece timeout/cancel

- `[架构推断]` cancel 分为 route cancel 与物理 slot回收：route可以立即拒绝新 CQE交付，但已经由 Storage/blocking task持有的 lease只能等待最终 Drop。
- `[架构推断]` cancel 后到达的 CQE仍必须 poll 和收敛 slot状态，不能只删除 request map，否则会泄漏 Posted slot。
- `[架构推断]` connection reset时应递增 connection/slot generation，使迟到 lease只执行安全回收，不被误当成当前 request buffer。
- `[源码确认]` Dragonfly 当前 `write_piece_timeout` 取消 async 写 future，但 blocking write可能继续；URMA shutdown/refcount设计必须覆盖这个既有行为。

### 7.3 不能使用的错误释放点

| 时刻 | 能否 repost/free | 结论 |
|---|---:|---|
| receive WR post 成功 | 否 | `[源码确认]` post 成功只表示 WR 入队，设备仍可写 SGE。 |
| recv CQE 到达 | 不能立即 repost | `[架构推断]` 仅表示 payload可交给应用；下游尚未消费。 |
| `Bytes` yield 到 stream | 否 | `[架构推断]` stream/Storage开始持有该 slot。 |
| CRC32 update 完成 | 否 | `[源码确认]` `Bytes` 随后还要被 blocking pwritev借用。 |
| `pwritev` 提交到 blocking pool | 否 | `[源码确认]` blocking closure拥有 `Bytes`。 |
| `pwritev` 返回且最后一个 `Bytes` drop | 是，可排队回收 | `[源码确认]` 当前 Storage至此不再读取 payload；`[架构推断]` Drop应触发 recycle而非任意线程直接post。 |
| Piece metadata commit | 不必等待到此才回收每个 chunk | `[架构推断]` chunk buffer可在各自文件写完成后流水回收，metadata是Piece级完成。 |
| Segment unregister | 仅所有 WR/CQE/lease清零后 | `[架构推断]` 否则存在 DMA或悬空 `Bytes` 引用风险。 |

## 8. 第一版实现选择判断（仅分析）

| 选择 | 源码依据 | 判断 |
|---|---|---|
| 先做 copy-out | perftest management 已使用 CQE 后 `memcpy`；与 slot立即repost兼容 | `[架构推断]` 正确性最简单，适合作为首个基线。 |
| 再做 lease-backed Bytes | Dragonfly 已有 `Bytes::from_owner(PooledChunk)`；Storage全程接受并持有 `Bytes` | `[架构推断]` 接口上可行，是验证零额外 body copy 的最小演进。 |
| CQE 后立即 repost零复制 slot | Storage会持有 `Bytes` 到 blocking pwritev完成 | `[架构推断]` 不安全，应排除。 |
| 每 chunk register/unregister | perftest先长期注册整个 buffer，再重复post WR | `[架构推断]` 不符合已验证使用方式，也放大注册成本，应排除。 |
| Poller直接同步写盘 | 当前 Storage已提供CRC、batch、pwritev、timeout和metadata语义 | `[架构推断]` 会绕开成熟路径，不是最小映射。 |

## 9. 必须实验验证的清单

1. `[待实验验证]` 成功 recv CQE 后，目标设备对 receive buffer的 CPU 可见性、cache同步和跨线程 happens-before 保证。
2. `[待实验验证]` JFR RQ starvation/RNR、重试时长、错误 CQE与恢复后的顺序。
3. `[待实验验证]` 同一 Jetty/JFR/JFC及多 Jetty共享JFC时的 SEND/recv CQE ordering。
4. `[待实验验证]` `completion_len`、`user_ctx`、remote id在目标 transport mode和错误路径中的一致性。
5. `[待实验验证]` 从 registered slot构造 lease-backed `Bytes` 后跨 Tokio runtime与blocking pool持有的安全性和性能。
6. `[待实验验证]` Piece timeout时已排队/运行的 `pwritev` 导致 slot lease滞留的最长时间。
7. `[待实验验证]` copy-out与lease模式在不同 chunk、JFR depth和磁盘条件下的吞吐、P99、CPU、内存占用。
8. `[待实验验证]` clean shutdown、peer crash、CQ overflow、Jetty error/flush时 outstanding slot是否无泄漏收敛。

## 10. 最终结论

`[源码确认]` Dragonfly 已把 transport body 收敛为 `PieceContentStream<Item=Bytes>`，Storage又以所有权 move方式把 `Bytes` 保持到 `pwritev` 完成；这使 URMA 接入无需改变 Storage接口，但也把 receive buffer的安全复用点严格推迟到最后一个 `Bytes` Drop。

`[源码确认]` URMA源码已经提供映射所需的三个基础事实：JFR WR的 SGE指定 registered receive buffer，`user_ctx` 可从 WR穿透到 recv CQE，`completion_len` 给出有效字节数。perftest也确认预投、poll和repost闭环，但没有实现异步下游所有权。

`[架构推断]` 最小正确架构应由 `UrmaCompletionPoller` 统一处理 CQE和recycle queue，由 `RegisteredBufferPool` 管理 slot状态与generation，由 lease-backed `Bytes` 把 slot生命周期延长到 Storage完成读取。若第一版优先验证互通与正确性，可先 copy-out；若验证数据面copy收益，再替换为 `Bytes::from_owner(UrmaRecvLease)`，而 `PieceContentStream` 和 Storage契约保持不变。
