# RDMA/URMA 上传下载路径对比与进度台账

更新时间：2026-08-28。

## 1. 结论

`[源码确认]` 除 provider 固有差异外，RDMA 与 URMA 必须保持相同的 Dragonfly Piece 业务链路：

```text
发现/选择 transport -> 请求 Piece -> metadata/限流 -> Storage 读写
-> receive-ready barrier -> bulk post/completion -> digest/metrics -> fallback/shutdown
```

固有差异只保留在 Fabric 以下以及 peer connection 生命周期：

- RDMA/libfabric：共享 RDM endpoint，provider address + tag 路由；当前每个 Piece 建立一个 TCP
  rendezvous connection；
- URMA/liburma：process-level Runtime/JFC/Segment，per-peer RC Jetty；一个 TCP control connection
  和 Jetty 顺序复用多个 Piece。

URMA 当前已完成 native/Fabric/lane/session，以及 client 侧下载 adapter（`client::urma`）与
`URMADownloader` 注册到 `DownloaderFactory`（discovery 缓存、penalty/backoff、fabric 懒初始化、
per-parent Session 复用、失败退休）。server 侧 listener/admission/readiness、dfdaemon optional
task 和 TCP `DFUR` discovery 已接入。`piece.rs` 已通过通用 `Downloader` 边界接入
normal/persistent/persistent-cache 三条 production path：URMA 建连/首读失败直接回退
TCP，URMA stream 开始后的传输或 Storage finish 失败会先重置 partial Piece，再整块
TCP 重下。现有 adapter
应复用 Dragonfly 的 Storage、Downloader、限流、metrics 和 TCP fallback，不复制 demo 的文件传输
或 benchmark 结构。

接口对齐只发生在业务边界：两者都通过 `Downloader::download_*` 返回
`PieceContentStream + offset + digest`，server 都提供 optional `run`/readiness/discovery 语义。
RDMA `acquire_buffer`/`PooledBuf`、tagged post/CQ 与 URMA fixed slot、Jetty credit/owner-thread command
属于各自 transport-private 实现，不要求同名同参，也不得暴露给 `piece.rs`。URMA 已将
`RuntimeConfig`、`UrmaLaneConfig`、`UrmaServerHandler` 和 registry mutation 收回 storage 内部。

## 2. 下载路径

### 2.1 目标调用链

```text
Piece::download_piece_from_parent
  -> UrmaDownloader：选择 parent、discovery/cache/backoff、失败回退
  -> UrmaClient：取得或建立 persistent peer session
  -> UrmaClientSession::request_piece
  -> loop UrmaClientSession::receive_next_window
  -> bounded PieceContentStream
  -> Storage::download_piece_from_parent_finished
  -> length + CRC32 digest 校验、metadata finish
```

### 2.2 与 RDMA 对照

| 链路阶段 | RDMA production path | URMA 当前对应 | 状态 |
|---|---|---|---|
| Piece 协议选择和 TCP fallback | `resource/piece.rs` | 三类 Piece 均通过 `Arc<dyn Downloader>` 选择 URMA，并保留 TCP 地址 | 已完成（真机待验） |
| capability discovery/cache/backoff | `RDMADownloader` + `discover` | `URMADownloader` + `discover` | 已完成 |
| process fabric 初始化/失败退休 | `RDMADownloader::fabric` | `URMADownloader::fabric` + `UrmaFabric::start/shutdown/readiness` | 已完成（懒初始化/失败退休/掉线重连） |
| peer connection | 每 Piece TCP rendezvous | `URMADownloader` 缓存 `UrmaClient`，单 Session slot 串行复用 lane | 已完成（真机复用待验） |
| Request/Ready 协商 | `RDMAClient::handle_download` | `request_piece` | 已对齐 |
| post receive + receive-ready | `receive_stream` + `RecvPosted` | `receive_next_window` + `RecvPosted` | 已对齐 |
| operation completion 校验 | tag/chunk/length | lane/sequence/length | 已对齐 |
| 内容向上交付 | `RDMAStreamReader`/`PieceContentStream` | `client::urma` 投递 owned `Bytes` window | 已完成 |
| Storage 写入和 digest | 通用 stream；另有 RDMA direct-window 优化 | 通用 stream（Storage 消费 `PieceContentStream`） | 已完成（Copy-RX） |
| stream drop/timeout/fallback | retire endpoint/session，TCP 重下 | 最终 window 受 Done gate；整 Piece timeout；延迟失败退休 Session；Storage 失败后 reset + 整块 TCP 重下 | 已完成（故障注入/真机待验） |

Phase A 的 URMA adapter 使用 owned window -> `Bytes` -> bounded `PieceContentStream`。RDMA 的
registered-window direct `pwrite + digest` 不作为 URMA 首次接入门槛，但已确定为
[Phase B production 性能数据路径](./phase-b-performance-data-path.md) 的必做项。

## 3. 上传路径

### 3.1 目标调用链

```text
dfdaemon UrmaServer task
  -> listener/readiness/admission
  -> UrmaServerSession::accept（每 peer 一次）
  -> loop receive_request
       -> Storage::get_piece/get_persistent_*
       -> upload bandwidth limiter + started metric
       -> Storage::upload_* -> io::RangeReader
       -> ready(metadata)
       -> loop next_window_len -> read_exact -> send_next_window
       -> finish_piece + finished/traffic metric
```

### 3.2 与 RDMA 对照

| 链路阶段 | RDMA production path | URMA 当前对应 | 状态 |
|---|---|---|---|
| dfdaemon server task | `RDMAServer::run` | `UrmaServer::run` optional task | 已完成 |
| listener/readiness | TCP listener + `CapabilityRegistry` | listener + live `CapabilityRegistry` + `DFUR` discovery | 已完成 |
| connection/transfer admission | semaphore | connection semaphore + Fabric command admission | 已完成（BUSY typed reply） |
| peer transport 建立 | 共享 endpoint + per-Piece request | `UrmaServerSession::accept` + bind Jetty | 已有 |
| Piece 请求 | handler 读 Request | `receive_request` | 已有 |
| metadata lookup | `Storage::get_*` | `UrmaServerHandler::piece_metadata`，覆盖三类 Piece | 已完成 |
| not-found/busy/internal reply | RDMA `abort(Error frame)` | handler + `reject_piece(code, message)` | 已完成（BUSY 等 listener admission） |
| bandwidth limiter/metrics | 已有 | handler upload limiter + started/finished/failure/traffic | 已完成 |
| Storage 数据源 | `open_piece_source`/`RangeReader`/optional mmap | `upload_* -> RangeReader -> bounded owned window` | 已完成（不做 mmap） |
| receive-ready + SEND/completion | handler + Fabric | `send_next_window` + Fabric | 已对齐 |
| Piece Done | Done 后连接结束 | `finish_piece` 后 session 回 Idle | 已对齐；持久复用 |
| server shutdown/drain | task shutdown + Fabric close | clear advertisement -> close listener -> abort/drain lanes -> Fabric shutdown | 已完成（真机待验） |

Phase A 只使用 `RangeReader::read_exact` 填充一个 owned window。RDMA 的 mmap、双 registered send
ring、Storage read 与 NIC send overlap 已纳入
[Phase B production 性能数据路径](./phase-b-performance-data-path.md)，不再作为未定的可选优化。

## 4. Session production contract

2026-08-26 已完成 adapter 接入前的第一轮 contract 收敛：

- peer `Error` 不再压成普通 protocol string；`PeerRejected { code, message }` 保留
  incompatible/not-found/busy/internal，供 downloader fallback/backoff 决策；
- active Piece 的 TCP control read/write 受 `control_timeout` 约束，超时返回带 operation 名称的
  `ControlTimeout` 并 retire 当前 lane；空闲等待下一 Piece 使用独立 Session idle timeout，正常到期
  close lane，不分类为 transport error；
- `request_piece` 和 `receive_request` 返回 owned metadata/request，避免 adapter 在后续可变 Session
  调用前持有借用；
- `UrmaServerSession::reject_piece` 可向 peer 返回明确错误并终止当前保守型 Phase A session；
- negotiated `max_inflight_chunks` 不得超过 client local Jetty `recv_depth` 或 server local Jetty
  `send_depth`。

尚未放入 Session 的职责：

- 全局 TX/RX slot budget 和多 peer admission；
- discovery/cache/backoff；
- Storage metadata/RangeReader；
- limiter、metrics、digest 和 TCP fallback。

这些属于 dfdaemon/client/server adapter，塞入 Session 会重新形成一套 transport application。

## 5. 实现进度

| 工作项 | 状态 | 下一验收点 |
|---|---|---|
| native Runtime/JFC/JFR/Segment/Jetty ownership | 已完成（纯测试/编译） | 真实 provider startup/shutdown |
| per-operation completion、timeout、drain/reap | 已完成（纯测试/编译） | 真机 CQE/flush |
| persistent lane + sequential Piece Session | 1 GiB 真机确认 client/server 各建立 1 lane、1 次 `reused=false`、255 次 `reused=true`；已修复并发 singleflight 和 30s active timeout 误杀 idle Session（storage 34/client 62 tests） | 等待 40s 跨任务复用；超过 450s 正常 idle close 后无 fallback 重连 |
| Session production contract | 已完成 | adapter error/fallback 测试 |
| `client::urma` + `PieceContentStream` | 已完成（含 Done gate/整 Piece timeout）；1 GiB + 12,345 bytes 真机下载及 SHA-256 已通过 | persistent/cache Piece 双端 |
| `URMADownloader` 接入 `DownloaderFactory` | 已完成 | 真机 discovery/fallback 决策 |
| config `UrmaServer`（dfdaemon） | 已完成 | example YAML + 运行验证 |
| `server::urma` + Storage/RangeReader | normal Piece 真机通过，含 4 MiB/64 KiB 均不能整除的 12,345-byte 尾部 | persistent/cache、not-found、限流真机验证 |
| dfdaemon server wiring（listener/readiness） | 已完成（纯测试/编译） | optional capability 真机验证 |
| `piece.rs` 整体 fallback | 已完成（normal/persistent/cache）；建连/请求前失败、三类流中断、digest mismatch、Storage timeout 已有故障注入测试 | 真机断链与 TCP 重下 |
| metrics/shutdown orchestration | 已完成基础 wiring | 真机 outstanding WR shutdown |
| registered lease、RX direct-write、双 window、mmap、post/CQ batch | Phase B 已规划，待实现 | 按 B1-B7 顺序完成；真实 provider correctness 后做最终同口径 benchmark |

当前验证：

```text
cargo fmt --all -- --check                                    PASS
cargo check -p dragonfly-client --features urma                PASS
cargo test -p dragonfly-client-storage --features urma urma::  34 passed / 0 failed
cargo test ... discovery_is_fail_closed_until_listener_publishes  1 passed / 0 failed
cargo test -p dragonfly-client --features urma --lib           62 passed / 0 failed（含 URMA singleflight/退避/fallback）
cargo test -p dragonfly-client --features urma --lib resource::piece::tests::test_urma_fallback_matrix
                                                               1 passed / 0 failed（6 场景）
cargo test -p dragonfly-client-config                          47 passed / 0 failed
```

说明：
- `rdma` feature 的 `cargo check` 需要 libfabric 头文件（`rdma_fabric.h`），本机仅有运行时
  `libfabric.so.1`、缺 dev 包，因此 `--features rdma,urma` 联合编译未在本机验证；
- 链接/运行 `urma` 相关测试需要 `LD_LIBRARY_PATH`/`-rpath-link` 指向 UMDK core 与 common 的
  build 目录（`liburma.so` 传递依赖 `liburma_common.so`）；
- 上述验证使用本地 UMDK build tree，未启动真实 provider。
