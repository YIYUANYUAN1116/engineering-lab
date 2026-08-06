# 灵衢 UB 基础与 Dragonfly 接入前置知识整理

> 适用范围：Dragonfly × 灵衢 UnifiedBus（UB）研究项目前置学习笔记。  
> 目标：建立 UB 的整体认知，补齐网络与高性能通信基础，理解 UMS、URMA、ubcore、UBS Memory、UBS IO、UBS Engine、UBS Virt，并为后续 Dragonfly 原生接入 URMA 做准备。
>
> 证据标记：
>
> - `[已确认]`：来自已阅读的官方文档、现有 Dragonfly 源码分析或已确认事实；
> - `[架构理解]`：为了帮助理解而做的类比或结构化解释；
> - `[待源码确认]`：需要继续阅读 UMDK、liburma、ubcore、UVS 等源码；
> - `[待环境验证]`：需要在 openEuler、UB 设备及实际 Dragonfly 环境中实验。

---

## 1. 研究目标与基本原则

本项目研究目标不是简单地“把 Dragonfly 的 TCP 换成 UB”，而是依次完成：

1. 梳理 Dragonfly 的控制面、数据面、缓存和 Piece 生命周期；
2. 理解 UB 的通信、内存、IO、调度和虚拟化能力；
3. 建立 Dragonfly 与 UB 的接口及语义映射；
4. 测量当前 TCP/QUIC、CPU、磁盘和数据复制瓶颈；
5. 对比 UMS、URMA Send/Recv、URMA Read/Write 等接入方式；
6. 完成最小原型；
7. 再决定正式代码改造方案。

重要原则：

- 不能把 UB 简化为“更快的 TCP”；
- 不能把 UB 简单等同于 RDMA；
- 不能仅根据端口、函数名或协议宣传推断真实数据路径；
- URMA Completion 不等于 Dragonfly Piece 业务完成；
- 接入方案必须以源码、官方文档和实验结果为准。

---

# 第一部分：网络与高性能通信基础

## 2. Linux 网络基本数据路径

一个应用从本机向远端发送数据，典型路径为：

```text
Application
    ↓
Socket API
    ↓
Kernel TCP/IP Stack
    ↓
NIC Driver
    ↓
NIC / DMA
    ↓
Network
    ↓
Remote NIC
    ↓
Remote Kernel
    ↓
Remote Application
```

应用调用：

```c
send(fd, buf, len, 0);
```

并不代表用户态 Buffer 被网卡直接发送。传统 TCP 发送通常包含：

```text
User Buffer
    ↓ copy
Kernel Socket Buffer
    ↓ DMA
NIC
```

### 2.1 Copy、DMA 与 Zero Copy

- **Copy**：CPU 将数据从一块内存复制到另一块内存；
- **DMA**：设备直接访问内存，CPU 只负责提交描述符；
- **Zero Copy**：减少或消除中间数据复制；
- **Kernel Bypass**：绕过传统内核网络协议栈，让应用更直接地操作设备或队列。

关键区别：

```text
DMA：谁搬数据
Zero Copy：是否发生额外复制
```

存在 DMA 不代表端到端零拷贝。例如：

```text
User Buffer
    ↓ copy
Kernel Buffer
    ↓ DMA
NIC
```

仍然发生了一次用户态到内核态复制。

### 2.2 Page Cache

Linux 普通文件读写通常经过 Page Cache：

```text
Disk
  ↓
Page Cache
  ↓
Application
```

刚写入文件后马上读取，通常会命中 Page Cache，不需要重新从物理磁盘读取。

在 Dragonfly 中，Child Peer 收到 Piece 后写入 Task File；当后续继续读取 Content，尤其 `need_piece_content=true` 时，通常会从 Page Cache 读取，但仍存在：

- 文件读系统调用；
- Page Cache 到用户态 Buffer 的复制；
- 完整 Piece 的内存分配；
- dfdaemon 到 dfget 的再次传输。

`FDCache` 缓存的是打开的文件对象或文件描述符，不是文件数据；真正缓存内容的是 Linux Page Cache。

---

## 3. TCP、QUIC、RDMA 的定位

### 3.1 TCP

TCP 提供：

- 可靠传输；
- 有序字节流；
- 重传；
- 流量控制；
- 拥塞控制。

主要成本包括：

```text
系统调用
用户态/内核态切换
Socket Buffer
内核 TCP/IP 协议栈
数据复制
软中断和包处理
```

### 3.2 QUIC

QUIC 可以理解为：

```text
UDP
+
用户态可靠传输
+
TLS
+
多 Stream
```

QUIC 的优势包括协议可快速演进、多 Stream 和连接迁移，但不一定在所有场景中比 TCP 快，因为它可能增加：

- 用户态协议栈 CPU；
- 加密开销；
- Endpoint 和 Connection 创建成本。

`[已确认]` 当前 Dragonfly 每个 Piece 请求都会进入新的连接创建流程：

- TCP 创建新的 `TcpStream`；
- QUIC 创建新的 Endpoint、Connection 和双向 Stream。

因此，当前实现没有充分发挥 QUIC 长连接多 Stream 的理论优势。

### 3.3 RDMA

RDMA 的核心目标是：

> 通过注册内存、硬件队列和 DMA，实现远端内存直接访问，减少内核协议栈和 CPU 数据搬运。

典型核心对象：

| 对象 | 含义 |
|---|---|
| QP | Queue Pair，发送和接收队列 |
| CQ | Completion Queue，完成通知 |
| MR | Memory Region，注册内存 |
| WR | Work Request，提交给设备的任务 |
| CQE | Completion Queue Entry，操作完成结果 |

典型操作：

- Send/Receive；
- RDMA Read；
- RDMA Write；
- Atomic。

RDMA 的编程模型不是简单的 Socket 字节流，而是：

```text
创建通信资源
→ 注册内存
→ 提交异步操作
→ 等待完成事件
```

---

## 4. 网络性能指标

不能只看链路带宽。完整性能评价至少包括：

| 指标 | 含义 |
|---|---|
| Bandwidth | 链路理论最大能力 |
| Throughput | 应用实际有效传输速度 |
| Latency | 请求或操作完成延迟 |
| TTFB | Time To First Byte，首字节延迟 |
| PPS | 每秒包处理能力 |
| CPU Cost | user/system CPU、softirq、协议栈成本 |
| Memory | RSS、Buffer 占用、注册内存规模 |
| I/O | 读写带宽、IOPS、await、Page Cache 命中 |

对 Dragonfly 应关注：

- Task 完成时间；
- Piece P50/P95/P99；
- 首字节延迟；
- Parent 和 Child CPU；
- 系统调用、上下文切换、softirq；
- Parent 文件读取；
- Child 文件写入和 CRC32；
- 每 Piece 建链次数；
- warm Page Cache 与 cold storage 的差异。

---

# 第二部分：灵衢 UB 整体认知

## 5. UB 是什么

`[架构理解]` 灵衢 UnifiedBus（UB）是一套面向算力基础设施的统一互联体系，目标不只是节点通信，还包括：

- 通信资源；
- 内存资源；
- I/O 资源；
- 资源管理和拓扑；
- 虚拟化环境中的资源交付。

可理解为：

```text
Application
    ↓
UB Service Core
    ↓
UB OS Component
    ↓
UB Hardware / Fabric
```

UB 不应简单理解为：

- 一个 TCP 替代协议；
- 一套 RDMA API；
- 一个网卡驱动。

通信只是 UB 的一个组成部分。

---

## 6. UB 与 TCP、RDMA、CXL 的区别

| 技术 | 核心目标 |
|---|---|
| TCP/IP | 通用、可靠的节点间通信 |
| QUIC | 用户态现代可靠传输 |
| RDMA | 高性能远程内存访问和数据搬运 |
| CXL | CPU 与内存、设备之间的资源扩展和池化 |
| UB | 更广义的通信、内存、IO、资源和虚拟化统一互联 |

可以用以下演进关系帮助理解，但不能视为严格替代链：

```text
TCP/IP
  ↓
高性能通信：RDMA
  ↓
内存/设备资源扩展：CXL 类思想
  ↓
统一资源互联：UB
```

---

## 7. UB Service Core 五类能力

### 7.1 UBS Communication

负责高速通信能力，可能包含：

- Socket 兼容通信；
- 原生 URMA 通信；
- RPC；
- 消息队列；
- 连接、消息、内存操作和 Completion。

与 Dragonfly 的主要映射：

```text
TCP/QUIC + Vortex
        ↓
UB Communication / URMA
```

### 7.2 UBS Memory

负责：

- 统一内存编程；
- 远程内存；
- 共享内存；
- 池化内存。

四个概念要区分：

| 概念 | 核心问题 |
|---|---|
| 统一地址空间 | 不同内存如何统一表示、寻址和管理 |
| 远程内存 | 物理内存不在本机，如何访问 |
| 共享内存 | 多个主体如何访问同一份数据 |
| 池化内存 | 多块内存如何汇聚和动态分配 |

它们不是同一个概念，但一块内存资源可能同时属于多个类别。

### 7.3 UBS IO

UBS IO 更偏通用全局 I/O 缓存和访问加速，可能支持：

- POSIX；
- HDFS；
- Block；
- KV；
- 内存、SSD、SSU 等多级介质；
- 预取、淘汰和应用亲和。

与 Dragonfly 的关系：

```text
Dragonfly：面向 Task/Piece/Peer 的 P2P 内容分发缓存
UBS IO：面向应用 I/O 路径的全局读写缓存
```

两者有交集，但不是同一个系统。

潜在组合方式：

1. UBS IO 作为 Dragonfly Content Backend；
2. UBS IO 作为额外一级缓存；
3. UBS IO 替代部分 Parent Peer 数据服务。

第三种会显著改变 Dragonfly DAG、Parent 和 Scheduler 语义，风险最大。

### 7.4 UBS Engine

UBS Engine 偏控制面，负责：

- 节点和资源管理；
- 资源池；
- UB 拓扑；
- 动态调度；
- 高可用；
- 资源发现。

与 Dragonfly Scheduler 的区别：

```text
Dragonfly Scheduler：决定 Piece 从哪个 Peer 获取
UBS Engine：管理资源在哪里、如何组织、如何分配
```

二者更适合协作，而不是替代。

UB 拓扑、链路、资源池和可达性未来可能成为 Dragonfly Parent 评分特征。

### 7.5 UBS Virt

UBS Virt 让虚拟机和容器能够使用 UB 能力，涉及：

- VM/Container 通信；
- 设备和资源暴露；
- 隔离；
- 迁移；
- 快速恢复；
- 资源回收。

在 Kata 场景中，可能有三种方式：

1. Host 使用 UB，Guest 不感知；
2. 通过虚拟设备向 Guest 暴露；
3. UB 设备直通。

第一种适合快速原型，第三种性能潜力大但迁移和隔离更复杂。

---

# 第三部分：UB OS Component 与通信软件栈

## 8. UB OS Component 的定位

UB OS Component 位于上层服务和硬件之间，承担：

- 设备管理；
- 用户态和内核态通信接口；
- 内存管理；
- 地址映射；
- 虚拟化；
- 工具和管理命令。

重点组件包括：

```text
ubfi
ubus
vendor_ubus
ubcore
uburma
ubagg
liburma
uvs
UMS
liburpc
libumq
libcdma
libummu
ubutils
ubctl
```

不能把这些组件统称为 UB Service Core。

---

## 9. UMS 与 URMA 的关系

### 9.1 两条通信路径

```text
Socket-compatible application
        ↓
       UMS
        ↓
      URMA
        ↓
UB OS / UB Device
```

```text
Native UB application
        ↓
      liburma / URMA
        ↓
UB OS / UB Device
```

### 9.2 UMS

UMS 的目标是保持标准 Socket 编程语义：

```c
socket()
connect()
send()
recv()
```

典型使用方式：

```c
socket(AF_SMC, SOCK_STREAM, 0);
```

也可以通过 `LD_PRELOAD` 将普通 `AF_INET` Socket 透明转换为 `AF_SMC`。

UMS 的基本思路：

```text
使用 TCP/SMC 机制进行能力协商和建链
        ↓
成功时使用 URMA/UB 数据通路
        ↓
失败时回退普通 TCP
```

UMS 仍然保持：

- 字节流语义；
- Socket 阻塞/非阻塞行为；
- poll/epoll；
- close；
- 上层 framing。

UMS 适合最低改动接入，但已经确认主线选择 URMA，因此 UMS 后续仅作为兼容和性能对照方案。

### 9.3 Tokio TcpStream 与 AF_SMC

Dragonfly 当前通过：

```rust
TcpStream::connect(addr).await
```

创建 `AF_INET/AF_INET6` Socket。

两种 UMS 接法：

1. **透明方式**：通过 `ums_run` 或 `LD_PRELOAD` 拦截 `socket()`，Dragonfly 代码基本不变；
2. **显式方式**：自己调用 `socket(AF_SMC, SOCK_STREAM | SOCK_NONBLOCK, 0)`，再用 Tokio `AsyncFd` 包装非阻塞 fd。

显式方式不能直接复用 `TcpStream::connect()`，因为 Tokio 没有直接创建 `AF_SMC` Socket 的接口。

---

## 10. ubcore、uburma、liburma 的位置

更准确的 URMA 用户态调用路径可以理解为：

```text
URMA Application
       ↓
liburma USER API
       ↓
User-space Provider
       ↓
liburma CMD API
       ↓
uburma.ko
       ↓
ubcore.ko
       ↓
Kernel Provider / UB Device
```

### 10.1 liburma

- 面向用户态应用提供 URMA API；
- 管理 Context、JFC、JFS、JFR、Jetty、Segment 等对象；
- 加载或调用 Provider；
- 封装用户态快速路径和内核命令路径。

### 10.2 uburma

- 内核模块；
- 将 ubcore 能力暴露给用户态；
- 处理用户态命令、资源创建和映射。

### 10.3 ubcore

- UB 核心内核框架；
- 负责设备、通信资源和底层 Provider 的统一管理；
- 是 UMS、URMA 等能力的共同基础。

### 10.4 UVS

`[待源码确认]` UVS 与 UB 传输服务、EID、建链、虚拟化和路径管理有关，但不能简单等同于 RDMA CM。

---

# 第四部分：URMA 原生通信模型

## 11. URMA 与 RDMA 的对象映射

| RDMA | URMA | 说明 |
|---|---|---|
| Device / Context | Device / Context | 设备和进程操作上下文 |
| QP | Jetty 或 JFS+JFR | 功能相近，结构不同 |
| SQ | JFS | 发送方向资源 |
| RQ | JFR | 接收方向资源 |
| CQ | JFC | 完成队列 |
| MR | Segment | 可被 URMA 操作的内存资源 |
| WR | Work Request | 提交给设备的异步操作 |
| CQE | Completion Entry | 操作完成结果 |
| GID/QPN/IP 等 | EID、Jetty、资源信息 | 地址和通信实体标识 |

这些只是功能映射，不能假设字段、权限和状态机完全一致。

---

## 12. URMA 核心对象

### 12.1 Context

Context 是应用打开某个 URMA 设备后获得的本地操作上下文。

后续资源通常归属于 Context：

```text
Context
├── JFC
├── JFS
├── JFR
├── Jetty
└── Segment
```

### 12.2 JFC

`Jetty for Completion`，保存发送、接收、Read、Write 等操作的完成结果。

关键认知：

```text
Post 成功
≠
操作完成
```

应用必须 Poll JFC 获得 Completion。

### 12.3 JFS

`Jetty for Send`，用于提交发送方向操作，例如：

- SEND；
- READ；
- WRITE；
- Atomic。

即使 Read 的数据方向是远端到本地，它仍是本端主动发起的操作，因此通常属于发送方向资源。

### 12.4 JFR

`Jetty for Receive`，接收方提前提交接收 Buffer。

SEND/RECV 是双边操作：

```text
Receiver Post RECV
        ↓
Sender Post SEND
        ↓
双方产生 Completion
```

若 JFR 没有足够 Receive Buffer，发送可能受流控或失败。

### 12.5 Jetty

Jetty 是更完整的事务层通信资源，可同时包含发送、接收和完成关系。

### 12.6 Segment

Segment 是被 URMA 描述和管理的一段内存资源，可能具有：

- 地址；
- 长度；
- 本地读写权限；
- 远程读写权限；
- Token；
- 生命周期。

普通 `Vec<u8>` 或 `Bytes` 不等于 Segment。Segment 未注销前，底层内存不能移动或释放。

### 12.7 UBVA 与 EID

UBVA 可理解为 UB 地址体系中的虚拟地址，涉及：

```text
EID + VA
```

远端资源不是简单通过 `IP + pointer` 描述，而需要 EID、资源对象、地址和权限信息。

---

## 13. URMA 操作模型

### 13.1 SEND/RECV

适合：

- 控制消息；
- 请求/响应；
- 小消息；
- 第一版 Dragonfly URMA 原型。

Dragonfly 映射：

```text
Child SEND DownloadPiece Request
Parent RECV Request
Parent SEND Piece Metadata / Chunk
Child RECV Piece Data
```

优点：接近现有 Vortex 请求—响应模型。  
缺点：需要预投接收 Buffer，连接多时 JFR 内存占用可能很大。

### 13.2 URMA Write

发起方直接将本地 Segment 数据写入远端 Segment。

Dragonfly 映射：

```text
Child 提供接收 Buffer Descriptor
Parent URMA WRITE Piece Bytes
Child 收到 Completion / Notification
```

优点：适合大 Piece 数据通路。  
缺点：Buffer 生命周期、Credit、权限和流控复杂。

### 13.3 URMA Read

发起方主动从远端 Segment 拉取数据。

Dragonfly 映射：

```text
Parent 暴露 Piece Segment
Child URMA READ Piece
```

优点：保持 Child 主动拉取，热点 Piece 可支持多个 Child Read。  
缺点：当前 Piece 主要存在 Task File，仍需把文件数据准备到可远程访问 Segment。

### 13.4 Atomic

可用于远端计数器、队列索引和轻量同步，但不能用来直接替代 Dragonfly Scheduler、RocksDB metadata 或复杂业务状态机。

---

## 14. URMA API 生命周期

完整生命周期：

```text
1. 初始化 liburma
2. 枚举设备并创建 Context
3. 创建 JFC
4. 创建 JFS/JFR/Jetty
5. 分配并注册 Segment
6. 通过带外通道交换远端资源信息
7. Import 远端 Jetty/Segment
8. 建立传输关系
9. 接收侧预投 RECV
10. 构造并 Post Work Request
11. 设备异步执行
12. Poll JFC
13. 处理成功或错误
14. 按反向顺序释放资源
```

### 14.1 带外通道

URMA 数据面建立前，双方需要交换：

- EID；
- Jetty/JFS/JFR 信息；
- Segment 地址和长度；
- 权限或 Token；
- 传输模式。

交换方式可以是：

- TCP；
- RPC；
- Unix Socket；
- 配置或服务发现；
- Dragonfly Scheduler/Manager 扩展字段。

即使 Piece 数据走 URMA，也不意味着系统完全不再需要 TCP 或 RPC。

### 14.2 Completion 语义

需要区分：

```text
URMA 操作完成
        ↓
Dragonfly Piece 业务完成
```

Dragonfly Piece 完成仍然需要：

```text
收到预期长度
+
CRC/digest 校验成功
+
写入 Content 成功
+
Piece metadata 更新成功
+
通知 waiter
```

URMA Completion 只能替代其中的“通信或内存操作完成”。

### 14.3 资源销毁

销毁前必须：

```text
停止提交请求
→ 等待或取消在途 WR
→ 释放远端导入资源
→ 销毁 Jetty/JFS/JFR
→ 销毁 JFC
→ 注销 Segment
→ 释放 Buffer
→ 销毁 Context
```

不能在设备仍然 DMA 时释放或移动内存。

---

# 第五部分：Dragonfly 与 URMA 接入设计

## 15. Dragonfly 当前数据面关键结论

`[已确认]` Dragonfly normal Piece 数据面：

```text
Child
  ↓ Scheduler 获取 Candidate Parents
ParentSelector
  ↓
TCPDownloader / QUICDownloader
  ↓
Vortex DownloadPiece
  ↓
PieceContentStream
  ↓
write_range_from_stream
  ↓
Task File
  ↓
CRC/digest + metadata + notifier
```

Parent 上传：

```text
Task File / Page Cache
  ↓
FDCache + RangeReader
  ↓
TCP/QUIC + Vortex
  ↓
Child
```

关键特点：

- Scheduler gRPC 属于控制面，Piece bytes 不经过 Scheduler；
- TCP 和 QUIC 上层均使用 Vortex framing；
- 当前每 Piece 新建 TCP/QUIC 连接；
- Piece 以流式 Chunk 写入 Task File；
- `need_piece_content=true` 会写完后再次从本地文件读取；
- standard、persistent、persistent-cache 都可落本地文件，但生命周期和 GC 语义不同。

---

## 16. 确认后的主路线

已确认正式接入方向为：

> **Dragonfly 原生接入 URMA。**

因此后续：

- UMS 仅作为兼容与性能对照；
- 主线研究 liburma/URMA API；
- 第一版优先保证正确性；
- 后续再使用 Read/Write 发挥原生能力。

候选原型排序：

```text
第一原型：URMA SEND/RECV
第二原型：SEND/RECV 控制面 + URMA WRITE 数据面
后续优化：共享 Piece Segment + URMA READ
```

---

## 17. 建议的 Dragonfly URMA 组件

```rust
pub struct UrmaRuntime {
    // Device、Context、JFC、Completion Poller
}

pub struct UrmaConnection {
    // Parent EID、Jetty/JFS/JFR、远端资源
}

pub struct UrmaBufferPool {
    // 长期注册 Segment
}

pub struct UrmaClient {
    // Request ID、在途 Piece、WR 和 Completion
}

pub struct UrmaDownloader {
    // 实现 Dragonfly Downloader
}

pub struct UrmaServer {
    // 接收 Piece 请求并发送 Piece
}
```

建议生命周期：

| 资源 | 生命周期 |
|---|---|
| Device / Context | dfdaemon 进程级 |
| JFC / Completion Poller | Worker 或 Runtime 级 |
| Jetty / JFS / JFR | Parent 连接级 |
| Segment / Buffer Pool | 长期复用 |
| Work Request | Piece 请求级 |
| Completion | 单次操作级 |

不能照搬当前 TCP 模型，在每个 Piece 请求中创建和销毁全部 URMA 资源。

---

## 18. Vortex 与 PieceContentStream 如何处理

第一版建议：

> 保留 Vortex 的业务语义，但不强制保留 TCP 字节流形式。

保留的业务对象：

```text
DownloadPiece(task_id, piece_number)
PieceContent(offset, length, digest)
```

底层可以改为：

```text
URMA Request Message
URMA Metadata Message
URMA Piece Chunk Messages
```

为了减少对 Dragonfly Storage 层的修改，第一版也可以将 Completion Poller 收到的 Chunk 通过 Tokio Channel 转为：

```text
URMA Completion Poller
        ↓
mpsc::Sender<Bytes>
        ↓
ReceiverStream
        ↓
PieceContentStream
```

这样现有：

```text
write_piece_from_stream
write_range_from_stream
CRC32
metadata
```

仍可复用。

---

## 19. 推荐开发路径

不建议一开始直接大规模修改 Dragonfly。

建议顺序：

```text
1. 在 openEuler + UB 环境跑通官方 urma_perftest
2. 阅读 SEND/RECV 示例和真实头文件
3. 写最小 C Piece Demo
4. 写 Rust FFI 包装
5. 写 Rust 文件 → URMA → 文件 Demo
6. 实现 Dragonfly UrmaServer / UrmaClient
7. 实现 UrmaDownloader
8. 增加连接复用和长期注册 Buffer Pool
9. 做 correctness、故障和性能测试
10. 再研究 URMA WRITE / READ 优化
```

最小 Demo 范围：

```text
一个 Client
一个 Server
一个 16 MiB Piece
单连接
单并发
普通文件读取和写入
长度与 digest 校验
```

这样可以先隔离：

- URMA API；
- 设备配置；
- 建链；
- Segment；
- JFR；
- Completion；
- Rust FFI。

---

## 20. URMA 源码阅读策略

URMA 源码有必要看，但不需要读完再编码。

采用：

> **边做最小原型，边按问题追源码。**

优先看：

```text
UMDK tools/urma_perftest
liburma API 头文件
Segment 创建和注册
JFC Poll
JFS/JFR/Jetty 创建
Post SEND/READ/WRITE
Import/Unimport
错误码映射
```

按问题追源码：

| 问题 | 阅读方向 |
|---|---|
| API 不知道怎么调用 | `urma_perftest`、示例和头文件 |
| Post 成功但无 Completion | JFC、Provider 和 Poll 路径 |
| 建链失败 | EID、Jetty、UVS、Import/Bind |
| Segment 注册失败 | liburma Segment API、uburma CMD |
| 内存占用过高 | JFR 深度、共享 JFR、Buffer Pool |
| 性能不理想 | Provider fast path、批量 Post、队列深度 |
| 错误码不明确 | liburma、ubcore、Provider 映射 |

普通 Ubuntu 适合源码阅读和 clangd；真正编译、加载模块和运行 URMA 原型，优先使用 openEuler + UB 设备环境。

---

# 第六部分：当前待确认问题

## 21. URMA API 与通信语义

- Jetty、JFS、JFR 各自适用哪些 transport mode？
- SEND/RECV 是否可靠、有序？顺序作用域是什么？
- JFC Completion 的本地和远端可见性保证是什么？
- Segment 是否需要 Pin Memory？
- Segment 的 Token、权限和 Key 如何表达？
- 一个 JFC 是否适合多个 Connection 共用？
- JFR 是否支持共享接收队列或标签分发？
- 是否支持批量 Post、批量 Poll？
- 在途请求如何取消或 Flush？

## 22. Dragonfly 接入语义

- URMA Endpoint 信息由 Scheduler 还是 Peer 自己交换？
- Parent Descriptor 中需要增加哪些 EID、Jetty、Capability 字段？
- 一个 Parent Connection 是否可并发传多个 Piece？
- Request ID 如何映射 Task/Piece/Parent？
- Chunk 大小和 JFR Buffer 大小如何选择？
- Digest 在传输中还是传输后计算？
- URMA 错误如何映射为切换 Parent、重试或回源？
- 如何保留 TCP/QUIC fallback？
- Server 如何同时服务大量 Child？
- Buffer Pool 和 Segment 如何做 NUMA 亲和？

## 23. 性能与实验

必须对比：

```text
TCP
QUIC
UMS（对照）
URMA SEND/RECV
URMA WRITE
URMA READ
```

测试维度：

- Piece：4 MiB、16 MiB、64 MiB；
- 并发 Piece：1、4、16、32；
- Child 数量：1、4、16；
- warm Page Cache / cold storage；
- 单连接 / 每 Piece 建链；
- Buffer 长期注册 / 每次注册；
- CPU、延迟、吞吐、内存占用、JFR 深度；
- 故障、重试和 fallback。

---

# 24. 当前阶段结论

1. UB 是统一资源互联体系，不能简单等同于 RDMA或更快的 TCP。
2. UMS 保持 Socket 兼容，底层利用 URMA/UB，适合作为低改动对照路径。
3. URMA 是 Dragonfly 正式接入主线，使用显式资源、Segment、Work Request 和 Completion 模型。
4. 第一版建议使用 URMA SEND/RECV，先跑通正确性，再研究 WRITE/READ。
5. Dragonfly 当前的 Piece、digest、Task File、metadata、Notifier 和 Scheduler 语义必须保留。
6. URMA 资源应长期复用，不能每 Piece 创建 Context、Jetty 和 Segment。
7. URMA 源码按问题阅读即可，不必先读完整 UMDK。
8. 下一步应先跑通 `urma_perftest`，然后写最小 Piece Client/Server Demo。

