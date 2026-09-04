# URMA RM 是否更适合 Dragonfly P2P：连接模型与演进评估

更新时间：2026-09-04。

## 0. 文档目的与当前结论

本文是后续独立研究 URMA Reliable Message（RM）数据面的入口文档。它比较 Dragonfly RDMA 候选实现的
`FI_EP_RDM`、当前 Dragonfly URMA RC persistent lane，以及可能的 process-wide URMA RM backend。

当前结论：

- `[源码确认]` URMA RM 在应用可见的资源模型上确实比 RC 更接近 libfabric `FI_EP_RDM`：一个本地
  JFS/Jetty 可以通过每个 WR 携带的 target 向多个远端发送，共享 JFR 可以接收多个远端的消息；
- `[架构判断]` RM 长期更契合 Dragonfly 动态多 peer、高 fan-out 的 P2P 拓扑，潜在收益主要是减少
  per-peer Jetty/JFR/TP 资源和建链成本，而不是天然提高单 peer 带宽；
- `[设计决定]` 当前不能把 RC 直接替换成 RM。先保留 RC production path，把 RM 做成独立、可协商、
  可回退的实验 backend；
- `[性能判断]` RM 不是从当前 134.25 Gbps 直接到 400 Gbps 的捷径。当前设备上的 RC perftest 已达到
  约 399 Gbps，另有裸 SEND_IMM 约 539 Gbps 的记录，说明首要差距仍可能位于 source-fill、CRC32、
  Storage、allocator、CQ/owner 和 Piece 生命周期；
- `[关键风险]` RM 共享 JFR 没有当前 libfabric tagged receive 的直接等价物。当前按 lane/transfer
  预绑定 RX slot 再发 credit 的设计不能原样复用，必须改成匿名 RX pool 和完成后 demux；
- `[待验证]` 当前服务器不可用，本轮没有进行 RM 真机实验，也不把文档判断登记为 provider 结论。

## 1. 三种连接模型

| 维度 | RDMA 候选 `FI_EP_RDM` | URMA RM 目标形态 | 当前 URMA RC |
|---|---|---|---|
| 本地数据面对象 | daemon 级共享 endpoint | process-wide JFS/JFR 或 RM Jetty | 每 peer 一个 RC Jetty + JFR |
| 远端选择 | provider address + per-transfer tag | 每个发送 WR 指定 imported `tjetty`/`tjfr` | Jetty bind 唯一远端 |
| 接收与 completion | 共享 CQ，tagged receive | 共享 JFR/JFC，CQE 携带 `remote_id`/IMM | per-lane JFR，共享 process JFC |
| 应用视角 | reliable datagram | reliable message、一对多 | reliable connection、一对一 |
| 底层连接 | provider 管理 | provider 可用 native RM/XRC，也可能展开为多个 RC QP | 唯一 peer TP |
| Piece concurrency | 单 endpoint 多 Piece | 单共享队列多 peer、多 Piece | persistent lane 内多 Piece |
| peer 故障隔离 | endpoint 内按 operation/address 处理 | 必须自行实现 target-local retirement | 可退休单独 lane |

### 1.1 RDMA PR #1945 的准确含义

`[源码确认]` PR #1945 的 concurrency=1/2/4/8/16/32 不是多 lane/QP：

- 一个 `Fabric` 封装一个共享 `FI_EP_RDM` endpoint；
- 所有 transfer 共享 CQ 和一个 progress thread；
- 每个 Piece 单独建立 TCP rendezvous connection；
- bulk transfer 使用互不相交的 tag range 在共享 endpoint 上并发。

因此与它最接近的 URMA 结构对照是“一个共享数据面对象上的并发 Piece”，不是现有 B7 的 L8
多 daemon fan-out。L8×CC8 是 URMA 额外的扩展维度，报告时必须同时写 lane 数和每 lane Piece CC。

参考源码：

- `/home/yuan/workspace/dev/client` 的 `origin/urma-p2p`；
- `dragonfly-client-storage/src/rdma/fabric.rs`：共享 endpoint、tag range、单 progress thread；
- `dragonfly-client-storage/src/client/rdma.rs`：per-Piece TCP rendezvous。

### 1.2 URMA RM 的“一对多”不是完全无状态

`[UMDK 文档确认]` RM 允许一个 Jetty/JFS 与多个远端 Jetty/JFR 通信；RC 一个 Jetty 只能 bind 一个
远端。RM 对应用表现为 connectionless，但仍需导入远端 target，底层也可能为目标建立 TP/QP：

- 基于 XRC/native RM 时，同一个本地对象到目标进程可只有一个连接；
- 基于 RC 模拟 RM 时，同一目标可能使用多个 QP，执行序和完成序不一定成立；
- 每个发送 WR 的 `urma_jfs_wr_t.tjetty` 指定目标；
- 接收 CQE 的 `urma_cr_t.remote_id` 标识发送方，`imm_data` 可携带应用路由 identity。

所以 RM 与 `FI_EP_RDM` 是“应用资源模型相近”，不是 wire、matching 和故障语义完全相同。

## 2. 为什么 RM 可能更适合 Dragonfly P2P

Dragonfly daemon 的 parent 由 scheduler 动态选择，同一进程可能在短时间内面对大量 peer。理想 RM
资源树可以是：

```text
process UrmaFabric
  ├─ shared TX JFS / send JFC
  ├─ shared RX JFR / recv JFC
  ├─ registered TX/RX arenas
  ├─ imported target cache
  │    ├─ peer A -> target/generation/outstanding
  │    ├─ peer B -> target/generation/outstanding
  │    └─ peer C -> target/generation/outstanding
  └─ transfer router
       └─ (remote_id, peer_generation, transfer_id, sequence)
```

潜在收益：

1. 避免为每个 peer 固定创建一套 Jetty/JFR 和大 depth queue；
2. 降低 peer churn 时 descriptor exchange、bind/unbind、JPN/TP 和 provider 对象成本；
3. 将队列深度按进程聚合，减少 per-lane capacity 闲置；
4. 更容易对全部 peer 实施全局 admission、公平性和 registered-byte budget；
5. 更接近 Dragonfly 动态 peer graph，而不是把每个 peer 长期映射成独占硬件连接。

这些收益最可能出现在高 fan-out、parent 频繁变化和大量低复用 peer。对于单个稳定热点 peer，当前
RC persistent lane 已支持同 lane 多 Piece，结构更简单且故障域更小，RM 未必更优。

## 3. 不能直接切换的关键原因

### 3.1 共享 JFR 的 receive matching

当前 RC 流程先为某个 `(lane, transfer, sequence)` 分配 RX slot、向该 lane 的 JFR post RECV，再通过
`RecvPosted` 允许指定 sender 发送。每个 JFR 只属于一个 peer，因此 receive buffer 的归属在 DMA 前
已经确定。

RM 共享 JFR 下，任意已获准 peer 的消息都可能消耗下一个 posted RECV。当前 UMDK 公共
`urma_jfr_wr_t` 只有 SGE、`user_ctx` 和 next，没有类似 libfabric receive tag 的 per-WR 匹配字段；
发送方 `remote_id` 和 `SEND_IMM` 要在 CQE 到达后才可见。因此不能把 slot 预先绑定给某个 transfer。

目标模型必须改为：

```text
post anonymous registered RX slots to shared JFR
  -> receive CQE
  -> validate local JFR id and slot generation
  -> identify peer by remote_id
  -> decode transfer_id + sequence from SEND_IMM
  -> dispatch immutable lease to the matching Piece
  -> consumer finishes CRC/write
  -> recycle slot and replenish shared JFR
```

这要求：

- completion key 从 `(lane_id, sequence)` 改为
  `(remote_id, peer_generation, transfer_id, sequence)`；
- transfer ID process-global，或至少与 peer identity/generation 联合唯一；
- RX slot 在 CQE 前保持匿名，CQE 后才转移给 Piece；
- shared posted depth、全局 credit 和 per-peer quota 同时受限；
- unexpected peer、重复 IMM、超 credit 发送和 stale remote ID 必须 fail closed；
- 一个 peer 不得独占 JFR 或消耗其他 peer 的保底 receive capacity。

### 3.2 故障域扩大

当前 RC lane 可以把单独 Jetty/JFR 切到 ERROR，等本 lane outstanding WR、flush-done 和 active lease
全部闭合后退休。共享 RM JFS/JFR 不能因为一个 Piece 或 peer timeout 就整体进入 ERROR，否则一个坏
peer 会中断所有健康 peer。

RM backend 必须明确三层错误：

| 错误级别 | 处理 |
|---|---|
| transfer-local | 只失败/回退一个 Piece，保持 peer target 和共享队列可用 |
| peer-local | 停止向 target 发新 WR；等待该 peer outstanding 清零；unimport 并增加 generation |
| fabric/device | 停止所有 admission；flush/retire 共享 JFS/JFR；重建整个 Fabric |

provider 是否允许在共享对象继续服务其他 peer 的同时可靠完成 target-local retirement，必须用真实
provider fault test 验证，不能只根据 API 形态推断。

### 3.3 flow control 与公平性

共享 JFR 的 posted RECV 是全局资源。仅有一个全局 credit 会允许快 peer 或异常 peer 抢光 RX；仅有
per-peer credit 又可能造成总 credit 超过真实 posted depth。需要至少两级 admission：

```text
global posted-RX permits
  + per-peer quota / deficit scheduling
  + per-transfer pipeline depth
```

TX 同样需要 process JFS depth 和 registered TX slot 的 required-first、公平排队，避免单个 upload peer
长期占据共享 SQ。

### 3.4 ordering 不能作为隐含前提

当前协议已有 `transfer_id + sequence + SEND_IMM` 校验，应继续把乱序视为正常可路由事件，而不是依赖
RM provider 的执行序或完成序。原因是 UMDK 文档明确区分 native/XRC RM 和基于多个 RC QP 实现的
RM，后者不保证执行/完成顺序。

## 4. 性能判断

### 4.1 目前没有“RM 比 RC 快”的证据

当前环境已有 RC 裸能力：

```text
device          udmac0d1e2
mode            URMA_TM_RC
message         64 KiB
JFS / JFR       128 / 512
throughput      47558.59 MiB/s ≈ 399 Gbps
```

后续裸 SEND_IMM 还记录过约 539.24 Gbps。由此可知 RC transport 本身可以达到目标量级；Dragonfly
L8×CC8 只有 134.25 Gbps，不能据此把连接模式认定为首要瓶颈。

RM process-wide queue 可能减少连接和队列资源，但也可能引入：

- 单共享 JFS doorbell/SQ 串行化；
- 多 peer head-of-line blocking；
- 更复杂的 anonymous RX dispatch；
- per-peer fairness bookkeeping；
- provider 内部多 QP 选择和无序 completion；
- 一个共享队列上的更大 fault blast radius。

RM 是否更快只能通过同设备、同 message、同 depth、同 completion moderation 的 RM/RC A/B 回答。

### 4.2 provider/profile 限制必须 capability gate

UMDK v25.12 release notes 对 bonding+CTP RM 给出的限制是最大消息 4 KiB、TAACK 可靠但无数据重传；
这不等同于当前普通 `udmac0d1e2 + RTP`，但证明 RM 语义不能跨 provider/profile 一概而论。RM backend
启用前必须协商并记录：

- device 是否支持 `URMA_TM_RM`；
- `RTP/CTP`、order type、native RM/XRC/RC-emulation 能力；
- max message size 是否支持当前 64 KiB chunk；
- SEND_IMM 宽度、`remote_id` 和 completion 行为；
- RNR retry、错误超时、retransmission 和 multipath 语义；
- local loopback 是否支持。

## 5. 推荐实施路线

### 阶段 RM0：文档和 capability probe

目标：不改 production path，确认目标设备 RM 能力。

- 保存 `urma_admin show` 的 trans mode、TP type、max message、depth、ordering 等能力；
- `urma_perftest send_bw` 用同参数分别运行 `-p 0` RM 和 `-p 1` RC；
- 固定 64 KiB、JFS128、JFR512、相同 completion moderation、CPU/NUMA；
- 验证 RM 双向 SEND_IMM、`remote_id`、RNR、peer exit 和 target unimport。

### 阶段 RM1：per-peer RM compatibility backend

目标：只隔离 transport mode，不先改变资源共享模型。

- 仍为每个 peer 创建 RM Jetty/JFR；
- descriptor 加 transport mode/version；
- RM 只 import、不 bind；
- 复用现有 Piece protocol、buffer、credit、SEND_IMM 和 Storage；
- RM/RC capability negotiation，失败回退 RC/TCP；
- 对比单 peer CC1/2/4/8/16/32 的 transport-only 和 CRC32+pwrite。

这一阶段回答“目标 provider 的 RM 是否正确、稳定、性能正常”，不宣称已经实现共享 endpoint。

### 阶段 RM2：process-wide shared RM prototype

目标：真正验证类似 `FI_EP_RDM` 的资源模型。

- process-wide JFS/JFR/JFC；
- imported target cache 和 peer generation；
- anonymous registered RX pool；
- `remote_id + SEND_IMM` completion router；
- global + per-peer + per-transfer admission；
- target-local drain/unimport，不为普通 peer error poison shared JFS/JFR；
- RC backend 保留，配置/协商可切换。

### 阶段 RM3：多 peer 决策实验

至少比较：

| 场景 | 主要指标 |
|---|---|
| 单 peer Piece CC sweep | aggregate throughput、CPU、CQE/post、p99 Piece latency |
| 多 peer fan-out/fan-in | throughput scaling、Jain fairness、completion skew |
| peer churn | connect/import/bind p50/p95/p99、provider resource 数 |
| 一个慢 peer | 健康 peer 吞吐和 tail latency，是否 HOL |
| peer crash/timeout | target-local retirement、其他 peer 是否无损 |
| RX abuse/RNR | quota、credit、shared JFR 是否被耗尽 |
| shutdown with outstanding WR | WR/slot/import/JFS/JFR/JFC 是否全部闭合 |

只有 RM 在 correctness/fault 全通过后，且在高 fan-out 下显著降低资源或建链延迟，并取得稳定吞吐、
CPU 或 tail-latency 收益，才考虑改变默认模式。单 peer 不退化只是必要条件，不是切换默认的充分条件。

## 6. 当前代码改造面

当前实现把 RC 假设写入以下位置：

- `dragonfly-client-storage/src/urma/ffi/shim.c`
  - JFS/JFR 固定 `URMA_TM_RC`；
  - descriptor import 拒绝非 RC；
  - import 后必须 bind，close 前必须 unbind；
  - 一个 wrapper 只保存一个 `target`；
- `dragonfly-client-storage/src/urma/lane.rs`
  - `JettyConfig` 和状态机明确是 RC lane；
  - `UrmaJetty` 只允许一个 imported/bound remote；
- `dragonfly-client-storage/src/urma/runtime.rs`
  - 每次 `create_lane` 创建一套 Jetty/JFR；
- `dragonfly-client-storage/src/urma/completion.rs`
  - RX routing key 包含 lane id；
  - retirement 和 native local id 均以 lane 为边界；
- `dragonfly-client-storage/src/urma/session.rs`
  - TCP control、ClientLane 和 native Jetty 生命周期一一绑定；
  - native receive permits 为 per-lane。

因此 production RM 不应通过在 shim 中把三处 `URMA_TM_RC` 改成 `URMA_TM_RM` 实现。应抽出
`RcPeerLane` 与 `RmSharedFabric` 两种明确 backend，保持共同的 Piece/Storage contract。

## 7. 新对话建议起点

新对话可以从下面的目标开始：

> 基于 `urma-rm-for-dragonfly-p2p-evaluation.md`，先完成 RM0/RM1 设计，不改现有 RC production
> 默认路径。确认 UMDK RM 的 descriptor、import、SEND_IMM、remote_id、JFS/JFR、completion、错误和
> unimport 语义，给出最小兼容原型、wire capability 变更、测试矩阵和明确的 go/no-go gate。

建议首先阅读：

1. 本文；
2. `phase-b-performance-data-path.md`；
3. `rdma-urma-upload-download-path-comparison.md`；
4. `real-provider-validation-runbook.md`；
5. UMDK `doc/en/urma/URMA User Guide.md` 的 transport modes/data plane；
6. UMDK `src/urma/lib/urma/core/include/{urma_api.h,urma_types.h}`；
7. 当前 `dragonfly-client-storage/src/urma/{runtime,lane,completion,session}.rs` 和 `ffi/shim.c`；
8. `/home/yuan/workspace/dev/client` 的 RDMA 候选分支 `origin/urma-p2p`。

## 8. 证据边界

- `[源码确认]`：UMDK API/types、当前 Dragonfly URMA、RDMA 候选分支；
- `[实验确认]`：本文引用的既有 RC/perftest 和 Dragonfly B7 数据；
- `[架构判断]`：RM 更适合高 fan-out 资源模型，但尚未用 Dragonfly RM backend 验证；
- `[待验证]`：RM provider 性能、共享 RX matching、peer-local retirement、公平性、故障和 shutdown；
- 服务器恢复前不得把 RM 标记为 production-ready，也不得将裸 RM perftest 外推为 Dragonfly E2E。

