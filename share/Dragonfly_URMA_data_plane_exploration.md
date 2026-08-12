# Dragonfly * URMA 适配

## 1. Dragonfly 组件介绍

### 1.1 定位

Dragonfly 是一个基于 P2P 的文件分发系统，主要用于：

- 容器镜像加速
- 大文件分发
- AI 模型文件分发

核心思想：

> 将一次文件下载转换为多个 Peer 之间的数据协同传输，降低源站压力，提高分发效率。

### 1.2 核心组件

```text
                 Manager
                    |
              Scheduler
                    |
      +-------------+-------------+
      |                           |
    Peer                       Peer
      |
   dfdaemon
      |
 下载 / 缓存 / 上传
```

|组件|职责|
|-|-|
|Manager|集群管理、配置管理|
|Scheduler|任务调度、Peer选择|
|Peer|数据传输节点|
|dfdaemon|客户端代理，负责下载、缓存|
|Seed Peer|源数据节点|

---

## 2. Dragonfly 架构

### 2.1 控制面

```text
Manager
   |
Scheduler
   |
Peer状态管理
Task调度
Parent选择
```

负责：

- Peer注册
- Task管理
- Parent Peer选择
- 调度策略

---

### 2.2 数据面

```text
Child Peer

dfdaemon

    |
 Downloader

    |
 TCP / QUIC

    |

Parent Peer
```

负责：

- Piece请求
- Piece传输
- 数据校验
- 缓存管理

---


## 3. Dragonfly 数据流转

当前 Dragonfly standard Piece 数据路径：

```text
用户请求文件

      |
      v

dfdaemon(Client)

      |
      v

Scheduler

选择 Parent Peer

      |
      v

Piece 下载任务

      |
      v

Downloader

      |
      +----------------+
      |                |
      v                v

 TCP Transport    QUIC Transport

      |                |

      +-------网络------+

              |
              v

        Parent Peer

              |
              v

       Piece Cache / Storage

              |
              v

        Piece Data返回

              |
              v

          Child Storage

              |
              v

          文件合并
```

核心调用关系：

```text
Piece

 |

Downloader

 |

TCPDownloader / QUICDownloader

 |

PieceContentStream

 |

Storage
```

Dragonfly 已经通过 Downloader 抽象隔离传输实现，Storage 只依赖 PieceContentStream，因此理论上可以增加新的 Transport 实现。

---

## 4. URMA 组件介绍

### 4.1 定位

URMA 是灵衢 UB 提供的数据通信接口，提供面向硬件通信资源的数据传输模型。

与传统 Socket 不同，URMA 使用：

- Context
- Jetty
- Completion Queue
- Registered Memory

等对象管理通信。

---

### 4.2 核心对象

```text
Application

    |
    v

URMA API

    |
    v

Context

    |
    +------ JFR
    |
    +------ JFC
    |
    +------ Jetty
    |
    +------ Memory Segment

    |
    v

Provider / Hardware
```

|组件|作用|
|-|-|
|Context|URMA上下文|
|JFR|接收请求队列|
|JFC|完成事件队列|
|Jetty|通信端点|
|Segment|注册内存|
|WR|发送/接收请求|
|CQE|完成事件|

---

## 5. URMA 初始化流程

```text
urma_init

    |
    v

获取 Device

    |
    v

创建 Context

    |
    v

创建 JFR

    |
    v

创建 JFC

    |
    v

创建 Jetty

    |
    v

注册 Memory

    |
    v

交换 Descriptor

    |
    v

Bind

    |
    v

Ready
```


## 6. 数据流转对比：TCP vs URMA

### 6.1 TCP 数据路径

```text
Application

    |
    v

User Buffer

    |
    | copy

    v

Kernel Socket Buffer

    |
    v

NIC

    |
    v

Network

    |
    v

NIC

    |
    v

Kernel Socket Buffer

    |
    | copy

    v

User Buffer

    |
    v

Application
```

TCP特点:
```text
用户态 Buffer
      |
      v
Kernel Socket Buffer
      |
      v
NIC
      |
      v
Network
      |
      v
NIC
      |
      v
Kernel Socket Buffer
      |
      v
用户态 Buffer
```



---

### 6.2 QUIC 数据路径
```
Application

    |
    v

User Buffer

    |
    v

QUIC Library
(User Space)

    |
    v

UDP Socket

    |
    v

Kernel UDP/IP Stack

    |
    v

NIC

    |
    v

Network

    |
    v

NIC

    |
    v

Kernel UDP/IP Stack

    |
    v

UDP Socket

    |
    v

QUIC Library
(User Space)

    |
    v

User Buffer

    |
    v

Application

```

QUIC特点：

```text
用户态 Buffer
      |
      v
QUIC Library
      |
      v
UDP Socket Buffer
      |
      v
NIC
      |
      v
Network
      |
      v
NIC
      |
      v
UDP Socket Buffer
      |
      v
QUIC Library
      |
      v
用户态 Buffer
```

### 6.3 URMA 数据路径

```text
Application

    |
    v

Registered Memory

    |
    v

WR

    |
    v

URMA Provider

    |
    v

NIC

    |
    v

Network

    |
    v

NIC

    |
    v

Remote Jetty

    |
    v

Remote Registered Memory

    |
    v

CQE

    |
    v

Application
```

核心流程：

```text
Application Buffer

        |

Registered Memory

        |

SEND WR

        |

Jetty

        |

Remote Jetty

        |

CQE
```

---

### 6.3 TCP 、QUIC、 URMA 核心区别

|        | TCP              | QUIC                    | URMA                 |
| ------ | ---------------- | ----------------------- | -------------------- |
| 传输基础   | TCP              | UDP                     | UB/URMA              |
| 可靠性实现  | Kernel TCP Stack | User Space QUIC Library | Provider/Hardware    |
| 协议处理位置 | Kernel           | User Space              | Provider + Hardware  |
| 通信模型   | Byte Stream      | Stream/Frame            | Message + Completion |
| 主要接口   | Socket           | QUIC API                | URMA API             |
| 完成通知   | read/epoll       | QUIC事件                  | CQE                  |


---



## 7. Dragonfly 接入 URMA 涉及的关键适配点


### 7.1. URMA 通信层（Transport）

解决：

Dragonfly 的 Piece 数据如何通过 URMA 从 Parent 传输到 Child。

当前：
```
Downloader

    |

TCP / QUIC

    |

Parent Peer
```
接入后：
```
Downloader

    |

UrmaDownloader

    |

URMA Transport

    |

Jetty / SEND / RECV

    |

Parent Peer
```
需要处理：

URMA连接建立
Jetty创建与绑定
Request/Response消息设计
Peer间通信管理
重连和异常处理
### 7.2. 数据面处理（Data Path）

解决：
```
URMA收到的数据如何转换成 Dragonfly 能消费的数据。
```

Dragonfly当前接口：
```
Downloader

    |

PieceContentStream

    |

Storage
```

因此 URMA需要实现：
```
URMA CQE

    |

Buffer处理

    |

Bytes

    |

PieceContentStream

    |

Storage
```
需要处理：

Piece Request
Piece Metadata
Data Chunk
End/Error消息
request_id映射
Buffer生命周期
CQE处理
数据校验


### 7.3. URMA 生命周期管理（Runtime）

解决：

URMA通信资源如何初始化、维护和释放。

包括：
```
urma_init

    |

Device

    |

Context

    |

JFC

    |

Jetty

    |

Memory Register

    |

Shutdown
```
需要管理：

liburma初始化
Device选择
Context生命周期
JFC/CQ管理
Jetty连接池
Registered Memory
Buffer Pool
异常恢复

这个属于 URMA 基础设施层，

### 7.4. Scheduler / 控制面适配

解决：
```
Scheduler如何知道哪些Peer支持UB/URMA，以及如何选择。
```
当前：
```
Scheduler

    |

Parent Peer

    |

TCP Port
QUIC Port
```
未来可能：
```
Scheduler

    |

Parent Peer

    |
    + TCP endpoint
    + QUIC endpoint
    + URMA endpoint
    + UB capability
```
需要：
UB节点发现方式
URMA能力上报
Peer能力模型扩展
调度策略是否考虑：
UB网络拓扑
带宽
延迟
内存资源


## 8.测试阶段

### 8.1：URMA基础能力测试
目的：

验证URMA通信能力。

环境：

```
Node A

URMA Parent

        |

        |

Node B

URMA Child
```
```
验证：

init
device发现
context创建
jetty创建
shutdown
通信

验证：

SEND/RECV
CQE
数据正确性
```
### 8.2 Dragonfly + URMA功能测试

单文件下载 
验证：
```
 文件完整性；
 Piece数据校验正确性。
```
多Piece并发 
验证：

```
 多request_id并发管理；
 TX/RX Buffer资源复用；
 CQE到Piece任务的完成事件路由正确性。
```
Cache场景
验证：

```
第一次：

Origin

 |

Parent Cache

 |

Child

第二次：

Parent Cache

 |

Child
```
### 8.3 性能测试

通过构造不同文件大小、不同任务并发量的测试场景，对比 TCP、QUIC、URMA 三种数据传输方案的：

- 端到端时延
- 网络吞吐率
- Peer资源占用

评估 URMA 相比现有 TCP/QUIC 方案在大规模文件分发场景下的性能收益，并分析不同方案的适用场景。


## 9.当前阶段
当前完成

已完成：

- Dragonfly架构和数据路径分析
- Downloader接入点分析
- URMA通信模型分析
- URMA Transport Demo设计

待完成:

- URMA Transport Demo 通信验证
- Dragonfly URMA适配
- Dragonfly URMA功能/性能测试