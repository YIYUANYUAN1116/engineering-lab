# 架构决策：Dragonfly 长期骨架与 URMA demo 复用边界

> 决策日期：2026-08-25  
> 状态：Superseded（2026-08-25 storage-aligned revision）  
> 适用范围：阶段 A 及后续多 Piece、多 peer、8 节点演进

> 本文记录早期独立 transport crate 决策，已由
> [storage-aligned revision](./architecture-storage-aligned-revision.md) 取代。native ownership
> 原则继续有效；独立 crate、独立 Piece codec/OOB 和公开 Engine 边界不再有效。

## 1. 决策

Dragonfly URMA 适配采用“长期资源骨架先稳定，阶段能力逐步扩展”的方式：

```text
dfdaemon
  -> 进程级 UrmaEngine
  -> peer 级持久 PeerLane
  -> Piece 级 PieceSession
```

阶段 A 就建立这三层 ownership，但只实现：

```text
一个 Runtime/device/EID
一个 peer
一条持久 RC lane
同一时刻一个 active PieceSession
多个 Piece 顺序复用该 lane
```

后续阶段在相同接口和资源层次上增加：

- 多 peer lane registry；
- 同 lane 多 PieceSession；
- session 公平调度；
- 多 Runtime/NUMA shard；
- registered window 直写 Storage；
- 重连、退避和更完整的故障恢复。

这些是容量和策略扩展，不改变 Runtime、Lane、Session 的 ownership。

## 2. 为什么整体架构以 Dragonfly 为准

URMA demo 验证的是 transport foundation 和真实 provider 行为：

```text
UMDK FFI
Runtime/JFC/shared JFR/Segment/Jetty
descriptor exchange/import/bind
SEND/RECV/linked WR/CQE
registered buffer ownership
credit、CRC32、file-to-file
shutdown/drain
```

它没有完整实现 dfdaemon 长期运行时需要的：

```text
Downloader 生命周期
真实 Storage/RangeReader
Piece metadata commit
Parent discovery/cache/backoff
进程级 server + downloader 资源共享
Tokio async adapter
多 Piece/多 peer admission
TCP fallback
daemon readiness/shutdown
metrics/配置/打包
```

因此 demo 不能决定 Dragonfly 的模块和资源层次；但 Dragonfly 也不应重写 demo 已经验证的
native transport。

最终原则：

```text
Dragonfly 决定 architecture / lifecycle / integration contract
URMA demo 提供 native transport implementation / provider evidence
```

## 3. 明确复用与新增边界

### 3.1 从 demo 机械迁移，不重新实现

- `build.rs` 中 feature-off 和 UMDK 定位方式；
- `ffi/`、pointer-free shim ABI 和 bindgen allowlist；
- `runtime.rs` 的设备/Context/JFC/shared JFR/Segment 创建及回滚；
- `connection.rs` 的 Jetty、import/bind、post/poll、drain；
- `buffer.rs` 的 slot/registered-memory 状态机；
- `completion.rs` 和 `wr.rs` 的 CQE、`user_ctx`、generation 校验；
- `jetty.rs`、`oob.rs` 的 descriptor codec 和 handshake；
- `message.rs` 的 v3 Request/Metadata/Data/End/Error；
- linked SEND/RECV WR 和 `bad_wr` 部分提交处理；
- Dragonfly-compatible CRC32 helper；
- 已验证的真实 provider 约束和回归参数。

迁移时允许做：包名、错误类型、日志 facade、配置 DTO 和 module visibility 的适配。未经新的
源码或真机证据，不改变 shared JFR、WR lifetime、completion retirement 和 shutdown 安全规则。

### 3.2 Dragonfly 适配必须新增

- 专用 URMA owner/progress 线程；
- Tokio-safe `UrmaEngineHandle`；
- peer lane registry 和阶段 A 的 single-peer admission；
- `PieceSession` 与 demo v3 `request_id` 的映射；
- `UrmaDownloader -> PieceContentStream`；
- `UrmaServer -> Storage/RangeReader`；
- TCP Piece endpoint capability discovery；
- URMA readiness、failure cache/backoff；
- “URMA transport + Storage finish”整体 TCP fallback；
- dfdaemon 配置、启动、shutdown、metrics 和测试。

### 3.3 阶段 A 不新增

- 不创建第二套 Request/Metadata/Data/End/Error wire protocol；
- 不把 libfabric `Fabric` 抽象强行套在 URMA 上；
- 不重新实现 demo 的 Runtime/buffer/completion/Jetty；
- 不让 transport crate 依赖 Dragonfly Storage；
- 不让 raw UMDK handle 跨 Tokio 线程；
- 不为了阶段 A 并发而修改已验证的 native ownership。

## 4. 稳定模块边界

建议模块关系：

```text
dragonfly-client-urma-transport
  |-- native foundation（由 demo 迁入）
  |-- UrmaEngine owner thread
  |-- PeerLane
  |-- PieceSession transport state
  `-- transport DTO / v3 codec

dragonfly-client-storage
  |-- client::urma::UrmaClient / UrmaPieceStream
  |-- server::urma::UrmaServer
  |-- Storage / RangeReader adapter
  `-- capability discovery registry

dragonfly-client
  |-- UrmaDownloader
  |-- Piece-level attempt/fallback
  `-- dfdaemon startup/shutdown wiring
```

transport crate 不引用 `Storage`、`metadata::Piece` 或 dfdaemon config；适配层不接触 raw UMDK
handle。这个依赖方向后续不改变。

## 5. 稳定接口

接口形状可以在编码时按 Rust 类型收敛，但职责固定为：

```text
UrmaEngine::start(config)
  -> UrmaEngineHandle

UrmaEngineHandle::connect(peer_descriptor)
  -> PeerLaneHandle

PeerLaneHandle::open_piece(request_id, task_id, piece_number)
  -> PendingPiece

PendingPiece
  -> metadata future
  -> bounded body receiver
  -> abort handle
```

阶段 A `connect()` 最多返回一条 lane，`open_piece()` 通过容量为 1 的 admission 串行化。
后续增加 HashMap、多个 lane 或多个 session，不改变调用方看到的 Piece 契约。

Parent 方向同样通过 lane/session command 接口发送 v3 Metadata/Data/End；Storage reader 留在
Tokio server task，不进入 native owner thread。

## 6. demo v3 与 PieceSession 的关系

不另造 session wire header：

- demo v3 `request_id` 就是阶段 A 的 `PieceSessionId`；
- v3 `sequence` 继续表示 Piece 内 Data 顺序；
- Metadata 继续携带 offset、length、digest algorithm/value；
- End 继续校验 total length 和 Data count；
- Error 分 Metadata 前和 Metadata 后传播。

Lane generation 用于本地 `user_ctx`/CQE 和 control handshake。阶段 A 中 TCP control connection、
RC Jetty 和 generation 同寿命；lane 重建后旧 Jetty 已销毁，旧 generation 的本地 completion
不能命中新 lane。除非真实 provider 测试证明存在跨 lane wire 混入风险，否则不扩展 DATA
header。

## 7. 为什么阶段 A 就保留持久 lane

若阶段 A 按 Piece 创建/销毁 Jetty，后续改为持久 lane 时必须重做：

- Runtime/Connection borrow ownership；
- descriptor/control connection 生命周期；
- RX prepost 和 credit 生命周期；
- shutdown/drain 边界；
- server/downloader 的 manager 注入方式。

这不是内部优化，而是基础资源层次变化。为了避免后续推翻，阶段 A 保留持久 lane；但不提前
实现多 session 并发、公平调度和复杂重连。

## 8. 如何控制阶段 A 不膨胀

持久 lane 不等于一次完成生产版 transport。阶段 A 通过硬限制控制复杂度：

```text
peer_count = 1
lane_count_per_peer = 1
active_sessions_per_lane = 1
runtime_shards = 1
RX ownership = copy mode
transport = RC SEND/RECV
piece kind = standard Piece only
```

数据结构保留可扩展身份，例如 peer key、lane generation、request/session ID；调度算法只实现
single-entry admission。后续放宽限制，不重写 ownership。

## 9. 后续变更纪律

以下变更应要求新的源码或真机证据，并记录 ADR：

- shared JFR 改为 non-shared JFR；
- SEND/RECV 改为 READ/WRITE；
- raw UMDK wrapper 增加 `Send/Sync`；
- Runtime 从 owner thread 迁移到 Tokio worker；
- PieceSession 拥有 Context/JFC/registered region；
- buffer 在 completion/drain 前释放；
-取消 TCP fallback 或允许 partial Piece 续传；
- transport crate 反向依赖 Storage/Scheduler。

性能参数如 window、post-list、poll batch 可以根据实验调整，不属于基础架构变化。
