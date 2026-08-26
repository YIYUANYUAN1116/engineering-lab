# RDMA/URMA 上传下载路径对比与进度台账

更新时间：2026-08-26。

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

URMA 当前已完成 native/Fabric/lane/session，尚未进入 dfdaemon production path。后续 adapter
应复用 Dragonfly 的 Storage、Downloader、限流、metrics 和 TCP fallback，不复制 demo 的文件传输
或 benchmark 结构。

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
| Piece 协议选择和 TCP fallback | `resource/piece.rs` | 无 | 待实现 |
| capability discovery/cache/backoff | `RDMADownloader` + `discover` | `Discover/Capability` codec | 只有 wire |
| process fabric 初始化/失败退休 | `RDMADownloader::fabric` | `UrmaFabric::start/shutdown/readiness` | 内核已有，未注入 dfdaemon |
| peer connection | 每 Piece TCP rendezvous | `UrmaClientSession::connect` | 已有；持久 lane 是设计差异 |
| Request/Ready 协商 | `RDMAClient::handle_download` | `request_piece` | 已对齐 |
| post receive + receive-ready | `receive_stream` + `RecvPosted` | `receive_next_window` + `RecvPosted` | 已对齐 |
| operation completion 校验 | tag/chunk/length | lane/sequence/length | 已对齐 |
| 内容向上交付 | `RDMAStreamReader`/`PieceContentStream` | owned `Vec<u8>` window | adapter 待实现 |
| Storage 写入和 digest | 通用 stream；另有 RDMA direct-window 优化 | 无 | adapter 待实现 |
| stream drop/timeout/fallback | retire endpoint/session，TCP 重下 | Session drop/timeout abort lane | 下层已有，上层策略待实现 |

Phase A 的 URMA adapter 使用 owned window -> `Bytes` -> bounded `PieceContentStream`。RDMA 的
registered-window direct `pwrite + digest` 是优化，不作为 URMA 首次接入门槛。

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
| dfdaemon server task | `RDMAServer::run` | 无 | 待实现 |
| listener/readiness | TCP listener + `CapabilityRegistry` | capability codec | 待实现 |
| connection/transfer admission | semaphore | Fabric 普通命令 admission | server admission 仍待实现 |
| peer transport 建立 | 共享 endpoint + per-Piece request | `UrmaServerSession::accept` + bind Jetty | 已有 |
| Piece 请求 | handler 读 Request | `receive_request` | 已有 |
| metadata lookup | `Storage::get_*` | 无 | adapter 待实现 |
| not-found/busy/internal reply | RDMA `abort(Error frame)` | `reject_piece(code, message)` | 2026-08-26 已完成 |
| bandwidth limiter/metrics | 已有 | 无 | adapter 待实现 |
| Storage 数据源 | `open_piece_source`/`RangeReader`/optional mmap | `next_window_len` 等待输入 | RangeReader adapter 待实现 |
| receive-ready + SEND/completion | handler + Fabric | `send_next_window` + Fabric | 已对齐 |
| Piece Done | Done 后连接结束 | `finish_piece` 后 session 回 Idle | 已对齐；持久复用 |
| server shutdown/drain | task shutdown + Fabric close | Fabric drain/shutdown | orchestration 待实现 |

Phase A 只使用 `RangeReader::read_exact` 填充一个 owned window。RDMA 的 mmap、双 registered send
ring、Storage read 与 NIC send overlap 归入后续优化。

## 4. Session production contract

2026-08-26 已完成 adapter 接入前的第一轮 contract 收敛：

- peer `Error` 不再压成普通 protocol string；`PeerRejected { code, message }` 保留
  incompatible/not-found/busy/internal，供 downloader fallback/backoff 决策；
- 所有 TCP control read/write 受 `control_timeout` 约束，超时返回带 operation 名称的
  `ControlTimeout`，并 retire 当前 lane；
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
| persistent lane + sequential Piece Session | 已完成（纯测试/编译） | 同 lane 10 Piece 双端测试 |
| Session production contract | 已完成 | adapter error/fallback 测试 |
| `client::urma` + `PieceContentStream` | 未开始 | normal/persistent/cache Piece |
| `server::urma` + Storage/RangeReader | 未开始 | not-found、尾 window、限流 |
| discovery/listener/config/readiness | 未开始 | optional capability/fallback |
| dfdaemon wiring/metrics/shutdown | 未开始 | feature-on daemon lifecycle |
| zero-copy、双 window、mmap | 后续优化 | benchmark 证明收益 |

当前验证：

```text
cargo fmt --all -- --check                                    PASS
cargo check -p dragonfly-client --features urma                PASS
cargo test -p dragonfly-client-storage --features urma urma::  27 passed / 0 failed
```

上述验证使用本地 UMDK build tree，未启动真实 provider。
