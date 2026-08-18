# Dragonfly TCP/QUIC Piece 下载连接数分析

## 分析版本

本次结论基于最新拉取的 client：

```text
version: v1.5.0-7-g39d20aa3
commit:  39d20aa3b3753eb5a790b0e9d2e54358807b5d87
```

该版本已经包含 QUIC Connection 复用改动。

## 核心结论

假设同一台 dfdaemon 上同时存在 1000 个活跃下载任务，每个任务配置：

```yaml
download:
  concurrentPieceCount: 16
```

TCP 和 QUIC 的结果不同：

| 协议 | Piece 传输方式 | 1000 × 16 的结果 |
|---|---|---|
| TCP | 每个正在下载的 piece 使用一条独立 TCP 连接 | 最坏约 16000 条瞬时物理连接 |
| QUIC | 复用 QUIC Connection，每个 piece 使用一个双向 stream | 约 16000 个 stream，物理连接数通常远小于 16000 |

Dragonfly 当前没有针对所有任务的全局 piece 并发数或物理连接数硬限制。

因此：

- TCP 连接数会随活跃任务数近似线性增长；
- QUIC stream 数会随活跃任务数近似线性增长；
- QUIC 物理连接数主要随实际访问的父节点地址数增长；
- QUIC 冷启动时仍可能因并发初始化竞态出现短时连接风暴。

## 单任务 Piece 并发限制

每个任务在下载时分别创建 Semaphore：

```rust
let semaphore = Arc::new(Semaphore::new(
    self.config.download.concurrent_piece_count as usize,
));
```

这个 Semaphore 属于单个任务，不是 dfdaemon 全局共享的并发控制器。

总 piece 并发可以近似表示为：

```text
P(t) ≈ Σ min(任务剩余 piece 数, concurrentPieceCount)
```

如果 1000 个任务真正同时活跃，每个任务都有至少 16 个可下载的 P2P piece：

```text
P(t) ≈ 1000 × 16 = 16000
```

Rust 配置结构自身的裸默认值是 8，Dragonfly Helm Chart 将其覆盖成 16。线上实际数值应以 dfdaemon 最终加载的配置为准。

实际并发通常会受到以下因素影响：

- 任务是否真正同时进入下载阶段；
- scheduler 是否已经返回可用 parent；
- 每个任务是否至少还有 16 个未完成 piece；
- piece 是否命中本地存储；
- piece 是从 peer 下载还是直接回源；
- 下载带宽、上传带宽和磁盘吞吐；
- 相同任务之间的下载协调和去重。

## TCP：一个并发 Piece 对应一条连接

最新 TCPClient 仍然只保存配置和服务器地址，不保存底层 TCP socket。每次 piece 请求都会执行：

```rust
tokio::net::TcpStream::connect(self.addr.clone())
```

连接在 piece 内容流读取完成、失败或被取消后关闭。

因此瞬时 TCP 连接数近似为：

```text
C_tcp(t) ≈ 当前正在通过 P2P TCP 下载的 piece 数
```

在上述场景中：

```text
C_tcp(t) ≈ 1000 × 16 = 16000
```

这是瞬时活跃连接数。如果任务包含更多 piece，后续 piece 会继续新建连接，所以任务生命周期内累计建立的 TCP 连接次数可能远大于 16000。

### TCP 的 32 个连接槽不是物理连接上限

TCP downloader 定义了：

```rust
MAX_CONNECTIONS_PER_ADDRESS = 32
```

但是 pool 中复用的是无连接状态的 TCPClient 对象。多个 piece 即使取得同一个 TCPClient，仍然会分别执行 `TcpStream::connect`。

所以该常量不能限制：

- 到单个 parent 的 TCP 物理连接数；
- dfdaemon 的 TCP 总连接数；
- TCP 连接建立速率。

## QUIC：复用 Connection，每个 Piece 使用一个 Stream

最新 QUICClient 持有一个长期 QUIC Connection：

```rust
pub struct QUICClient {
    config: Arc<Config>,
    addr: String,
    connection: Connection,
}
```

创建 QUICClient 时建立 Connection；后续每个 piece 只在已有 Connection 上打开新的双向 stream：

```rust
let (mut writer, reader) = self.connection.open_bi().await?;
```

因此最新版本是：

```text
一个并发 piece ≈ 一个 QUIC 双向 stream
多个 stream 复用一个 QUIC Connection
```

## QUIC 每个父节点最多 32 个连接槽

QUIC downloader 使用父节点地址加随机槽位作为连接池 key：

```rust
format!(
    "{}-{}",
    addr,
    fastrand::usize(..MAX_CONNECTIONS_PER_ADDRESS),
)
```

其中：

```rust
MAX_CONNECTIONS_PER_ADDRESS = 32
```

稳定状态下，同一父节点地址最多保留约 32 个 QUIC Connection：

```text
QUIC stream 数 ≈ 当前并发 P2P piece 数

QUIC 保留连接数
≤ 32 × 实际访问到的不同父节点地址数
```

例如 16000 个 piece stream 都访问同一个 parent 地址：

```text
stream 数：     约 16000
Connection 数：最多约 32
```

如果任务访问大量不同 parent，连接数仍会随着 parent 地址数量增长。32 是单地址限制，不是 dfdaemon 全局限制。

## 每条 QUIC Connection 默认最多 100 个双向 Stream

Dragonfly 当前没有显式调用 `max_concurrent_bidi_streams`，因此使用锁定依赖 `quinn-proto 0.11.15` 的默认值：

```rust
max_concurrent_bidi_streams: 100u32.into()
```

对于 piece 下载，server 允许 client 在一条 Connection 上同时打开约 100 个双向 stream。

同一 parent 地址最多使用 32 条 Connection，因此理论并发 stream 容量大约是：

```text
32 Connection × 100 stream/Connection
= 3200 个活跃 stream
```

当某条 Connection 达到 stream 上限时，新的 `open_bi().await` 会等待已有 stream 关闭，不会继续在该 Connection 上无限创建 stream。

100 是当前 Quinn 依赖的默认行为，不是 Dragonfly 暴露的稳定配置项。依赖升级后默认值可能变化。若它对容量规划很重要，建议由 Dragonfly 显式配置。

## QUIC 冷启动连接竞态

虽然连接池稳定状态下每个 parent 地址最多保留 32 个条目，但首次高并发访问空池槽时存在连接初始化竞态。

当前 Pool 获取条目的过程是：

1. 查询 key 是否已经存在；
2. 不存在时调用异步 factory 创建 client；
3. QUIC factory 创建 UDP socket 并完成 QUIC 握手；
4. 握手完成后才调用 `or_insert` 将 client 放入池中。

如果多个请求同时访问一个尚未初始化的 key，它们可能各自建立 QUIC Connection。最终只有一个 Connection 被池保留，其余 Connection 在竞争失败后被丢弃。

因此需要区分：

```text
稳态保留连接：同一 parent 最多约 32 条
冷启动连接尝试：短时间内可能明显超过 32 条
```

极端情况下，冷启动握手次数可能随同时到来的 piece 请求数增长。这里缺少 per-key singleflight、异步 OnceCell 或初始化锁。

它可能造成：

- UDP socket 和本地临时端口短时上涨；
- QUIC TLS 握手 CPU 峰值；
- parent 端短时 Connection 数上涨；
- 无效连接建立后立即关闭；
- 冷启动延迟和丢包增加。

## Client Pool 的 2000 是软容量

piece downloader 配置：

```text
capacity:     2000
idle timeout: 420 秒
```

pool capacity 不是拒绝新条目的硬上限。当前逻辑只是定期检查是否超过 capacity，并清理没有活跃请求或超过 idle timeout 的条目。

超过 2000 时仍然可以创建新条目。有活跃请求的条目不会被清除，清理本身还有时间间隔，因此突发流量中 pool size 可以超过 2000。

- 对 TCP，pool 保存无连接 TCPClient，不能限制 socket 数；
- 对 QUIC，pool 保存持有 Connection 的 QUICClient，但 2000 仍只是软清理阈值。

## RequestGuard 的作用范围

downloader 会为 pool entry 创建 RequestGuard，防止请求期间删除正在使用的 client entry。

不过下载接口取得 PieceContentStream 后立即返回，RequestGuard 也随函数返回而释放；piece 内容此后才通过返回的 stream 继续读取。

因此 RequestGuard 主要保护“建立请求并取得响应 stream”的阶段，不覆盖完整 piece 内容传输生命周期，也不是并发请求、stream 或连接数限制器。

## 服务端没有应用层全局连接限制

### TCP Server

TCP storage server 使用：

```rust
socket.listen(1024)?;
```

1024 是等待 `accept` 的连接队列 backlog，不是 ESTABLISHED 最大连接数。

连接被 accept 后，服务端直接创建 Tokio task 处理，没有全局连接 Semaphore。遇到 `EMFILE` 等 accept 错误时会记录错误并继续 accept，但这只是容错，不是容量控制。

### QUIC Server

QUIC storage server 持续 accept Connection，为握手和 Connection handler 创建 Tokio task；每个双向 stream 又创建独立处理 task。

没有发现：

- 最大 QUIC Connection 数；
- 全局 stream Semaphore；
- 单 client IP 的 Connection 限制；
- 主动拒绝超过容量的 piece 请求机制。

### gRPC `requestRateLimit`

dfdaemon download/upload gRPC server 的 `requestRateLimit` 限制 gRPC 请求速率，不直接限制 storage TCP 4005 或 QUIC 4006 上的 piece 连接和 stream。

它可能降低新任务进入速度，但不能保证活跃任务总数或 P2P piece 总并发不超过某个值。

## 系统实际瓶颈

Dragonfly 没有全局硬限制时，最终由以下资源兜底：

- dfdaemon 进程 `RLIMIT_NOFILE`/`ulimit -n`；
- 节点系统级 file descriptor 上限；
- TCP ephemeral port 范围；
- TCP `TIME_WAIT` 数量；
- Kubernetes 节点、NAT 网关和防火墙 conntrack 容量；
- UDP socket 和 UDP buffer；
- QUIC Connection、stream 和 TLS 状态内存；
- Tokio task 数量；
- 父节点上传带宽和磁盘吞吐。

16000 条 TCP 连接会在下载端和对应上传端分别占用 file descriptor，还要为 scheduler、manager、dfdaemon gRPC、代理和系统服务预留资源。

## 优化建议

### TCP

如果可能出现数百到上千个并发任务，建议增加 dfdaemon 全局 P2P piece Semaphore，而不是只依赖单任务 `concurrentPieceCount`：

```text
globalPieceConcurrency
= min(FD 预算, 内存预算, 网络预算, parent 承载预算)
```

降低 `concurrentPieceCount` 只能降低单任务峰值，不能消除大量任务并发时的线性叠加。

### QUIC

建议：

1. 保留当前 Connection 多 stream 复用方案；
2. 为 pool 同一个 key 增加 singleflight 或异步初始化锁；
3. 将每地址 Connection 数和每 Connection stream 数改成显式配置；
4. 增加 Connection、活跃 stream、等待 stream permit 和握手次数指标；
5. 如果目标是严格保护节点资源，仍需增加 dfdaemon 全局 stream/piece Semaphore。

## 运行时观测

TCP 可以重点观察：

```bash
ss -s
ss -tan state established
cat /proc/<dfdaemon-pid>/limits
ls /proc/<dfdaemon-pid>/fd | wc -l
cat /proc/sys/net/ipv4/ip_local_port_range
conntrack -S
```

QUIC 还应观察：

- dfdaemon UDP socket 数；
- 当前 QUIC Connection 数；
- 每条 Connection 的活跃 stream 数；
- `open_bi` 等待时间；
- QUIC 握手速率和失败率；
- UDP receive/send buffer error；
- 丢包、重传和 RTT。

## 最终判断

1. **TCP：** 1000 个活跃任务乘以 16 个并发 piece，瞬时连接数接近 16000 的估算成立，连接数会随活跃任务数近似线性增长。
2. **最新 QUIC：** 16000 表示并发 stream 数，不再直接表示物理 Connection 数。
3. **QUIC 稳态连接：** 同一 parent 地址最多保留约 32 条 Connection，每条默认允许约 100 个并发双向 stream。
4. **QUIC 冷启动：** pool 初始化没有 singleflight，瞬时连接尝试可能超过每地址 32 条。
5. **全局上限：** Dragonfly 当前没有全局 piece、TCP Connection、QUIC Connection 或 QUIC stream 硬限制；pool capacity 2000 也不是硬上限。
