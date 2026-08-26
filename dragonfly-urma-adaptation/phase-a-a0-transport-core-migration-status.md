# A0：transport core 机械迁移状态

> 日期：2026-08-25  
> 状态：历史记录；源码已迁入 storage module  
> Dragonfly client：`urma-p2p` / `1ccc7d1`  
> URMA demo：`tcp-urma-file-transfer` / `6150f2159736de49cf6588d98ecb7967501a319f`

## 1. 本批结论

> 当前源码位置与边界见
> [storage-aligned revision](./architecture-storage-aligned-revision.md)。本文中的独立 crate、
> public boundary、message/OOB 和测试数量是迁移当时状态，不再描述当前代码。

已在 Dragonfly client workspace 新增独立 crate：

```text
client/dragonfly-client-urma-transport
```

第一批只迁移 URMA native transport foundation、v3 Piece codec 和 CRC32 helper，没有修改
Dragonfly Piece、Storage、dfdaemon、配置或现有 libfabric RDMA 路径。

迁移后的 native core 文件逐一使用 `cmp` 与 demo 目标提交核对，结果完全一致。当前只有以下
Dragonfly workspace 接入文件是新增编写的：

- crate `Cargo.toml`；
- crate `src/lib.rs` 导出边界（A1 又增加 `engine` 导出）；
- crate `MIGRATION.md` 来源和 benchmark 边界；
- workspace member 和 lockfile。

## 2. 已迁移文件

### 2.1 构建和 FFI

```text
build.rs
src/ffi/mod.rs
src/ffi/shim.c
src/ffi/shim.h
src/ffi/wrapper.h
```

保留：

- feature-off 时不探测 UMDK；
- bindgen allowlist；
- pointer-free shim ABI；
- raw handle 单 owner、Drop 和 startup rollback；
- linked WR C arena 与 `bad_wr` 部分提交语义。

### 2.2 native transport core

```text
src/runtime.rs
src/connection.rs
src/jetty.rs
src/jfc.rs
src/buffer.rs
src/completion.rs
src/wr.rs
src/oob.rs
```

保留：

- Runtime/device/EID/Context；
- send/recv JFC、shared JFR；
- registered Segment 和 slot 状态机；
- RC Jetty descriptor exchange、import/bind；
- SEND/RECV post、CQ poll、completion route；
- `user_ctx`、connection ID/generation；
- TX completion 前不复用、RX ownership、drain/shutdown；
- linked SEND/RECV batch 和 CQ moderation；
- completion/slot/queue diagnostics。

### 2.3 Piece codec 和 digest

```text
src/message.rs
src/digest.rs
src/error.rs
```

阶段 A 使用：

- v3 Request/Metadata/Data/End/Error；
- `request_id` 作为 PieceSession ID；
- CRC32 algorithm/value 与 `crc32:<decimal>` 转换；
- frame/version/length/sequence/digest 边界检查。

## 3. 明确没有迁移的 benchmark 代码

以下文件/目录未进入 Dragonfly workspace：

```text
src/benchmark.rs
src/tcp_benchmark.rs
src/urma_benchmark.rs
src/urma_benchmark/
src/file_comparison.rs
src/bin/benchmark.rs
src/bin/parent.rs
src/bin/child.rs
tests/b0_benchmark_cli.rs
demo benchmark/status documents
```

因此没有带入：

- benchmark CLI/scenario matrix；
- fixed-TX/file-to-file profile 选择；
- warmup、计时边界和吞吐比较策略；
- CPU usage/result report；
- standalone FileSource/FileSink；
- output file fresh/cleanup policy；
- TCP sendfile/userspace 基线；
- benchmark regression orchestration。

## 4. core 文件中由 benchmark 驱动加入的能力

不能仅凭“为了 benchmark 加入”判断是否删除，需要看它解决的是实验造数还是生产生命周期。

### 4.1 benchmark-only，暂时隔离保留

| 能力 | 判断 | 当前处理 |
| --- | --- | --- |
| `alias_tx_slots` | fixed-TX/perftest-style immutable payload 复用，Dragonfly 文件 Piece 不需要 | 首次机械迁移保留，不导出给 adapter；provider 等价回归后单独删除 |
| `prepare_aliased_tx_batch` | fixed-TX profile 专用 | 同上，不允许生产调用 |
| legacy v2 Ping/Pong、SHA-256 M4 message | 里程碑诊断，不是 production Piece wire | 暂留用于 lane/codec 回归；生产 adapter 只用 v3 |

首次迁移不立即删除这些路径，是为了避免把 native lifetime 修改和代码搬迁混在同一审查单元。
删除时需要独立 diff，并重新执行 feature-on compile 和真实 provider regression。

### 4.2 由性能实验推动，但属于 transport core

| 能力 | 保留原因 |
| --- | --- |
| linked SEND/RECV WR | 减少 post/doorbell 开销；同时包含部分提交和 WR owner 的正确生命周期 |
| CQ moderation | 决定 TX retirement frontier，既是性能也是 buffer 回收正确性 |
| registered RX lease | 阶段 A copy-RX 暂不用，但后续直写 Storage 的安全 ownership 基础 |
| completion/post-list counters | 生产 timeout、queue stall 和部分提交诊断需要 |
| slot state snapshots | shutdown、credit 和 buffer 泄漏诊断需要 |
| bounded registered pool | 生产必须限制 pinned memory，不是 benchmark policy |
| CRC combine helper | 阶段 A可不用；后续并行 window CRC 可复用，纯算法且无 transport policy |

这些能力不因阶段 A 暂时没有 caller 而删除。

## 5. 当前 public boundary

新 crate 当前导出：

- Runtime config/capability/ABI baseline；
- Connection/Jetty descriptor和状态；
- buffer config/slot状态；
- completion diagnostics；
- v3/legacy message codec；
- CRC32 helper；
- crate 自有 Error/Result。

raw FFI、JFC、Segment、WR handle 仍是 crate-private。下一批 `UrmaEngine`、`PeerLane` 和
`PieceSession` 在同一 crate 内使用这些 private API，不需要扩大 unsafe surface。

## 6. 验证结果

### 6.1 机械一致性

对下列所有迁移文件执行源/目标 `cmp`：

```text
build.rs
buffer/completion/connection/digest/error/jetty/jfc/message/oob/runtime/wr.rs
ffi/mod.rs, shim.c, shim.h, wrapper.h
```

结果：`PASS`。

### 6.2 feature-off

执行：

```text
cargo fmt --all -- --check
cargo check --offline -p dragonfly-client-urma-transport --no-default-features
cargo test --offline -p dragonfly-client-urma-transport --no-default-features
```

结果：

```text
fmt PASS
check PASS
unit 43 passed（包含 A1 新增的 3 个 engine 边界测试）
doc tests 0 passed / 0 failed
```

feature-off 有一个已有 `dead_code` warning：`SlotStateSnapshot::observe` 只在 native path/test
使用。本批不为消除 warning 修改机械迁移文件。

### 6.3 feature-on compile/link

使用：

```text
UMDK_INCLUDE_DIR=/home/yuan/workspace/cloud-native/umdk/src/urma/lib/urma/core/include
UMDK_LIB_DIR=/home/yuan/workspace/cloud-native/umdk/build-urma/lib/urma/core
LD_LIBRARY_PATH=/home/yuan/workspace/cloud-native/umdk/build-urma/lib/urma/core:/home/yuan/workspace/cloud-native/umdk/build-urma/common
```

执行结果：

```text
cargo check --offline -p dragonfly-client-urma-transport --features urma       PASS
cargo test --offline -p dragonfly-client-urma-transport --features urma --no-run PASS
```

本地 UMDK 是 build-tree library；其 `liburma.so` 的 `DT_NEEDED` 包含非标准文件名
`liburma_common.so.SOVERSION`，所以链接测试二进制时需要把 `build-urma/common` 放入
`LD_LIBRARY_PATH`。只传 `UMDK_LIB_DIR` 时的失败已确认是这份 UMDK build tree 的传递依赖，
不是迁移差异。本批不把该本地路径固化进 `build.rs`。

feature-on 当前有较多 `dead_code` warning，因为 native core 已迁入，而 PeerLane/PieceSession caller
尚未实现。后续接入后应显著减少；本批不使用 crate 级 `allow(dead_code)` 隐藏分类问题。

### 6.4 尚未验证

- 没有运行真实 provider；
- 没有创建 Runtime/Jetty；
- 没有验证新 crate 的 OOB Ping/Pong；
- 没有接 Dragonfly Piece/Storage；
- 没有验证 persistent lane 或 sequential Piece；
- 没有进行性能比较。

不能把 feature-on compile/link 标记为 UDMA 行为验证。

## 7. 后续状态

A1 已在不修改上述机械迁移文件的前提下增加 owner thread 骨架，详见
`phase-a-a1-engine-owner-status.md`。下一批是 A2 persistent PeerLane，不修改 `piece.rs`、Storage
或 dfdaemon main。
