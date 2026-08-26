# A1：UrmaEngine owner thread 骨架状态

> 日期：2026-08-25  
> 状态：历史记录；Engine 已收敛为 storage-private UrmaFabric

## 1. 结论

> 当前 owner thread 位于 `dragonfly-client-storage/src/urma/fabric.rs`，不再作为独立 crate
> public API。最新边界见
> [storage-aligned revision](./architecture-storage-aligned-revision.md)。

已新增 `dragonfly-client-urma-transport/src/engine.rs`，把 demo native core 放进符合 Dragonfly
长期结构的进程级执行边界：

```text
Tokio callers
    │ UrmaEngineHandle + bounded command channel
    ▼
dedicated OS owner thread
    │ owns all !Send / !Sync native objects
    ▼
UrmaRuntime → JFC/JFR/Segment → future persistent PeerLane
```

这不是把 demo benchmark runner 再实现一次。owner thread 不感知文件、吞吐、轮次、warmup 或
benchmark profile，只负责 native 资源的创建、命令串行化和逆序销毁。

## 2. 已实现

- `UrmaEngine::start(RuntimeConfig)` 同步等待 owner thread 的 native startup 结果；
- `UrmaRuntime` 在 owner thread 内创建、使用并销毁，不给 native wrapper 添加 `Send`/`Sync`；
- Tokio 调用侧只获得 cloneable `UrmaEngineHandle`；
- 使用容量为 16 的 bounded command channel，避免未来请求无限堆积；
- readiness 状态：`Starting`、`Ready`、`Failed(detail)`、`Stopped`；
- 显式异步 `shutdown()` 发送关闭命令并 join owner thread；
- 最后一个 handle 被 Drop 时也会关闭 channel、等待 owner thread 回收 runtime；
- startup 失败时先由 `UrmaRuntime` 回滚 native resource tree，再向调用方返回原始错误。

当前 command enum 只有 `Shutdown`。这是有意限制：PeerLane、PieceSession 还没有确定命令参数和
返回值之前，不预造 benchmark 风格的 `run_transfer` 大命令。

## 3. 与 demo 的关系

机械迁移的 demo core 保持原样；`engine.rs` 是 Dragonfly integration code，不属于机械拷贝。

保留的真实 transport 能力：

- process-global liburma/runtime ownership；
- device/EID、JFC/JFR、registered memory 生命周期；
- startup rollback、shutdown failure 聚合；
- 后续 PeerLane 使用的 connection/Jetty/WR/CQ primitives。

没有引入的 benchmark policy：

- parent/child runner 和 scenario orchestration；
- fixed-TX/file-to-file profile；
- FileSource/FileSink；
- warmup、计时、CPU 和 throughput report；
- TCP baseline；
- 每轮重新创建 runtime/connection 的执行方式。

## 4. 验证

feature-off：

```text
cargo fmt --all -- --check                                      PASS
cargo check --offline -p dragonfly-client-urma-transport        PASS
cargo test  --offline -p dragonfly-client-urma-transport        PASS
43 passed / 0 failed
```

新增边界测试确认：

- feature-off startup 会在 owner thread 内失败并完成 join；
- command capacity 为 0 时在创建 thread 前拒绝配置。
- `UrmaEngineHandle` 满足 `Send + Sync`，Tokio task 间传递的只有安全 facade。

feature-on：

```text
cargo check --offline -p dragonfly-client-urma-transport --features urma          PASS
cargo test  --offline -p dragonfly-client-urma-transport --features urma --no-run PASS
```

feature-on 只证明当前 UMDK headers/library 可编译链接。尚未在真实 provider 上成功创建 engine，
所以不能把 `Ready`/`Stopped` 行为标为实验确认。

## 5. 下一步：A2 persistent PeerLane

A2 应继续保持架构单向演进：

1. lane 状态和 `PeerId → PeerLane` 映射归 owner thread 所有；
2. OOB 只做 capability/Jetty descriptor/control handshake；
3. 每个 peer 建一个持久 `UrmaConnection`，多个 PieceSession 顺序复用；
4. 第一版只允许一个 peer、一个 lane、一个 active session；
5. 使用 demo v3 Request/Metadata/Data/End/Error，不新增一套 wire protocol；
6. 不接 `piece.rs`，先以 transport crate 内 loopback/双端 harness 验证 lane 建连、复用和 teardown。

A2 不应把 `UrmaConnection` 暴露给 Tokio task，也不应使用 `unsafe impl Send/Sync` 绕开 owner
thread。这样后续扩展多 peer、多 session 时只增加调度和状态，不需要改变 native ownership。
