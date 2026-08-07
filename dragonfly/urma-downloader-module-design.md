# Dragonfly UrmaDownloader Rust Module 与 Struct 设计

> [源码确认] Dragonfly 核对基线为根仓库 `2acbb8b6414939919cfc8474bf0ba4c38ae2c8ba`、`client` 子仓库 `017575a58d0abad6b3b274142fa470d47d8db327`；UMDK 核对基线为 `3bfff198329a497ed49e53bd5585c34bcb7c9d88`。
>
> [架构设计] 本文是第一版可实现的 Rust 代码结构设计，范围限定为 standard Piece、单 Parent/Child、duplex Jetty、SEND/RECV、copy 模式，并复用现有 `PieceContentStream + Storage`。
>
> [架构设计] 本文不设计 zero-copy、READ/WRITE、Scheduler endpoint 扩展和 multi-peer 连接池。

## 1. 设计结论

1. [源码确认] `Downloader` trait 位于 `client/dragonfly-client/src/resource/piece_downloader.rs`，而 TCP/QUIC 的低层 client、server 和 `PieceContentStream` 位于 `dragonfly-client-storage`。
2. [源码确认] `dragonfly-client` 依赖 `dragonfly-client-storage`；storage crate 不能反向依赖 `dragonfly-client`，否则会形成 Cargo 依赖环。
3. [架构设计] 因此代码分为两层：URMA FFI、Runtime、Connection、Buffer、Completion、Protocol 和 Parent Server 放在 `dragonfly-client-storage/src/urma/`；实现 `Downloader` trait 的 `UrmaDownloader` 放在 `dragonfly-client/src/resource/urma_downloader.rs`。
4. [架构设计] `UrmaDownloader` 必须是薄 adapter：它只发起 standard Piece request、等 Metadata，再返回 `(PieceContentStream, offset, digest)`；CQ polling、buffer 回收和 wire protocol 都不放入 downloader。
5. [架构设计] 一个专用 `CompletionPoller` I/O 线程独占 URMA Context/JFC/Jetty 原生 handle，并同时处理 post command 与 CQ polling；Tokio 任务不直接调用 raw URMA API。
6. [架构设计] CQE 到达后，poller 验证 status/长度，从 RX slot 拷贝为 `Bytes`，通过 `UrmaRequestState` 的 bounded `mpsc` 发给 `UrmaPieceStream`，最终 box 为现有 `PieceContentStream`。
7. [架构设计] 第一版用一个 request permit 硬性保证单 outstanding Piece；protocol 仍保留 `request_id` 和 `sequence`，避免以后扩展时重写 wire format。
8. [待验证] provider ABI/link 方式、Jetty 目标传输模式、post 后 WR/SGE 描述符可释放时机、CQ poll 线程模型和 RNR 行为必须在目标 UMDK/provider 上验证。

## 2. 放置位置与 crate 边界

### 2.1 现有目录事实

| 现有对象 | 位置 | 结论 |
|---|---|---|
| `Downloader` trait、TCPDownloader、QUICDownloader | `client/dragonfly-client/src/resource/piece_downloader.rs` | [源码确认] 这是 Piece 业务层与 transport adapter 的边界。 |
| `TCPClient` / `QUICClient` | `client/dragonfly-client-storage/src/client/` | [源码确认] 当前低层 Piece 传输 client 属于 storage crate。 |
| `TCPServer` / `QUICServer` | `client/dragonfly-client-storage/src/server/` | [源码确认] Parent 侧从 `Storage::upload_piece()` 获得 RangeReader 并发送 Piece。 |
| `PieceContentStream` | `client/dragonfly-client-storage/src/client/mod.rs:26` | [源码确认] 统一类型是 `BoxStream<'static, io::Result<Bytes>>`。 |
| dfdaemon server 组装 | `client/dragonfly-client/src/bin/dfdaemon/main.rs` | [源码确认] main 创建 Storage/TCP/QUIC server、Task，spawn server 并统一处理 shutdown。 |

### 2.2 确定的目录树

```text
client/
  dragonfly-client-storage/
    build.rs                         # feature=urma 时生成/链接 FFI
    Cargo.toml
    src/
      lib.rs                         # pub mod urma
      urma/
        mod.rs                       # 只导出稳定上层 API
        ffi.rs                       # raw bindings + RAII handle，唯一 unsafe 集中点
        protocol.rs                  # Request/Metadata/Data/End/Error codec
        buffer.rs                    # registered region、RX/TX slot 与 lease
        request.rs                   # UrmaRequestState、UrmaPieceStream
        connection.rs                # OOB handshake、logical connection、send command
        completion.rs                # CompletionPoller、CQE route、repost
        runtime.rs                   # Runtime 创建、I/O thread、shutdown
        server.rs                    # Parent request loop + Storage RangeReader sender

  dragonfly-client/
    Cargo.toml
    src/
      resource/
        mod.rs
        piece_downloader.rs          # 保留 trait 和 TCP/QUIC，不塞 URMA 底层逻辑
        urma_downloader.rs           # UrmaDownloader implements Downloader
        piece.rs                     # 增加 optional URMA downloader 和 standard Piece 分支
      bin/dfdaemon/main.rs            # Runtime/Connection/Server 组装
```

- [架构设计] `request.rs` 是在示例 module tree 上的必要增补：RequestState 是 CQE 线程与 async stream 之间的共享状态，不应隐藏在 `downloader.rs` 或 `completion.rs` 中。
- [架构设计] storage crate 中不新增名为 `downloader.rs` 的模块；其 client-side 发送能力就是 `UrmaConnection`，真正的 Dragonfly Downloader adapter 必须位于能看到 `Downloader` trait 的 `dragonfly-client` crate。

### 2.3 feature 和平台边界

- [架构设计] `dragonfly-client-storage` 增加 `urma` Cargo feature，URMA module 使用 `cfg(all(target_os = "linux", feature = "urma"))`。
- [架构设计] `dragonfly-client` 增加透传 feature `urma = ["dragonfly-client-storage/urma"]`；非 Linux 或未开 feature 的日常 TCP/QUIC build 不引入 UMDK header/library。
- [架构设计] `build.rs` 只在 `CARGO_FEATURE_URMA` 存在时处理 bindings 和 `liburma` link directive；不允许普通 build 因未安装 UMDK 失败。
- [架构设计] raw bindgen 类型不从 `urma::mod.rs` 公开导出；对外只暴露 Runtime、Connection、Server、Endpoint、Metadata 和错误类型。
- [待验证] 目标环境中 UMDK header 安装路径、`liburma.so` soname、rpath/loader 配置以及是使用安装头文件还是固定 commit 的预生成 bindings，需在第 1 个实现 commit 确定。

## 3. 模块职责与允许依赖

| 模块 | 职责 | 允许依赖 |
|---|---|---|
| `ffi` | [架构设计] 封装 `urma_init/context/JFC/Jetty/Segment/post/poll` 和逆序销毁；将 null/status 转为 `UrmaError`。 | [架构设计] 只依赖 bindings/libc，不依赖 Tokio、Storage 或 Piece。 |
| `protocol` | [架构设计] 定长 header 和五类 message 的 encode/decode/validate。 | [架构设计] 只依赖 bytes/标准库，不依赖 FFI。 |
| `buffer` | [架构设计] 已注册 region 的 slot 切分、TX lease、RX state、SGE address/length 查询。 | [架构设计] 依赖 `ffi`，但不解析 Piece message。 |
| `request` | [架构设计] Piece request 状态机、metadata oneshot、body mpsc、stream Drop/失败/结束。 | [架构设计] 依赖 `protocol`，不调 raw FFI。 |
| `connection` | [架构设计] OOB 交换、connection id/generation、request registry、单 Piece permit、向 I/O thread 发 post/close command。 | [架构设计] 依赖 `runtime/request/protocol`，不 poll CQ。 |
| `completion` | [架构设计] 拥有 URMA native handles，处理 command、poll CQE、路由 Request/response、回收 TX/repost RX。 | [架构设计] 依赖 `ffi/buffer/protocol/request`，不执行 Storage I/O。 |
| `runtime` | [架构设计] 创建 poller thread，提供 command sender，管理 Ready/Draining/Stopped 和 join。 | [架构设计] 组装 `completion/buffer/ffi`，不解析 Task/Piece。 |
| `server` | [架构设计] OOB accept，接收 Request event，调 Storage、读 RangeReader、发 Metadata/Data/End/Error。 | [架构设计] 依赖 `Storage + UrmaRuntime/Connection`，不自己 poll CQ。 |
| `resource/urma_downloader` | [架构设计] 实现 Dragonfly `Downloader`，将 `PendingUrmaPiece` 转为现有 tuple。 | [架构设计] 只依赖 storage crate 公开 URMA API 和 `Downloader` trait。 |

## 4. 支撑类型与所有权约定

### 4.1 公开标识与配置类型

```rust
pub struct ConnectionId(u32);
pub struct ConnectionGeneration(u32);
pub struct RequestId(u64);
pub struct SlotId(u32);

pub struct UrmaEndpoint {
    pub oob_addr: SocketAddr,
    pub device_name: String,
    pub eid_index: u32,
}

pub struct UrmaRuntimeConfig {
    pub device_name: String,
    pub eid_index: u32,
    pub jfc_depth: u32,
    pub rx_slot_count: u32,
    pub tx_slot_count: u32,
    pub slot_size: usize,
    pub poll_batch: usize,
}
```

- [架构设计] 上述是设计级字段，ID newtype 防止 request/slot/connection 索引在 `user_ctx` 编解码时混用。
- [架构设计] `slot_size` 包含 32-byte protocol header，Data 最大 payload 为 `slot_size - HEADER_LEN`。
- [架构设计] 第一版 `rx_slot_count` 同时是对端可使用的最大 receive credit；`tx_slot_count` 是本地最大 outstanding SEND 数的内存上限。
- [待验证] 这些值必须不超过 `urma_query_device()` 返回的 JFC/JFS/JFR 能力，并与 provider 要求的页、cache line 和 Segment 对齐一致。

| 原型起始值 | 建议 |
|---|---|
| `slot_size` | [架构设计] 从 512 KiB 开始，与当前 Storage 默认 read/write buffer 数量级一致，且 4–64 MiB Piece 会必然覆盖多 Data message。 |
| `rx_slot_count` | [架构设计] 从 4 开始，提供小的 receive window 并便于观察背压。 |
| `tx_slot_count` | [架构设计] 第一版从 1 开始，使 Parent 每次只有一条 SEND 未回收，优先验证正确性。 |
| `body_channel_capacity` | [架构设计] 从 4 开始，不超过 RX slot count。 |
| `jfc_depth/poll_batch` | [待验证] 先按“不小于对应 outstanding WR 上限”计算，最终值必须经 device capability query 接受。 |

### 4.2 FFI RAII handle

```rust
struct UrmaContextHandle { raw: NonNull<urma_context_t> }
struct UrmaJfcHandle { raw: NonNull<urma_jfc_t> }
struct UrmaJettyHandle { raw: NonNull<urma_jetty_t> }
struct UrmaTargetJettyHandle { raw: NonNull<urma_target_jetty_t> }
struct UrmaRegisteredSegment { raw: NonNull<urma_target_seg_t> }
```

- [源码确认] UMDK perftest 的依赖顺序是 Context -> JFC -> Jetty/remote target -> registered Segment/WR，销毁前还需转 ERROR 并 drain outstanding WR。
- [架构设计] 不依赖 Rust 字段自动 Drop 顺序来完成正常 shutdown；`CompletionPoller::shutdown()` 显式按 drain -> unbind/unimport -> delete Jetty -> delete JFC -> unregister Segment -> delete Context -> `urma_uninit` 顺序执行。
- [架构设计] handle 的 Drop 只作为初始化半途失败时的保险网，不得在尚有 outstanding WR 时直接销毁底层资源。
- [待验证] 这些 handle 不在第一版实现无条件 `Send/Sync`；是否允许跨线程移动和并发调用必须由 URMA API/provider 契约确认。

## 5. 核心 Struct 设计

### 5.1 `UrmaRuntime`

```rust
#[derive(Clone)]
pub struct UrmaRuntime {
    inner: Arc<UrmaRuntimeInner>,
}

struct UrmaRuntimeInner {
    config: UrmaRuntimeConfig,
    command_tx: mpsc::Sender<IoCommand>,
    buffer_pool: Arc<UrmaBufferPool>,
    state: AtomicU8,
    next_connection_id: AtomicU32,
    io_thread: Mutex<Option<std::thread::JoinHandle<UrmaResult<()>>>>,
}
```

```rust
pub struct UrmaRuntimeParts {
    runtime: UrmaRuntime,
    inbound_requests: mpsc::Receiver<InboundPieceRequest>,
}
```

| 字段 | 作用 |
|---|---|
| `config` | [架构设计] 保存已经校验的 device/queue/slot 参数，启动后不可变。 |
| `command_tx` | [架构设计] Tokio/Server/Connection 通过有界 Tokio mpsc command queue 要求 connect、post send、close、shutdown；async sender 在 queue full 时等待，不阻塞 Tokio worker，也不暴露 native handle。 |
| `buffer_pool` | [架构设计] Runtime 生命周期的注册内存，由 connection/server 租用 TX slot，由 poller 管理 RX slot。 |
| `state` | [架构设计] `Starting -> Ready -> Draining -> Stopped/Failed`，阻止 shutdown 后继续 post。 |
| `next_connection_id` | [架构设计] 为逻辑 connection 分配本地 ID，第一版最多一个 active connection。 |
| `io_thread` | [架构设计] 保存 poller 线程 JoinHandle；dfdaemon shutdown 时必须显式 join。 |

- [架构设计] `UrmaRuntime::start()` 先创建 inbound request channel，再创建线程并在线程内完成 `urma_init/context/JFC/Segment`，只在 Ready oneshot 成功后返回 `UrmaRuntimeParts`。Parent 将整个 parts 交给 `UrmaServer::new()`；Child 调 `parts.into_client_runtime()` 取出 Runtime 并关闭未使用的 inbound receiver。
- [架构设计] native Context/JFC/Jetty handle 不是 `UrmaRuntimeInner` 字段，而是 I/O 线程栈上 `CompletionPoller` 的字段，从结构上防止 Tokio 线程误用 raw API。
- [架构设计] Runtime 是 dfdaemon 进程级资源，不随 Piece 创建或 Drop。

### 5.2 `UrmaConnection`

```rust
#[derive(Clone)]
pub struct UrmaConnection {
    inner: Arc<UrmaConnectionInner>,
}

struct UrmaConnectionInner {
    runtime: UrmaRuntime,
    id: ConnectionId,
    generation: ConnectionGeneration,
    peer: UrmaEndpoint,
    state: AtomicU8,
    next_request_id: AtomicU64,
    single_request: Arc<Semaphore>,
    requests: DashMap<RequestId, Arc<UrmaRequestState>>,
}
```

| 字段 | 作用 |
|---|---|
| `runtime` | [架构设计] 保证 command channel、buffer pool 和 poller 活得比 connection 长。 |
| `id + generation` | [架构设计] 作为 CQE `user_ctx` 和 command 的本地路由键，防止 close/重建后的迟到 CQE 命中新 connection。 |
| `peer` | [架构设计] 保存 OOB 地址与远端配置身份；Jetty descriptor/import handle 只由 poller 线程持有。 |
| `state` | [架构设计] `Connecting -> Ready -> Draining -> Closed/Failed`；不在 Ready 时拒绝新 request。 |
| `next_request_id` | [架构设计] 单调分配非 0 request ID；同一 generation 不重用。 |
| `single_request` | [架构设计] 容量为 1 的 Tokio semaphore，实现本阶段单 outstanding Piece 约束。 |
| `requests` | [架构设计] CQE 路由表；虽然第一版只有一项，仍用 request ID 而不是全局 mutable current request。 |

- [架构设计] `start_piece(task_id, number)` 获取 `single_request` permit，创建 RequestState 并先插入 registry，然后编码/post Request；“先注册后 post”避免极快 response 先于路由表到达。
- [架构设计] `start_piece()` 返回 `PendingUrmaPiece { metadata_rx, stream }`；Downloader 等 metadata，stream 可已经在 channel 中缓存早到 Data。
- [架构设计] post Request 同步失败时必须从 registry 移除 state、失败 metadata/body、释放 permit 和 TX slot。
- [待验证] OOB 交换后是否必须 `urma_bind_jetty()`、需交换哪些 token/transport 属性，取决于原型选定的可靠传输模式。

### 5.3 `UrmaBufferPool`

```rust
pub struct UrmaBufferPool {
    memory: RegisteredMemory,
    slot_size: usize,
    rx_slots: Box<[RxSlotMeta]>,
    tx_slots: Box<[TxSlotMeta]>,
    free_tx: Mutex<VecDeque<SlotId>>,
    free_tx_permits: Semaphore,
}

struct RegisteredMemory {
    base: NonNull<u8>,
    len: usize,
    layout: Layout,
}

struct RxSlotMeta { state: AtomicU8, generation: AtomicU32 }
struct TxSlotMeta { state: AtomicU8, generation: AtomicU32 }
```

| 字段 | 作用 |
|---|---|
| `memory` | [架构设计] 一次对齐分配的连续 memory，前半切 RX slots，后半切 TX slots；对应 `UrmaRegisteredSegment` handle 由 poller 线程独占。 |
| `slot_size` | [架构设计] 所有 receive WR 的 SGE len 与发送 message 的最大物理长度。 |
| `rx_slots` | [架构设计] 记录 `Free -> Posted -> Completed -> DeliverPending -> Posted`；只由 poller 改变主状态。 |
| `tx_slots` | [架构设计] 记录 `Free -> Leased -> Posted -> Free`；TX lease Drop 在未 post 时回滚，post 后由 send CQE 回收。 |
| `free_tx + permits` | [架构设计] async 获取 TX slot；CQE 线程将 slot ID 推回短临界区队列并 add permit。 |

- [架构设计] `TxSlotLease` 提供唯一 mutable slice 用于 encode/read file；调用 `commit(len, user_ctx)` 后它不再允许 Rust 侧访问 payload，直到 send CQE。
- [架构设计] RX slot 不包装成 `Bytes`；poller 按 `completion_len` 从 slot slice 拷贝出新 `Bytes`，因此 Storage 寿命与 registered region 解耦。
- [架构设计] `RegisteredMemory` 只表示稳定地址内存；I/O 线程在 Runtime 启动时用它创建 `UrmaRegisteredSegment`，shutdown 时先 unregister Segment，join poller 后才允许 pool 释放 memory。
- [架构设计] BufferPool 内部需要一个经审核的 unsafe `Send/Sync` 封装；安全性不来自 pointer 自身，而来自 RX/TX slot 状态机保证任一时刻只有 CPU 或 device/唯一 lease 访问对应 slot。
- [架构设计] `user_ctx` 不存 raw pointer，而是可验证的紧凑 ID；解码后必须同时校验 slot generation 和 connection generation。
- [待验证] `user_ctx` 64 bit 中 connection/generation/direction/slot 的位宽必须结合最终 queue 上限确定；第一版也可将 `user_ctx` 作为 slab index，避免过早固化 bit layout。

### 5.4 `UrmaRequestState`

```rust
pub(crate) struct UrmaRequestState {
    request_id: RequestId,
    connection_generation: ConnectionGeneration,
    progress: Mutex<RequestProgress>,
    metadata_tx: Mutex<Option<oneshot::Sender<UrmaResult<PieceMetadata>>>>,
    body_tx: Mutex<Option<mpsc::Sender<io::Result<Bytes>>>>,
    request_permit: Mutex<Option<OwnedSemaphorePermit>>,
}

struct RequestProgress {
    phase: RequestPhase,
    next_sequence: u32,
    expected_length: Option<u64>,
    received_length: u64,
    data_messages: u32,
}
```

| 字段 | 作用 |
|---|---|
| `request_id + connection_generation` | [架构设计] 同时校验 wire request 和本地 connection 世代。 |
| `progress` | [架构设计] 单个短临界区保护 `WaitingMetadata -> Streaming -> Ended/Failed/Draining`、sequence 和 length 不变式。 |
| `metadata_tx` | [架构设计] Metadata 或 Metadata 前 Error 只完成一次；Downloader 持有 receiver。 |
| `body_tx` | [架构设计] bounded Tokio mpsc sender；poller 使用 `try_send` 而不是 blocking/await send。 |
| `request_permit` | [架构设计] 从 request 建立保留到 End/Error/连接失败，保证 transport 上一次只有一个 Piece。 |

- [架构设计] Metadata 到达时设置 `expected_length`、完成 oneshot并进入 Streaming；Data 必须在 Metadata 之后，sequence 严格递增，累计长度不得超过 expected length。
- [架构设计] End 必须匹配 total length 和 chunk count，然后 take/drop `body_tx` 使 stream 在已排队 Bytes 消费完后返回 EOF。
- [架构设计] Error/CQE 失败在 metadata 前完成 metadata error；在 Streaming 阶段则向 body channel 尝试投递一个 `io::Error`，然后关闭 sender。
- [架构设计] `UrmaPieceStream` 持有 body receiver 和 `Arc<UrmaRequestState>`，实现 `Stream<Item = io::Result<Bytes>>`；box 后即为现有 `PieceContentStream`。
- [架构设计] stream 在 End 前 Drop 时将 state 置为 Draining；由于最小 protocol 不含 Cancel，poller 继续消费并丢弃该 request 的 Data，直到 End/Error 才移除 registry 和释放 permit。

### 5.5 `UrmaDownloader`

```rust
pub struct UrmaDownloader {
    connection: UrmaConnection,
    metadata_timeout: Duration,
}
```

| 字段 | 作用 |
|---|---|
| `connection` | [架构设计] dfdaemon main 预先建立并注入的单 Parent connection；不在每个 Piece 中建 Jetty。 |
| `metadata_timeout` | [架构设计] 覆盖 Request post 到 Metadata/Error 的等待；body 继续由 Storage `write_piece_timeout` 限制。 |

- [源码确认] `Downloader::download_piece()` 签名为 `(addr, number, host_id, task_id) -> (PieceContentStream, offset, digest)`；现有 TCP/QUIC implementation 也不使用 `host_id`。
- [架构设计] URMA implementation 使用注入的静态 connection；`addr` 在第一版只用于校验/记录“选中的逻辑 Parent”，真实 OOB/URMA endpoint 来自 URMA 配置。
- [架构设计] `download_piece()` 调 `connection.start_piece()`，对 metadata receiver 做 timeout，成功后返回 `(stream, metadata.offset, metadata.digest)`。
- [架构设计] `download_persistent_piece()` 和 `download_persistent_cache_piece()` 在本阶段明确返回 `DFError::Unimplemented`，不暗中 fallback 到 TCP。
- [架构设计] `UrmaDownloader` 不通过现有 `DownloaderFactory::new(protocol, config)` 创建；该 factory 是同步且只有 config，无法正确注入已建立的 async Runtime/Connection。

### 5.6 `UrmaServer`

```rust
pub struct UrmaServer {
    runtime: UrmaRuntime,
    storage: Arc<Storage>,
    upload_bandwidth_limiter: Arc<RateLimiter>,
    listen_addr: SocketAddr,
    request_rx: mpsc::Receiver<InboundPieceRequest>,
    shutdown: Shutdown,
    shutdown_complete_tx: mpsc::UnboundedSender<()>,
}
```

| 字段 | 作用 |
|---|---|
| `runtime` | [架构设计] accept OOB 后在同一 Runtime 上创建单个 Parent-side connection，发 response message。 |
| `storage` | [架构设计] 复用 `piece_id()` 和 `upload_piece()`，不绕过 Piece 完成等待、metadata 和 RangeReader。 |
| `upload_bandwidth_limiter` | [架构设计] 与 TCP/QUIC Parent handler 保持同类上传限速边界。 |
| `listen_addr` | [架构设计] 只用于 OOB bootstrap listener，Piece Request/Metadata/Data 不经该 TCP stream。 |
| `request_rx` | [架构设计] poller 解码到 Request message 时投递的 inbound queue；第一版 server loop 串行处理。 |
| `shutdown*` | [架构设计] 接入 dfdaemon 现有 shutdown barrier，先停 accept/request，再要求 Runtime drain。 |

- [架构设计] Request handler 从 `task_id + piece_number` 生成 piece ID，调 `Storage::upload_piece()` 得到 RangeReader 和 metadata，先发 Metadata，再反复 acquire TX slot -> `read` -> encode Data -> post SEND，最后发 End。
- [架构设计] server 不显式等每个 send CQE；TX pool 只在 CQE 后归还 slot，因此下一次 acquire 自然形成有界 send window。
- [架构设计] `upload_piece()` 或 RangeReader 读失败时发 Error；若 transport 已不可用，将 connection 转 Failed 并依靠 drain 回收 slot，不伪造 End。

### 5.7 `CompletionPoller`

```rust
struct CompletionPoller {
    context: UrmaContextHandle,
    send_jfc: UrmaJfcHandle,
    recv_jfc: UrmaJfcHandle,
    local_segment: UrmaRegisteredSegment,
    buffer_pool: Arc<UrmaBufferPool>,
    command_rx: mpsc::Receiver<IoCommand>,
    connections: HashMap<ConnectionId, PollerConnection>,
    pending_deliveries: VecDeque<PendingDelivery>,
    server_request_tx: mpsc::Sender<InboundPieceRequest>,
    state: PollerState,
}

struct PollerConnection {
    generation: ConnectionGeneration,
    jetty: UrmaJettyHandle,
    remote_jetty: UrmaTargetJettyHandle,
    logical: Weak<UrmaConnectionInner>,
}
```

- [架构设计] poller 循环每轮按顺序执行：处理有上限的 commands -> poll send JFC batch -> poll recv JFC batch -> retry pending deliveries -> 执行 idle backoff/检查 shutdown。
- [架构设计] send CQE 成功时根据 `user_ctx` 回收 TX slot；失败时先将 connection 置 Failed，再失败所有 RequestState，最后进入 drain。
- [架构设计] recv CQE 必须验证 status、`completion_len >= HEADER_LEN`、`completion_len <= slot_size`、header 声明长度等于 CQE 长度，然后才解码 payload。
- [架构设计] Request message 投递到 `server_request_tx`；Metadata/Data/End/Error 按 connection generation + request_id 路由到 `UrmaRequestState`。
- [架构设计] Data 复制成 Bytes 后对 per-request bounded channel 调 `try_send`；若 channel full，将 `(slot_id, Bytes, state)` 放入 `pending_deliveries`，**暂不 repost 该 RX slot**，以 RX slot 作为 receive credit 形成有界背压。
- [架构设计] pending Bytes 已与 slot memory 解耦，但 slot 仍被故意扣留；当 `try_send` 成功或 request 进入 Draining 丢弃该 Bytes 后，才 repost receive WR。
- [待验证] command batch、CQ poll batch、idle spin/yield/sleep 策略需根据真实 CPU 占用与延迟调整。

## 6. 最小 URMA Piece Message Protocol

### 6.1 传输约定

- [架构设计] 每个 URMA SEND 只携带一条完整 message，一条 message 不跨 BufferSlot；message 物理长度为 `32 + payload_len`。
- [架构设计] 所有整数使用 big-endian，字符串使用 UTF-8；decoder 先检查上限再分配。
- [架构设计] 第一版 server -> child 顺序固定为 `Metadata(seq=0) -> Data(seq=1..N) -> End(seq=N+1)`，任何阶段可以 Error 终止。
- [架构设计] child -> parent Request 使用 `seq=0`；该最小 protocol 没有 ACK 和 Cancel，send CQE 不代表 Child Storage 已完成。

### 6.2 公共 header（32 bytes）

| offset | 长度 | 字段 | 规则 |
|---:|---:|---|---|
| 0 | 4 | `magic` | [架构设计] ASCII `DFUR`，不匹配则协议错误。 |
| 4 | 2 | `version` | [架构设计] 第一版为 1，不支持的版本直接 Error/关连接。 |
| 6 | 1 | `message_type` | [架构设计] 1=Request, 2=Metadata, 3=Data, 4=End, 5=Error。 |
| 7 | 1 | `flags` | [架构设计] 第一版必须为 0，非 0 按 unsupported 处理。 |
| 8 | 2 | `header_len` | [架构设计] 固定为 32，为后续扩展保留显式边界。 |
| 10 | 2 | `reserved0` | [架构设计] encode 为 0，decode 必须为 0。 |
| 12 | 8 | `request_id` | [架构设计] 非 0，一个 Piece 的全部 response 与 Request 相同。 |
| 20 | 4 | `sequence` | [架构设计] 按上述方向内顺序校验。 |
| 24 | 4 | `payload_len` | [架构设计] 必须 `<= slot_size - 32`，且 `32 + payload_len == completion_len`。 |
| 28 | 4 | `reserved1` | [架构设计] encode/decode 规则同 `reserved0`。 |

### 6.3 Request

```text
u32 piece_number
u16 task_id_len
u16 reserved
u8  task_id[task_id_len]
```

- [源码确认] 当前 Vortex standard Piece request 的业务定位信息就是 `task_id + piece_number`；`host_id` 未被 TCP/QUIC downloader 使用。
- [架构设计] `task_id_len` 必须大于 0，且整条 Request 不超过 slot payload；Parent 在生成 piece ID 前验证 UTF-8。

### 6.4 Metadata

```text
u64 offset
u64 piece_length
u16 digest_len
u16 reserved
u8  digest[digest_len]
```

- [源码确认] `Downloader` 返回给现有 Piece/Storage 链路的 metadata 只需 `offset + digest`；Storage 上层已持有调度给出的 expected Piece length。
- [架构设计] protocol 仍传 `piece_length`，RequestState 用它防止 Data 超长并校验 End；Storage 仍作为最终 length/digest 判定者。
- [架构设计] digest 是现有 Dragonfly metadata 中的 UTF-8 字符串表示，不在 URMA 协议层重新定义 hash algorithm。

### 6.5 Data

```text
u8 data[payload_len]
```

- [架构设计] Data 不再携带 offset；RequestState 依赖严格 sequence 和累计长度把 payload 适配为有序 stream。
- [架构设计] 最后一个 Data 可小于最大 payload，不允许零长 Data。

### 6.6 End

```text
u64 total_length
u32 data_messages
u32 reserved
```

- [架构设计] End 本身不携带 Piece bytes；`total_length` 必须等于 Metadata length 和 RequestState 累计长度，`data_messages` 必须等于实际 Data 数。
- [架构设计] End 仅表示 Parent 已完成该 request 的 message 发送；Child Piece 只有在 Storage 写入、CRC32/digest 校验和 metadata commit 后才成功。

### 6.7 Error

```text
u32 error_code
u16 message_len
u16 reserved
u8  message[message_len]
```

| code | 含义 |
|---:|---|
| 1 | [架构设计] invalid request/protocol |
| 2 | [架构设计] task or piece not found |
| 3 | [架构设计] parent storage read failed |
| 4 | [架构设计] parent busy/resource exhausted |
| 255 | [架构设计] internal error |

- [架构设计] Error payload message 是有上限的诊断文本，不序列化 Rust/UMDK 内部结构，不传递敏感 token/descriptor。
- [架构设计] transport CQE failure 无法保证 Error message 仍能发送，这类错误由本地 connection 失败直接终止 RequestState。

### 6.8 OOB handshake 与 Piece protocol 的边界

- [架构设计] OOB TCP 只交换 protocol version、EID/device identity、Jetty descriptor、transport mode 和必要 token/authorization descriptor，然后执行 import/bind/Ready barrier。
- [架构设计] Request/Metadata/Data/End/Error 全部通过 URMA SEND/RECV；不允许用 OOB TCP 传 Piece body，否则不能验证本阶段目标。
- [待验证] perftest 的 management exchange 可作为原型参考，但其固定 token 和 benchmark barrier 不是生产认证/授权协议。

## 7. CQE 到 `PieceContentStream` 的精确路径

```text
device writes RX BufferSlot
  -> recv CQE { status, user_ctx, completion_len }
  -> CompletionPoller::poll_recv()
  -> decode user_ctx => connection/generation/slot
  -> validate CQE + protocol header
  -> copy Data payload => Bytes
  -> connection.requests[request_id]
  -> UrmaRequestState::on_data(sequence, Bytes)
  -> body_tx.try_send(Ok(Bytes))
  -> UrmaPieceStream::poll_next()
  -> BoxStream<'static, io::Result<Bytes>>
  -> PieceContentStream
  -> Storage::download_piece_from_parent_finished()
```

### 7.1 Metadata 路径

```text
recv Metadata CQE
  -> CompletionPoller validates type/seq/length
  -> UrmaRequestState::on_metadata()
  -> metadata_tx.send(PieceMetadata)
  -> UrmaDownloader::download_piece() wakes
  -> returns (PieceContentStream, offset, digest)
```

- [架构设计] Metadata 和 body 使用两个不同通道：oneshot 确定 Downloader 返回时机，bounded mpsc 承载后续 stream items。
- [架构设计] Data 若紧跟 Metadata 到达，body mpsc 可在 Downloader 返回 stream 前缓存它；不丢数据，也不要求 Storage 先 poll。

### 7.2 bounded backpressure

- [架构设计] body channel capacity 建议等于或小于 RX slot count，但 `pending_deliveries + channel items` 的最大数必须受 RX slot count 约束。
- [架构设计] channel full 时不能在 poller OS 线程调 `blocking_send`，也不能无限复制到另一个 unbounded queue。
- [架构设计] 扣留已完成 RX slot 不 repost，会逐步消耗对端可用 RQE；Parent TX 最终因 receive credit/RNR 而受限，不会让 Child 内存无界增长。
- [待验证] 达到 RQ 耗尽时目标 transport 的 RNR retry、send CQE 状态和超时行为需实测；如果不能满足稳定背压，第二版需增加显式 credit message。

### 7.3 End、Error 和 Drop

- [架构设计] End 完成后 RequestState 先关 body sender，再从 connection registry 移除；stream 持有的 Arc 使 state 活到队列排空。
- [架构设计] Error 转成 downloader-level error 或 stream `io::Error`，不伪装成 EOF；EOF 只能由通过长度校验的 End 产生。
- [架构设计] Storage timeout/drop stream 时，RequestState 进入 Draining 并停止向 channel 投递；poller 仍回收/repost RX slots，直至 End/Error，避免后续 message 污染下一个 Piece。
- [待验证] 如果 Parent 故障后永远不发 End/Error，Draining 必须有 connection-level timeout，超时后将 Jetty 转 ERROR 并 drain CQ；具体 timeout 需与 Storage timeout 协调。

## 8. dfdaemon 中的创建与关闭顺序

### 8.1 Child

```text
load/validate Config
  -> UrmaRuntime::start()
  -> UrmaConnection::connect(parent_oob_endpoint)
  -> UrmaDownloader::new(connection)
  -> Task::new(..., Some(Arc<UrmaDownloader>))
  -> Piece::new(..., optional urma_downloader)
  -> standard Piece download
```

- [架构设计] connection 建立是 async，因此必须在 dfdaemon main 中、`Task::new()` 之前完成；不将 async 建链隐藏在当前同步 `DownloaderFactory::new()` 内。
- [架构设计] 当 `download.protocol != "urma"` 或 Cargo feature 未开启时，`Task::new()` 收到 `None`，TCP/QUIC 路径不变。

### 8.2 Parent

```text
load/validate Config
  -> UrmaRuntime::start()
  -> UrmaServer::new(runtime, storage, limiter, oob_listen)
  -> UrmaServer::run()
       -> accept one OOB connection
       -> import/bind Jetty + prepost RX + Ready barrier
       -> process one inbound Piece request at a time
```

- [架构设计] 原型配置显式区分 `parent` 和 `child` role，避免第一版在同一 Runtime 上处理同时 incoming/outgoing 多 connection。
- [架构设计] 这是原型启动约束，不是最终 Dragonfly Peer 角色模型；当前任务明确不设计 multi-peer/生产集成。
- [架构设计] Parent 进程的 `download.protocol` 可继续为 TCP 或用预置内容，同时 `urma.enabled=true, role=parent` 对 Child 提供 URMA server；只有 Child 需要 `download.protocol="urma"`。
- [架构设计] 测试环境必须保证 Scheduler/静态候选中唯一 logical Parent 就是 `parentOobAddr` 对应的 Peer；否则 Piece 实际数据源与记录的 parent ID 可能不一致。

### 8.3 shutdown

```text
global shutdown signal
  -> stop accepting OOB and new Piece requests
  -> UrmaConnection state = Draining
  -> fail/wait active RequestState
  -> IoCommand::Shutdown
  -> jetty ERROR + drain send/recv CQ
  -> destroy/import/register/context in reverse order
  -> join CompletionPoller thread
  -> notify shutdown_complete
```

- [架构设计] main 必须显式 await `UrmaRuntime::shutdown()`，不只 abort `UrmaServer` Tokio task 或等 Arc Drop。
- [源码确认] perftest 清理路径会先转 ERROR/drain，再 disconnect/unimport、delete Jetty/JFC、unregister memory 和 uninit；Runtime shutdown 需保留这个依赖顺序。

## 9. 需要修改或新增的 Dragonfly 文件

### 9.1 `dragonfly-client-storage`

| 文件 | 修改 |
|---|---|
| `client/dragonfly-client-storage/Cargo.toml` | [架构设计] 增加 `urma` feature、Linux-only FFI/build dependencies。 |
| `client/dragonfly-client-storage/build.rs` | [架构设计] feature 开启时查找 UMDK headers/lib，生成 bindings 并输出 link directive。 |
| `client/dragonfly-client-storage/src/lib.rs` | [架构设计] 条件导出 `pub mod urma`。 |
| `src/urma/mod.rs` | [架构设计] 组织可见性，只 re-export Runtime/Connection/Server/Endpoint/config/error。 |
| `src/urma/ffi.rs` | [架构设计] bindings、RAII handles、status/null 转换。 |
| `src/urma/protocol.rs` | [架构设计] 五类 message 与 header codec，包含纯内存单元测试。 |
| `src/urma/buffer.rs` | [架构设计] RegisteredMemory、UrmaBufferPool、TxSlotLease、slot state 与 `user_ctx` 索引。 |
| `src/urma/request.rs` | [架构设计] UrmaRequestState、PieceMetadata、PendingUrmaPiece、UrmaPieceStream。 |
| `src/urma/connection.rs` | [架构设计] UrmaConnection、OOB handshake DTO/codec、request registry 与 send API。 |
| `src/urma/completion.rs` | [架构设计] CompletionPoller、IoCommand、CQE validation/routing、pending delivery/repost。 |
| `src/urma/runtime.rs` | [架构设计] UrmaRuntime start/ready/shutdown/join。 |
| `src/urma/server.rs` | [架构设计] UrmaServer OOB accept、Storage request handler、RangeReader -> TX slots。 |

### 9.2 `dragonfly-client`

| 文件 | 修改 |
|---|---|
| `client/dragonfly-client/Cargo.toml` | [架构设计] 增加 `urma` feature 并透传 storage feature。 |
| `client/dragonfly-client/src/resource/mod.rs` | [架构设计] feature-gated 导出 `urma_downloader`。 |
| `src/resource/urma_downloader.rs` | [架构设计] 新增 UrmaDownloader 并实现现有 Downloader trait。 |
| `src/resource/piece.rs` | [架构设计] `Piece` 增 `Option<Arc<dyn Downloader>>` URMA 字段，constructor 接受注入，standard `download_from_parent()` 增 `"urma"` 分支；persistent 分支不改。 |
| `src/resource/task.rs` | [架构设计] `Task::new()` 增 optional URMA downloader 参数并传给 `Piece::new()`。 |
| `src/resource/persistent_task.rs` | [架构设计] 如 `Piece::new()` 统一签名改变，该调用点显式传 `None`。 |
| `src/resource/persistent_cache_task.rs` | [架构设计] 同上，传 `None`，不开启 URMA persistent-cache。 |
| `src/bin/dfdaemon/main.rs` | [架构设计] 按 role 创建 Runtime/Connection/Downloader 或 Server，在 Task 前注入，spawn server，shutdown 时 drain/join。 |

### 9.3 Rust workspace 与 lockfile

| 文件 | 修改 |
|---|---|
| `client/Cargo.toml` | [架构设计] 若 bindgen/FFI build 依赖统一由 workspace 管理，在 workspace dependencies 增加对应条目。 |
| `client/Cargo.lock` | [架构设计] 由 Cargo 根据新增 build dependencies 更新，不手工编辑。 |

### 9.4 配置

| 文件 | 修改 |
|---|---|
| `client/dragonfly-client-config/src/dfdaemon.rs` | [架构设计] `Config` 增 feature-neutral `Urma` 实验配置：enabled/role/device/eidIndex/oobListenAddr/parentOobAddr/JFC depth/slot size/count/poll batch/metadata timeout。 |
| 对应 config tests | [架构设计] 验证默认 disabled，role 与 parent/listen 地址条件校验，slot/JFC 非 0 和长度上限。 |

- [架构设计] Scheduler protobuf、`CollectedParent`、announcer 和 API 仓库不在本次修改列表中；`download.protocol="urma"` 时使用静态 `parentOobAddr`，调度候选只保留 logical parent ID/业务上下文。
- [架构设计] 第一版不修改 `dragonfly-client-core` 错误 enum；storage URMA 模块使用内部 `UrmaError`，UrmaDownloader 在 future 返回前映射为 `DFError::Unknown("urma: ...")`，stream 阶段映射为 `io::Error`。
- [待验证] 如果故障测试证明上层需要区分 protocol/CQE/connection/resource exhausted，后续应增加通用 transport error 类型，不长期依赖字符串匹配。

## 10. 实现 Commit 顺序

### Commit 1：feature、FFI build 与 protocol codec

- [架构设计] 增加 Cargo feature/build.rs/`urma::{ffi,protocol}`，仅实现 UMDK 加载能力和纯 codec。
- [架构设计] 单元测试覆盖五种 message round-trip、大小端序、截断、超长、未知 version/type/flag 和 UTF-8 失败。
- [待验证] 该 commit 的硬门槛是 `feature=urma` 能在目标机链接 `liburma`，默认 feature-off build 在无 UMDK 机器仍通过。

### Commit 2：BufferPool 与 slot state machine

- [架构设计] 实现对齐 region、Segment 注册、RX/TX 切分、TxSlotLease、`user_ctx` slab/index 和状态转移单元测试。
- [架构设计] 此 commit 不发 SEND，先证明重复 acquire/release、double free/repost 检测和半途失败清理。

### Commit 3：Runtime + CompletionPoller 资源骨架

- [架构设计] 实现 I/O thread 启动、Context/JFC/Segment 创建、command queue、Ready handshake 和显式 shutdown/join。
- [架构设计] 此 commit 允许还没有 Peer connection，但要能反复 start/stop 且无资源泄漏。

### Commit 4：OOB + duplex Jetty + SEND/RECV smoke test

- [架构设计] 实现单 Parent/Child OOB descriptor 交换、import/bind、RX 预投、Ready barrier 和一条固定 payload 双向 SEND/RECV。
- [待验证] 验证 CQE status、`completion_len`、`user_ctx`、send buffer 回收、receive repost 和程序退出 drain。

### Commit 5：RequestState 与 CQE -> PieceContentStream

- [架构设计] 实现 request registry、metadata oneshot、bounded body mpsc、UrmaPieceStream、sequence/length 状态机和 pending delivery 背压。
- [架构设计] 用模拟 CQE/slot 单元测试 Metadata + 多 Data + End、Error、channel full、stream early Drop 和 stale generation。

### Commit 6：UrmaServer + Storage RangeReader

- [架构设计] 实现 Inbound Request -> `Storage::upload_piece()` -> Metadata/Data/End，并输出 parent-side Error。
- [架构设计] 使用大于 slot payload 的测试 Piece，确认 RangeReader 被分成多个 SEND message。

### Commit 7：UrmaDownloader trait adapter

- [架构设计] 新增 `resource/urma_downloader.rs`，实现 standard `download_piece()`，persistent 方法返回 Unimplemented。
- [架构设计] 此 commit 直接调 adapter 并把返回 stream 交给 Storage，验证落盘 length/digest。

### Commit 8：dfdaemon 配置与主链组装

- [架构设计] 增加 config/validation、Task/Piece optional injection、`download.protocol="urma"` standard 分支、Parent/Child role 启动和 graceful shutdown。
- [架构设计] TCP/QUIC 默认行为保持不变，feature-off 测试与现有测试必须通过。

### Commit 9：真机集成、故障与可观测性

- [待验证] 真机测试单 Piece 多 chunk、连续重复下载、Parent not-found/read error、CQE error、Child Storage timeout、stream early Drop 和 shutdown during transfer。
- [架构设计] 补齐 request ID、post/CQE、bytes、slot states、channel full、RNR、drain duration 和 digest result 的 tracing/metrics。

## 11. 实现前必须关闭的待验证项

1. [待验证] 确定实际编译/运行环境中 UMDK header、`liburma.so`、provider `.so` 和 config 的安装位置。
2. [待验证] 确定 duplex Jetty 使用的 transport mode、import/bind 必需字段、RNR retry 和顺序保证。
3. [待验证] 从 URMA API 规范与 provider 实测确认 post 返回后 WR/SGE descriptor 可否立即重用；TX payload 仍严格保留到 send CQE。
4. [待验证] 确定 `urma_poll_jfc()` 与 post API 是否允许多线程调用；在结论出来前继续使用单 I/O 线程所有权模型。
5. [待验证] 验证扣留 RX slot 不 repost 时的实际 RNR 行为；确保它形成背压而不是连接级永久错误。
6. [待验证] 确定 `slot_size/rx_slot_count/tx_slot_count/JFC depth` 的设备能力校验和最小可运行组合。
7. [待验证] 确定 OOB Ready barrier 之前双方都已经 post 足够 receive WR，避免第一条 Request 早于 Parent RQ Ready。

## 12. 不变式清单

- [架构设计] 任何时刻每个 TX slot 最多只有一个 owner；post 后在 send CQE/drain 前不可写。
- [架构设计] 任何 Posted RX slot 不可被 CPU 当作稳定 payload 读取；只有成功 recv CQE 使它进入 Completed。
- [架构设计] 任何 CQE 必须先通过 connection generation + slot generation 校验，再触发业务状态转移。
- [架构设计] 任何 Data 必须在 Metadata 之后、End 之前、sequence 正确且不使累计长度超限，才能进入 `PieceContentStream`。
- [架构设计] EOF 只由合法 End 产生；Error、CQE failure、timeout 不得伪装为 EOF。
- [架构设计] Piece 成功只由现有 Storage 完成长度、digest、metadata 提交后宣告；send/recv CQE 都不是 Piece 成功边界。
- [架构设计] unregister Segment 必须发生在停止新 post、Jetty ERROR、CQ drain、所有 TX/RX slot 脱离设备访问之后。
