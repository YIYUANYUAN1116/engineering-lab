# RDMA/URMA 上传下载路径对比与进度台账

更新时间：2026-08-29。

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

URMA 已完成 native/Fabric/lane/session、client/server adapter、discovery/readiness、dfdaemon wiring、
三类 Piece fallback，以及 Phase B B1-B4 RX/TX production 数据路径。demo 只提供已验证的 URMA
provider/WR/lease 实现依据；Dragonfly 没有迁移 demo 的独立文件协议、benchmark CLI 或 application
结构。

当前 production RX 是 `NIC DMA -> registered RX spans -> positional write + CRC32 -> recycle`，没有
额外 userspace staging copy。transport-neutral `Downloader` 兼容调用仍保留一次 lease -> `Bytes`
聚合 copy，但 `piece.rs` 的三类 production 路径不走该入口。当前 production TX 是
`MappedPiece/RangeReader -> registered TX spans -> NIC DMA`，没有 owned window 或逐 chunk payload
copy。B1-B4 尚未进行真实 provider 跨节点验证。

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
| Piece 协议选择和 TCP fallback | `resource/piece.rs` | 三类 Piece 优先走 URMA direct reader；失败 reset 后 TCP 整块重下 | 已完成，真机待验 |
| capability discovery/cache/backoff | `RDMADownloader` + `discover` | `URMADownloader` + `discover` | 已完成 |
| process fabric 初始化/失败退休 | `RDMADownloader::fabric` | `URMADownloader::fabric` + `UrmaFabric::get_or_start` | 已完成 |
| peer connection | 每 Piece TCP rendezvous | per-parent cached client、persistent Session/lane | Phase A normal 真机已确认复用 |
| Request/Ready 协商 | `RDMAClient::handle_download` | `request_piece` | 已对齐 |
| post receive + receive-ready | `receive_stream` + `RecvPosted` | 原子保留完整 window；最多预投递 2 个；完整 post 后 `RecvPosted` | B2 完成，真机待验 |
| operation completion 校验 | tag/chunk/length | lane/sequence/length + slot generation | B1/B2 完成，真机待验 |
| 内容向上交付 | `RDMAStreamReader`/`ReceivedWindow` | `UrmaStreamReader`/multi-span `UrmaReceivedWindow` | B3 完成 |
| Storage 写入和 digest | registered window direct write/hash | immutable spans 上 positional write 与 CRC32 并行 | B3 完成，真机待验 |
| stream drop/timeout/fallback | retire endpoint/session，TCP 重下 | Done gate、per-window/整 Piece timeout、owner recycle、partial reset | 已完成，真机待验 |

Phase A 的两段式 RX staging 和 B2 过渡期的一次 aggregate copy 已从 production Piece 路径移除。
B3 write/hash 共享 immutable lease，两个 blocking worker 都 join 后才显式 recycle；expected length、
累计 length 和 positional offset 有硬边界。兼容 `Downloader` trait 保留一次 copy，不代表 production
RX 数据路径。

### 2.3 Buffer 管理模型与对齐边界

RDMA 当前是受 byte semaphore 限界的动态 best-fit registered buffer cache；URMA 当前是一个预注册
Segment 上的固定 TX/RX slot pool。前者对不同尺寸和方向更灵活，后者没有热路径 registration miss、
状态更确定，但可能产生固定分区闲置、slot 内部碎片、multi-span syscall 和 owner recycle queue 开销。

当前决定是不照搬 RDMA allocator。B6 已在 fixed slots 上实现 process registered-byte ceiling、固定 TX
保底/RX 余量、non-blocking 第二 window acquire、生命周期审计和预算压力指标。尚未实现的是 TX/RX
shared overflow、动态 arena/size class 和严格跨 peer slot fairness；只有在真机观察到以下信号后才演进：

- TX/RX 一侧耗尽而另一侧长期空闲；
- payload/registered bytes 利用率低；
- `BufferUnavailable` 或 pipeline depth 1 降级频繁；
- WR/CQE、逐 span write、CRC 遍历或 owner recycle latency 限制 CPU/吞吐；
- 固定 pin 内存不满足 memlock、容器或多 device 约束。

优先先做 batching、调 slot size、TX/RX shared overflow 和多 size-class；完整动态 best-fit pool 是最后
选项。详细决策与触发条件见
[Phase B production 性能数据路径](./phase-b-performance-data-path.md#31-urma-slot-pool-是否对齐-rdma-buffer-pool)。

## 3. 上传路径

### 3.1 目标调用链

```text
dfdaemon UrmaServer task
  -> listener/readiness/admission
  -> UrmaServerSession::accept（每 peer 一次）
  -> loop receive_request
       -> Storage::get_piece/get_persistent_*
       -> upload bandwidth limiter + started metric
       -> optional MappedPiece / Storage::upload_* RangeReader
       -> ready(metadata)
       -> direct-fill TxWindowLease A
       -> loop send(current) || fill(next)，预算不足时 ring=1
       -> finish_piece + finished/traffic metric
```

### 3.2 与 RDMA 对照

| 链路阶段 | RDMA production path | URMA 当前对应 | 状态 |
|---|---|---|---|
| dfdaemon server task | `RDMAServer::run` | optional `UrmaServer::run` task | 已完成 |
| listener/readiness | TCP listener + `CapabilityRegistry` | listener + live registry + `DFUR` discovery | 已完成 |
| connection/transfer admission | semaphore | connection semaphore + Fabric command admission | 已完成 |
| peer transport 建立 | 共享 endpoint + per-Piece request | `UrmaServerSession::accept` + bind Jetty | 已有 |
| Piece 请求 | handler 读 Request | `receive_request` | 已有 |
| metadata lookup | `Storage::get_*` | `UrmaServerHandler`，覆盖三类 Piece | 已完成 |
| not-found/busy/internal reply | RDMA `abort(Error frame)` | typed handler/reply | 已完成 |
| bandwidth limiter/metrics | 已有 | server adapter limiter/metrics | 已完成 |
| Storage 数据源 | `open_piece_source`/`RangeReader`/optional mmap | `MappedPiece` 优先（配置开启）+ cache/mmap failure `RangeReader` fallback，直接填 TX lease | B4 完成，真机待验 |
| receive-ready + SEND/completion | registered ring + per-op completion | `send_next_registered_window`；整窗 CQE state 持有 lease | B4 完成，真机待验 |
| fill/SEND overlap | 两半 registered ring | 两个 exclusive lease；`send(current)` 与 `fill(next)` 并行，资源不足 ring=1 | B4 完成，真机待验 |
| Piece Done | Done 后连接结束 | `finish_piece` 后 session 回 Idle | 已对齐；持久复用 |
| server shutdown/drain | task shutdown + Fabric close | advertisement clear -> listener close -> lane/Fabric drain | 基础 wiring 完成，真机待验 |

B4 已移除 TX owned window、per-chunk `.to_vec()` 和 shim Segment copy。每个 negotiated message 独占
一个 slot，即使 chunk 小于固定 slot 也可同时 outstanding；最后一个 CQE 前 lease 不会返回/refill。
默认配置现在由 `maxRegisteredBytes=40 MiB` 和 `txRegisteredBytes=8 MiB` 计算出 TX 128/RX 512 slots；
`pipelineDepth=2` 将 TX 单 window 上限约束为 64 chunks，为第二 lease 留出空间。该固定分区仍不解决
TX/RX shared overflow 或严格跨 peer slot fairness，后续演进仍按第 2.3 节的证据门槛决定。

## 4. Session production contract

2026-08-26 已完成 adapter 接入前的第一轮 contract 收敛：

- peer `Error` 不再压成普通 protocol string；`PeerRejected { code, message }` 保留
  incompatible/not-found/busy/internal，供 downloader fallback/backoff 决策；
- active Piece 的 TCP control I/O 使用 `control_timeout`；空闲 Session 使用独立 idle timeout；
- `request_piece` 和 `receive_request` 返回 owned metadata/request，避免 adapter 在后续可变 Session
  调用前持有借用；
- `UrmaServerSession::reject_piece` 可向 peer 返回明确错误并终止当前保守型 Phase A session；
- negotiated `max_inflight_chunks` 不得超过 client local Jetty `recv_depth` 或 server local Jetty
  `send_depth`。

B2/B3 又增加以下 RX contract：

- logical window 的全部 slots 必须在任何 WR post 前原子保留；
- pipeline depth 最大为 2，permit 覆盖 pending、completed 和 consumer-held lease；
- 第二个 window 预算不足安全退化为深度 1；
- 完整 window post 成功后才能发送 `RecvPosted`；write/hash 完成并 recycle 后才释放真实预算；
- final lease 仍在 Done 校验后发布；blocking write 错误路径先 join 已提交 worker，再 reset/fallback。

不放入 Session 的职责：

- process 注册预算配置、方向分区和多 peer admission（由 shared Fabric/adapter 负责）；
- discovery/cache/backoff；
- Storage metadata/RangeReader；
- limiter、metrics、digest 和 TCP fallback。

这些属于 dfdaemon/client/server adapter，塞入 Session 会重新形成一套 transport application。

## 5. 实现进度

| 工作项 | 状态 | 下一验收点 |
|---|---|---|
| native Runtime/JFC/JFR/Segment/Jetty ownership | 已完成（纯测试/编译） | 真实 provider startup/shutdown |
| per-operation completion、timeout、drain/reap | 已完成（纯测试/编译） | 真机 CQE/flush |
| persistent lane + sequential Piece Session | Phase A 1 GiB 真机确认双方各 1 lane，`reused=false` 1 次、`true` 255 次 | 跨任务 idle 复用/正常 idle close |
| Session production contract | 已完成 | adapter error/fallback 测试 |
| `client::urma`/`URMADownloader`/server wiring | 已完成；Phase A 1 GiB + 12,345 bytes normal 真机通过 | persistent/cache、断链、not-found、限流真机 |
| 三类 Piece fallback | 代码完成，6 场景 matrix 通过 | 真机 partial reset + TCP 重下 |
| registered lease/generation/owner recycle/close guard（B1） | 代码和纯测试完成 | 真机 recycle/active-close/outstanding shutdown |
| registered completion/atomic reservation/双窗口（B2） | 代码和纯测试完成 | 真机连续 Piece、背压、drop、尾 window |
| Storage direct-write + digest overlap（B3） | 三类 Piece 代码和纯测试完成；production RX 0 staging-copy | 真机 correctness、故障与 overlap 指标 |
| TX direct-fill/双 ring/mmap（B4） | 代码和纯测试完成；production TX 1 次 source-fill copy | 真机 mmap/reader/tail/ring=1/2/CQE ownership |
| post/CQ/credit batch（B5） | linked SEND/RECV、partial-post 前缀记账、CQ batch/fair owner 代码完成；单 lane postListSize=1/8 真机正常路径矩阵完成，结果见 B7 性能台账 | feature-on 编译记录；真实 provider partial post/CQ/error/flush 与多 peer 校准 |
| budget/config/degradation（B6） | process byte ceiling、固定 TX/RX 分区、pipeline depth、optional second-window 退化和指标代码完成；inflight=16/32/64 单 lane真机矩阵完成 | feature-on 编译记录；真实 provider budget pressure、多 peer 进展与 shutdown 审计 |

截至 B4 的已验证基线：

```text
cargo fmt --all -- --check                                    PASS
cargo check -p dragonfly-client-storage                       PASS
cargo check -p dragonfly-client --features urma                PASS
cargo test -p dragonfly-client-storage --features urma urma::  43 passed / 0 failed
cargo test -p dragonfly-client-storage --features urma test_write_urma_stream
                                                               2 passed / 0 failed
cargo test -p dragonfly-client-storage --features urma --lib  155 passed / 0 failed
cargo test -p dragonfly-client --features urma --lib           62 passed / 0 failed
```

说明：client 全量测试的本地 scheduler socket 用例在沙箱外运行并 62/62 通过；URMA 使用本地 UMDK
build tree。上述结果是 B4 时点基线，不代表 B5/B6 已通过同一组命令。2026-08-30 的 B5/B6 本轮仅
确认 `cargo fmt --check`、`cargo metadata --no-deps` 和 `git diff --check`；feature-on check 被本机缺少
`protoc` 与 Perl 阻断。真实 provider 验证仍需按 runbook 统一执行。
