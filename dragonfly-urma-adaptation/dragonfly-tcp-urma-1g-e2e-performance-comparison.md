# Dragonfly TCP vs URMA 1 GiB E2E 下载性能对比

更新时间：2026-09-08

## 1. 文档说明

本文中的“时间”是由测试记录的 E2E throughput 换算得到的 **dfget 端到端 wall time / batch makespan**，不是纯 NIC wire time。

---

## 2. 测试环境

### 2.1 拓扑

- Parent：node1
- Child：node2
- URMA device：`udmac0d1e2`
- URMA transport：RTP + RC
- EID index：1
- URMA chunk：64 KiB
- TCP 对照环境：25 Gbps Ethernet
- Dragonfly workload：真实 `dfget + dfdaemon`
- Parent 在测试前完成 task preheat
- Child 禁止 back-to-source，确保测试主路径为 Parent P2P 下载

### 2.2 计时与吞吐口径

单任务：

```text
throughput = measured task 总字节数 / measured dfget wall time 总和
```

对于 1 GiB 文件：

```text
1 GiB = 1024 MiB

平均 E2E 时间(ms)
= 1024 / throughput(MiB/s) × 1000
```

多 lane fan-out：

```text
batch 数据量 = lane 数 × 1 GiB

batch makespan(ms)
= lane 数 × 1024 MiB / aggregate throughput(MiB/s) × 1000
```

所以多 lane 场景不能简单使用 `1024 / aggregate throughput` 作为单个任务完成时间。

---

## 3. TCP 单任务 Piece concurrency 基线

测试口径：

- 单个 `dfget`
- 1 GiB 文件
- task concurrency = 1
- CC = `download.concurrentPieceCount`
- 2 次 warmup + 5 次 measured task
- TCP 运行在 25 Gbps 网卡上
- TCP 使用当时 Dragonfly 默认 Piece geometry；未固定为后续 URMA 使用的 16 MiB Piece

| Piece CC | TCP Throughput (MiB/s) | Gbps | 1 GiB E2E 时间 |
|---:|---:|---:|---:|
| 1 | 1581.77 | 13.27 | **647.4 ms** |
| 2 | 2286.19 | 19.18 | **447.9 ms** |
| 4 | 2530.39 | 21.23 | **404.7 ms** |
| 8 | 2552.23 | 21.41 | **401.2 ms** |
| 16 | 2553.02 | 21.42 | **401.1 ms** |
| 32 | 2543.42 | 21.34 | **402.6 ms** |

TCP 在 CC4 后已经进入明显平台区。CC8～CC16 可视为当前 25 Gbps 环境下的有效饱和点：

```text
CC4   ≈ 404.7 ms / GiB
CC8   ≈ 401.2 ms / GiB
CC16  ≈ 401.1 ms / GiB
CC32  ≈ 402.6 ms / GiB
```

因此可以把当前 TCP 饱和能力概括为：

> **Dragonfly TCP 在该 25 Gbps 环境下，传输 1 GiB 数据约需要 0.40 s。**

---

## 4. URMA `pwritev` 后单任务 Piece concurrency 曲线

URMA 数据来自接收端改为 receive-window `pwritev` 后的正式 B7 曲线。

固定条件：

- 单个 `dfget`
- 1 GiB 文件
- Piece = 16 MiB
- chunk = 64 KiB
- `post1`
- `pipe2`
- `in16`
- task concurrency = 1
- CC = `download.concurrentPieceCount`
- 1 次 warmup + 3 次 measured task
- Parent mmap source
- Child registered RX direct-write
- receive-window `pwritev`
- CRC32 与 write 保留在正常 E2E 路径中

| Piece CC | URMA Throughput (MiB/s) | Gbps | 1 GiB E2E 时间 |
|---:|---:|---:|---:|
| 1 | 2223.98 | 18.66 | **460.4 ms** |
| 2 | 3290.99 | 27.61 | **311.2 ms** |
| 4 | 5021.63 | 42.12 | **203.9 ms** |
| 8 | 7100.04 | 59.56 | **144.2 ms** |
| 16 | 7570.72 | 63.51 | **135.3 ms** |
| 32 | 6762.88 | 56.73 | **151.4 ms** |

当前最优点为 CC16：

```text
7570.72 MiB/s
≈ 63.51 Gbps
≈ 135.3 ms / GiB
```

CC32 开始出现过并发回退，E2E 时间回升到约 151 ms。

---

## 5. TCP vs URMA：1 GiB 时间直观对比

下面直接把当前两条曲线按 1 GiB E2E 时间并列。

> 注意：TCP 旧基线使用默认 Piece geometry，URMA 使用固定 16 MiB Piece。因此本表适合作为“当前已测饱和性能参考”，不能标为严格同参数 A/B。后续若补做固定 16 MiB Piece 的 TCP 重测，应以新数据替换本表 TCP 列。

| Piece CC | TCP 1 GiB | URMA 1 GiB | 时间下降 | 吞吐倍率 |
|---:|---:|---:|---:|---:|
| 1 | 647.4 ms | **460.4 ms** | **28.9%** | 1.41× |
| 2 | 447.9 ms | **311.2 ms** | **30.5%** | 1.44× |
| 4 | 404.7 ms | **203.9 ms** | **49.6%** | 1.98× |
| 8 | 401.2 ms | **144.2 ms** | **64.1%** | 2.78× |
| 16 | 401.1 ms | **135.3 ms** | **66.3%** | 2.97× |
| 32 | 402.6 ms | **151.4 ms** | **62.4%** | 2.66× |

最直观的饱和点对比：

```text
TCP CC16:
1 GiB ≈ 401 ms

URMA CC16:
1 GiB ≈ 135 ms
```

即当前测得：

- 1 GiB E2E 时间约从 401 ms 降到 135 ms；
- 时间减少约 66%；
- 对应吞吐约提升 2.97×。

这不是“纯网络传输时延降低 66%”，而是 Dragonfly 真实 `dfget` E2E 路径的时间变化。

---

## 6. URMA 多 lane fan-out：1 GiB × N 批量完成时间

### 6.1 拓扑含义

这里的 L1/L2/L4/L8 不是“一个 Child 与 Parent 建 1/2/4/8 条 lane”。

实际 fan-out 拓扑为：

```text
一个 Parent dfdaemon
  ├─ child daemon 1 -> 1 persistent RC lane
  ├─ child daemon 2 -> 1 persistent RC lane
  ├─ ...
  └─ child daemon N -> 1 persistent RC lane
```

L2/L4/L8 的 child daemon 都运行在同一个物理 Child host 上。

该测试验证的是：

> 一个 Parent process 面向多个独立 Peer/daemon 的 fan-out aggregate throughput。

不是多物理 Child 节点扩展测试，也不是单文件多 lane striping。

每条 lane 内固定 CC8：

```text
L8 × CC8
= 8 个独立 Peer/lane
× 每 lane 最多 8 个 active Piece
```

### 6.2 数据与时间换算

固定主要条件：

- 每 task：1 GiB
- Piece：16 MiB
- 每 lane CC8
- `post1`
- `pipe2`
- `in16`
- 1 次 warmup + 3 次 measured batch

L1/L2/L4 来自 12.8 fan-out 曲线；L8 使用 permit 修复后 TX128 的两次纯 URMA PASS 均值 16003.86 MiB/s。

| Lane | 同时下载的数据量 | Aggregate MiB/s | Aggregate Gbps | Batch E2E 时间 | TX 条件 |
|---:|---:|---:|---:|---:|---|
| L1 | 1 GiB | 7212.60 | 60.50 | **142.0 ms** | TX64 MiB |
| L2 | 2 GiB | 10737.82 | 90.08 | **190.7 ms** | TX64 MiB |
| L4 | 4 GiB | 13648.16 | 114.49 | **300.1 ms** | TX64 MiB |
| L8 | 8 GiB | 16003.86 | 134.25 | **511.9 ms** | TX128 MiB（两次纯 URMA 均值） |

换成更直观的表达：

```text
L1：1 × 1 GiB -> 约 142 ms
L2：2 × 1 GiB -> 约 191 ms
L4：4 × 1 GiB -> 约 300 ms
L8：8 × 1 GiB -> 约 512 ms
```

L8 的含义是：

```text
总数据量 = 8 GiB
aggregate throughput ≈ 16003.86 MiB/s
batch makespan ≈ 512 ms
```

不能把 `1024 / 16003.86 ≈ 64 ms` 解释成某一个 1 GiB dfget 的真实完成时间；64 ms 只是 aggregate 带宽折算后的“等效传输 1 GiB 数据量时间”。

L8 两次纯 URMA PASS：

- 16053.62 MiB/s / 134.67 Gbps / Jain 0.99900
- 15954.09 MiB/s / 133.83 Gbps / Jain 0.99951

均值：

```text
16003.86 MiB/s
≈ 134.25 Gbps
```

高 Jain 指数说明 8 个任务吞吐较均衡，因此可以认为各 1 GiB task 的完成时间大致聚集在同一 batch makespan 附近，但不能把 aggregate throughput 当作单 lane throughput。

---

## 7. 当前可用于汇报的核心数字

### 单任务

| 场景 | 1 GiB E2E 时间 | 说明 |
|---|---:|---|
| TCP 饱和（CC8～16） | **约 401 ms** | 25 Gbps TCP 路径已基本打满 |
| URMA CC8 | **约 144 ms** | 16 MiB Piece + `pwritev` |
| URMA CC16 | **约 135 ms** | 当前单任务最佳点 |
| URMA CC32 | **约 151 ms** | 过并发开始回退 |

### 多 Peer fan-out

| 场景 | 总数据量 | 完成时间 |
|---|---:|---:|
| L1 × CC8 | 1 GiB | **约 142 ms** |
| L2 × CC8 | 2 GiB | **约 191 ms** |
| L4 × CC8 | 4 GiB | **约 300 ms** |
| L8 × CC8 | 8 GiB | **约 512 ms** |

可以概括为：

> **当前 Dragonfly TCP 在 25 Gbps 环境下传输 1 GiB 约 0.40 s；URMA 单任务最优约 0.135 s。一个 Parent 同时服务 8 个独立 Peer、总计传输 8 GiB 时，当前纯 URMA aggregate 约 134.25 Gbps，整批约 0.51 s 完成。**

---

