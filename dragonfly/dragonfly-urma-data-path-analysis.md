# Dragonfly 与 URMA 数据传输路径分析

## 1. 背景

Dragonfly 是基于 P2P 架构的数据分发系统，Peer 之间通过网络传输 Piece 数据。当前 Dragonfly 主要使用 TCP、QUIC 等协议完成 Peer 间数据传输。

URMA 提供基于消息语义的高性能通信能力，通过 SEND/RECV、Registered Memory、Completion Queue 等机制实现数据传输。

将 Dragonfly 接入 URMA 时，需要首先明确：

- Dragonfly Peer 之间传输的具体数据内容；
- 当前 TCP/QUIC 数据路径；
- URMA SEND/RECV 数据路径；
- 两种模型之间的差异。

---

# 2. Dragonfly Piece 数据模型

一次 Piece 下载过程：

```
Request
   |
Metadata
   |
Piece Data
   |
End
```

## 2.1 Request

Child Peer 请求指定 Piece：

```
DownloadPiece Request
{
    task_id,
    piece_number
}
```

用于标识任务以及需要获取的 Piece 编号。

## 2.2 Metadata

Parent 返回 Piece 元信息：

```
Piece Metadata
{
    offset,
    length,
    digest
}
```

包含 Piece 在文件中的位置、长度以及校验信息。

## 2.3 Piece Data

真正的数据内容：

```
Piece Body

chunk1
chunk2
chunk3
...
chunkN
```

## 2.4 End

表示 Piece 数据发送完成，用于触发最终校验。

---

# 3. Dragonfly TCP/QUIC 数据路径

## 3.1 总体流程

```
Parent Peer

Piece 文件
    |
 Storage
    |
 RangeReader
    |
 TCP send / QUIC stream
    |
 网络传输
    |
 TCP recv / QUIC recv

Child Peer

PieceContentStream
    |
 Bytes
    |
 Storage
    |
 Piece 落盘
```

---

# 4. TCP 数据路径

## Parent 发送

```
磁盘文件
   |
Storage
   |
RangeReader
   |
用户态 Buffer
   |
send()
   |
Kernel TCP Buffer
   |
NIC
   |
网络
```

## Child 接收

```
网络
   |
NIC
   |
Kernel TCP Buffer
   |
recv()
   |
Tokio TcpStream
   |
ReaderStream
   |
Bytes
   |
PieceContentStream
   |
Storage
```

TCP 提供 Byte Stream 语义，不感知 Piece 边界。Dragonfly 通过应用层协议维护 Request、Metadata、Data、End 等语义。

---

# 5. URMA 数据路径

## 5.1 总体流程

```
Parent Peer

Piece 文件
    |
 Storage
    |
 RangeReader
    |
 TX Buffer
    |
 SEND WR Submit
    |
 URMA Fabric
    |
 RX Buffer
    |
 CQE
    |
 Completion Poller
    |
 Bytes
    |
 PieceContentStream
    |
 Storage
```

---

# 6. URMA Message 模型

URMA 基于 Message Completion 模型：

```
SEND WR
   |
   |
RECV Buffer
   |
   |
Completion Queue Entry
```

## Request Message

```
Request
{
    task_id,
    piece_number
}
```

## Metadata Message

```
Metadata
{
    offset,
    length,
    digest
}
```

## Data Message

```
Data
{
    request_id,
    sequence,
    payload
}
```

## End Message

```
End
{
    total_length
}
```

---

# 7. URMA Buffer 生命周期

## Copy 模式

```
URMA RX Buffer
        |
        |
      memcpy
        |
        |
      Bytes
        |
        |
PieceContentStream
```

优点：生命周期简单，与现有 Dragonfly 数据流兼容。

缺点：增加一次 CPU copy。

## Zero Copy 模式

```
URMA RX Buffer
        |
 Buffer Lease
        |
 Storage
        |
 Release Buffer
        |
 Repost RECV
```

需要解决 Buffer 所有权以及复用时机问题。

---

# 8. TCP/QUIC 与 URMA 对比

|项目|TCP/QUIC|URMA|
|-|-|-|
|通信模型|Byte Stream|Message|
|发送方式|send/write|SEND WR|
|接收方式|recv/read|RECV WR|
|完成通知|read 返回|CQE|
|Buffer管理|Socket 管理|应用管理|
|内存模型|普通内存|Registered Memory|

---

# 9. Dragonfly 接入 URMA 映射

保持 Dragonfly 上层 Piece 语义不变：

```
UrmaDownloader
        |
UrmaConnection
        |
CompletionPoller
        |
RX Buffer
        |
PieceContentStream
        |
Storage
```

核心工作：

1. Piece 业务语义映射到 URMA Message；
2. Byte Stream 模型转换为 Message Stream；
3. CQE 转换为 Dragonfly PieceContentStream；
4. Buffer 生命周期管理。

---

# 10. 总结

Dragonfly 当前数据路径：

```
Storage
 |
TCP/QUIC
 |
Network
 |
PieceContentStream
 |
Storage
```

URMA 目标数据路径：

```
Storage
 |
TX Buffer
 |
SEND WR
 |
URMA Fabric
 |
RX Buffer
 |
CQE
 |
Completion Poller
 |
PieceContentStream
 |
Storage
```

URMA 接入并不是简单替换网络 API，而是完成数据模型、消息边界以及 Buffer 生命周期的重新设计。
