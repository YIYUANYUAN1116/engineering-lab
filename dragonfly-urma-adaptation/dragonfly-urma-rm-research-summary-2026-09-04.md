# Dragonfly × URMA RM 研究阶段总结

更新时间：2026-09-05

> 研究目标：评估 URMA Reliable Message（RM）是否比当前 URMA RC persistent-lane 模型更适合 Dragonfly P2P。
>
> 当前状态：服务器环境已被重置，暂无可用 URMA 真机环境；本阶段以 UMDK/liburma 源码、官方 API/示例、现有 Dragonfly URMA RC 实现与既有实验结果为依据，不将未验证内容写成 provider 实验事实。
>
> 原型状态：`urma-rm-prototype` 是 RM-only 实验分支，已完成 DFUR v3 显式 RM 标识、provider RM capability gate、RM import-only、独立 `TargetHandle`（native 边界已允许一个 Jetty 拥有多个 imported target）、`PeerTargetRegistry` 的 full-identity/generation/Active/Draining 生命周期、完整 completion `remote_id` DTO 透传，以及 receive CQE 在消费 WR ownership 前对授权 target 的 fail-closed 校验；配置与 wire decoder 均拒绝 RC，RC 基线由原分支/提交保留。高层 Runtime 仍暂时是一 Lane/一 Target，尚未把本地 Jetty/JFR 提升成单一共享对象。静态检查与纯软件测试已通过，但尚未经过真实 RM provider 数据面验证，因此不能据此宣称 RM 可用或性能成立。
>
> 核心关注点不是“RM 是否比 RC 更快”，而是：
>
> 1. RM 是否能降低 Dragonfly 动态多 Peer 场景下的连接、资源和生命周期复杂度；
> 2. RM 的共享 JFS/JFR 模型是否能正确承载多 Peer、多 Piece 并发；
> 3. shared RX、flow control 和 fault domain 新增的复杂度是否可控；
> 4. 当前 RC 已完成的 buffer、completion、Storage 和 TX/RX pipeline 工作能否继续复用。

---

## 0. 当前结论

### 0.1 总体判断

- `[源码/官方资料确认]` URMA RM 在应用可见的资源模型上明显比 RC 更接近 process-wide message fabric：一个本地 RM JFS/Jetty 可以面向多个远端 target，而 RC Jetty 通过 bind 固定到唯一远端。
- `[架构判断]` 对 Dragonfly 这种 Parent 动态变化、多 Peer、高 fan-out 的 P2P 系统，RM 的主要价值是减少 **per-peer native data-plane resource object**，而不是提升单 Peer 峰值带宽。
- `[设计判断]` RM 最值得删除的是当前 `PeerLane` 中每 Peer 独占的本地 Jetty/JFR/bind 资源层，而不是删除 Peer 控制面状态，更不是推翻整个 URMA 数据路径。
- `[源码/既有实现确认]` 当前 registered buffer、lease/generation、completion owner、TX double ring、Storage direct-write、CRC/pwrite overlap 等工作与 RC/RM 无关，大部分可以保留。
- `[关键风险]` RM 的 shared RX 会把复杂度从“per-peer connection lifecycle”转移到“shared receive routing / credit / fairness / fault isolation”。
- `[待真机验证]` 目标 `udmac0d1e2` provider 的 RM capability、最大消息长度、SEND_IMM/notify 位宽、RTP/CTP 组合、错误隔离和 target-local retirement 仍未知。
- `[性能判断]` 当前没有“RM 会比 RC 更快”的证据。既有 RC `urma_perftest` 已约 399 Gbps，说明 Dragonfly 应用路径当前差距不能简单归因于 RC transport mode。

### 0.2 一句话概括 RC 与 RM

当前 RC：

```text
Process
  -> Peer Connection / Lane
      -> Piece Transfer
```

RM 目标：

```text
Process-wide Fabric
  -> PeerControl / PeerTarget
      -> Piece Transfer
```

RM 不是“没有连接”，而是：

> **去 per-peer native data-plane connection resource 化。**

provider / ubcore 内部仍可能为不同 target 维护 TP/QP；Dragonfly 也仍需维护 TCP 控制连接、descriptor/capability、generation、health、credit 和 outstanding 等 `PeerControl` 状态，只是不再为每个 Peer 持有一整套本地 Jetty/JFR/bind 数据面状态机。

---

## 1. 证据等级

本文沿用项目统一口径：

- `[源码确认]`：从 Dragonfly、UMDK/liburma、urma_perftest 或官方 sample 源码直接确认；
- `[官方文档确认]`：从 openEuler/UMDK 官方 API Guide、User Guide、Release Note 确认；
- `[实验验证]`：当前已有 RC/Dragonfly 真机结果；
- `[架构判断]`：基于已确认 API 语义作出的 Dragonfly 设计判断；
- `[待源码确认]`：API/实现仍需继续下钻；
- `[待实验验证]`：必须在真实 provider/设备上验证。

特别说明：

> 当前没有 RM 真机实验，因此所有 RM provider 行为均不得标记为 `[实验验证]`。

---

## 2. 三种数据面模型对比

| 维度 | Dragonfly RDMA 候选 `FI_EP_RDM` | URMA RM 目标 | 当前 URMA RC |
|---|---|---|---|
| 本地数据面对象 | daemon 级共享 endpoint | process-wide JFS/JFR 或 RM Jetty | 每 Peer 一个 RC Jetty/JFR |
| 远端选择 | provider address | 每 WR 指定 imported target | Jetty bind 唯一 remote |
| Receive | tagged receive | shared JFR；保守基线为 completion-time demux | per-lane JFR |
| Completion | shared CQ | shared JFC | shared JFC，但按 lane 路由 |
| Piece 并发 | 一个 endpoint 多 Piece | 一个共享 Fabric 多 Peer、多 Piece | lane 内再做多 Piece |
| Peer 生命周期 | provider address/transfer | PeerTarget | PeerLane |
| 故障隔离 | endpoint 内按 op/address | 目标是 target-local | lane 是天然 native fault domain |

### 2.1 RDMA `FI_EP_RDM` 不等于多 Lane

此前源码已确认：

```text
1 daemon
  -> 1 shared FI_EP_RDM endpoint
  -> shared CQ
  -> 1 progress thread
  -> 多 Piece 用不同 tag range 并发
```

RDMA concurrency=1/2/4/8/16/32 指共享 endpoint 上的并发 Piece 数，不是 QP/lane 数。

因此与其最接近的 URMA 目标不是“多 RC lane”，而是：

```text
1 process-wide RM data-plane object
  -> 多 target
  -> 多 transfer
```

---

## 3. RM 的核心资源语义

## 3.1 RM 与 RC 的根本差异

RC：

```text
local Jetty
   |
   +---- bind ---- remote Jetty A
```

一个本地 RC Jetty 对应一个远端。

RM：

```text
shared RM JFS / Jetty
   |
   +---- target A
   +---- target B
   +---- target C
```

发送 WR 自己指定目标。

因此应用不需要：

```text
Peer A -> local lane A
Peer B -> local lane B
Peer C -> local lane C
```

而可以：

```text
WR1 -> target A
WR2 -> target B
WR3 -> target C
WR4 -> target A
```

### 3.2 `import`、`advise`、`bind`

这三个概念必须分开：

```text
import
= 导入并获得远端 target handle

advise
= 可选地为 local 与 target 准备/建议 transport channel

bind
= RC 专属的一对一连接绑定
```

当前可确认：

- `[官方文档确认]` RC 使用 bind/unbind；
- `[官方文档确认]` RM 可导入多个 target；connectionless Jetty 可按 provider/profile 选择 advise；
- `[官方文档确认]` RM import/advise/provider 可能在内部建立 target 对应的 TP；
- `[架构判断]` RM 不会让 per-peer transport state 从硬件/provider 内部消失，而是把它从 Dragonfly-owned lane 下沉到 provider/target 层。

因此 RM 基线状态机应为 `Imported -> Ready`；`Advised` 只作为 capability/profile-specific 的可选优化，通过 import-only 与 import+advise A/B 测试确认其是否影响首次发送延迟，不能作为 RM 正确性的固定前置条件。

因此更准确的对比是：

```text
RC:
Dragonfly 管 local Jetty + JFR + bind + TP lifecycle

RM:
Dragonfly 管 PeerTarget
provider/ubcore 管 target 后面的 TP lifecycle
```

---

## 4. RM Target 生命周期

RM 并不是“随手 import、随手 unimport”。

公开 API 语义要求：

> unimport target 前，应用必须保证经该 target 发出的 outstanding 请求已经通过 completion（包括错误 completion）收敛。

因此未来 `PeerTarget` 应有明确状态机：

```text
Unknown
  ↓
Importing
  ↓
Imported
  ↓
Ready / Active
  ↓
Draining
  ↓
outstanding == 0
  ↓
Unimported
```

建议对象：

```rust
PeerTarget {
    remote_id,
    imported_target,
    generation,
    outstanding,
    state,
    health,
}
```

### 4.1 `generation` 仍然必须保留

RM 不再有 lane generation，不代表 generation 可以完全删除。

例如：

```text
Peer A generation 7
  -> fault
  -> drain
  -> unimport

后来再次 import A
  -> generation 8
```

即使 provider identity 复用，旧 completion 也不能命中新 target。

因此当前 RC 已建立的：

```text
slot generation
late CQE protection
stale identity protection
```

仍然有价值。

---

## 5. RM TX：最明确的简化点

### 5.1 当前 RC

```text
Peer A
  -> Lane A
      -> Jetty A
          -> SEND
```

Parent 同时给 A/B/C 发数据，需要：

```text
Lane A
Lane B
Lane C
```

即使 JFC 和 Runtime 是共享的，本地 Peer transport object 仍然是 per-peer。

### 5.2 RM

```text
shared JFS
  |
  +-- WR target=A
  +-- WR target=B
  +-- WR target=C
```

所以 RM 的 TX 侧天然适合：

```text
process-wide queue
+
per-WR target selection
```

### 5.3 当前 TX 优化大部分保留

以下不依赖 RC：

```text
MappedPiece / RangeReader
        ↓
registered TX lease
        ↓
double TX ring
        ↓
fill(next) || send(current)
        ↓
completion
        ↓
slot recycle
```

RM 只是把：

```text
RC: send on lane A
```

变为：

```text
RM: post on shared JFS with target=A
```

因此当前这些能力都继续有价值：

- mmap source；
- RangeReader direct-fill；
- TX registered slot pool；
- double ring；
- post-list；
- completion moderation；
- TX slot ownership；
- fill/send overlap。

---

## 6. RM RX：真正需要重写的部分

RM 的难点主要在 RX。

### 6.1 RC 当前模型

RC 中：

```text
Peer A
  ↓
JFR A
  ↓
RX slot
```

因此在 DMA 前就知道：

```text
这个 receive 属于 Peer A
```

当前代码可以先决定：

```text
(lane, transfer, sequence)
  -> RX slot
```

然后 post 到该 lane JFR。

### 6.2 RM shared JFR

RM 目标：

```text
process-wide shared JFR
  ├─ slot 0
  ├─ slot 1
  ├─ slot 2
  └─ ...
```

如果使用保守的 no-tag-matching 模型，post receive 时只知道：

```text
这是一个可以被 provider 写入的 registered buffer
```

直到 CQE 才确定：

```text
谁发的
属于哪个 transfer
是第几个 chunk
```

所以：

```text
RC:
route before DMA

RM:
route after CQE
```

这是 RC→RM 最大的数据面语义变化。

---

## 7. `remote_id`：发送方识别

后续研究中一个重要确认是：

- `[源码确认]` receive completion 的 `remote_id` 类型是完整 `urma_jetty_id_t`，由 `(eid, uasid, id)` 组成，可以作为远端 Jetty identity 使用；
- `[官方示例源码确认]` 官方 RM sample 使用 `remote_id` 中的 EID/UASID 区分不同 client 并查找 imported target，但示例未比较 Jetty `id`，因此只证明其示例拓扑下的区分方式，不能证明任意多 Jetty 场景的完整唯一性。

因此 shared JFR 并不是“收到数据却不知道来自谁”。

可以形成：

```text
CQE.user_ctx
   -> local RX slot

CQE.remote_id
   -> PeerTarget
```

建议：

```text
PeerRegistry[(remote_id.eid, remote_id.uasid, remote_id.id)]
    -> authorized PeerTarget { current_generation, ... }
```

注意：

> `remote_id` 解决的是“哪个远端 Jetty”，不解决“哪个 Piece/Transfer”，也不等同于认证。它必须先命中 TCP 控制面预先授权的 descriptor；当前 generation 再通过 PeerTarget 状态和 routing token 所解析出的 Transfer generation 校验。未知或过期 identity 必须 fail-closed，不能根据未授权 CQE 动态创建 Peer。

`peer_generation` 是本地生命周期状态，不一定由 CQE 直接携带，不能假装它天然属于 `remote_id` 查找键。如果 provider 会在重连后复用同一个完整 Jetty identity，则必须依赖 drain/quiescence barrier 和 token/Transfer generation 防止旧 CQE 命中新对象；无法证明旧 DMA/CQE 已收敛时不得复用该身份对应的资源。

当前 FFI completion DTO 尚未透传 `remote_id`。RM2 前必须同步扩展 C shim、Rust DTO 和 completion router，并逐字段比较完整 identity；不能压缩成单个整数 ID。

---

## 8. Routing Token：Transfer 与 Sequence

同一个 Peer 可以同时有多个 Piece：

```text
Peer A
  ├─ Transfer 101
  ├─ Transfer 205
  └─ Transfer 991
```

因此还需要：

```text
transfer_id
sequence
```

RM 数据 CQE 理想信息：

```text
user_ctx
  -> local RX slot

remote_id
  -> Peer

IMM / notify / routing token
  -> transfer_id + sequence
```

建议不要把应用协议直接命名为 `imm_data`，而定义抽象：

```text
routing_token
```

backend 再映射到底层可用的 SEND_IMM/notify 能力。两者的 opcode、字段有效性和 provider 语义并不天然等价，必须通过 capability 协商选择精确映射，并由 RM0 真机测试确认。

### 8.1 Routing key 推荐结构

不建议直接只做一个 composite hash：

```text
(remote_id, transfer_id, sequence)
```

更建议分层：

```text
1. PeerRegistry[full remote_id]
2. 校验 PeerTarget state
3. Decode routing_token
4. peer.TransferRegistry[transfer_id]
5. 校验 transfer.peer、peer/transfer generation
6. 校验 sequence / length
```

这里明确采用 **peer-local `transfer_id`**：只有先用完整 `remote_id` 找到已授权的当前 PeerTarget，再结合 routing token 中可解析的 transfer/generation 信息，才能进入该 Peer 的 TransferRegistry。若未来改用 process-global `transfer_id`，则必须同时定义全局分配器和 ID reuse generation，不能沿用 lane-local ID 后直接建立全局 registry。

这样如果出现：

```text
remote_id = Peer A
token 却属于 Peer B 的 transfer
```

可以明确报：

```text
cross-peer routing violation
```

---

## 9. Tag Matching：目前不能作为基础设计

研究中发现：

- `[官方 API 确认]` JFR 创建配置存在 `tag_matching` capability/config 位；
- `[官方示例源码确认]` 官方 RM sample 使用 `URMA_NO_TAG_MATCHING`；
- `[尚未确认]` 当前通用 `urma_jfr_wr_t` 未看到类似 libfabric `tag/mask` 的 per-RQE 用户接口；
- `[待实验验证]` 目标 `udmac0d1e2` provider 是否支持可直接用于 Dragonfly Transfer matching 的 tagged receive。

因此当前设计决定：

> **Dragonfly RM 第一版 shared backend 不依赖 URMA tag matching。**

保守基线：

```text
URMA_NO_TAG_MATCHING
+
shared JFR
+
remote_id
+
routing token
+
completion-time demux
```

Tag matching 如果以后确认可用，只作为 provider-specific 优化研究，不作为 correctness 前提。

---

## 10. Shared RX Completion Router

RM 的核心模块应从当前 lane-oriented completion routing 演进为：

```text
                    Shared RM JFR
                         |
                    anonymous RQE
                         |
                         v
                        CQE
              +----------+----------+
              |          |          |
           user_ctx   remote_id   routing token
              |          |          |
              v          v          v
           RX slot    PeerTarget   Transfer/Seq
              \          |          /
               \         |         /
                +---- CompletionRouter
                          |
                 validate generations
                 validate peer state
                 validate credit
                 validate transfer
                 validate sequence/length
                          |
                          v
                    Registered RX lease
                          |
                    CRC32 + pwrite
                          |
                        recycle
                          |
                     repost JFR
```

推荐校验顺序：

```text
1. user_ctx -> slot
2. 校验 slot generation / Posted 状态
3. 校验 completion status
4. 若 status/opcode 允许可靠读取 source/token：full remote_id -> authorized PeerTarget
5. 校验 PeerTarget state
6. 校验 peer outstanding/credit
7. decode routing token
8. lookup peer-local Transfer
9. 校验 Transfer 属于该 Peer及 peer/transfer generation
10. 校验 sequence / completion_len
11. 交付 immutable registered lease
```

若第 3 步发现错误且该 status/opcode 下 source/token 无效，则跳过按 Transfer 归属，直接按第 13 章的字段有效性矩阵升级故障。

---

## 11. Buffer 生命周期基本不变

RM 不改变 DMA buffer ownership 的基本问题。

当前已验证/实现的生命周期：

```text
Free
  ↓
Posted
  ↓ CQE
Completed
  ↓
Leased
  ↓ Storage CRC/write
Recycle
  ↓
Free / Repost
```

无论 RC/RM：

> receive CQE 只表示 provider/NIC 已完成当前写入，不表示应用已经不再使用该 buffer。

因此必须继续保证：

```text
CQE
-> immutable RX lease
-> CRC32 / pwrite
-> consumer 全部完成
-> recycle
-> repost
```

不能 CQE 一到就立即 repost。

所以以下 Phase B 工作仍然直接有价值：

- RegisteredBufferPool；
- RX immutable lease；
- TX exclusive lease；
- slot generation；
- active lease close guard；
- Drop -> owner recycle；
- wrong-pool / double-recycle protection；
- late CQE protection；
- registered window direct-write。

需要注意，“保留”指 backing pool、lease 和 Storage API 可以复用，不代表 RX owner 状态机原样不动。shared anonymous RX slot 在 post 时尚不属于某个 Peer/Transfer，只能在 CQE 到达并完成 source/token 校验后绑定消费者；现有含 lane/generation 的 `WrToken` 编码和 completion owner 路由也必须随 RM2 调整。

---

## 12. RM Credit 与 Flow Control

这是 RM 相比 RC 新增复杂度最大的部分之一。

### 12.1 RC credit

当前：

```text
Lane A JFR post 64 RECV
    ↓
告诉 A：
你有 64 credit
```

因为 A 不可能消费 B 的 JFR。

### 12.2 RM shared credit

RM：

```text
shared JFR
posted RX = 512
```

这些 RQE 是 process-wide physical resource。

必须区分物理 buffer 生命周期与逻辑 credit 生命周期：

```text
F = free / unposted RX slots
P = posted, not-yet-consumed RQEs
R = received, awaiting CQE routing
L = completed buffers leased to Storage/consumer
C = total registered RX slots

G = granted but not-yet-consumed logical credits
I = credits consumed by authorized sends, not yet completed/recycled
```

物理 buffer 的核心守恒关系是：

```text
C = F + P + R + L
```

其中每个未使用的 credit 都必须由一个不同的、当前已 posted RQE 支撑：

```text
G <= P_reserved <= P
```

grant 被发送方消费后，对应 slot 会经历 provider-in-flight、`R` 和 `L` 等阶段；消息到达会消耗 RQE，使 `P` 下降，所以不能把 consumed/outstanding 与当前 `P` 直接比较。credit 只能在 CQE 已路由、所有消费者释放 lease、slot 成功 repost 后补回。provider-in-flight 到 `R` 的精确切换点需由真机可观测行为验证，但任何阶段都不能重复授予同一个 backing slot。

### 12.3 仅有 global credit 不够

如果只有：

```text
global credit = 512
```

Peer A 可能抢光：

```text
A outstanding = 512
B/C = 0
```

因此需要 per-peer fairness：

```text
global posted-RX permits
+
per-peer guaranteed quota
+
borrowable shared surplus
+
per-transfer pipeline depth
```

示例：

```text
physical posted = 512

A guaranteed = 32
B guaranteed = 32
C guaranteed = 32

shared surplus = 416
```

对遵守 credit 协议的 Peer，热点 A 可以借更多，而调度器仍为 B/C 保留逻辑额度。

但在 `NO_TAG_MATCHING` 的匿名 shared JFR 上，per-peer quota 只是协议级 admission/fairness，不是物理 RX 隔离：超发、旧 generation 晚到包或异常 Peer 仍可能消费下一个共享 RQE，直到 CQE 到达后才能识别来源。因此不能声称 A 在硬件层“无法侵占”B/C 的 RQE。

RM2 必须预留未授予的 emergency headroom，并对 `unknown remote_id`、stale generation、over-credit 和 token/Peer mismatch 执行 fail-closed；RM0/RM3 必须加入超发、RNR 和异常 sender 压测。这是 shared anonymous RX 的 go/no-go 风险。

建议未来对象：

```rust
PeerCredit {
    guaranteed,
    borrowed,
    outstanding,
}
```

---

## 13. RM Fault Domain

RM 是否值得作为长期架构，最大的未决问题之一就是：

```text
shared JFS
├─ target A fault
├─ target B healthy
└─ target C healthy
```

A 出问题后 B/C 能不能继续。

### 13.1 三层错误模型

RM backend 必须主动区分：

#### Transfer-local（仅在身份和字段有效时）

例如：

```text
Peer A / Transfer X / seq N completion error
```

处理：

```text
fail Transfer X
stop new WR for X
drain already-posted WR
Piece reset
TCP fallback
```

只有当该 completion status/opcode 下 `remote_id`、routing token、length 等身份字段明确有效，并且 provider/shared JFS/JFR 状态确认健康时，才按上述方式处理，不要立即 retire Peer A。

如果错误 CQE 无法可靠归属、关键字段在该错误状态下无效，或错误可能已影响 target/TP/shared native object，则必须升级为 PeerTarget-local 或 Fabric/device-local，不能猜测归属。RM0/RM2 应建立并验证“status × opcode × completion 字段有效性 × 升级级别”矩阵。

#### PeerTarget-local

如果确认 Peer/target 不健康：

```text
PeerTarget A:
Active
  ↓
Draining
```

流程：

```text
停止 A 新 transfer
停止 A 新 WR
等待 A outstanding 收敛
unadvise（若适用）
unimport target
generation++
```

目标：

```text
B/C 不受影响
shared JFS/JFR 继续工作
```

#### Fabric/device-local

只有 shared native object/device 本身出错：

```text
JFS/JFR/JFC fatal
device async fatal
无法证明 DMA 已收敛
```

才：

```text
stop all admission
drain/fail all PeerTarget
retire shared Fabric
rebuild
```

### 13.2 当前证据边界

- `[官方 API/资料确认]` completion 存在 operation error / remote abort 等错误类型；
- `[官方 API/资料确认]` RM target 可独立 import/unimport；
- `[架构判断]` API 资源模型具备 target-local retirement 的基础；
- `[待实验验证]` 某个 target/TP 出错后 shared JFS/JFR 是否还能稳定服务其他 target。

这个是 RM 的重要 go/no-go gate。

---

## 14. 一次 RM Piece 下载完整时序

建议目标流程：

```text
Child                                      Parent
  |                                           |
  | shared RM JFS/JFR 已存在                  |
  |                                           |
  |---- discovery / capability ------------->|
  |                                           |
  | PeerTarget cache miss?                    |
  |---- exchange target descriptor --------->|
  | import Parent target                      |
  | PeerTarget(A) Ready                       |
  |                                           |
  | allocate transfer_id = X                  |
  | register Transfer(X -> A)                 |
  |                                           |
  |---- Request(X, task, piece) ------------>|
  |                                           | Storage::get_piece
  |                                           | mmap / RangeReader
  |<--- Ready(offset,len,digest) ------------|
  |                                           |
  | acquire anonymous RX slots                |
  | post to shared JFR                        |
  | grant A credit for X                      |
  |---- WindowReady/Credit(X,n) ------------>|
  |                                           |
  |                                fill TX <--| source
  |                                           |
  |<==== SEND(target=Child, token=X/seq) =====|
  |                                           |
  | shared JFR CQE                            |
  |  user_ctx -> RX slot                      |
  |  remote_id -> Parent A                    |
  |  token -> X / sequence                    |
  |          |                                |
  |     CompletionRouter                      |
  |          |                                |
  | PeerControl(A).TransferRegistry[X]         |
  |          |                                |
  |   registered RX lease                     |
  |          |                                |
  |     CRC32 + pwrite                        |
  |          |                                |
  |        recycle                            |
  |          |                                |
  |      repost shared JFR                    |
  |          |                                |
  |   return/update Peer A credit             |
  |                                           |
  | ...                                       |
  |                                           |
  |<--- Done(X) ------------------------------|
  | wait expected CQEs + Storage              |
  | unregister Transfer(X)                    |
  | Piece success                             |
```

### 14.1 与 RC 最核心的区别

RC：

```text
先找到/建立 Lane A
再在 Lane A 里运行 PieceSession X
```

RM：

```text
找到/导入 PeerTarget A
Transfer X 直接运行在 process-wide Fabric 上
```

---

## 15. RC → RM 模块映射

| 当前 RC 模块/职责 | RM 处理 | 预计变化 |
|---|---|---|
| `UrmaFabric` / owner thread / Runtime | 拥有 shared RM JFS/JFR | 保留并重构 |
| send/recv JFC + progress | shared JFC | 基本保留 |
| Registered Segment / slot pool | process-wide arena | 基本保留 |
| RX/TX lease + generation | 保持 | 基本保留 |
| `user_ctx -> slot` | 保持 | 直接保留 |
| Storage direct-write | 保持 | 原样保留 |
| CRC32 + pwrite overlap | 保持 | 原样保留 |
| TX double ring | shared JFS 上发送 | 基本保留 |
| `UrmaLane` 的 per-peer native data-plane 部分 | shared RM 资源 | 删除/拆分；PeerControl 保留 |
| 每 Peer local Jetty/JFR | shared RM 资源 | 删除 |
| RC bind/unbind | RM target import；advise 可选 | 删除 |
| lane cache | PeerTarget cache | 大幅简化 |
| lane idle timeout | target cache lifecycle | 大幅简化 |
| lane ERROR/flush retirement | target-local + fabric-global | 重写 |
| `Session` | 拆为 `PeerControl` + `Transfer` | TCP 控制面保留，数据面缩小 |
| lane-based completion routing | source+transfer routing | 重写 |
| per-lane receive credit | global+peer+transfer credit | 重写 |
| per-lane recv depth | shared JFR depth | 池化 |

### 15.1 当前 RC 代码中最可能变化的位置

现有 RM 评估已经确认 RC 假设集中在：

```text
dragonfly-client-storage/src/urma/ffi/shim.c
dragonfly-client-storage/src/urma/lane.rs
dragonfly-client-storage/src/urma/runtime.rs
dragonfly-client-storage/src/urma/completion.rs
dragonfly-client-storage/src/urma/session.rs
```

大方向：

```text
lane.rs
  -> 拆除 RC-only native lane 资源；RM 侧变为 target.rs

session.rs
  -> 拆为 peer_control.rs + transfer.rs

completion.rs
  -> 从 lane routing 改为 shared RM demux

runtime/fabric
  -> 成为真正 process-wide RM resource owner

buffer / Storage
  -> backing pool/lease/Storage API 尽量不动；anonymous RX ownership 与 WrToken 路由调整

ffi/shim + completion DTO
  -> 透传完整 remote_id 和字段有效性
```

---

## 16. 推荐 RM 最终模块结构

```text
urma/
├─ fabric.rs
│   ├─ Runtime / Context
│   ├─ shared JFS/JFR/JFC
│   └─ owner/progress
│
├─ target.rs
│   └─ PeerTarget
│
├─ peer_control.rs
│   ├─ TCP control connection
│   ├─ capability / descriptor / generation
│   └─ health / credit / outstanding
│
├─ transfer.rs
│   └─ Piece Transfer
│
├─ completion.rs
│   ├─ TX completion
│   └─ RX completion-time demux
│
├─ buffer.rs
│   ├─ registered arena
│   ├─ RX lease
│   └─ TX lease
│
├─ flow.rs
│   ├─ global RX availability
│   ├─ per-peer quota
│   └─ transfer pipeline
│
└─ protocol.rs
    ├─ capability
    ├─ target descriptor
    ├─ transfer id
    └─ routing token
```

核心对象关系：

```text
RmFabric
├─ SharedTx
├─ SharedRx
├─ PeerTargetRegistry
├─ CompletionRouter
├─ RegisteredBufferPool
└─ PeerControl[*]
    ├─ PeerTarget
    └─ TransferRegistry
```

不再由每个 Peer 独占 native Lane；但逻辑 PeerControl 仍然存在：

```text
Fabric
  -> PeerControl
      -> PeerTarget
      -> Transfer
```

---

## 17. RM 是否真的比 RC 简单

目前判断：

### 17.1 明显减少的复杂度

RC 每 Peer：

```text
local Jetty
local JFR
bind/unbind
lane state
lane generation
lane cache
singleflight
idle timeout
reconnect
ERROR
flush-done
native resource retirement
recv-depth provisioning
```

RM 每 Peer 目标缩为：

```text
TCP control connection
capability / authorized descriptor
imported target
generation
outstanding
health
credit
```

### 17.2 RM 新增复杂度

全局新增：

```text
shared RX dispatcher
remote_id routing
routing-token decoding
global RX accounting
per-peer fairness
target-local fault handling
shared-fabric blast-radius management
```

因此复杂度模型大致从：

```text
RC:
O(peer × connection-state complexity)
```

变成：

```text
RM:
O(global shared-resource complexity)
+
O(peer × lightweight target state)
```

### 17.3 当前判断

- 对 1~2 个长期稳定 Peer：RC 简单、天然隔离，RM 优势未必明显；
- 对大量动态 Parent、Peer churn、高 fan-out：RM 更可能是净简化；
- Dragonfly 的调度模型天然属于后者，因此 RM 值得继续验证。

---

## 18. 性能判断

本研究不以 RM 超过 RC 吞吐为前提。

已有 RC 裸能力：

```text
device      udmac0d1e2
mode        RC
message     64 KiB
JFS/JFR     128 / 512

urma_perftest:
≈ 47558.59 MiB/s
≈ 399 Gbps
```

另有后续 bare SEND_IMM 更高结果记录。

因此：

> 当前 Dragonfly 应用吞吐距离 raw transport 上限较大，不能认为“换 RM”会自然解决性能问题。

RM 可能改善的是：

- Peer 建链/切换开销；
- provider object 数；
- queue depth 利用率；
- high-fanout resource pooling；
- Peer churn；
- global admission/fairness。

同时可能引入：

- shared SQ/JFS contention；
- head-of-line blocking；
- RX dispatcher CPU；
- global credit bookkeeping；
- fault blast radius。

最终性能必须做同参数 RM/RC A/B。

---

## 19. Provider/Profile 不能泛化

不能把：

```text
URMA_TM_RM
```

简单理解为：

```text
固定 message size
固定重传能力
固定 ordering
固定 routing token width
```

RM 的实际语义必须结合：

```text
device
+
provider
+
RTP / CTP
+
native RM / XRC / RC emulation
+
具体版本
```

因此未来 capability 至少要记录：

```rust
RmCapability {
    max_message_size,
    supports_send_imm,
    routing_token_bits,
    supports_tag_matching,
    supports_local_loopback,
    reliability_profile,
    tp_type,
    jfs_depth,
    jfr_depth,
}
```

并区分：

```text
advertised capability
```

与：

```text
validated usable semantics
```

---

## 20. RM0：服务器恢复后的第一轮验证

当前不建议直接写 production RM2 shared backend。

服务器恢复后首先做 RM0。

### 20.1 Capability

保存：

```text
urma_admin show
device capability
transport mode
TP type
max message
queue depth
ordering
```

必须确认：

```text
URMA_TM_RM 是否支持
RTP + RM 是否支持
CTP + RM 是否支持
64 KiB message 是否支持
SEND_IMM/notify 是否可用
routing token 可用位宽
remote_id completion 行为
```

### 20.2 RC/RM 基线

同设备、同 CPU/NUMA、同参数：

```text
RC  -p 1
RM  -p 0
```

固定：

```text
message 64 KiB
JFS 128
JFR 512
same completion moderation
```

不重点比较谁快，而是先确认：

```text
正确性
稳定性
资源模型
错误语义
```

### 20.3 Source identity

必须验证：

```text
A -> shared JFR
B -> shared JFR
```

CQE 是否可靠区分：

```text
remote_id=A
remote_id=B
```

同时必须验证完整 `(eid, uasid, id)`、重连后的 generation、未知 remote 和 stale remote；来源只允许匹配控制面已经授权的 PeerTarget，不能把 `remote_id` 当作认证凭据。

### 20.4 Routing token

验证：

```text
SEND_IMM / notify
-> receive CQE
```

以及：

```text
用户可用位宽
高并发下是否丢失/覆盖
不同 opcode/status 下字段是否有效
token 与 Peer/Transfer 不匹配时是否 fail-closed
```

### 20.5 Fault isolation

最重要的三节点测试：

```text
Server shared RM Fabric
├─ Client A
└─ Client B
```

A/B 持续并发传输，然后：

```text
1. kill A
2. A 停止 post receive，制造 RNR
3. A control connection断开
4. retire/unimport A
5. A outstanding WR timeout
6. A 超过 granted credit 持续发送
7. A 使用 stale generation / 未授权 identity 发送
```

观察：

```text
A completion status
shared JFS/JFR state
B 是否继续完成
B throughput 是否中断
是否出现 B flush/error
JFR 是否继续 repost
emergency headroom 是否仍能服务 B
异常 CQE 能否按字段有效性矩阵正确升级
```

Go/no-go 关键条件：

> A 的 target-local fault 必须能够在不破坏 B 正常 transfer 的情况下收敛。

---

## 21. 当前实现台账与后续阶段

### 21.1 已完成：RM-only 与 RM2 前置切面

当前工作分支：

```text
urma-rm-prototype
```

截至 2026-09-04，本地未提交改动已经完成：

1. **RM-only 分支边界**
   - 配置默认且只接受 `transportMode: rm`，显式拒绝 `rc`；
   - `TransportMode` 和 DFUR v3 wire decoder 只接受 URMA 公共值 `RM=1`；
   - provider capability gate 必须包含 `URMA_TM_RM`；
   - TCP fallback 保留，RC 由原分支/提交作为独立 A/B 基线，不在本分支维护同二进制兼容。
2. **移除 RC bind 假设**
   - 删除 Rust/C shim 的 bind/unbind 路径；
   - 修复“RM 已跳过 bind，但 SEND 仍检查 `bound != 0`，导致真实发送必然返回 `-ENOTCONN`”的问题；
   - RM import target 后即可作为 SEND 的显式目标。
3. **拆分 native Jetty 与 Target ownership**
   - 新增独立 `dfurma_target_t` / Rust `TargetHandle`；
   - `dfurma_jetty_t` 不再内嵌单一 target，native 边界允许同一 Jetty 导入多个 target；
   - SEND/SEND_IMM 每次显式传入 target，RECV 仍只依赖本地 Jetty/JFR；
   - Jetty 和每个 Target 分别统计 outstanding WR；Target 有未完成 SEND 时拒绝 unimport，Jetty 尚有 target/WR 时拒绝删除。
4. **完整 source identity 透传**
   - completion DTO 透传完整 `(eid[16], uasid, jetty_id)`；
   - C shim 对每个 completion DTO 先完整清零，避免 reserved/padding 未初始化；
   - `remote_id_valid` 只在 receive work-request completion 上成立。
5. **PeerTargetRegistry 与 fail-closed 路由**
   - 新增 `PeerTargetRegistry`，维护 `PeerTargetId -> entry` 与完整 `remote_id -> PeerTargetId` 双向索引；
   - 保存非零 generation 和 `Active -> Draining` 生命周期；
   - 拒绝重复 PeerTarget ID、重复 remote identity、未知 source 和 stale generation；
   - receive CQE 在 `take_outstanding()`、`wr_complete()` 和 buffer ownership 变化之前，先用硬件 `remote_id` 解析已授权 PeerTarget，再交叉校验 token 中的 PeerTarget ID/generation。

6. **RM2 shared endpoint 收敛（2026-09-04 第二轮）**
   - Runtime 新增唯一 `SharedRmEndpoint { UrmaJetty, descriptor, config, effective_post_list_size }`：第一个 Peer `create_lane` 时惰性创建并注册到 completion 路由，后续 Peer 复用同一 local descriptor；per-peer Jetty 配置与共享端点不一致直接报错（RM-only，不做隐式第二个端点）；
   - `UrmaJetty` 变为无内部 target 的共享 Jetty：`import_target` 返回独立 `TargetHandle` 交由 Peer 持有，`post_send_imm/post_send_batch` 显式携带 target；`export_descriptor` 在端点创建时导出一次；
   - `UrmaLane` 剥离 native Jetty，仅持有 `TargetHandle` + credits + 生命周期状态（`Created -> Ready -> Draining -> Closed`）；`connect_remote_descriptor/mark_ready/export_descriptor` 收敛为单一 `import_remote(jetty, descriptor)`，导入后即 Ready；
   - completion 路由收敛：`register_lane/lane_by_jetty_id/per-lane flush` 全部替换为 `register_endpoint` + 端点级 `EndpointLifecycle`；CQE 的 `local_id` 必须等于唯一共享端点 jetty id，否则 fail-closed（不触碰 WR ownership）；
   - RECV source 路由改为 `resolve_source(remote_id)`：从硬件上报远端身份经 `PeerTargetRegistry` 解析来源 Peer，`RegisteredRxCompletion.lane_id` 使用 source Peer，slot 归属仍跟随 posting WR token；
   - retirement 语义拆分：per-peer abort 仅 drain 该 PeerTarget（不再对共享 Jetty `mark_error`，避免拖垮其他 Peer）；仅 Runtime shutdown 超时才升级为端点级 flush（`begin_endpoint_flush` + Jetty `mark_error`，等待 `WR_FLUSH_ERR_DONE`），随后关闭共享 Jetty；
   - 测试同步改写：flush gate 收敛为端点级、跨 native jetty fail-closed、source 授权 fail-closed 等 6 个路由测试；新增端点配置一致性约束（同进程所有 Peer 共用一份 `UrmaLaneConfig`，client/server 均从同一全局 config 派生，已核实不冲突）。

7. **RM2 shared RX 与故障域修正（2026-09-05 review 修复）**
   - RX `WrToken` 改为 endpoint 级 anonymous token：`owner/lane_id=0` 只标识物理 slot、slot generation 和 RECV operation；逻辑 waiter 继续按 `(PeerTargetId, routing_token)` 独立登记，CQE 到达后再由 `remote_id + routing_token` 解析消费者；
   - 物理 RECV WR 不再计入某个 PeerTarget 的 outstanding。PeerTarget 退役会取消其逻辑 RX waiter，但匿名 RQE 保留在 shared JFR 中供其他 Peer 消费，避免空闲 shared RQE 导致 target 永久无法 unimport；
   - 增加 shared JFR endpoint 级 depth admission，所有 Peer 合计的 posted RECV 不得超过唯一 JFR 的 `recv_depth`；现阶段仍保留 per-session semaphore 作为逻辑流水线限制，尚未实现 guaranteed quota / borrowable surplus / emergency headroom；
   - 可可靠归属到 PeerTarget 的 WR error completion 改为排队触发该 PeerTarget drain，不再直接 poison 整个 shared Fabric；无法可靠取得 source/routing identity 的错误仍升级为 Fabric 级 fail-closed；
   - `PeerTargetRegistry` 允许同一 process-wide `remote_id` 对应多个 control-session alias，并使用已注册 routing token 消歧；Client transfer id 改为进程级分配，避免不同 session 从 1 开始造成 SEND_IMM identity 冲突；
   - Runtime shutdown gate 同时检查逻辑 lane 与 endpoint 全局 outstanding。即使所有 lane 已先退役，只要 anonymous RQE 尚未完成，也必须执行 endpoint ERROR/flush 并等待 `WR_FLUSH_ERR_DONE` 后才能删除 Jetty；
   - 新增 anonymous RX ownership、shared JFR capacity、PeerTarget retirement、remote identity alias 和 Peer-local completion error 的离线单测。2026-09-05 本机因缺少 `protoc` 未执行测试；`cargo fmt --check` 与 `git diff --check` 通过。

本阶段验证结果（截至第二轮 shared endpoint 收敛后）：

```text
cargo check --workspace --features dragonfly-client-storage/urma: PASS
cargo check -p dragonfly-client-storage --features urma-test-failpoints: PASS
dragonfly-client-storage lib tests (urma):        195 / 195 PASS
                                                   (其中 URMA 相关 84 / 84)
dead-code warnings vs 改造前基线:                 无新增
real RM provider / device:                        NOT RUN
```

以上 PASS 只证明 Rust/C ABI 能构建、纯软件状态机和回归测试成立，不证明 provider 的 RM SEND/RECV、CQE identity、错误隔离或性能成立。

### 21.2 当前仍存在的结构性缺口

shared endpoint 落地后（§21.1 第 6 项），Runtime 结构已收敛为：

```text
Runtime
  -> SharedRmEndpoint（唯一 Jetty/JFR/descriptor/config）
  -> HashMap<lane_id, UrmaLane>   // 仅 TargetHandle + credits + 状态
  -> CompletionRouter（端点级路由 + PeerTargetRegistry source 解析）
```

尚未完成：

- RX WR 已改为 anonymous token，完成时使用 `remote_id + routing_token` 解析逻辑 owner；`registered_rx_by_identity` 仍是扁平 `(PeerTargetId, routing_token)` 表，尚未下沉为 `PeerTarget -> per-peer TransferRegistry`；
- routing token 当前编码 process-wide `transfer_id + chunk sequence`，解决并发 session 的直接冲突；仍未编码 transfer generation，极端 ID wrap/reuse 命中旧 CQE 的风险尚未闭环；
- 已有 endpoint 级 posted-RX/JFR depth 硬门禁，但尚无 per-peer guaranteed quota、borrowable surplus、over-credit accounting 或 emergency headroom；
- per-peer `TargetHandle` 尚无 native 级 flush/错误查询接口，target-local 故障（对端 hang）只能靠 drain 超时 + 端点级 flush 兜底，粒度偏粗；
- 同一远端 descriptor 的多个 control session 目前会分别执行 `import_jetty` 并通过逻辑 alias 消歧；provider 是否允许/复用重复 import 仍需 RM0 真机确认，长期应评估按 remote descriptor 共享 native TargetHandle；
- 没有真实 provider，无法验证 import-only first-send、shared JFR 多 source、错误 CQE 字段有效性及 target-local 故障隔离。

### 21.3 下一步：RM2 改造顺序

§21.3 原第 1、2 步（提升 shared native endpoint、Peer 只持有 target/control state）已完成，见 §21.1 第 6 项。剩余按依赖顺序推进：

1. **完成 per-peer TransferRegistry**
   - registry 变为 `PeerTargetRegistry[remote_id] -> peer.TransferRegistry[transfer_id]`；
   - routing token 明确编码 `transfer_id + transfer generation/sequence`，防止 ID reuse 命中旧 CQE。
   - anonymous RX token、`local_id` shared endpoint 校验和 `remote_id + routing_token` completion-time demux 已完成。
2. **继续拆分故障域**
   - PeerTarget fault：停止 admission，等待该 target SEND outstanding 收敛，然后 unimport；
   - 可归属 WR completion error 已触发 PeerTarget drain；普通 target fault 尚缺 native per-target flush/错误查询接口；
   - 只有 fabric/device-local fatal 才 mark shared endpoint ERROR、drain all 并重建。
3. **完善 shared RX credit**
   - endpoint 级 JFR depth gate 已完成；继续落实完整 `F/P/R/L` buffer 守恒和 posted-RQE reservation；
   - 增加 global capacity、per-peer logical quota、borrowable surplus、per-transfer pipeline 和未授予 emergency headroom；
   - over-credit/unknown/stale sender 只能 fail-closed，不能宣称硬件级隔离。
4. **补离线测试与 provider gate**
   - 已补 remote alias、anonymous slot、target-local drain 和 shared capacity 单测；继续补 stale generation、credit 状态机和 shutdown flush 组合测试；
   - 无机器期间允许继续实现并保持默认不宣称可用；
   - 真机恢复后，RM0 capability/source/fault 测试是启用 shared RM 数据面的强制 gate。

### 21.4 RM0：待真机恢复后的强制验证

```text
RM descriptor / import-only / optional advise A/B
SEND / RECV / SEND_IMM
full remote_id stability under multiple sources
64 KiB message and routing-token width
RNR / timeout / retransmission
target unimport with and without outstanding WR
one target fault while sibling target continues
completion status/opcode field-validity matrix
```

### 21.5 RM3：Dragonfly 高 fan-out 决策

重点不是只看 Gbps，而是：

```text
Peer count
Jetty/JFR/TP/object count
queue depth utilization
registered bytes
import/build cost
Peer churn
CPU
tail latency
fairness
fault blast radius
throughput
```

只有当：

```text
correctness/fault PASS
+
多 Peer 资源/复杂度优势稳定
```

才考虑 RM 成为默认路径。

---

## 22. 当前最重要的未决问题

按优先级：

1. `[待实验验证]` `udmac0d1e2` 是否真正支持可用 RM；
2. `[待实验验证]` RM + RTP / CTP 的实际组合；
3. `[待实验验证]` 64 KiB message 是否支持；
4. `[待实验验证]` SEND_IMM/notify 可用位宽；
5. `[待实验验证]` 多 source shared JFR 的 `remote_id` 稳定性；
6. `[待实验验证]` target fault 是否污染 shared JFS/JFR；
7. `[待实验验证]` RNR retry / timeout / retransmission；
8. `[待实验验证]` target import/advise/first-send 的真实建链时机；
9. `[待源码/实验确认]` `URMA_WITH_TAG_MATCHING` 是否对目标 provider 有可用的 per-transfer matching 语义；
10. `[待实现评估]` global RX credit + per-peer quota 的最小复杂度；
11. `[待实验验证]` NO_TAG_MATCHING 下 over-credit/unknown/stale sender 对共享 RQE 的侵占范围和 emergency headroom；
12. `[待源码/实验确认]` 各 completion status/opcode 下 `remote_id`、routing token、length 字段有效性及故障升级矩阵。

---

## 23. 当前设计决定

截至本阶段，可以先记录以下决定：

### 决定 1：不以性能提升作为切 RM 的主要理由

RM 的主要评价维度：

```text
资源数
代码复杂度
Peer churn
故障隔离
公平性
CPU/尾延迟
性能
```

### 决定 2：不直接把当前 RC 改成 RM

不做：

```text
URMA_TM_RC -> URMA_TM_RM
```

这种常量替换。

应保持：

```text
RcBackend
RmBackend
```

独立演进。

### 决定 3：RM2 不依赖 tag matching

基线：

```text
shared JFR
+
NO_TAG_MATCHING
+
remote_id
+
routing token
+
completion-time demux
```

### 决定 4：保留已有 buffer / Storage / TX pipeline

RM 改造重点在：

```text
per-peer native Lane resource
PeerControl / Transfer split
completion routing
credit
fault domain
anonymous RX ownership / WrToken
```

而不是：

```text
registered buffer
lease lifecycle
Storage direct-write
CRC/pwrite
TX ring
```

### 决定 5：长期目标是删除 per-peer native Lane 资源层

目标从：

```text
Fabric -> PeerLane -> PieceSession
```

演进成：

```text
RmFabric
└─ PeerControlRegistry
    └─ PeerControl
        ├─ PeerTarget
        └─ TransferRegistry
```

这里删除的是每 Peer 独占的本地 Jetty/JFR/bind 及其 native lifecycle；TCP 控制连接、身份与 generation、health、credit、outstanding 等逻辑 Peer 状态继续保留。这是当前 RM 研究最核心的架构收益假设。

---

## 24. 阶段结论

当前证据已经足够支持继续研究 RM：

> **URMA RM 的应用资源模型确实比 RC 更贴近 Dragonfly 动态 P2P：共享本地数据面资源，通过 target 选择远端，通过 completion source identity 做多 Peer 区分。**

但还不能得出“RM 应替换 RC”的最终结论。

当前最大的正向证据：

```text
per-WR target
shared JFS/JFR
remote_id source identification
independent target import/unimport
```

最大的风险：

```text
shared RX routing
global/per-peer credit
NO_TAG_MATCHING 无硬件级 per-peer RX 隔离
source identity 不是认证
target fault isolation
provider/profile 差异
```

因此当前最合理的路线是：

```text
继续完成可离线验证的 RM2 shared endpoint 结构改造
  ↓
保持 shared RM 为未完成、未验证状态
  ↓
机器恢复后执行 RM0 capability / source identity / fault 真机门禁
  ↓
完成 RM2 provider correctness 与高 fan-out A/B 验证
  ↓
再决定 RM 是否具备 production adoption 条件
```

无机器期间可以继续推进可单测的结构改造，但不能绕过真机门禁把 shared RM 标记为可用。

---

## 25. 参考材料

本总结主要基于以下已有研究材料继续整理：

- `urma-rm-for-dragonfly-p2p-evaluation.md`
- `urma-perftest-analysis.md`
- `phase-b-performance-data-path.md`
- `rdma-urma-upload-download-path-comparison.md`
- `real-provider-validation-runbook.md`
- `dragonfly-rdma-source-reading-and-urma-design-notes.md`
- UMDK/openEuler URMA API Guide、User Guide、Release Notes
- UMDK 官方 `urma_sample`
- 当前 Dragonfly URMA RC implementation / Phase A/B 研究结果

后续真机 RM 结果应继续追加到本文，明确使用 `[实验验证]` 标记，不覆盖当前尚未验证的结论。
