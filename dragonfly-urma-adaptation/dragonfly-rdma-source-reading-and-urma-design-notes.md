# Dragonfly RDMA 源码阅读与 URMA 映射笔记

> 本文整理自当前对话，重点记录 Dragonfly RDMA 候选分支的数据路径、并发资源模型、Buffer 生命周期、Parent/Child 传输时序，以及对 URMA Lane / Session 设计和性能测试的启发。
>
> **证据说明**
> - `[候选分支源码确认]`：已从当前阅读的 Dragonfly RDMA 候选分支源码确认。
> - `[架构分析]`：基于源码结构与通信语义分析。
> - `[待验证]`：仍需结合具体常量、provider 或运行环境确认。
>
> 注意：本文所述 RDMA 实现来自 Dragonfly RDMA 候选分支，并非 Dragonfly 官方 main 已合入实现。

---

## 1. Dragonfly RDMA 单 Piece 总体调用链

普通 Dragonfly Piece 下载路径可概括为：

```text
Task
↓
Piece::download_from_parent()
↓
Downloader（TCP / QUIC）
↓
PieceContentStream
↓
Storage
↓
CRC32 + pwritev
```

RDMA 候选分支为标准 Piece 增加了一条高性能路径：

```text
Piece::download_from_parent()
↓
download_piece_from_parent_over_rdma()
↓
RDMADownloader::download_piece_stream()
↓
RDMAClient::download_piece()
↓
RDMAStreamReader
↓
Storage::download_piece_from_parent_finished_rdma()
↓
registered RX window
↓
CRC / pwrite
↓
metadata::Piece
```

关键区别：

- `RDMADownloader` 仍实现原有 `Downloader` trait。
- trait 兼容路径会把 `RDMAStreamReader` 适配成 `PieceContentStream`。
- 标准 Piece 快路径不会走这个 trait adapter，而是直接返回 `RDMAStreamReader` 给 RDMA-specific Storage。
- 这样可以尽量保留 registered RX window，避免先 staging 成普通 `Bytes`。

---

## 2. RDMA 下载不是“纯 RDMA”：TCP 负责 rendezvous 控制面

### 2.1 `RDMAClient::download_piece()`

```rust
pub async fn download_piece(
    &self,
    number: u32,
    task_id: &str,
) -> ClientResult<(RDMAStreamReader, u64, String)>
```

外层主要负责：

```text
piece_timeout
↓
handle_download()
```

`handle_download()` 会：

1. 建立 TCP 连接；
2. 发送 RDMA Request；
3. 等待 Parent Ready；
4. 准备 RX registered buffer；
5. spawn 真正的 `receive_stream()`；
6. 立即返回 `RDMAStreamReader + offset + digest`。

因此：

```text
TCP
= control / rendezvous

Fabric
= Piece 数据面
```

控制帧主要包括：

```text
Request
Ready
RecvPosted
Done
Error
```

Piece payload 不通过 TCP 传输。

---

## 3. Request / Ready 参数协商

Child 在 Request 中带上：

```text
task_id
piece_number
chunk_size
max_inflight_chunks
capability
client_endpoint
tag
```

其中：

- `chunk_size`：一次 tagged SEND/RECV 的 payload 大小。
- `max_inflight_chunks`：一个 window 中允许同时 outstanding 的 chunk 数。
- `client_endpoint`：Child Fabric endpoint 地址。
- `tag`：本次 Piece transfer 的 base tag。

最终 chunk size 会取：

```text
min(
    配置 chunk_size,
    Fabric max_msg_size
)
```

Parent Ready 返回：

```text
offset
length
digest
chunk_size
max_inflight_chunks
server_endpoint
```

Child 会校验 Parent 不能返回比自己声明能力更激进的参数。

---

## 4. Piece、Chunk、Window 三层关系

一个 Piece 会被切成多个 chunk：

```text
Piece
├─ chunk 0
├─ chunk 1
├─ chunk 2
└─ ...
```

计算：

```text
chunk_count = ceil(piece_length / chunk_size)
```

一个 receive window 最多容纳：

```text
max_inflight_chunks
```

个 chunk。

因此：

```text
window_capacity
≈ chunk_size × max_inflight_chunks
```

例如：

```text
Piece = 64 MiB
chunk = 64 KiB
max_inflight = 64

window = 4 MiB
Piece 一共 1024 chunks
每个 window 64 chunks
```

---

## 5. Child：`receive_stream()` 数据接收主循环

核心逻辑：

```text
准备 window A
├─ post_recv chunk 0..N
└─ RecvPosted(A)

准备 window B
├─ post_recv 下一批 chunk
└─ RecvPosted(B)

等待 window A 全部 recv completion
↓
window A -> RDMAStreamReader -> Storage

补下一 window
↓
等待 window B
...
```

### 5.1 关键状态

```rust
let mut posted: VecDeque<PostedWindow>;
let mut posted_chunk = 0;
let mut drained_chunk = 0;
```

含义：

- `posted_chunk`：已经 post RECV 的 chunk 数量。
- `drained_chunk`：已经完成 CQE 且 window 已交给上层的 chunk 数量。
- `posted`：已经 post，但还未完全 drain 的 receive windows。

外层循环：

```text
while drained_chunk < chunk_count
```

因为“已经 post 完”不代表“已经收完”。

---

## 6. Child 双 receive-window pipeline 的设计意图

候选实现最多维持两个 receive windows。

目的不是多建连接，而是隐藏控制面 RTT：

单 window：

```text
post A
↓
RecvPosted A
↓
Parent SEND A
↓
A 完成
↓
再 post B
↓
RecvPosted B
↓
Parent SEND B
```

中间存在控制面空洞。

双 window：

```text
post A
RecvPosted A

post B
RecvPosted B

Parent:
SEND A
紧接着 SEND B
```

因此：

> Child 双 window pipeline 主要用于隐藏 `RecvPosted` 控制面 RTT，让 Parent 尽量持续发送。

---

## 7. `RecvPosted` 本质是显式 RX Credit

Child 必须：

```text
先 post 所有 RECV
↓
再发送 RecvPosted
```

不能反过来。

否则：

```text
Parent 收到 RecvPosted
↓
立即 SEND
↓
Child RECV 尚未真正 post
```

会产生竞态。

因此 `RecvPosted` 的语义是：

> “这些 chunk 对应的 receive buffer 已经真实可用。”

也可以理解为：

```text
Child grants receive credit
```

---

## 8. `post_recv()`：逻辑 RECV 到 libfabric `fi_trecv`

调用链：

```text
receive_stream
↓
Fabric::post_recv()
↓
Fabric::post(...)
↓
dfrdma_trecv()
```

每个 chunk 一个 tagged RECV。

核心参数：

```text
registered buffer
local offset
len
tag + chunk index
context
```

最终语义：

```text
fi_trecv(
    window_base + local_offset,
    len,
    mr_desc,
    tag + chunk,
    ctx_addr
)
```

---

## 9. `CtxBlock` / `ctx_addr` / `PendingOp`

每次 post 之前：

```rust
let ctx = Box::new(CtxBlock(...));
let ctx_addr = &*ctx as *const CtxBlock as usize;
let (tx, rx) = oneshot::channel();
```

然后先插入：

```text
pending[ctx_addr] = PendingOp
```

再调用 provider post。

这是一个重要并发不变量：

```text
pending insert
必须早于
fi_trecv / fi_tsend
```

否则 completion 可能极快返回，而 progress thread 找不到对应 operation。

`PendingOp` 中持有：

```text
id
oneshot tx
_ctx
_buf
```

其中：

- `_ctx`：保证 completion context 地址在 CQE 前一直有效。
- `_buf`：保证 registered buffer 在 provider operation completion 前一直存活。

---

## 10. `OpHandle` 的职责

post 成功后返回：

```text
OpHandle {
    ctx_addr,
    id,
    rx,
    inner,
    armed
}
```

它不是 buffer ownership 本身，更像：

> 某一次 provider operation 的 async completion ticket。

后续：

```text
Fabric::wait(op)
```

等待它完成。

---

## 11. CQ Progress Loop

完整闭环：

```text
fi_trecv / fi_tsend
↓
provider / NIC
↓
CQE
↓
progress_loop()
↓
dfrdma_cq_read_batch()
↓
completion.context
↓
pending.remove(ctx_addr)
↓
Completion { len, err }
↓
op.tx.send(...)
↓
Fabric::wait()
```

### 11.1 批量 CQ Poll

```text
CQ
↓
一次最多读取 CQ_BATCH_SIZE 条 completion
```

这是批量轮询，而不是每次只读一条。

### 11.2 completion correlation

post 时：

```text
ctx_addr -> provider context
```

CQE 返回：

```text
entries[index].context
```

然后：

```text
pending.remove(context)
```

找到对应 `PendingOp`。

### 11.3 completion length

CQE 的：

```text
len
```

被封装成：

```text
Completion { len, err }
```

最终返回给：

```text
receive_stream()
```

用于校验：

```text
actual len == expected len
```

---

## 12. CQ Polling 策略

`progress_loop()` 不是纯 busy poll，也不是纯 event-driven。

策略：

```text
有 completion
→ 紧凑继续 poll

没有 pending op
→ idle sleep

有 pending op，但暂时无 CQE
→ yield 若干轮
→ 再短 sleep
```

属于 adaptive polling。

---

## 13. `Fabric::wait()`：正常 completion、错误和 timeout

正常：

```text
oneshot rx
↓
Completion(err == 0)
↓
返回 completion.len
```

provider error：

```text
Completion(err != 0)
↓
fi_error(...)
```

Fabric shutdown：

```text
PendingOp sender 被 drop
↓
oneshot receiver closed
↓
"rdma fabric is shut down"
```

---

## 14. timeout 后为什么不能直接释放 buffer

timeout 只代表：

```text
规定时间内没等到 completion
```

不代表：

```text
NIC / provider 已经停止访问 buffer
```

因此流程是：

```text
timeout
↓
op.cancel()
↓
等待 pending(ctx, id) 消失
↓
意味着 cancel completion 或 late completion 被 reap
```

如果在 grace period 内仍无法确认 operation 终止：

```text
fail_and_abort(endpoint)
```

即：

> 宁可 retire 整个 endpoint，也不能冒险复用可能仍被 DMA 的 buffer。

---

## 15. 为什么 `ctx_addr + op_id` 两个标识都存在

`ctx_addr`：

```text
provider completion correlation key
```

`op_id`：

```text
Rust 侧 generation / identity
```

原因是 heap 地址理论上可以被复用。

例如：

```text
旧 op:
ctx_addr = 0x1234
id = 10

新 op:
ctx_addr = 0x1234
id = 900
```

因此 cancellation / pending check 需要：

```text
ctx_addr + op_id
```

避免 stale handle 混淆。

---

## 16. Parent 发送主路径：`handle_piece()`

Parent 流程：

```text
收到 Request
↓
查 Piece metadata
↓
协商 chunk / inflight
↓
resolve Child Fabric endpoint
↓
upload bandwidth limiter
↓
open_piece_source()
↓
申请 registered TX staging ring
↓
先填第一个 window
↓
发送 Ready
↓
循环等待 RecvPosted
↓
post_send current window
↓
wait SEND CQE
↓
同时准备 next window
↓
全部发送完成
↓
Frame::Done
```

---

## 17. Parent PieceSource

`open_piece_source()` 有两条路径：

```text
mmap enabled
↓
map_upload_piece()
↓
PieceSource::Mapped

否则 / mmap失败
↓
upload_piece()
↓
PieceSource::Reader
```

Persistent Piece / Persistent Cache Piece 也复用已有 Storage upload reader。

因此 RDMA 并没有重新实现 Dragonfly Storage 语义，而是复用已有 Piece source。

---

## 18. `PieceSource::fill()` 确认 Parent copy 边界

### 18.1 Mapped

```rust
dst.copy_from_slice(src);
```

明确是：

```text
mmap file-backed memory
↓
CPU memcpy
↓
registered TX window
```

所以：

> mmap 并不是 mmap 文件页直接 RDMA 发出。

### 18.2 Reader

```rust
reader.read_exact(dst).await
```

当前层直接把数据读入 registered TX buffer：

```text
Storage AsyncRead
↓
registered TX window
```

是否底层还有其它中间 buffer，需要继续看具体 Reader 实现，不能只凭这一层断言完全零中间 copy。

---

## 19. Parent 双 registered TX ring

单 window：

```text
fill A
↓
SEND A
↓
wait SEND CQE
↓
refill A
```

双 ring：

```text
A 正在 SEND
||
CPU fill B

B 正在 SEND
||
CPU refill A
```

因此 Parent 双 TX ring 的主要目的：

> 隐藏 source.fill / memcpy / read 的时间，让数据准备和网络发送 overlap。

注意和 Child 双 receive-window 的目的不同：

```text
Child 双 RX window
→ 隐藏 RecvPosted RTT

Parent 双 TX ring
→ 隐藏 source fill/copy 开销
```

---

## 20. SEND completion 与 RECV completion 不是同一个语义

Parent：

```text
SEND CQE
→ 当前 TX ring half 可以重新覆盖写入
```

Child：

```text
RECV CQE
→ provider 不再写当前 RX window
→ 可以把 window 交给 Storage
```

但 Child RX window 还要经过：

```text
CRC
pwrite
Storage consume
```

之后才能真正回 pool。

因此：

```text
RECV CQE 到达
≠ RX buffer 可以立即重新 post
```

---

## 21. `Done` 的真实语义

Parent 在所有 window 的 local SEND completion 都 wait 完之后：

```text
Frame::Done
```

但：

```text
Parent SEND completion
≠ Child local RECV completion
```

因此 Child 即使先收到 Done，也仍然必须等待自己剩余的 RECV CQE。

---

# 22. Registered Buffer Pool 与全局预算

`Fabric::acquire_buffer(len)` 的核心资源：

```text
pool
budget semaphore
```

多个 Piece 共享同一个 Fabric，所以也共享：

```text
registered buffer pool
registration budget
```

流程：

```text
acquire_buffer(len)
↓
pool.take_best_fit(len)
├─ 有 → 直接复用
└─ 无
    ↓
try acquire budget permits
├─ 成功 → register_buffer(len)
└─ 不足 → 等 pool changed / permit / failure
```

---

## 23. `max_registered_bytes` 到底是什么

重要结论：

> `max_registered_bytes` 是 Fabric 级注册内存预算配置，不是当前剩余可用内存，也不是单次下载大小。

例如 Parent：

```rust
if window_capacity * 2 <= self.max_registered_bytes {
    ring_windows = 2;
}
```

这个判断只说明：

```text
从配置总容量上
一个双 window ring 是合法的
```

并不能保证：

```text
当前并发情况下
一定还有足够 free budget
```

真正运行时是否能拿到注册内存由：

```text
budget semaphore
```

决定。

---

## 24. 为什么 Child 第二个 window 用 `try_acquire_buffer()`

假设全局 budget 只能支持两块 window：

```text
Piece A 持有 1 块
Piece B 持有 1 块
```

如果 A/B 都：

```text
持有 1 块
再阻塞等待第 2 块
```

可能产生资源僵局。

因此设计原则是：

> 已经持有注册内存的调用者，额外 pipeline window 必须 non-blocking acquire。

拿不到：

```text
退化成单 window
继续往前传
```

额外 pipeline depth 是优化，不是传输前进所必须的资源。

---

## 25. Parent 和 Child 对注册预算的策略不同

### Parent

Parent 如果决定双 ring：

```text
一次 acquire 2 × window_capacity 的 staging buffer
```

不会先拿 A 再等 B。

如果全局 budget 暂时不够：

```text
等待 acquire_buffer
↓
transfer_timeout
↓
超时返回 BUSY
```

### Child

第一块 RX window：

```text
必须 acquire
```

第二块：

```text
try_acquire
```

拿不到则：

```text
2-window pipeline
↓
自动降为 1-window pipeline
```

---

## 26. Tag Range：多个并发 Piece 的数据面隔离

`next_tag()`：

```rust
tag_counter.fetch_update(..., |next| {
    next.checked_add(TAG_RANGE_SIZE)
})
```

不是一个 transfer 分一个 tag，而是：

> 一个 transfer 一次预留一整段 tag range。

示意：

```text
Piece A:
base = 0
range = [0, TAG_RANGE_SIZE)

Piece B:
base = TAG_RANGE_SIZE
range = [TAG_RANGE_SIZE, 2*TAG_RANGE_SIZE)
```

每个 chunk 使用：

```text
base_tag + chunk_index
```

因此并发 Piece 可以共享同一个 endpoint，但数据消息不会因为 tag 撞在一起。

---

## 27. Tag 与 ctx_addr 是两个不同层次的 identity

```text
tag
= 网络/provider message matching
= 哪个 SEND 匹配哪个 RECV

ctx_addr
= 本机 completion correlation
= 这个 CQE 属于哪个 PendingOp
```

并发模型：

```text
Shared Fabric Endpoint
├─ Piece A tag range
│   ├─ chunk tags
│   └─ local ctx per op
│
└─ Piece B tag range
    ├─ chunk tags
    └─ local ctx per op
```

---

# 28. Fabric teardown 与 DMA 安全

`Fabric::drop()`：

```text
fail_and_abort()
↓
shutdown = true
↓
join progress thread
↓
pool.close()
```

关键原则：

> 必须先终止 provider / endpoint 对 buffer 的访问可能性，再释放内存。

---

## 29. `fail_and_abort()`

如果 endpoint close：

```text
成功
+
provider 明确保证 endpoint close drains outstanding operations
```

才：

```text
pending.clear()
```

释放 pending buffers。

否则：

```text
pending buffers remain quarantined for process lifetime
```

也就是说：

> 无法证明 device 不再访问的内存，宁可泄漏 / quarantine，也不能释放产生 DMA use-after-free 风险。

---

## 30. Pool close

```rust
fn close(&self) {
    closed = true;
    notify_waiters();
    idle.clear();
}
```

三类 buffer：

### 30.1 pending 中

```text
provider 可能还在访问
```

必须先确认 endpoint close/drain。

### 30.2 idle pool 中

```text
没有 outstanding op
```

`pool.close()` 可直接释放。

### 30.3 下游 lease 中

例如：

```text
RDMAStreamReader
Storage CRC/pwrite
```

这些 lease 自己还活着。

pool close 后：

```text
lease drop
↓
不再回 pool
↓
直接销毁 registration
```

---

# 31. 单 Piece 完整时序

```text
Child                                               Parent
-----                                               ------

Piece::download_from_parent()
  |
  |-- RDMADownloader::download_piece_stream()
  |
  | TCP connect -----------------------------------> handle()
  |
  | Request {
  |   piece,
  |   chunk_size,
  |   inflight,
  |   client_endpoint,
  |   tag
  | } --------------------------------------------> handle_piece()
  |                                                 |
  |                                                 | get Piece
  |                                                 | resolve Child endpoint
  |                                                 | open source
  |                                                 | acquire TX ring
  |                                                 | fill first window
  |
  | <--------------------------------------------- Ready
  |
  | acquire RX window
  | spawn receive_stream()
  |
  | post RECV A
  | RecvPosted A --------------------------------->
  |
  | post RECV B
  | RecvPosted B --------------------------------->
  |                                                 |
  |                                                 | post SEND A
  |                 <========= DATA ================|
  |                                                 | concurrently fill B
  |
  | RECV CQE                                        | SEND CQE
  | progress_loop                                   | TX A reusable
  |
  | window A -> RDMAStreamReader -> Storage
  |
  |                 <========= DATA ================|
  |
  | ...
  |
  | <--------------------------------------------- Done
  |
  | wait remaining RECV CQE
  |
  | CRC / pwrite / Piece finish
```

---

# 32. URMA：为什么要 Lane / Session

URMA 设计不应该机械复制 libfabric `Fabric + tag range`，因为 URMA RC Jetty 的连接语义不同。

推荐分层：

```text
UrmaEngine / Runtime
↓
PeerLane
↓
PieceSession
```

一句话记忆：

> Engine 管进程资源，Lane 管 Peer 连接，Session 管一次 Piece。

---

## 33. Engine

Engine/Runtime 管：

```text
device
context
JFC
registered-memory pool
progress thread
全局 transport state
```

它回答：

> “这个 dfdaemon 进程怎么使用 URMA？”

---

## 34. PeerLane

Lane 管：

```text
remote peer identity
Jetty
bind
Ready / Failed / Draining 状态
connection health
重连
peer-scoped transport resources
```

它回答：

> “我和 Parent A 的 URMA 通道是什么状态？”

Lane 是长期资源。

生命周期可能远长于一个 Piece。

---

## 35. PieceSession

Session 管：

```text
task_id
piece_id / piece_number
length / offset / digest
session_id
window
WR
timeout
bytes transferred
Storage consumer
```

它回答：

> “这一次 Piece 传输现在进行到哪了？”

Session 是短生命周期对象：

```text
Piece 开始
↓
Session create
↓
传输完成 / 失败
↓
Session drop
```

Lane 不需要销毁。

---

## 36. Lane / Session 的核心设计意图

如果没有 Lane：

```text
Piece 1
create Jetty
bind
transfer
destroy

Piece 2
create Jetty
bind
transfer
destroy
```

会反复承担昂贵连接成本。

Lane 抽出来后：

```text
PeerLane(A)
Jetty = J1
↓
Session(piece 1)
↓
Session(piece 2)
↓
Session(piece 3)
```

长期复用连接。

因此：

> Lane 首要目的不是支持复杂并发，而是把“连接生命周期”和“Piece 生命周期”分离。

---

## 37. Phase A 为什么仍然只允许 1 active Session

Phase A 可以：

```text
1 PeerLane
↓
最多 1 active PieceSession
```

即：

```text
Piece A 完成
↓
复用同一 Lane
↓
Piece B
```

先不做：

```text
1 Lane
├─ Session A
├─ Session B
└─ Session C
```

这样可以暂时避免：

```text
completion demux
session fairness
RX slot 分配
多 session timeout 相互影响
```

但提前保留 Lane / Session 边界，未来可以自然演进到：

```text
1 Lane : N Sessions
```

---

# 38. RDMA 与 URMA 的结构映射

RDMA 候选：

```text
Fabric
↓
shared endpoint
↓
transfer tag range
↓
Piece transfer
```

URMA 更可能：

```text
UrmaEngine
↓
PeerLane / RC Jetty
↓
PieceSession
```

不能简单认为：

```text
RDMA Fabric == URMA Lane
```

更准确是：

```text
RDMA:
Engine/Fabric
  └─ Transfer

URMA:
Engine
  └─ PeerLane
       └─ PieceSession
```

URMA 多出的 Lane 层主要来自 RC connection semantics。

---

# 39. Dragonfly RDMA 性能参考

候选 RDMA 文档中有一组 one-rail EFA 测试。

参考数据：

| 并发 Piece | RDMA transport-only | CRC32 + write |
|---:|---:|---:|
| 1 | 44.7 Gbps | 22.1 Gbps |
| 2 | 78.9 Gbps | 39.0 Gbps |
| 4 | 122.5 Gbps | 72.0 Gbps |
| 8 | 198.9 Gbps | 118.1 Gbps |
| 16 | 261.0 Gbps | 139.2 Gbps |
| 32 | 277.0 Gbps | 130.1 Gbps |

这说明：

> 单 Piece 不需要、也通常不会直接跑满整个 400G fabric。

更重要的是看：

```text
Throughput vs concurrency
```

---

## 40. “400G 只跑到 50G”怎么理解

不能简单说：

```text
400G hardware
↓
50G application
↓
只有 12.5%，所以实现有问题
```

因为：

```text
Fabric capacity
≠ 单 Session 应用 throughput
```

单 Piece / 单 Session 还受：

```text
source.fill
CRC
Storage write
CQ progress
window pipeline
CPU
内存带宽
```

限制。

Dragonfly RDMA 候选实现本身：

```text
单 Piece transport-only
≈ 44.7G
```

但增加并发后：

```text
32 Pieces
≈ 277G aggregate
```

因此：

> 单 Session 50G 本身并不能说明 transport 有严重问题；真正需要观察的是多 Session/Piece 能否 scale。

---

# 41. 单向 / 双向带宽口径

Dragonfly Piece 数据基本是：

```text
Parent -----------------> Child
```

属于单向数据传输。

URMA `send_bw` 也应优先与这种单向 Parent→Child 口径比较。

双向：

```text
A -----------------> B
A <----------------- B
```

如果工具报告两个方向相加：

```text
380G + 380G = 760G aggregate
```

则不能直接拿 760G 和 Dragonfly 单向吞吐比较。

---

# 42. URMA 性能测试建议：分层验收

不要只测“400G 是否跑满”，建议分四层。

## 42.1 Provider baseline

```text
urma_perftest
```

用于确认硬件/provider 极限。

## 42.2 Transport-only

```text
registered TX
↓
URMA
↓
registered RX
```

排除：

```text
文件
CRC
Storage write
```

重点测：

```text
1 / 2 / 4 / 8 / 16 Sessions
aggregate throughput
```

## 42.3 Dragonfly 单 Piece E2E

```text
Storage
↓
TX fill
↓
URMA
↓
RX
↓
CRC
↓
write
```

第一版不应把“接近 400G”作为验收要求。

## 42.4 Dragonfly 多 Piece aggregate

建议：

```text
concurrent Pieces:
1 / 2 / 4 / 8 / 16 / 32
```

关注三条曲线：

```text
Throughput vs concurrency
CPU vs throughput
latency/stall vs concurrency
```

---

# 43. URMA 第一阶段建议性能判断

以下不是硬 SLA，而是初期判断参考：

| 层级 | 第一阶段参考 |
|---|---:|
| URMA Provider baseline | 接近 perftest 能力 |
| transport-only 单 Session | 40G+ 已有意义 |
| transport-only 多 Session | 应明显随并发增长 |
| Dragonfly 单 Piece E2E | 20~30G+ 可接受 |
| Dragonfly 多 Piece | 重点看 aggregate scaling |
| Buffer 生命周期 | 0 UAF / 0 长期泄漏 / 0 hang |
| Lane 复用 | 后续 Piece 不重复建立 Jetty |
| CQ | 无异常 stuck / 极端空轮询 |
| Storage | 可明确定位 CRC/write 是否成为瓶颈 |

---

# 44. 当前阶段最重要的设计结论

1. **不要把 URMA 当成“替换 TCP API”来做。**
2. **Engine / Lane / Session 是生命周期边界，不是为了多抽类。**
3. **Lane 的首要价值是复用 Peer 连接。**
4. **Session 表示一次 Piece transfer。**
5. **Phase A 完全可以 1 Lane : 1 active Session。**
6. **Registered buffer 必须同时考虑 provider lifecycle 和 application lease lifecycle。**
7. **CQE 到达不等于 RX buffer 已可立即重新 post。**
8. **额外 pipeline depth 是性能优化资源，不应成为传输前进的硬条件。**
9. **多个 Piece 的正确性需要独立考虑消息 identity、completion identity 和注册内存预算。**
10. **性能评估应该看并发 scaling，而不是只拿应用吞吐除以“400G”。**

---

# 45. 后续建议阅读 / 验证方向

下一阶段可以继续围绕 URMA 设计验证：

```text
UrmaEngine
├─ Runtime / Device / Context
├─ JFC progress
├─ MR / Segment pool
│
└─ PeerLane
    ├─ Jetty
    ├─ bind / reconnect
    ├─ health
    │
    └─ PieceSession
        ├─ WR lifecycle
        ├─ user_ctx
        ├─ RX lease
        ├─ timeout / drain
        └─ Storage integration
```

重点验证：

- Lane 建立一次后是否可连续复用多个 Piece；
- Session 结束时哪些资源归 Session、哪些继续留在 Lane；
- JFC completion 如何从 `user_ctx` 路由回 Session；
- Session timeout 是否必须摧毁整个 Lane；
- 一个 Lane 后续如何支持多个并发 Session；
- registered memory budget 是否为 Engine 级全局预算；
- 多 Piece 并发下如何避免“持有一个 window 再等第二个 window”的资源死锁；
- transport-only 与 Dragonfly E2E 的性能差距具体落在哪一层。

