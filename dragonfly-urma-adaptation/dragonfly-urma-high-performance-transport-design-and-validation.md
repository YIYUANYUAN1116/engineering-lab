# Dragonfly × URMA 高性能 P2P 数据传输适配方案与阶段验证材料

## 1. 项目背景

Dragonfly 是面向云原生场景的大规模 P2P 文件和镜像分发系统，传统 Peer 间 Piece 数据传输主要依赖 TCP 通道。在高带宽互联场景下，单 TCP 流难以充分利用底层高速网络能力，因此需要探索基于 URMA 的高性能数据传输路径。

本项目目标是在保持 Dragonfly 原有 Piece 调度、缓存、校验和存储模型不变的基础上，引入 URMA 高性能数据面，实现：

* 保留 Dragonfly Scheduler/Peer/Storage 业务模型；
* 使用 URMA 替代 Piece bulk data 传输路径；
* 支持 TCP fallback，保证兼容性和可靠性；
* 为后续 RM、READ 等更高阶数据面能力演进提供基础。

当前方案遵循“先研究、后设计、再优化”的原则，所有关键设计基于 Dragonfly 源码、URMA/UMDK 能力分析以及真实 provider 实验结果。

---

# 2. 整体架构设计

## 2.1 设计原则

URMA 适配不改变 Dragonfly 上层 Piece 生命周期。

整体链路保持：

```
Scheduler
    |
Peer选择
    |
Piece Transfer
    |
URMA Transport
    |
Storage
    |
CRC32 + pwrite/pwritev
```

URMA 只负责高吞吐数据面，不替代：

* Task 管理；
* Piece 调度；
* Metadata 管理；
* Storage 校验；
* TCP fallback。

当前设计确认：

> RC 和 RM 并不是两套业务协议，而是在同一套 Piece/Window/Chunk 模型下，对底层 endpoint 和 peer 资源组织方式进行不同实现。

---

# 3. URMA RC 数据路径设计

## 3.1 当前生产基线

当前 RC 方案采用：

```
dfdaemon
 |
UrmaFabric
 |
UrmaLane
 |
Peer Session
 |
Piece Transfer
 |
Window
 |
Chunk
 |
SEND/RECV
 |
Storage
```

Phase A 最小闭环设计：

```
一个 dfdaemon
 └── 一个进程级 UrmaFabric
     └── 一个 owner/progress线程
         └── 一个远端 peer
             └── 一条 persistent RC lane
                 └── Piece Session
```

---

## 3.2 Window/Chunk 分层模型

一次 Piece 传输拆分为：

```
Piece
 |
Transfer
 |
Window
 |
Chunk
 |
URMA WR
```

其中：

| 层级       | 作用                             |
| -------- | ------------------------------ |
| Piece    | Dragonfly业务基本单元                |
| Transfer | 一次实际传输实例                       |
| Window   | registered memory租约和pipeline单位 |
| Chunk    | 单个SEND/RECV消息单位                |

设计约束：

* dfdaemon启动时预注册Segment；
* 不按Piece重复register/unregister；
* TX/RX使用进程级buffer pool；
* pipelineDepth控制同时持有Window数量；
* RX只有在真正post成功后发送RecvPosted credit。

---

# 4. Buffer 生命周期设计

## 4.1 RX路径

当前优化后的RX路径：

```
NIC DMA
 |
registered RX window
 |
CRC32
 |
pwrite/pwritev
 |
recycle slot
```

避免：

```
NIC
 |
temporary buffer
 |
heap copy
 |
Storage
```

额外userspace staging copy降低为0。

核心生命周期：

```
RECV CQE
   |
RegisteredRxWindowLease
   |
CRC / write消费
   |
lease释放
   |
slot recycle
   |
credit返回
```

RX slot 在 Storage 消费完成前不能重新使用，避免 provider 仍访问内存导致数据破坏。

---

## 4.2 TX路径

TX采用：

```
MappedPiece / RangeReader
        |
TxWindowLease
        |
registered TX spans
        |
SEND
        |
CQE
        |
slot recycle
```

当前支持：

* mmap direct-fill；
* 双Window pipeline；
* send(current) 与 fill(next) 并行。

---

# 5. 性能验证结果

## 5.1 Transport基础能力验证

URMA transport优化后：

* fixed-TX 8GiB：

  * 406.27 Gbit/s；
* fixed-TX 64GiB：

  * 389 Gbit/s。

与 `urma_perftest send_bw`：

* 398.95 Gbit/s

处于同一量级。

说明：

> URMA provider transport能力不是当前Dragonfly路径主要瓶颈，后续重点转向应用数据路径优化。

---

## 5.2 Dragonfly真实Piece路径验证

真实provider环境：

* node1 Parent；
* node2 Child；
* URMA设备 udmac0d1e2。

关键结果：

* 单lane post8-in64：

  * 2410.47 MiB/s；

* 单任务CC16：

  * 7570.72 MiB/s；
  * 63.51 Gbps；

* L8 × CC8：

  * 16003.86 MiB/s；
  * 134.25 Gbps。

当前瓶颈已经从：

```
transport能力不足
```

转变为：

```
TX allocator
owner/CQ处理
CRC32
Storage
pipeline调度
```

等应用路径问题。

---

# 6. RM演进方案

## 6.1 为什么研究RM

当前RC模型：

```
Process
 |
Peer Connection
 |
Lane
 |
Piece Transfer
```

每个Peer维护独立native资源。

RM目标：

```
Process-wide Fabric
 |
PeerTarget
 |
Piece Transfer
```

RM主要价值：

* 降低大量动态Peer场景下native resource数量；
* 减少peer churn时Jetty/JFR/bind成本；
* 更符合Dragonfly动态P2P拓扑。

不是为了直接提升单Peer峰值带宽。

---

## 6.2 RM资源模型

目标结构：

```
UrmaFabric

 ├── shared RM JFS
 ├── shared JFC
 ├── PeerTarget Registry
 │      |
 │      +-- peer A
 │      +-- peer B
 │
 ├── registered buffer pool
 |
 └── transfer router
```

RM替换：

```
PeerLane
```

而不是替换：

```
Piece Transfer
Window
Buffer Lease
Storage Pipeline
```

---

# 7. READ能力探索

## 7.1 READ设计目标

READ模式目标：

* 控制面继续使用TCP；
* metadata、Segment交换通过control session完成；
* bulk data通过one-sided READ完成。

第一版设计：

```
Parent

Piece mmap
 |
Export Segment


Child

Import Segment
 |
READ WR
 |
local registered buffer
 |
Storage
```

---

## 7.2 READ真实provider验证

已完成独立probe：

环境：

* RM/RTP；
* udmac0d1e2；
* EID index 1。

验证内容：

* READ WR提交；
* CQE路由；
* owner retirement；
* Segment生命周期；
* unimport/unregister流程。

结果：

* 64MiB source；
* 64条READ；
* CQE全部成功；
* SHA256校验通过；
* drain前unimport返回BUSY；
* drain后正常释放。

当前结论：

READ具备进入Dragonfly独立backend验证条件，但：

* 跨节点READ未验证；
* revoke/error场景未验证；
* E2E性能未与SEND/RECV严格A/B。

---

# 8. 当前阶段成果总结

## 已完成

### 架构设计

* 完成Dragonfly Piece与URMA数据路径映射；
* 完成RC/RM统一传输抽象设计；
* 明确控制面和数据面边界。

### 工程实现

* URMA Fabric/Lane/Session框架；
* persistent lane；
* registered memory lease；
* RX direct-write；
* TX direct-fill；
* mmap上传路径；
* fallback机制。

### 性能验证

完成真实provider：

* transport能力验证；
* 单Piece性能测试；
* 多Piece并发测试；
* B7性能数据闭环。

---

# 9. 后续规划

## 短期

1. 完成RC生产路径性能继续优化：

   * allocator；
   * owner/CQ；
   * CRC/Storage pipeline。

2. 完成RM跨节点provider验证：

   * shared RX；
   * peer routing；
   * fault isolation。

## 中期

验证：

```
RM + SEND/RECV
        |
        |
RM + READ
```

两种数据面的A/B性能。

## 长期

形成：

```
Dragonfly Scheduler
        |
URMA Resource Manager
        |
RM shared Fabric
        |
READ/WRITE/SEND多模式数据面
```

根据Piece大小、节点拓扑、资源状态动态选择最佳传输方式。

---

# 结论

当前Dragonfly × URMA适配已经完成从：

“transport demo验证”

到：

“Dragonfly生产数据路径适配”

的阶段转换。

RC方案已经完成真实provider闭环并具备性能优化基础；

RM方案解决动态多Peer资源模型问题；

READ方案完成基础provider能力验证。

后续重点不是简单替换通信协议，而是在Dragonfly P2P模型下，根据Piece生命周期、资源状态和硬件能力选择最合适的数据传输模型。
