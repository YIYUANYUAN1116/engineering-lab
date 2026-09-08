# Dragonfly TCP / URMA 性能对比

## 1. 测试目的

本轮测试主要评估 Dragonfly 在当前环境下使用 URMA 替代 TCP 进行 Peer-to-Peer 数据传输后的性能收益。

本文统一使用：

> **传输 1 GiB 数据需要多少时间**

作为主要展示口径，同时保留吞吐数据用于辅助说明。

---

## 2. 测试环境

| 项目 | 配置 |
|---|---|
| 测试节点 | 2 台服务器，Parent / Child |
| Dragonfly workload | `dfget + dfdaemon` |
| 测试文件 | 1 GiB |
| TCP 网络 | 25 Gbps Ethernet |
| NVMe 存储性能 | 顺序读写约 6.6–6.7 GB/s（4 GiB direct I/O 实测） |
| URMA 设备 | `udmac0d1e2` |
| URMA 模式 | RTP + RC |
| URMA Chunk | 64 KiB |
| URMA Piece | 16 MiB |
| 数据校验 | CRC32 |
| Child 写入 | `pwritev` |

测试结果均以真实 `dfget` 端到端执行时间为口径，包括 Piece 下载、数据传输、CRC 校验、落盘和任务收尾。

---

## 3. 单任务 TCP / URMA 对比

### 3.1 1 GiB 下载时间

| Piece 并发 | TCP | URMA |
|---:|---:|---:|
| 1 | 647 ms | 460 ms |
| 2 | 448 ms | 311 ms |
| 4 | 405 ms | 204 ms |
| 8 | 401 ms | 144 ms |
| 16 | 401 ms | **135 ms** |
| 32 | 403 ms | 151 ms |

### 3.2 结果趋势

TCP 在 CC4 后已经基本达到平台：

```text
CC4   ≈ 405 ms
CC8   ≈ 401 ms
CC16  ≈ 401 ms
CC32  ≈ 403 ms
```

即当前 25 Gbps TCP 环境下：

> **1 GiB 下载时间稳定在约 0.40 s。**

URMA 随 Piece 并发提升仍能继续扩展：

```text
CC1   ≈ 460 ms
CC2   ≈ 311 ms
CC4   ≈ 204 ms
CC8   ≈ 144 ms
CC16  ≈ 135 ms
CC32  ≈ 151 ms
```

目前阶段最佳点为 CC16：

> **1 GiB 下载时间约 0.135 s。**

---

## 4. 核心性能结论

选取双方已经基本进入稳定区间的结果：

| 指标 | TCP | URMA |
|---|---:|---:|
| 代表并发 | CC16 | CC16 |
| 1 GiB 下载时间 | **401 ms** | **135 ms** |
| 吞吐 | 2553 MiB/s | 7571 MiB/s |
| 约合带宽 | 21.4 Gbps | 63.5 Gbps |

结果表明：

- 1 GiB 下载时间从约 **401 ms 降至 135 ms**；
- E2E 时间减少约 **66%**；
- 实际 Dragonfly 下载吞吐约提升 **2.97 倍**。

可以概括为：

> **在当前测试环境下，URMA 将 Dragonfly 单任务 1 GiB P2P 下载时间从约 0.4 秒降低到约 0.135 秒。**

---

## 5. URMA 多 Peer 聚合能力

在单任务性能验证基础上，进一步测试 Parent 同时服务多个独立 Peer 的场景。

测试模型：

```text
1 个 Parent dfdaemon
    ↓
1 / 2 / 4 / 8 个独立 Child daemon
```

每个 Child daemon 独立建立一条 persistent RC lane，每条 lane 内 Piece 并发为 CC8。

这里的 L8 表示：

> **一个 Parent 同时服务 8 个独立 Peer。**


### 5.1 测试结果

| 并发 Peer | 总数据量 | 聚合吞吐 | 聚合带宽 | 整批完成时间 |
|---:|---:|---:|---:|---:|
| L1 | 1 GiB | 7213 MiB/s | 60.5 Gbps | **142 ms** |
| L2 | 2 GiB | 10738 MiB/s | 90.1 Gbps | **191 ms** |
| L4 | 4 GiB | 13648 MiB/s | 114.5 Gbps | **300 ms** |
| L8 | 8 GiB | 16004 MiB/s | 134.3 Gbps | **512 ms** |

### 5.2 结果解读

随着 Peer 数增加，Parent 聚合带宽持续提升：

```text
L1   60.5 Gbps
L2   90.1 Gbps
L4  114.5 Gbps
L8  134.3 Gbps
```

L8 场景下：

> **一个 Parent 同时向 8 个 Peer 传输共 8 GiB 数据，整批约 0.51 s 完成。**

---

## 6. 当前阶段结论

当前阶段可以得到以下结论：

### 6.1 单任务性能

TCP 在 25 Gbps 网络下已经基本达到平台，1 GiB 下载约需要：

> **0.40 s**

URMA 当前最佳结果为：

> **0.135 s**

单任务 E2E 性能已经表现出明显优势。

### 6.2 并发扩展能力

URMA 从 CC1 提升到 CC16 时，单任务性能持续提升，表明当前数据路径可以通过 Piece 并发进一步利用底层传输能力。

CC32 开始回退，因此当前最合理的单任务并发点在：

> **CC8 ～ CC16**

### 6.3 多 Peer 聚合能力

Parent 在 L1 → L8 的过程中，聚合吞吐从约：

> **60.5 Gbps → 134.3 Gbps**

说明当前实现已经具备多 Peer fan-out 扩展能力。

但 L8 的 134 Gbps 与底层 URMA transport 约 400 Gbps 级能力仍有明显差距，后续性能优化空间仍主要位于 Dragonfly 应用数据路径，而不是简单继续增加并发。

---
