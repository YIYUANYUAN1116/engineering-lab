# 阶段 A：最小 Dragonfly Piece over URMA 闭环实施分析

> 分析日期：2026-08-25  
> 范围：Dragonfly `dfdaemon` 之间的 standard Piece Parent → Child 传输  
> 数据面：UMDK/liburma、RTP + RC、双边 SEND/RECV  
> 目标：形成可嵌入 Dragonfly、可失败回退 TCP、可继续演进到并发 Piece 的最小正确性基线

> 2026-08-25 修订：实现模块已改为 `dragonfly-client-storage::urma`，不再使用独立
> transport crate、自有 v3 Piece codec 或私有 OOB。完成定义继续有效；源码结构以
> [storage-aligned revision](./architecture-storage-aligned-revision.md) 为准。

> 2026-08-26 状态校准：`UrmaFabric`、owned `UrmaLane`、persistent client/server Session、
> `DFUR` v1 control protocol、receive credit 和 completion/drain 已实现。文中的运行时结构和
> A0～A4 状态已按当前源码更新；未完成部分主要是 client/server Storage adapter、dfdaemon
> wiring 和真实 provider 验证。

> 2026-09-04 状态覆盖：本文继续作为 Phase A 最小闭环的历史设计基线。A0-A4 已完成并被 Phase B/B8
> production path 取代：当前 RX 为 registered window direct-write + CRC32，TX 为 mmap/RangeReader
> direct-fill，同一 persistent lane 已支持并发 Piece 和 native RX window concurrency。本文第 3 节中
> “不支持多 peer/并发 Piece”等条目描述的是 Phase A 范围，不是当前实现限制。最新 correctness、性能和
> 未完成验证项以 Phase B 文档与 B7 性能台账为准。

## 1. 结论

本阶段遵循 [架构决策：Dragonfly 长期骨架与 URMA demo 复用边界](./architecture-decision.md)。
Dragonfly 决定长期运行的 Fabric/Lane/Session 资源层次；`urma-transport-lab` 的
`tcp-urma-file-transfer` 分支提供 native transport 实现和真实 provider 证据。阶段 A 不重新
实现 demo 已有的 FFI、Runtime、Connection、buffer 和 completion；Dragonfly Piece control
使用 storage-aligned `DFUR` v1，而不是搬入 demo 的文件传输协议。

阶段 A 不是把 `urma-transport-lab` 作为依赖接进 Dragonfly，也不是把候选 RDMA
`Fabric` 的 FFI 函数替换成 liburma。需要完成三层工作：

```text
Dragonfly Piece / Storage 接缝
        + URMA async adapter
        + 已验证 native transport foundation
```

阶段 A 的最小生产对齐结构是：

```text
一个 dfdaemon
  -> 一个进程级 UrmaFabric
  -> 一个专用 URMA owner/progress 线程
  -> 一个远端 peer
  -> 一条持久 RC duplex lane
  -> 同一时刻一个 standard PieceSession
  -> 多个 Piece 可以顺序复用该 lane
```

首版采用 copy-RX：

```text
registered RX slot
  -> RECV CQE
  -> 按 lane/sequence/length 路由并校验 completion
  -> copy 为 owned Bytes
  -> PieceContentStream
  -> Dragonfly 现有 Storage CRC32 + pwrite 路径
  -> release/repost RX slot
```

这样会比候选 libfabric RDMA 的 registered-window 直写多一次内存 copy，但能把 DMA buffer
生命周期与 Tokio/Storage blocking write 生命周期隔开，是阶段 A 最小且可验证的边界。零拷贝
RX lease、CRC/pwrite 直接读取 registered window 留到后续阶段。

## 2. 阶段 A 的完成定义

### 2.1 必须完成

1. `dfdaemon` 使用独立 Cargo feature `urma` 编译；feature-off 不需要 UMDK。
2. 一个进程只能初始化一套 URMA Runtime，server 和 downloader 共享同一个 manager。
3. Child 通过 Parent 已知的 TCP Piece 地址发现 URMA capability 和 control port，不修改
   Scheduler/Manager protobuf。
4. Child 和 Parent 建立一条 RTP + RC duplex Jetty lane，使用真实 shared JFR。
5. 一个 lane 同时最多承载一个 standard Piece，但完成后可以顺序传输后续 Piece，不按 Piece
   重建 Context/JFC/JFR/Jetty。
6. Parent 使用真实 Dragonfly Storage 查询 Piece metadata，并通过现有 `upload_piece()` /
   `RangeReader` 读取 Piece range。
7. Child 在收到 Metadata 后返回与现有接口同形的
   `(PieceContentStream, offset, digest)`，不等待完整 Piece。
8. Data 经 URMA SEND/RECV 进入 registered RX slot，CQE 后转成 owned `Bytes`，由现有
   Storage 写入并计算 CRC32。
9. End、实际长度、sequence、Parent digest、Child Storage digest 全部一致后，Piece 才成功。
10. discovery、建连、Metadata 前失败、Data 中途失败、End/length/digest 错误和 Storage 写入
    失败，都必须使本次 URMA Piece 失败并重新走完整 TCP Piece 下载。
11. URMA 初始化或 server listener 失败不能使 dfdaemon/TCP Piece server 退出。
12. 支持显式 shutdown：停止 admission、终止 session、排空或隔离 WR、unbind/delete Jetty、
    unregister Segment、delete JFC/Context、`urma_uninit()`。

### 2.2 验收场景

至少通过以下场景，才能标记阶段 A 完成：

| 场景 | 验收条件 |
| --- | --- |
| feature-off build/test | 不查找 UMDK header/library，Dragonfly TCP/QUIC 行为不变 |
| feature-on compile | 使用指定 UMDK include/lib 成功编译和链接 |
| 4 MiB standard Piece | 两节点真实 provider 下载成功，length/digest 正确 |
| 64 MiB standard Piece | 多 DATA message，未聚合整 Piece，length/digest 正确 |
| 10 个 Piece 顺序复用 lane | Runtime/Jetty 不重建，所有 Piece 正确完成 |
| Parent 无 Piece | URMA 明确返回 NOT_FOUND，上层按现有 parent retry/fallback 处理 |
| URMA 未启用或不可达 | discovery fail-closed，完整 Piece 走 TCP |
| Data 中途断链 | partial Piece 标记重置，随后完整 TCP 下载成功 |
| 错误 sequence/length/digest | 不产生伪 EOF，不提交错误 Piece，TCP 重下成功 |
| dfdaemon shutdown | 无仍可 DMA 的 buffer 被释放，无永久卡住的 join |

吞吐不是阶段 A 的完成条件；真机吞吐只作为回归观测，不能以弱化 completion、CRC、Storage
或 shutdown 安全换取数字。

## 3. 明确不在阶段 A 做的内容

- 不支持多 peer；
- 不支持并发 PieceSession；
- 不支持同一 lane 的公平调度；
- 不验证两个节点同时互相下载的双向建连竞态；
- 不支持 persistent Piece 和 persistent-cache Piece；
- 不做 registered RX window 直写 Storage；
- 不做动态 mmap registration 或文件页直接注册；
- 不做 URMA READ/WRITE、remote Segment 或 UBS Memory；
- 不做多 NIC、NUMA shard、双 lane 或 8 节点拓扑；
- 不修改 Scheduler、Manager 或 Peer protobuf；
- 不承诺认证、跨租户隔离和生产安全模型已经完成；
- 不以 demo benchmark CLI、FileSink 或独立 parent/child binary 作为生产接口。

## 4. 当前源码事实和必须解决的差距

### 4.1 Dragonfly 已有稳定接缝

`[源码确认]` 当前 `Downloader` 的 standard Piece 契约是：

```text
download_piece(addr, number, host_id, task_id)
  -> Result<(PieceContentStream, offset, digest)>
```

`PieceContentStream` 是 `BoxStream<'static, io::Result<Bytes>>`。Storage 已经负责：

- 按 Piece offset 写 task content；
- 流式 CRC32；
- expected length/digest 校验；
- Piece metadata finished；
- 写失败时的错误传播。

因此阶段 A 不应复制一套 Storage sink。URMA downloader 只需可靠地产出 metadata 和 byte
stream。

`[候选分支源码确认]` 当前 libfabric RDMA 分支已经提供可参考的：

- TCP Piece port capability discovery；
- Parent capability readiness registry；
- downloader capability cache和失败退避；
- server Piece namespace 查询、RangeReader/mmap source；
- URMA/RDMA 失败后重新走 TCP 的 Piece-level 接缝；
- daemon 中 optional server 不影响 TCP 服务的启动方式。

### 4.2 demo native owner 模型不能直接进入 Tokio

`[源码确认]` demo 的 `UrmaRuntime` 内含 `PhantomData<Rc<()>>`，明确是 `!Send/!Sync`；
`NativeRuntime`、JFC、JFCE、Segment、Jetty、WR owner 也采用相同约束。

`[源码确认]` `UrmaConnection<'runtime>` 同时持有：

```text
&'runtime mut UrmaBufferPool
&'runtime send JFC
&'runtime recv JFC
&'runtime JFCE
Jetty
CompletionPoller
```

这保证 demo 中 native 对象由单线程按严格顺序销毁，但无法把 Runtime 放进
`Arc<Mutex<_>>` 后交给 Tokio worker，也无法让 Runtime 与借用它的 Connection 一起组成普通
self-referential manager。

`[设计决定]` 阶段 A 不给 raw handle 或这些 wrapper 直接添加 `unsafe impl Send/Sync`。
采用专用 owner 线程：native Runtime、Connection 和全部原始 handle 从创建到销毁始终位于
该线程；Tokio 侧只持有 channel sender、oneshot receiver 和纯 Rust DTO。

这是阶段 A 相比 standalone demo 的第一项实质重构，也是此前粗略工期最容易低估的部分。

### 4.3 libfabric Fabric 不能作为通用 backend trait 直接复用

`[源码确认]` 候选 `Fabric` 假设一个共享 `FI_EP_RDM` endpoint，以 provider address + tag
路由并发 transfer。URMA/UDMA 使用 RC Jetty，需要 descriptor exchange、import、bind 和
connection generation。

阶段 A 可以复用 RDMA 上层 Piece/控制面思路，但 URMA transport manager、lane 和消息路由
必须独立实现。现在就抽象一个同时覆盖 libfabric tag 和 URMA RC 的“大一统 Fabric trait”会
把两套不同语义塞进错误抽象，阶段 A 不做。

## 5. 阶段 A 运行时结构

```text
Tokio / dfdaemon
|
|-- UrmaTransportManagerHandle  (Send + Sync)
|     |-- bounded ordinary-command admission
|     |-- non-dropping abort/shutdown path
|     |-- health/readiness snapshot
|     `-- shutdown handle
|
|-- UrmaDownloader
|     `-- metadata oneshot + PieceContentStream receiver
|
|-- UrmaServer
|     `-- Storage/RangeReader + lane send commands
|
`-- dedicated urma-engine OS thread
      |-- UrmaRuntime                 (!Send, thread-owned)
      |-- send/recv JFC + shared JFR
      |-- RegisteredBufferPool
      |-- HashMap<u16, UrmaLane>       (已实现 owned lane)
      `-- CompletionRouter            (per-WR oneshot routing)

Tokio/storage side
      |-- UrmaClientSession / UrmaServerSession（已实现）
      |-- 同一 lane 最多一个 active Piece
      `-- client::urma / server::urma adapter（待实现）
```

### 5.1 `UrmaFabricHandle`（已实现）

Tokio 可见的 manager 只暴露异步命令，不暴露 raw UMDK 类型：

```rust
pub struct UrmaFabricHandle {
    command_tx: mpsc::UnboundedSender<CommandEnvelope>,
    command_slots: Arc<Semaphore>,
    readiness: watch::Receiver<FabricReadiness>,
}

enum FabricCommand {
    CreateLane { config, reply },
    BindLane { lane_id, descriptor, reply },
    PostReceive { lane_id, sequence, completion, reply },
    GrantSendCredit { lane_id, count, reply },
    Send { lane_id, bytes, sequence, completion, reply },
    CloseLane { lane_id, reply },
    AbortLane { lane_id, reply },
    Shutdown { reply: oneshot::Sender<Result<()>> },
}
```

具体枚举可以在实现中收敛，但必须满足：

- 普通业务命令在发送前必须取得有界 semaphore permit，并由 queued envelope 持有到 owner 取走；
- abort、timeout cleanup 和 shutdown 不受普通命令 admission 阻塞，不能因业务队列已满而丢失；
- 每个需要确认 native ownership 转移的命令有 reply；
- reply success 表示 post/状态转换已被 engine 接受，不等于 Piece 成功；
- channel 关闭能使所有 pending metadata/body waiter 收到明确错误。

### 5.2 owner/progress loop（已实现）

owner 线程不能在 Tokio channel 或 Storage I/O 上无限阻塞。建议循环：

```text
1. 无 outstanding WR 时阻塞等待 FabricCommand
2. 有 outstanding WR 时每轮有界处理 command burst
3. poll shared send/recv JFC（UMDK 单次最多 poll 16 CR）
4. 校验 CR status/opcode/user_ctx/generation 并退休 WR/slot
5. 直接完成对应 `UrmaOpHandle` 的 per-operation oneshot
6. active completion 时 yield；empty poll 时短退避
7. 自动关闭 outstanding 清零的 Draining lane；响应 shutdown
```

owner 线程不执行：

- `RangeReader` 文件读取；
- CRC32 或 `pwrite`；
- TCP discovery/control socket I/O；
- Tokio future；
- 会阻塞的业务日志或 metrics export。

### 5.3 `UrmaLane`（已实现，不再是待实现 `PeerLane`）

当前 `UrmaRuntime` 使用 `HashMap<u16, UrmaLane>` 保存 owned lane。每条 `UrmaLane` 保存：

```text
lane id / generation
device capability snapshot
Jetty / imported target / bind state
validated remote receive credits
lane lifecycle state
```

状态机：

```text
JettyCreated -> DescriptorExchanged -> Bound -> Ready -> Draining -> Closed
                                      |        |
                                      +------> Failed
```

Session 和 TCP peer identity 位于 Tokio/storage 侧，不进入 owner-owned native lane。Child 主动连接
Parent；当前 lane token 携带本地 generation，Phase A 不解决双方同时主动建连和 generation rollover。

## 6. Discovery、control plane 和建连

### 6.1 Discovery

复用 Parent 已知的 TCP Piece endpoint，但使用独立 URMA discriminator，避免旧 peer 或 RDMA
frame 把 URMA capability 误解为 libfabric capability。

Advertisement 至少包含：

```text
protocol version
transport kind = URMA
control port
fabric/reachability tag
device transport type
transport mode = RC
selected EID 或建连所需的稳定身份
provider max message size
queue/window capability
```

规则：

- 只有 URMA Runtime、control listener 和 server handler 均 ready 后才 publish；
- shutdown 或 engine failure 立即 clear；
- 旧 peer、错误 version、tag 不同、RC 不支持都 fail-closed；
- discovery 结果可以短期缓存，transport failure 立即失效；
- `device_name` 是本地配置，不应被当作跨节点必须相同的可达性标识。

### 6.2 Lane control connection

阶段 A 建议一条持久 TCP control connection 与一条 RC lane 同寿命：

```text
Child connects URMA control port
 -> DFUR Connect(capability + client Jetty descriptor)
 <- DFUR Connected(server Jetty descriptor)
 <-> import / bind，lane Ready
 -> DFUR Request(PieceKind/task/piece/chunk/inflight)
 <- DFUR Ready(offset/length/digest/chunk/inflight)
 -> post native RECV，再发送 DFUR RecvPosted(window)
 <- URMA SEND/RECV bulk completion；最后收到 DFUR Done
 -> 后续 DFUR Request（顺序复用同一 control connection 和 lane）
```

descriptor 必须通过显式 codec 发送，不能把任意网络字节 reinterpret 为 UMDK struct。沿用 demo
中 pointer-free descriptor 和长度上限校验。

阶段 A 不需要为每个 Piece 再创建 control TCP connection。control connection 断开使 lane 和
当前 session 失败，但不能据此立即释放仍可能被设备访问的 buffer；native completion/drain 才是
释放依据。

## 7. Piece wire protocol

### 7.1 `DFUR` v1 control frames（已实现）

阶段 A 使用独立于 RDMA `DFRD` 和 demo 文件传输协议的 `DFUR` v1：

```text
Connect(capability, client_descriptor)           -> 建立 peer lane
Connected(server_descriptor)                     -> 完成 descriptor exchange
Request(kind, task_id, piece_number, chunk, inflight)
Ready(offset, length, digest, chunk, inflight)
RecvPosted(start_chunk, chunk_count)              -> SEND credit/barrier
Done                                              -> Piece 完成
Error(code, message)                              -> Piece/lane 失败
Discover / Capability                            -> optional capability discovery
```

所有整数固定端序；task ID、digest、error message、descriptor 和整 frame 均设硬上限。Metadata
digest 直接携带 Dragonfly 使用的 digest string。RX credit 采用 `RecvPosted` window：Child 必须先
成功 post native RECV，Parent 验证连续 window 后才向 Fabric grant SEND credit。

### 7.2 bulk wire 与本地 operation identity

bulk bytes 直接由 URMA SEND/RECV 传输，不再嵌套 demo DATA header。Piece 顺序复用且 Phase A
同一 lane 只有一个 active Piece，因此 chunk index 作为本地 operation sequence；它不进入 bulk
payload，而是由 CompletionRouter 与 outstanding WR 一起保存。

```text
control: DFUR envelope + frame payload
bulk:    raw Piece chunk bytes
local:   lane id + generation + operation type + slot -> WR user_ctx
router:  user_ctx -> expected chunk sequence + oneshot waiter
```

接收端依次校验：

1. DFUR envelope magic/version/type/length 合法；
2. `RecvPosted` 与 expected start/count 完全一致且非零；
3. CQE 的 lane、operation、slot 与 outstanding WR 匹配；
4. completion 中保存的 chunk sequence 等于 expected chunk；
5. completion length 等于该 chunk 的协商长度，尾 chunk 单独计算；
6. 所有 chunk 完成后才接受/发送 Done，累计长度由 Metadata shape 决定。

单 WR 大小同时受 provider max message 和 registered slot size 约束；没有额外 DATA header，计算为：

```text
min(configured_slot_size, provider_max_msg_size)
```

## 8. 数据流与背压

### 8.1 Parent TX

```text
UrmaServer async task
 -> Storage::get_piece
 -> Storage::upload_piece / RangeReader
 -> 按 UrmaServerSession::next_window_len 读取 owned window
 -> UrmaServerSession::send_next_window 等待并校验 RecvPosted
 -> Fabric grant remote receive credits
 -> 每 chunk 复制到 TX slot并 post SEND WR
 -> per-operation SEND completion 后回收 TX slot
```

当前 Session 已实现逐 window、逐 chunk post 和 completion wait。linked WR batch、双 window
Storage-read/SEND overlap 留到 benchmark 后；TX slot 在对应 completion retirement 前不得复用。

Parent 只有在收到 Child `RecvPosted` 后才能 grant 并消费对应数量的 SEND credit。abort/shutdown
走 Fabric lifecycle path，不受普通业务 command admission 阻塞。

### 8.2 Child RX

```text
UrmaClientSession 计算下一个 bounded ReceiveWindow
 -> 为 window 中每个 chunk post native RECV
 -> TCP 发送 RecvPosted(start_chunk, chunk_count)
 -> 等待每个 per-operation RECV completion
 -> 校验 lane/sequence/chunk length
 -> registered slot 复制为 owned chunk，再聚合为 owned window Vec
 -> client::urma adapter 转成 Bytes 并投递 PieceContentStream（待实现）
```

`PieceContentStream` 的 channel 必须有界。Fabric owner 不接触 body channel，CQ progress 与
Storage 消费解耦。待实现 client adapter 应：

- 用独立 Tokio transfer task 驱动 Session；
- 使用 bounded channel 投递 owned `Bytes` window；
- channel 满时由 transfer task await，owner thread 仍继续 poll CQ；
- stream drop 时结束 task并丢弃/abort Session，不把半完成 Session 放回 peer cache；
- 每次只在消费侧允许继续推进时请求下一 receive window。

这使内存上界约为：

```text
registered RX slots + 当前 owned window + body channel capacity * window size
```

而不是 Piece length。

### 8.3 Metadata 返回时机

Child adapter 必须先建立 bounded body channel 和 transfer task，再发送 DFUR `Request`。收到并校验
DFUR `Ready` 后，`UrmaDownloader::download_piece()` 应立即返回：

```text
(PieceContentStream, offset, crc32 digest)
```

此时后续 bulk chunk 可以尚未到达。不得等完整 Piece，也不得将完整 Piece 聚合进 Vec；当前
Session 只聚合一个 bounded receive window。

## 9. Dragonfly 集成点

### 9.1 建议新增文件

```text
client/dragonfly-client-storage/src/urma/
  mod.rs
  ffi/mod.rs
  ffi/shim.c
  ffi/shim.h
  runtime.rs
  buffer.rs
  completion.rs
  error.rs
  fabric.rs
  lane.rs
  rendezvous.rs
  session.rs

client/dragonfly-client-storage/src/client/urma.rs
client/dragonfly-client-storage/src/server/urma.rs
```

阶段 A 以迁移/收敛 demo 的 native foundation 为主，不让 production crate 依赖
`urma-transport-lab`。

### 9.2 需要修改的现有文件

| 文件/区域 | 阶段 A 修改 |
| --- | --- |
| storage `Cargo.toml` / `build.rs` | 新增独立 `urma` feature；feature-on bindgen、编译 shim、链接 `liburma` |
| storage `client/mod.rs` | feature gate 导出 `client::urma` |
| storage `server/mod.rs` | feature gate 导出 `server::urma` |
| config `dfdaemon.rs` | 新增 `UrmaServer` 配置、默认值和组合校验 |
| dragonfly-client `Cargo.toml` | `urma = ["dragonfly-client-storage/urma"]` |
| `piece_downloader.rs` | 新增 `UrmaDownloader`，接入 discovery cache/health backoff |
| `piece.rs` | 将“URMA 下载 + Storage finish”作为一个可失败单元；任意错误后重置 partial Piece 并完整 TCP 重下 |
| dfdaemon `main.rs` | 创建一次 shared manager，注入 downloader/server；按 readiness publish/clear capability；参与 shutdown |
| TCP server discovery 分支 | 识别独立 URMA discriminator并返回 URMA advertisement |
| docs/tests/config examples | 增加 feature、运行依赖、配置和真实 provider 验证说明 |

### 9.3 当前配置示例（B6 更新）

```yaml
storage:
  server:
    urma:
      enable: false
      port: 4008
      device: udmac0d1e2
      eidIndex: 1
      fabricTag: supernode-a
      maxRegisteredBytes: 40MiB
      txRegisteredBytes: 8MiB
      maxInflightChunks: 512
      postListSize: 1
      pipelineDepth: 2
      maxConcurrentTransfers: 64
      transferTimeout: 30s
      mmapContent: false

download:
  protocol: urma
```

阶段 A 配置原则：

- `enable` 只控制是否 serve，`download.protocol: urma` 控制是否主动下载；
- 任一角色需要 URMA 时启动同一个 manager；
- `fabricTag` 是 operator 声明的可达域；
- provider message size 当前按固定 64 KiB slot 和 capability clamp，不暴露旧草案的 `messageSize/window`；
- `maxRegisteredBytes` 是 process 预注册总量，`txRegisteredBytes` 是固定 TX 保底，RX 使用余量；默认
  40 MiB/8 MiB 对应 TX 128/RX 512 slots，两个方向必须各至少保留一个 slot；
- `pipelineDepth` 为 1..2，第二窗口使用 non-blocking admission，资源不足退化为单窗口；
- `postListSize` 为 1..64，默认 1；真实 provider 校准前不照搬 demo 的 16；
- 非 Linux 或无 `urma` feature 时，明确记录 URMA disabled，但 TCP 服务继续。

## 10. Piece 失败和 TCP fallback 边界

fallback 必须覆盖完整“传输 + Storage finish”，不能只覆盖 `download_piece()` 返回 Metadata
之前的错误。

```text
mark Piece downloading
 -> try URMA discovery/connect/open/stream/storage finish
 -> success: commit Piece
 -> any error:
      abort URMA session
      reset partial Piece metadata/state
      ensure outstanding local write task已结束
      从 offset 0 重新请求同一完整 Piece over TCP
      TCP storage finish
```

不能在 URMA stream 已部分写文件后，从失败 offset 继续拼 TCP；阶段 A 没有断点续传协议。

错误分类至少包含：

| 错误 | lane 处理 | Piece 处理 |
| --- | --- | --- |
| capability/version/tag 不兼容 | lane 不建立，短期缓存 incompatible | TCP fallback |
| Parent NOT_FOUND | Phase A `reject_piece` 后 retire lane | 返回 parent failure，由现有上层选择其他 Parent |
| Parent BUSY | Phase A 保守 retire lane | 当前 Piece TCP fallback |
| session protocol/length/digest 错误 | 当前 lane 标记 failed（保守） | reset + TCP fallback |
| CQE error / control disconnect | lane failed、停止新 session、drain | reset + TCP fallback |
| Storage write error | abort当前 session；lane 是否保留取决于 drain 结果 | reset；由上层决定 TCP/其他 Parent |
| local Runtime fatal | Fabric Failed、clear advertisement | TCP fallback |

在确认所有 blocking write 已 join 前，不允许启动会写同一 Piece range 的 TCP fallback，避免迟到
URMA write 覆盖 TCP 数据。copy-RX 使 NIC 不再持有交给 Storage 的 Bytes，但 blocking pwrite 的
完成边界仍需遵守。

## 11. Shutdown 顺序

建议顺序：

```text
1. clear URMA advertisement
2. stop accepting control connections / new sessions
3. close Piece body senders，唤醒 metadata/body waiter
4. server 停止读取新的 Piece source
5. 当前 lane进入 Draining，停止 DATA admission和新 credit
6. poll send/recv completion 到完成、flush/error，或达到 drain deadline
7. 对无法证明停止 DMA 的 WR/registration做安全隔离，不提前释放
8. unbind/unimport/delete Jetty
9. unregister Segment / free registered pool
10. delete JFC/JFCE/Context，urma_uninit
11. owner thread退出并 join
```

`Drop` 只能是最后保险，不能作为正常 async shutdown 协议。dfdaemon shutdown handle 应等待
Fabric owner thread 给出明确完成结果。

已落实的底层 shutdown/CQ 约束（2026-08-26）：

- `dfurma_runtime_close` 仅在完整成功时消费 wrapper；任一步返回错误时 Rust owner 仍持有 live
  handle，可重试 close，不得出现“C 已 free、Rust 仍保留 raw pointer”的混合语义；
- `urma_poll_jfc` 已返回的整个 CQ batch 必须逐条路由和退休，单条 flush/completion/protocol
  error 不能让 router 提前返回并丢弃同 batch 后续 WR；
- send JFC 的错误不能阻止同一 progress iteration 继续处理 recv JFC；完成两侧 drain 后再返回
  记录到的首个错误；
- `URMA_CR_WR_FLUSH_ERR_DONE` 是无有效 WR `user_ctx` 的 drain sentinel，真实 outstanding WR 由
  `URMA_CR_WR_FLUSH_ERR` completion 退休，不能混为同一种 CQE。shared-JFC 模式下 sentinel 必须按
  `cr.local_id` 路由到对应 lane；Jetty 与其 owned shared JFR 都已成功置 ERROR、普通 WR 已清零且
  send JFC 的 sentinel 已到达后，才能 unbind/unimport/delete。recv JFC 不产生该 sentinel，只排空
  JFR ERROR 产生的 completion。

## 12. 实施任务拆分

### A0：构建、配置与协议骨架

- 增加 `urma` Cargo feature 和 UMDK 定位逻辑；
- 迁入精简后的 FFI/shim，保持 pointer-free ABI；
- 配置结构、默认值、校验和示例；
- URMA discovery/control/DATA codec 及纯单元测试；
- feature-off CI 基线。

交付门槛：feature-off 全绿；feature-on 在有 UMDK 环境完成 compile/link；codec fuzz-like
边界测试覆盖截断、超长、错误 version 和整数溢出。

### A1：engine owner thread 和 native foundation

- 将 Runtime/JFC/shared JFR/Segment/BufferPool/Jetty 保持在一个 OS thread；
- 定义 manager handle、bounded commands、oneshot replies、health/readiness；
- 迁入 completion/status/opcode/user_ctx 校验；
- 实现明确 startup rollback 和 shutdown；
- 不对 raw wrapper 添加无依据的 `Send/Sync`。

当前进度（2026-08-26）：owner progress loop、普通 create/bind/post/send/close 的 bounded
admission、绕过业务 admission 的 abort/shutdown lifecycle path、per-operation `UrmaOpHandle`、
persistent peer session 和 first-failure poison gate 已实现。每个
outstanding WR 持有独立
oneshot completion；已删除全局 completion event channel。无 outstanding WR 时 owner 阻塞等待
命令；有 outstanding WR 时无需新命令也会持续 poll shared JFC。调用方 drop handle 不释放 native
ownership；timeout 将 lane 置为 Draining/ERROR 并等待 CQE/flush 回收，不假设 liburma 有可靠的
per-WR cancel。当前仍缺 dfdaemon client/server Storage adapter、真实 provider progress/mark-error
flush/shutdown 验证；本地 lane Ready 仍不代表远端已 post RECV。

控制面 foundation 同日完成：通用 frame envelope、Piece request/metadata、ReceiveWindow 和错误码
已从 libfabric 专属 capability/endpoint/tag 中拆出；RDMA v2 wire 顺序由 golden test 固定；URMA
使用独立 `DFUR` v1。session adapter 把 lane handshake 与 Piece protocol 分开：
`Connect/Connected` 只在 peer lane 建立时交换 capability 和双向 Jetty descriptor；随后同一 TCP
control connection/Jetty 可顺序复用 `Request/Ready/RecvPosted/Done` 传输多个 Piece。Child 先 post
RECV 再发送 `RecvPosted`，Parent 逐 window 严格验证后才 grant SEND credit。session Drop/timeout 会
abort lane，最后一个 CQE/flush 退休后 owner 自动 reap Draining lane。

当前 owner 即使在普通 outstanding WR 已清零后，也会为处于 Draining 的 lane 继续 poll shared send
JFC，避免漏掉稍后到达的 `WR_FLUSH_ERR_DONE`；该 fake CQE 不再因无有效 `user_ctx` 被当作 Fabric 级
completion corruption。`WR_SUSPEND_DONE` 保持独立的异常 lifecycle 事件，当前仍按 fatal progress
error 处理，不与正常 retirement 混用。

2026-08-26 完成 adapter 前 production contract 收敛：peer Error 保留 code/message；control
read/write 全部有显式 timeout；request/metadata 改为 owned 返回；server 增加 `reject_piece`；
negotiated inflight 不能超过本地 Jetty send/recv depth。Storage、discovery、全局 buffer admission、
limiter、metrics 和 fallback 仍明确留在 client/server/dfdaemon adapter。上传下载逐段对照和后续
滚动进度见 `rdma-urma-upload-download-path-comparison.md`。

交付门槛：mock/feature-on 生命周期测试；重复启动/失败回滚；无 lane shutdown；有 outstanding
WR 的 drain/timeout 路径可诊断。

### A2：单 peer 持久 lane

- URMA server control listener；
- capability negotiation；
- descriptor exchange、import/bind、READY；
- lane generation；
- shared JFR 初始 RX post和 credit；
- 一个 lane 完成后不销毁，可接收下一个顺序 session。

交付门槛：真实 provider Ping/Pong；连续建链/关闭；同 lane 顺序传输至少 10 个小 frame/session。

### A3：standard Piece server/downloader

- Parent Storage metadata 与 RangeReader；
- 复用 v3 Request/Metadata/Data/End/Error 和现有 OOB credit；
- Child metadata oneshot、bounded body channel、`PieceContentStream`；
- strict session/sequence/offset/length 校验；
- CRC32 digest 映射；
- single active session admission。

交付门槛：4 MiB、64 MiB 和尾部非整 payload Piece 两节点成功；stream 在 Metadata 后立即返回；
内存不会随 Piece length 增长。

### A4：Dragonfly wiring、fallback 和验证

- manager 注入 dfdaemon server/downloader；
- TCP Piece endpoint discovery；
- optional server readiness；
- URMA + Storage finish 的整体 fallback；
- metrics/logging；
- fault injection和真实 provider回归；
- 运维构建/运行文档。

交付门槛：第 2.2 节验收矩阵全部通过，并明确区分 unit、feature-on compile、software/mock 与
真实 UDMA provider 结果。

## 13. 测试计划

### 13.1 不依赖硬件

- protocol encode/decode round-trip；
- frame/task/digest/message length 上限；
- session state machine；
- generation 和迟到消息拒绝；
- strict sequence、offset、short/overlong Piece；
- Metadata 前 Error 与 Metadata 后 stream Error；
- bounded channel backpressure；
- stream early drop；
- command channel关闭；
- startup rollback / readiness clear；
- partial Piece reset 后 TCP fallback；
- feature-off config parse 和默认行为。

### 13.2 feature-on / UMDK 可用但无真实 provider

- bindgen ABI/layout test；
- C shim compile/link；
- Runtime error mapping；
- 不存在设备、错误 device/EID 时 fail-safe；
- server enable 失败不影响 TCP daemon。

mock 或只编译不能标记为 URMA 行为已验证。

### 13.3 两节点真实 UDMA provider

固定记录：

```text
OS / arch
UMDK commit/build
provider/device
EID index
transport mode
message payload / window / post-list
Piece size/count
length/digest
CQE error
fallback result
双方 binary sha256
```

顺序：Ping/Pong、单 4 MiB、单 64 MiB、尾 Piece、10 Piece lane reuse、Child 慢消费、control
断开、Parent kill、错误 digest、正常 shutdown。

## 14. 可观测性最低要求

日志/metrics 至少能回答：

- manager 是否初始化、为何 disabled；
- 当前 lane state/generation/peer；
- session ID、task ID、piece number、offset/length；
- send/recv post、CQE、CQE error、empty polls；
- active/pending TX/RX slot；
- body channel backpressure 次数和时长；
- credit current/granted/returned；
- URMA success/failure/fallback count；
- lane connect/reuse/failure；
- shutdown drain 时间与 outstanding WR。

不要在每个 DATA/WR 热路径输出 info 日志；逐 WR 诊断只在 debug/trace 或失败快照中启用。

## 15. 工作量重新估算

进一步核对 demo 的 `!Send/!Sync` 和 borrow-based connection owner 后，阶段 A 不是简单移植，
需要一次受控的 async adapter/owner-thread 重构。按一名熟悉 Rust、Dragonfly 和 URMA 的工程师：

| 工作包 | 乐观 | 保守 |
| --- | ---: | ---: |
| A0 构建/配置/协议 | 2 天 | 4 天 |
| A1 owner thread/native foundation | 4 天 | 7 天 |
| A2 单 peer 持久 lane | 3 天 | 5 天 |
| A3 standard Piece 数据闭环 | 4 天 | 7 天 |
| A4 fallback/测试/真机 | 4 天 | 7 天 |
| 合计 | 17 人天 | 30 人天 |

即约 3.5～6 人周。环境调度、UMDK/驱动问题和真实设备排队不计入纯开发时间。若阶段 A 只做
“每 Piece 新建连接、单次跑通”的演示，可以缩短，但会把最关键的 manager ownership 和
lane reuse 问题推迟，不能作为后续并发阶段的可靠基础，因此不推荐。

## 16. 推荐开工顺序和第一批代码边界

第一批实现建议只覆盖 A0 + A1，不立刻修改 Piece 主路径：

1. 在 storage crate 增加独立 `urma` feature；
2. 迁入最小 FFI/shim、Runtime capability、shared JFR、registered pool和 completion token；
3. 实现专用 owner thread 与 `UrmaTransportManagerHandle`；
4. 完成无 peer 的 startup/readiness/shutdown；
5. 用 feature-off、feature-on compile 和 lifecycle 测试冻结边界；
6. 再进入 A2 的真实 lane handshake。

这样每一步都有明确可验证产物，同时不提前扰动 Dragonfly 现有 TCP/QUIC Piece 下载路径。
