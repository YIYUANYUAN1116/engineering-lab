# Dragonfly TCP/QUIC 数据路径到 URMA 通信模型的映射分析

> 本文基于已有 `03-download-flow.md`、`09-data-plane-and-ub-integration-analysis.md`、`10-piece-stream-and-storage-path-analysis.md`，以及 `ub/` 下 URMA/`urma_perftest` 分析继续下钻，不重复介绍 Dragonfly 或 UB 的通用架构。
>
> Dragonfly 源码核对基线：`/home/yuan/workspace/dev/Dragonfly2`，commit `2acbb8b6414939919cfc8474bf0ba4c38ae2c8ba`（2026-07-29）。URMA API 事实沿用 `ub/urma-perftest-analysis.md` 的 UMDK commit `2a958bb4212d`。
>
> 证据标记：`[源码确认]` 表示可由上述 Dragonfly 或 UMDK 源码直接确认；`[架构推断]` 表示依据两侧已确认接口作出的接入判断；`[需要验证]` 表示仍依赖真实 URMA provider、硬件或协议实验。

## 1. 结论摘要

1. `[源码确认]` Dragonfly 已有 `Downloader` trait；TCP 和 QUIC 都实现它，并统一返回 `(PieceContentStream, offset, digest)`。这是 Child 侧最小切入点。
2. `[源码确认]` TCP/QUIC 共用的是 **Downloader 返回契约、PieceContentStream、Vortex 业务协议和后续 Storage 写路径**；没有覆盖 client、server、连接和发送操作的统一 `Transport` trait。
3. `[源码确认]` 当前每个 Piece 都建立新 TCP 连接；QUIC 也为每个 Piece 新建 Endpoint、Connection 和一个双向 stream。`TCPDownloader/QUICDownloader` 的 pool 只缓存无连接状态的 client wrapper，不能视为连接池。
4. `[源码确认]` request 在连接建立后由 `write_all()` 发送；Piece body 并不是在 `TCPClient::download_piece()` 内读完，而是在 Storage 调用 `PieceContentStream::try_next()` 时按需接收。
5. `[源码确认]` `PieceContentStream` 拥有 TCP `OwnedReadHalf` 或 QUIC `RecvStream`，其生命周期覆盖网络 body 接收；流被消费、报错、超时或丢弃后，相应底层接收对象才释放。
6. `[架构推断]` URMA 不能把 `TcpStream`、request、stream、buffer 分别机械替换成 Jetty、WR、CQE、Segment。正确关系是：长期通信资源承载多个 Piece 级 WR；一串 receive CQE 驱动一个有序 `PieceContentStream`；注册 Segment 的 buffer 在 Storage 完成 `pwritev()` 前不能归还或重新 post。
7. `[架构推断]` 验证单 Peer、单 Piece 的第一版原型，以 duplex Jetty 的 SEND/RECV 最接近现有请求—元数据—body 语义。最小集是 URMA runtime、连接/描述符交换、注册 buffer pool、URMA client、URMA server、completion-to-stream adapter 和 `UrmaDownloader`，不必先设计完整 Transport 抽象。

## 2. Dragonfly 当前 Piece 下载链路

### 2.1 从调度结果到 Downloader

standard task 的 Parent 下载主链：

```text
Task::download_partial_with_scheduler_from_parent()
  -> PieceCollector::run()
  -> PieceCollector::collect_from_parents()
       -> DfdaemonUploadClient::sync_pieces()
       -> 得到 piece availability + ip/tcp_port/quic_port
  -> ParentSelector::select()
  -> Piece::download_from_parent()
  -> TCPDownloader::download_piece() / QUICDownloader::download_piece()
  -> TCPClient::download_piece() / QUICClient::download_piece()
  -> PieceContentStream
  -> Storage::download_piece_from_parent_finished()
  -> Content::write_piece_from_stream()
  -> write_range_from_stream()
  -> CRC32 + pwritev + metadata.download_piece_finished()
```

- `[源码确认]` Piece 并发由 `Task::download_partial_with_scheduler_from_parent()` 中的 semaphore 控制；每个 `CollectedPiece` 选择一个 Parent 后调用 `Piece::download_from_parent()`。源码：`client/dragonfly-client/src/resource/task.rs`，约 1271–1504 行。
- `[源码确认]` `PieceCollector` 的 gRPC `sync_pieces()` 只获取 Piece 可用性和下载端点，Piece body 不走该 gRPC stream。源码：`resource/piece_collector.rs`，约 195–288 行。
- `[源码确认]` `Piece::download_from_parent()` 按 `config.download.protocol` 和 Parent 的端口字段在 TCP/QUIC 间选择，随后把返回 stream 交给 Storage。源码：`resource/piece.rs`，约 343–448 行。

### 2.2 TCPDownloader / TCPClient 精确调用链

```text
Piece::download_from_parent()
  -> TCPDownloader::download_piece(addr, number, host_id, task_id)
       -> get_entry_key(addr)
       -> client_pool.entry(key, addr)
       -> Entry<TCPClient>::request_guard()
       -> TCPClient::download_piece(number, task_id)
            -> timeout(piece_timeout, handle_download_piece(...))
            -> Vortex::DownloadPiece(...).into()             // 构造 request Bytes
            -> connect_and_write_request(request)
                 -> timeout(DEFAULT_CONNECT_TIMEOUT,
                            TcpStream::connect(addr))          // connect 位置
                 -> socket options
                 -> TcpStream::into_split()
                 -> OwnedWriteHalf::write_all(&request)       // request 发送位置
                 -> flush()
            -> read_header(&mut OwnedReadHalf)
            -> read_piece_content(&mut OwnedReadHalf)         // 只读响应 metadata
            -> content_stream(OwnedReadHalf)
                 -> ReaderStream::with_capacity(...).boxed()
  -> Storage::download_piece_from_parent_finished(&mut stream)
       -> write_range_from_stream()
            -> stream.try_next().await                        // Piece body 实际接收位置
```

- `[源码确认]` `connect` 的唯一实际位置是 `TCPClient::connect_and_write_request()` 内的 `tokio::net::TcpStream::connect(self.addr.clone())`。源码：`client/dragonfly-client-storage/src/client/tcp.rs`，约 255–263 行。
- `[源码确认]` Vortex request 在同一函数 split 之后由 `writer.write_all(&request)`、`writer.flush()` 发出。源码：同文件约 291–300 行。
- `[源码确认]` header 和 `PieceContent` metadata 在 `handle_download_piece()` 内通过 `read_exact()` 先消费；body 的首字节位置由剩余的 `OwnedReadHalf` 保留。源码：同文件约 90–123、303–359 行。
- `[源码确认]` body 真正被拉取的位置是 `write_range_from_stream()` 的 `stream.try_next().await`。对 TCP，这会驱动 `ReaderStream` 从 `OwnedReadHalf` 读入其内部 `BytesMut`。源码：`client/dragonfly-client-storage/src/io.rs`，约 388–480 行。

### 2.3 QUIC 对应链路及差异

```text
QUICDownloader::download_piece()
  -> QUICClient::download_piece()
  -> handle_download_piece()
       -> 构造相同 Vortex DownloadPiece request
       -> connect_and_write_request()
            -> Endpoint::client(local_ip:0)
            -> endpoint.connect(parent, "d7y")
            -> connection.open_bi()
            -> SendStream::write_all(request)
       -> read_header(RecvStream)
       -> read_piece_content(RecvStream)
       -> content_stream(RecvStream)
            -> unfold + read_chunk(usize::MAX, ordered=true)
            -> chunk.bytes
```

- `[源码确认]` QUIC 的连接建立位置是 `QUICClient::connect_and_write_request()` 中的 `Endpoint::client()`、`connect()` 和 `open_bi()`；request 在随后 `SendStream::write_all()` 发送。源码：`client/dragonfly-client-storage/src/client/quic.rs`，约 260–313 行。
- `[源码确认]` Piece body 由 `content_stream()` 中的 `RecvStream::read_chunk(..., true)` 产生；`ordered=true` 与下游按到达顺序推进文件 offset 的要求一致。源码：同文件约 58–70 行。
- `[源码确认]` Parent QUIC server 能在一条已接受的 connection 上循环 `accept_bi()`；但当前 Child 每次 Piece 都创建新 Endpoint 和 Connection，未利用这个多 stream 能力。源码：`storage/src/server/quic.rs`，约 107–185 行；`storage/src/client/quic.rs`，约 260–313 行。

### 2.4 Parent 端 request 与 Piece 发送

TCP Parent：

```text
TCPServer::run() -> listener.accept()
  -> TCPServerHandler::handle(TcpStream)
       -> read_header() + read_download_piece()
       -> handle_piece(piece_id, task_id)
            -> Storage::get_piece()
            -> Storage::upload_piece()
            -> RangeReader(task file range)
       -> write_response(PieceContent metadata)
       -> write_stream(RangeReader)
            -> Linux: sendfile_range(file -> socket)
            -> non-Linux: copy_buf()
```

QUIC Parent 在 `QUICServerHandler::handle_stream()` 中执行同样的 Vortex dispatch 和 `Storage::upload_piece()`，body 使用 `copy_buf(RangeReader, SendStream)`。

- `[源码确认]` 当前 Parent 数据源是 task 文件的 `RangeReader`，不是一块已注册网络内存。Linux TCP 特化使用 `sendfile`，QUIC 使用用户态读取/发送。源码：`storage/src/server/tcp.rs` 的 `handle_piece()`、`write_stream()`；`server/quic.rs` 的同名函数。
- `[架构推断]` URMA 原型若使用 SEND/RECV，Parent 必须把 `RangeReader` 分块读入已注册 staging Segment 后 post SEND；现有 Linux TCP `sendfile` 快路径不能直接复用。
- `[需要验证]` 目标 URMA provider 是否支持文件页注册、ODP、设备直读文件或等价能力；现有 `urma_perftest` 仅确认对对齐分配的用户态内存调用 `urma_register_seg()`，不能据此宣称可直接注册 task file range。

## 3. PieceContentStream 与 buffer 生命周期

### 3.1 PieceContentStream 生命周期

`[源码确认]` 类型定义为：

```rust
pub type PieceContentStream =
    BoxStream<'static, std::io::Result<bytes::Bytes>>;
```

生命周期如下：

```text
连接建立并发送 request
  -> 读完 response header + PieceContent metadata
  -> 创建 PieceContentStream（拥有 TCP reader 或 QUIC RecvStream）
  -> 返回 Piece::download_from_parent()
  -> Storage 按需 poll stream
  -> 每个 Bytes：CRC32 -> 加入 write batch -> pwritev
  -> 收到期望 Piece 长度
  -> 等待最后一次 pwritev 完成
  -> 校验 digest并提交 metadata
  -> stream 离开作用域并 drop
```

- `[源码确认]` TCP stream closure 捕获并拥有 `OwnedReadHalf`；QUIC stream closure 捕获并拥有 `RecvStream`。因此返回 `PieceContentStream` 后仍可继续收 body，而无需 `TCPClient/QUICClient` 自身保存连接。
- `[源码确认]` `download.piece_timeout` 包住的是 `handle_download_piece()`，在返回 stream 时已经结束，并不覆盖整个 body；body 写入由 `storage.write_piece_timeout` 在 `Storage::download_piece_from_parent_finished()` 外层限制。
- `[源码确认]` Storage 以期望长度为终止边界：达到长度后可不再 poll EOF；超长最后一个 `Bytes` 只做 `truncate()` 视图截断，不足则报 length mismatch。
- `[架构推断]` URMA adapter 不能把“收到一个 CQE”等同于流结束。它需要明确 chunk 长度、序号/offset 和 Piece EOF/总长度，并在取消或超时后停止向已销毁的 Piece channel 投递。

### 3.2 当前 client pool 与连接生命周期

- `[源码确认]` `TCPClient` 和 `QUICClient` 结构体只保存 `Arc<Config>` 与 `addr: String`，不保存 socket、Endpoint 或 Connection。
- `[源码确认]` downloader pool 的 `Entry`/`request_guard` 在 `download_piece()` 返回 `PieceContentStream` 时即结束保护；真正的 Piece body 此后由 stream 独立持有网络 reader。
- `[源码确认]` 因而所谓 client reuse 只是 wrapper reuse。每个 TCP Piece 都新建连接；每个 QUIC Piece都新建 Endpoint、Connection 和 bi stream。
- `[架构推断]` URMA 资源创建、注册和建链成本通常更高，不能照搬“每 Piece connect”；Context/JFC/Jetty/Segment 应至少跨一个 Piece 复用，具体生命周期见第 6 节原型边界。

### 3.3 Bytes 与落盘 buffer 生命周期

TCP：

```text
kernel socket buffer
  -> ReaderStream 内部 BytesMut
  -> split/freeze 得到 Bytes
  -> Vec<Bytes> write batch
  -> spawn_blocking 持有 batch
  -> IoSlice 借用 Bytes
  -> pwritev 返回
  -> clear batch，drop Bytes backing allocation
```

QUIC 从 quinn 的 `Chunk.bytes` 开始，后半段相同。

- `[源码确认]` `write_range_from_stream()` 同时最多存在“正在积累的 batch”和“一个 in-flight pwritev batch”；在 blocking write 返回前，in-flight `Bytes` 不能释放。源码：`storage/src/io.rs`，约 400–469 行。
- `[源码确认]` `Bytes` 被 move 到 batch，没有再拼成完整 Piece buffer；`IoSlice` 只借用 payload。
- `[架构推断]` URMA receive slot 的安全归还点不是 recv CQE，也不是 `PieceContentStream` yield，而是最后一个引用该 slot 的 `Bytes` 被 drop。若在 CQE 后立即 repost 同一 slot，Storage 尚未完成 CRC/pwritev 时可能被下一条消息覆盖。
- `[架构推断]` 最小 adapter 有两种实现边界：一是把注册 slot 复制到普通 `Bytes` 后立即 repost（简单但多一次 copy）；二是用自定义 owner 把注册 slot 包装成 `Bytes`，在 drop callback 中归还/repost（接近零额外 copy，但要求 owner 可 `Send + 'static` 且取消安全）。
- `[需要验证]` 目标 liburma/provider 是否允许 Segment buffer 跨 Tokio runtime 与 blocking thread 持有、注册内存能否安全封装为 `Bytes` owner，以及 deregister 前如何确认所有 DMA/WR 和 `Bytes` 引用均已结束。

## 4. Dragonfly 现有抽象层

### 4.1 已存在的 Downloader trait

`[源码确认]` `client/dragonfly-client/src/resource/piece_downloader.rs` 定义：

```rust
#[async_trait]
pub trait Downloader: Send + Sync {
    async fn download_piece(...)
        -> Result<(PieceContentStream, u64, String)>;
    async fn download_persistent_piece(...)
        -> Result<(PieceContentStream, u64, String)>;
    async fn download_persistent_cache_piece(...)
        -> Result<(PieceContentStream, u64, String)>;
}
```

TCPDownloader 与 QUICDownloader 都实现该 trait。`DownloaderFactory::new()` 当前只接受字符串 `"tcp"`、`"quic"`。

### 4.2 TCP 和 QUIC 共用与不共用的层

| 层次 | 是否共用 | 结论 |
|---|---:|---|
| `Downloader` 方法签名 | 是 | `[源码确认]` 三类 Piece 均返回统一 tuple。 |
| `PieceContentStream` | 是 | `[源码确认]` 都适配成 `BoxStream<Result<Bytes>>`。 |
| Vortex 业务消息 | 是 | `[源码确认]` request/response tag 与 metadata 结构相同。 |
| Storage 写入、CRC、metadata/notifier | 是 | `[源码确认]` 从 stream 起完全汇合。 |
| client 连接与发送实现 | 否 | `[源码确认]` `client/tcp.rs`、`client/quic.rs` 各自实现。 |
| Parent listener/handler/uploader | 否 | `[源码确认]` `server/tcp.rs`、`server/quic.rs` 各自实现，存在重复 dispatch。 |
| 统一 `Transport` trait | 否 | `[源码确认]` 在 Piece TCP/QUIC 数据面源码中不存在此抽象。 |
| endpoint 表达 | 部分 | `[源码确认]` `CollectedParent` 固定为 IP + TCP/QUIC port，没有 URMA endpoint/descriptor。 |

### 4.3 新增 UrmaDownloader 的最小切入点

最窄 Child 边界是：

```text
UrmaDownloader::download_piece(...)
  -> UrmaClient 请求远端 Piece
  -> 返回 (completion-driven PieceContentStream, offset, digest)
  -> 复用现有 Storage/CRC/metadata/notifier
```

需要修改的现有选择点只有：

1. `piece_downloader.rs`：增加 `UrmaDownloader` 和 factory 的 `"urma"` 分支；
2. `piece.rs`：持有/选择 URMA downloader，并取得 URMA endpoint；
3. `dfdaemon.rs` 配置：允许 `download.protocol = "urma"` 及最小 URMA 资源参数；
4. dfdaemon 启动路径：启动 Parent `UrmaServer`；
5. Parent endpoint 获取：原型期可静态配置/OOB，正式融入 `PieceCollector` 时再扩展 `SyncPiecesResponse` 与 `CollectedParent`。

`[架构推断]` 不建议为了第一版先抽象统一 `Transport`：它会同时牵动 listener、connection、request framing、file sender 和 endpoint schema，而 `Downloader + PieceContentStream` 已足以隔离 Child 后半段。若原型证明 URMA 可行，再从 TCP/QUIC/URMA 的共同实践中提取 Transport，风险更低。

## 5. Dragonfly 到 URMA 的映射表

> URMA 对象关系需要先纠正：`Context -> Jetty -> JFS/JFR` 不是所有模式下的固定创建链。`[源码确认]` `urma_perftest` 的 duplex 模式创建 Jetty（带 embedded/shared JFR），simplex 模式创建独立 JFS + JFR；二者是候选资源模型，不是必然层层嵌套。

| Dragonfly 对象/动作 | URMA 对应 | 标记 | 映射边界 |
|---|---|---|---|
| dfdaemon 进程中的网络运行时 | Device + Context | `[架构推断]` | Context 是资源归属域，不对应某一个 `TcpStream`；应进程级复用。 |
| TCP listener / QUIC Endpoint | 本地 Jetty（duplex）或 JFS+JFR（simplex）及其发布信息 | `[架构推断]` | URMA 没有可直接照搬的 socket `listen/accept` 语义；需 descriptor 发布/import/bind。 |
| `TcpStream` / QUIC `Connection` | 已导入并建立关系的 remote Jetty；或本地 JFS + imported remote JFR | `[架构推断]` | 这是“可提交事务的 peer session”对应，不是一根字节流。 |
| TCP `connect(addr)` / QUIC `connect()` | OOB 交换 EID、Jetty/JFR/Segment descriptor，import remote resource，必要时 bind/advise | `[源码确认]` + `[架构推断]` | API 步骤由 perftest 确认；Dragonfly 放置位置是接入推断。 |
| `TCPClient/QUICClient` wrapper | `UrmaClient` + `UrmaConnection` handle | `[架构推断]` | URMA client 必须持有真实长期资源，不能像当前 wrapper 只存 addr。 |
| Vortex `DownloadPiece(task_id, number)` | 小型 SEND WR 的应用 payload | `[架构推断]` | URMA opcode 不理解 task/piece；仍需保留业务字段和 request id。 |
| request `Bytes` | local registered Segment 中的一段 local SGE | `[架构推断]` | inline SEND 是否可免注册取决于消息大小和 provider 能力。 |
| `write_all(request)` | 构造 WR/SGE，`urma_post_jetty_send_wr()` 或 `urma_post_jfs_wr()` | `[源码确认]` + `[架构推断]` | post 成功只代表入队，不代表传输完成。 |
| Parent `read_header/read_download_piece` | recv CQE 后解析 receive slot 中的 request | `[架构推断]` | Parent 必须预先 `post_recv`/RQE。 |
| Vortex `PieceContent(offset,length,digest)` | 独立 metadata SEND，或第一条响应消息 | `[架构推断]` | CQE 不携带 Dragonfly metadata，不能省略此消息。 |
| Parent `RangeReader` | 读文件后填充的 registered staging Segment | `[架构推断]` | 当前证据不支持把 task file range 直接当 Segment。 |
| Parent Piece body write | 每 chunk 的 SEND WR + local SGE | `[架构推断]` | 需要 credit、chunk 序号/offset、EOF 与错误消息。 |
| kernel socket receive buffer / quinn reassembly buffer | Child 预注册的 receive Segment buffer slots | `[架构推断]` | JFR/Jetty receive queue 必须预投 RQE。 |
| `PieceContentStream<Item=Bytes>` | recv JFC poller 根据 CQE 取回 slot，按 request id/序号产生的有序异步 stream | `[架构推断]` | 一个 stream 通常对应多个 CQE；stream 绝不等于单个 CQE。 |
| `Bytes` chunk | registered Segment 的有效 slice（SGE 指向 buffer，CQE `completion_len` 给有效长度） | `[源码确认]` + `[架构推断]` | `completion_len`/`user_ctx` 由 perftest 确认；包装成 Rust `Bytes` 需实现 ownership。 |
| `ReaderStream::poll_next` / `RecvStream::read_chunk` | `urma_poll_jfc()` 或 event + poll 后唤醒 Tokio stream | `[架构推断]` | 需 completion poller、waker/channel 和错误传播。 |
| TCP/QUIC body EOF | 应用层 Piece 总长度/EOF 消息，且所有相关 recv CQE 成功 | `[架构推断]` | URMA completion 本身不提供“一个 Piece 结束”的通用语义。 |
| socket/stream error | post 返回值、`bad_wr`、CQE status、async event、应用超时 | `[源码确认]` + `[架构推断]` | 仍需转换为 `io::Error` 并触发 Dragonfly retry/parent 重选。 |
| `pwritev()` 完成 | 无 URMA 替代；继续由 Storage 完成 | `[源码确认]` | recv CQE 只表示通信接收完成，不表示文件写入完成。 |
| CRC/digest + metadata finished | 无 URMA 替代；继续由 Dragonfly 完成 | `[源码确认]` | Piece 业务完成晚于 URMA completion。 |
| downloader 并发 semaphore | WR 深度、receive credits 之外的 Piece 级并发控制 | `[架构推断]` | 两层限制应同时存在，不能把 queue depth 当 Piece 并发语义。 |

### 5.1 用已知 URMA 对象链表达一次 SEND/RECV chunk

duplex Jetty 路径：

```text
Context
  -> local Jetty + send/recv JFC
  -> import/bind remote Jetty
  -> receiver: registered Segment slot -> recv SGE -> recv WR/RQE
  -> receiver: urma_post_jetty_recv_wr()
  -> sender: registered Segment slice -> send SGE -> SEND WR
  -> sender: urma_post_jetty_send_wr()
  -> send JFC CQE + recv JFC CQE
  -> recv CQE(user_ctx, completion_len)
  -> PieceContentStream item
```

simplex 路径等价地使用：

```text
Context -> JFS + JFR + JFC
post: urma_post_jfs_wr() / urma_post_jfr_wr()
```

- `[源码确认]` SEND 的 WR 引用 local SGE；receive WR/RQE 引用预投的 local SGE；`user_ctx` 可在 CQE 中恢复，recv CQE 提供 `completion_len`。
- `[源码确认]` READ/WRITE 使用 local SGE 与 imported target Segment/remote SGE；普通 READ/WRITE 不要求对端 post recv。
- `[架构推断]` 第一版选 SEND/RECV 是因为它最接近 Dragonfly 当前的双边 request/response stream，并非证明其性能最优。
- `[需要验证]` 目标设备上 duplex/simplex、transport mode、最大 SGE/WR、ordering、RNR、CQ 深度与 event 模式的实际支持矩阵。

## 6. 第一版单 Peer Piece 传输原型

### 6.1 验证范围

只验证：一个 Parent、一个 Child、一个 standard Piece、单连接、低并发，Parent 从现有 task file 读取，Child 继续走现有 CRC + `pwritev` + metadata 完成链路。数据面采用 SEND/RECV；不引入 READ/WRITE，不替换 Dragonfly Storage，不设计多租户安全或 Scheduler 的最终 schema。

### 6.2 需要新增的最小模块

| 模块 | 最小职责 | 生命周期/边界 |
|---|---|---|
| `UrmaRuntime` | 初始化 liburma，选择 device/EID，创建 Context、send/recv JFC，运行 completion poller | dfdaemon 进程级；不能每 Piece 创建。 |
| `UrmaBufferPool` | 分配并注册 request/metadata/data Segment slots；维护 free/in-flight/stream-owned 状态 | runtime 级长期复用；slot 在 `Bytes` drop 后才可回收/repost。 |
| `UrmaConnection` | 保存 Parent/Child Jetty identity、imported target、bind 状态、request-id 空间和连接关闭状态 | Peer 对级，跨 Piece 复用。 |
| `UrmaControl/OOB` | 交换 EID、Jetty descriptor、token/权限和协议版本；完成 import/bind | 原型可用静态配置或 TCP/gRPC OOB；正式版本再接 `SyncPiecesResponse`。 |
| `UrmaClient` | 编码 `DownloadPiece`，post request SEND；关联 request id；接收 metadata/body CQE | 对上返回 offset、digest 和 stream。 |
| `CompletionStreamAdapter` | `CQE.user_ctx -> request/slot`；检查 status/length；排序并唤醒 `PieceContentStream`；处理取消 | JFC poller 与 Tokio 之间；这是 URMA 到现有 Storage 的核心桥。 |
| `UrmaServer` | 预投 request RQE；解析 task/piece；调用 `Storage::upload_piece()`；把 RangeReader 分块读入 registered slots 后 post SEND | Parent 常驻服务；第一版只处理 standard Piece。 |
| `UrmaDownloader` | 实现现有 `Downloader`，调用 `UrmaClient` 并返回 `(PieceContentStream, offset, digest)` | Dragonfly Child 最小集成点。 |
| 配置与启动 glue | `protocol=urma`、设备/EID/queue/buffer 参数；启动/停止 runtime/server；逆序 drain/销毁 | 修改 config、dfdaemon main、factory/selection。 |

若只做仓外 C/Rust 互通 demo，前四至七项仍然存在，只是 `UrmaDownloader` 和 Dragonfly glue 可以暂缓。若目标是“Dragonfly 中真实下载一个 Piece”，上述九项缺一不可。

### 6.3 最小运行时序

```text
启动阶段
  Parent/Child: init -> Context -> JFC -> Jetty -> registered buffer pool
  双方 OOB 交换 descriptor/token -> import/bind
  双方预投 request/response receive WR

下载阶段
  Child UrmaDownloader.download_piece(task_id, number)
  -> 分配 request id 和 request slot
  -> post SEND request
  -> Parent recv CQE，解析 request
  -> Parent Storage::upload_piece() 得到 RangeReader
  -> Parent SEND PieceContent metadata
  -> Parent 循环：file read -> registered slot -> post SEND chunk
  -> Child recv CQE -> completion adapter -> PieceContentStream<Bytes>
  -> 现有 write_range_from_stream() -> CRC -> pwritev
  -> digest compare -> metadata commit -> notifier
```

### 6.4 原型必须守住的正确性边界

1. `[架构推断]` request、metadata、body chunk 都带 request id；body 还应有序号或 offset，不能仅靠 CQE 到达顺序猜测 Piece 归属。
2. `[架构推断]` Parent 发送前 Child 必须有 receive credit；slot 只有在下游释放后才能 repost。
3. `[架构推断]` sender CQE、receiver CQE、stream 已交付、文件已写、digest 已验证、metadata 已提交是六个不同状态。
4. `[源码确认]` post 同步成功不等于完成；必须检查 post status/`bad_wr` 和 CQE status。
5. `[架构推断]` timeout/取消必须解除 request-id 映射并安全 drain outstanding WR，之后才允许解绑 Jetty、注销 Segment或释放 buffer。
6. `[源码确认]` Dragonfly 当前 expected Piece length来自 task/piece 层，offset/digest 来自 Parent `PieceContent` metadata；URMA adapter 必须保留这组契约。

### 6.5 第一版明确不做

- 不新增完整通用 `Transport` trait；
- 不用 URMA Segment 替代 task file 或 RocksDB metadata；
- 不做 READ/WRITE 数据路径；
- 不承诺零复制；第一版允许 Parent file-to-registered-buffer copy；
- 不把 URMA CQE 当作 Dragonfly Piece 完成；
- 不一次覆盖 persistent/persistent-cache、多 Parent、多 Piece 复用、生产级鉴权和 Scheduler schema。

## 7. 需要优先验证的问题

1. `[需要验证]` 目标环境可用的 device、EID index、duplex Jetty transport mode及 bind/import 流程。
2. `[需要验证]` receive queue 缺 credit 时的 RNR/重试/错误表现，以及 Tokio consumer 变慢时的背压上限。
3. `[需要验证]` 同一 Jetty 上多 WR/CQE 的顺序保证；跨 JFC/Jetty 是否必须由应用序号重排。
4. `[需要验证]` 最大消息/SGE、推荐 chunk、JFC 深度、completion moderation 对单 Piece latency 和吞吐的影响。
5. `[需要验证]` registered Segment 的内存 pin、NUMA、注册/注销成本和自定义 `Bytes` owner 的线程安全。
6. `[需要验证]` peer crash、timeout、CQ overflow、进程退出时的 error event、drain 和资源回收行为。
7. `[需要验证]` 与当前 TCP Linux `sendfile` 相比，Parent 多出的 file-to-Segment copy 是否抵消 URMA 数据面收益。

## 8. 最终判断

`[架构推断]` **接入可行，且 Child 侧已有足够窄的扩展边界**：`UrmaDownloader -> PieceContentStream -> 现有 Storage`。真正的工作量不在 trait 本身，而在 Parent URMA 服务、endpoint/OOB、completion 驱动的异步流和 registered buffer ownership。

`[架构推断]` 第一版不应把 URMA 资源按当前 TCP 的每 Piece connection 生命周期创建。合理的最小映射是：Context/JFC 为进程级，Jetty/连接为 Peer 对级，registered buffer pool 长期复用，WR/CQE 为 chunk/操作级，Dragonfly Piece 完成仍由现有长度、CRC、落盘和 metadata 状态机决定。
