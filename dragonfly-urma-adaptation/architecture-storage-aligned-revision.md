# 架构修订：URMA 作为 Dragonfly Storage 内部传输后端

> 决策日期：2026-08-25  
> 最近更新：2026-08-28  
> 状态：Accepted  
> 取代：独立 `dragonfly-client-urma-transport` crate 方案

## 1. 决策

URMA 适配保持 Dragonfly 现有 storage transport 结构。URMA native 实现是
`dragonfly-client-storage` 的内部模块，不再构建一套独立 transport framework：

```text
dragonfly-client-storage
  src/rendezvous.rs           native transport 共用 frame/Piece/window/error schema
  src/urma/fabric.rs       Tokio-safe facade 与 owner thread
  src/urma/runtime.rs      进程级 Context/JFC/Segment/Lane resource tree
  src/urma/completion.rs   shared JFC 的唯一 poller 与全局 CQE router
  src/urma/lane.rs         owned Jetty、descriptor、WR identity 与 lane state
  src/urma/buffer.rs       process-wide registered slot pool
  src/urma/ffi/            pointer-free UMDK shim ABI
  src/client/urma.rs       Piece download adapter（已实现）
  src/server/urma.rs       Piece upload adapter（后续实现）
```

Cargo feature `urma` 定义在 `dragonfly-client-storage`，由 `dragonfly-client` 同名 feature
向下传递。storage 的 `build.rs` 同时负责隔离可选的 libfabric RDMA 和 UMDK/URMA native 构建；
feature-off 不探测 UMDK。

## 2. demo 的新定位

`urma-transport-lab/tcp-urma-file-transfer` 只作为以下内容的实现和实验参考：

- UMDK Runtime、Context、JFC/JFR、Segment、Jetty API 顺序；
- descriptor export/import/bind；
- SEND/RECV、CQE、registered-memory 与 WR lifetime；
- startup rollback、drain 和 shutdown；
- 真实 provider 的限制与回归方法。

demo 不再决定：

- Dragonfly crate/module 边界；
- Piece request/metadata/error 协议；
- TCP rendezvous/discovery；
- client/server/downloader 接口；
- digest 与 Storage commit 语义。

因此已经删除 demo 派生的独立 v3 Piece codec 和私有 OOB 协议。控制面从现有
Dragonfly/RDMA rendezvous 与 Piece contract 出发，在需要 UMDK descriptor 字段时扩展，而不是
平行维护第二套协议。

## 3. 保留的 native correctness core

- pointer-free C shim 与 shim-only bindgen allowlist；
- Runtime/JFC/shared JFR/registered Segment/Jetty 生命周期；
- fixed registered slot 状态机；
- one-WR signaled SEND/RECV；
- CQE 路由、generation/user_ctx 校验；
- completion 前不释放 DMA buffer；
- drain、错误聚合和逆序 shutdown。

这些实现按 production ownership 职责分成私有 Rust 文件，而不是按 UMDK 资源名逐个建文件。
它们不是 public transport API。

## 4. 当前代码状态

- 独立 `dragonfly-client-urma-transport` 已从 workspace 删除；
- native core 已迁入 `dragonfly-client-storage/src/urma`；
- owner thread 已收敛为 storage-private `UrmaFabric`；
- 借用 Runtime 的 `UrmaConnection<'runtime>` 已改为可长期保存的 owned `UrmaLane`；
- completion polling 已从 per-connection 提升为 shared JFC 的唯一全局 router，CQE 再按
  lane/generation/slot 路由；
- `connection.rs`、`jetty.rs`、`jfc.rs`、`wr.rs` 已分别收敛到 `lane.rs` 与 `runtime.rs`；
- receive completion 已通过 opaque `ReceivedChunk` 隔离 backing ownership；当前 Phase A
  实现仍从 C-owned Segment 复制，后续可在不改变 client/storage contract 的情况下改为
  registered-window guard；
- 自有 `message.rs`、`oob.rs` 已删除；
- 默认 storage build 通过且不访问 UMDK；
- `--features urma` 编译、链接和 URMA 模块单测通过；
- `UrmaFabricHandle` 已提供 storage-private 的 create/bind/post-receive/send/close 命令。普通业务
  命令先取得 bounded semaphore admission，再进入 owner thread；abort/shutdown 走不受该 admission
  阻塞的 lifecycle path，避免满队列丢失清理请求。序列化的 Jetty descriptor 和 owned bytes 是
  唯一跨线程数据，native handle 仍不离开 owner；
- owner thread 已实现 command/completion progress loop：无 outstanding WR 时阻塞等待命令，有
  outstanding WR 时每轮有界处理命令并 poll shared send/recv JFC，空 poll 退避、活跃 poll yield；
- `post_receive/send` 已返回独立 `UrmaOpHandle`；每个 outstanding WR 在 router 内持有自己的
  oneshot sender，CQE 直接完成对应 operation，不再经过全局 completion queue，也不要求 session
  adapter 再维护一层 sequence-to-pending map；
- progress/CQE/protocol error 会将 Fabric 标记为 `Failed/poisoned` 并唤醒全部 pending waiter，
  但 native WR、registered slot 仍保留到 CQE/flush 真正退休；operation timeout 会把所属 lane 置为
  Draining/ERROR、停止新 post，而不是伪造 liburma 不保证支持的 per-WR cancel；
- `client::urma` 已实现：`discover` 做 TCP rendezvous 探测，`URMADownloader` 按 parent 缓存
  `UrmaClient`，client 内的单 Session slot 串行复用同一 TCP control connection/Jetty；有界
  `futures::channel::mpsc` 把 owned `Bytes` window 流式投递成 `PieceContentStream`。最后一个 window
  只在 `Done` 校验成功后发布，整个后台 Piece transfer 受 `piece_timeout` 约束；延迟失败会退休
  parent client，并进入 penalty/backoff。`URMADownloader` 已注册到 `DownloaderFactory`，但
  `piece.rs` production 选择与 Storage finish 整体 TCP fallback 仍待接入；
- `server::urma::UrmaServerHandler` 已实现 Storage adapter：按 PieceKind 查询三类 metadata，调用
  现有 `upload_*` 获得 `RangeReader`，在 upload limiter 后复用一个有界 owned window，并按
  `next_window_len -> read_exact -> send_next_window` 驱动 Session；not-found/invalid/internal 使用
  typed Error，整个 Piece 受 `piece_timeout` 约束；
- `server::urma::UrmaServer` 已实现 listener、connection semaphore/BUSY reply、Fabric readiness
  监听、capability publish/clear、连接任务回收和显式 shutdown；TCP Piece endpoint 已支持 `DFUR`
  discovery，dfdaemon 已按 optional fast-path 启动，失败不会终止 TCP/QUIC；
- `dragonfly-client-config` 已新增 `UrmaServer`（`StorageServer.urma`），`UrmaClient` 超时从
  `storage.server.urma.transfer_timeout` 读取；
- 尚未实现 `piece.rs` 整体 fallback，真实 Piece 闭环未验证。

### 4.3 通用 rendezvous 与 URMA receive credit

2026-08-26 已把 RDMA rendezvous 分为两层：

```text
storage/rendezvous.rs
  frame envelope / bounded payload reader
  PieceKind / PieceRequest / PieceMetadata
  ReceiveWindow / RendezvousError / shared error codes

rdma/rendezvous.rs                 urma/rendezvous.rs
  provider + fabric tag             transport_type + fabric tag + max message
  libfabric endpoint + tag          client/server Jetty descriptor
  RDMA magic/version                 URMA magic/version
```

RDMA 仍使用原 `DFRD` magic、v2 version、frame discriminant和字段顺序；新增 golden-wire 测试固定
Request 的 v2 bytes，避免公共 DTO 抽取意外改变现有 RDMA peer compatibility。URMA 使用独立
`DFUR` magic/v1，复用 Piece/window/error 语义，但不携带 libfabric provider endpoint/tag。

lane credit 语义也已修正：本地 `post_receive` 不再给本地 SEND 放行。只有 session 控制面验证
peer 的连续、非零 `RecvPosted` window 后，才能通过
`UrmaFabricHandle::grant_send_credit` 授予对应数量的 remote receive credits；每次成功 native SEND
post 消费一个 credit。这样本地 lane Ready 与 peer receive-ready 被明确分离。

storage-private session adapter 已实现上述 primitives 的串联；client downloader adapter 与
server/Storage adapter、dfdaemon listener/readiness/discovery 均已接入，`piece.rs` production path
尚未接入。
控制面明确拆成两个生命周期：

```text
peer lane（一次）: Connect(capability + client descriptor)
                  -> Connected(server descriptor) -> 双端 bind

Piece（可重复）: Request -> Ready -> [post RECV -> RecvPosted -> grant credit
                -> SEND/RECV completion]... -> Done -> session 回到 Idle
```

`UrmaClientSession`/`UrmaServerSession` 在一个 TCP control connection 和 Jetty 上顺序复用多个
Piece；descriptor 不再放进每个 Piece Request/Ready。Phase A 先固定顺序复用，后续若需要同一
lane 并发 Piece，再在该边界增加 session id 和调度，不改变 Fabric/native ownership。

### 4.4 业务接口与 transport-private 接口收敛

2026-08-28 在接 `piece.rs` 前完成 public surface 审计。对齐原则是统一 Dragonfly 业务语义，
不伪造 libfabric 与 liburma 并不共有的 native 资源 API：

| 边界 | 对外契约 | 当前处理 |
|---|---|---|
| Piece 业务层 | `Downloader::download_* -> PieceContentStream + offset + digest` | TCP/QUIC/RDMA/URMA 统一 |
| client adapter | `discover`、三类 `download_*`、失败退休 | RDMA/URMA 语义对齐 |
| server adapter | `new`、capability registry、`run`、optional failure | RDMA/URMA 语义对齐 |
| Fabric/native | buffer lease、post、completion、tag/credit、lane | provider-private，不做假统一 |

本轮具体收敛：

- `UrmaClient::new` 不再让上层传 `UrmaLaneConfig`，而是从 dfdaemon 的
  `maxInflightChunks` 在 adapter 内构造 lane 参数；
- `UrmaFabric::get_or_start` 对外只接收 device/EID，`RuntimeConfig` 与 `runtime` 模块退回
  storage crate 内部；
- `UrmaServerHandler` 退回模块私有，dfdaemon 只依赖 `UrmaServer`；
- `CapabilityRegistry::publish/clear/get` 退回 storage crate 内部，dfdaemon 只负责创建并注入 registry；
- URMA buffer/Session/post/completion API 保持 `pub(crate)`，业务 crate 没有直接调用者。

RDMA 的 `Fabric::acquire_buffer` 是 dynamic registration/pool lease，实现上服务于 RDMA adapter；
URMA 的 Segment/fixed slot 由 owner thread 独占，不能为了接口同名把 registered slot guard 暴露给
业务层。Phase A 不为 URMA 增加 `acquire_buffer`，也不把 RDMA 的 registered-memory 直写特例定义成
所有 transport 必须实现的业务接口。

2026-08-26 Session production contract 已进一步收敛：peer Error 保留 code/message；全部 control
read/write 有显式 timeout；request/metadata 使用 owned 返回；server 可以 `reject_piece`；协商的
inflight 不得超过本地 Jetty 对应 send/recv depth。完整的 RDMA/URMA production path 对照和滚动
进度见 `rdma-urma-upload-download-path-comparison.md`。

验证：

```text
默认 rendezvous tests                             8 passed / 0 failed
URMA module tests（含 protocol/credit/session/server adapter） 32 passed / 0 failed
TCP discovery fail-closed test                         1 passed / 0 failed
cargo check -p dragonfly-client --features urma   PASS
RDMA feature-gated Rust client/server type-check  PASS
```

本机没有 libfabric headers/library，因此标准 `cargo check --features rdma` 在 build.rs 的依赖检查处
停止；本轮没有完成 libfabric C shim compile/link。RDMA codec 默认测试和 Rust-only cfg type-check
均通过，但不能替代后续有 libfabric 环境的正式 feature build。

### 4.1 2026-08-26 production audit P0 修复

本轮修复两个在真实错误与 shutdown 路径上可能破坏 native ownership 的问题：

1. `dfurma_runtime_close` 的返回约定统一为“仅成功时消费 wrapper”。当 Context 已删除但
   `urma_uninit()` 失败时，C shim 保留 runtime wrapper，Rust `NativeRuntime` 也继续持有该指针，
   允许显式 shutdown 或 `Drop` 重试；不再出现 C 已释放而 Rust 再次 close 的悬空指针路径。
2. JFC poll 返回一个 batch 后，即使其中某条 CQE 路由失败，也继续路由本 batch 的所有后续
   CQE；send JFC 出错也不阻止本轮继续 poll recv JFC。完整退休已被 provider 消费的 WR/slot 后，
   再把记录到的第一个错误返回给 runtime。`WR_FLUSH_ERR_DONE` 仍作为无 WR `user_ctx` 的 drain
   sentinel，不能据此提前跳过同 batch 的真实 WR completion。

约束由此明确为：

- native close 返回错误时，调用方仍拥有原 handle；返回成功后 handle 才失效；
- provider 一次 poll 返回的 batch 是不可回放的消费单元，router 不得因单条错误提前退出；
- 错误 batch 中成功路由出的业务 completion 仍直接交付给各自 `UrmaOpHandle`；对应 native WR、
  计数器和 buffer slot 同步退休。首个错误在完整 drain 当前 batch/send+recv JFC 后返回并 poison
  Fabric，其余 pending waiter 被失败唤醒，但 native ownership 不提前释放。

验证：

```text
cargo fmt --all -- --check                                      PASS
cargo test -p dragonfly-client-storage --features urma urma --lib
13 passed / 0 failed（新增 batch error 后继续 drain 的回归测试）
```

该验证使用本地 UMDK build tree 完成编译、链接和纯单元测试；没有启动真实 provider，因此
`urma_uninit()` 失败注入以及真机 flush batch 仍需后续 native failure-injection/hardware 测试。

### 4.2 Fabric owner progress 与最小命令面

2026-08-26 已完成：

```text
Tokio/storage caller
  | create_lane / bind_lane / post_receive -> UrmaOpHandle / send -> UrmaOpHandle
  | ordinary: bounded semaphore admission + oneshot reply
  | lifecycle: abort/shutdown bypass business admission
  v
single owner thread
  |-- command burst（每轮有界）
  |-- runtime.poll_once（存在 outstanding WR 时持续执行）
  |-- CompletionRouter outstanding WR -> per-operation oneshot
  `-- poison admission gate + teardown
```

全局 `FabricCompletion` channel 已删除。每个 post 的 handle 是唯一逻辑 completion owner；调用方
drop handle 只代表不再等待，router 仍持有 WR/buffer，CQE 到达后照常回收。显式 timeout 除了结束
等待，还会请求 lane `mark_error` 进入 Draining，以 flush outstanding；在 outstanding 清零前
`close_lane` 仍拒绝释放 Jetty。fatal progress error 会先失败唤醒所有 waiter，再继续安全 drain，
因此慢 session consumer 不再能因为共享 channel 堵塞而 poison 整个 Fabric。

这一步只解决“谁拥有 native state、谁推进 CQ、如何把完成事件送回 async caller”，尚未解决远端
是否已 post RECV。`bind_lane` 后的本地 Ready 不能等价为 peer receive-ready；在控制面 credit/barrier
完成前，不应直接用于生产双端发送。

验证结果：

```text
cargo fmt --all -- --check                                      PASS
cargo check -p dragonfly-client-storage                          PASS
cargo check -p dragonfly-client --features urma                  PASS
cargo test -p dragonfly-client-storage --features urma urma --lib
32 passed / 0 failed
```

上述测试覆盖命令/config DTO、per-operation completion/timeout、control timeout、typed peer error、
local lane inflight bound、batch drain、first-failure poison、credit/window 校验和同 lane多 Piece
codec 流程；真实 owner progress、mark-error flush 与 shutdown 仍需 UMDK provider 双端测试确认。

## 5. 下一实现顺序

- [x] 实现 `client::urma` 的 Piece receive stream、最终 `Done` gate、整 Piece timeout，以及
  per-parent persistent Session slot（已完成，2026-08-28）；
- [x] 实现 `server::urma` 的 Storage/RangeReader adapter，按 `next_window_len` 有界读取并发送
  （已完成，2026-08-28）；
- [x] 接 dfdaemon listener/discovery、connection admission、readiness 和 shutdown（已完成，
  2026-08-28）；
- [ ] 接 `piece.rs` 的 URMA + Storage finish 整体 TCP fallback；
- [ ] 用真实 provider 验证一次建 lane 后顺序传输至少 10 个 Piece，以及 mark-error flush/reap；
- [ ] 增加 native failure injection，覆盖 runtime close retry、owner poison、flush batch 和 shutdown；
- [ ] 在真实吞吐基线证明 memcpy 是瓶颈后，把 `ReceivedChunk` backing 换成注册窗口 guard。

不在 `client::urma/server::urma` 的调用契约明确前增加 placeholder public API。
