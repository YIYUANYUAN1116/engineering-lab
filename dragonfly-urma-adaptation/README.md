# Dragonfly URMA 适配文档

更新时间：2026-09-28。本目录把源码背景、当前架构、实现状态、真实 Provider 数据与历史材料分开。引用技术结论时先核对模式（RC SEND/RECV、RM SEND/RECV、RM READ）、分支、日期、拓扑和实验条件。

## 当前状态

| 路线 | 文档确认的状态 | 当前事实来源 |
|---|---|---|
| RC SEND/RECV | 已有跨节点 Dragonfly 正常路径与真实 Provider 性能验证；仍需按场景核对故障、shutdown 与扩展门禁。 | [RC/RM 架构主文档](./02-architecture/Dragonfly-URMA-RC-RM-technical-solution-discussion-2026-09-10.md)、[历史 Provider 台账](./04-performance/history/real-provider-performance-ledger.md) |
| RM SEND/RECV | 共享 RM 资源模型和原型已研究；现有单机/分层结果不足以替代跨节点正确性、隔离与同条件 A/B 门禁。 | [RC/RM 架构主文档](./02-architecture/Dragonfly-URMA-RC-RM-technical-solution-discussion-2026-09-10.md)、[历史 Provider 台账](./04-performance/history/real-provider-performance-ledger.md) |
| RM READ | 双机 CTP 正常路径的 128/128 Piece、CRC/写入、内容校验及 fallback 0 已有结果；故障/撤权竞态、部署栈和严格 SEND/RECV A/B 仍待验证。 | [RM READ 状态](./03-implementation/rm-read/status.md)、[RM READ 设计主文档](./02-architecture/dragonfly-urma-rm-read-design-and-roadmap.md)、[历史 Provider 台账](./04-performance/history/real-provider-performance-ledger.md) |

上表只作入口摘要。**架构以两篇设计主文档为准，RM READ 完成度以状态页为准，旧性能数字与实验限制以历史台账为准；新数据按 [04-performance 约定](./04-performance/README.md)进入对应模式页，并逐条关联环境文档。**

## 目录与导航

| 目录 | 用途 | 主要文档 |
|---|---|---|
| [01-background](./01-background/) | Dragonfly RDMA、URMA 源码研究与设计依据 | [源码阅读与映射](./01-background/dragonfly-rdma-source-reading-and-urma-design-notes.md)、[RDMA/URMA 路径对照](./01-background/rdma-urma-upload-download-path-comparison.md)、[Buffer 分析](./01-background/buffer-analysis.md)、[RDMA P2P 参考](./01-background/rdma-p2p.md) |
| [02-architecture](./02-architecture/) | 当前有效的设计 | [RC/RM 架构主文档](./02-architecture/Dragonfly-URMA-RC-RM-technical-solution-discussion-2026-09-10.md)、[RM READ 设计主文档](./02-architecture/dragonfly-urma-rm-read-design-and-roadmap.md) |
| [03-implementation](./03-implementation/) | 实现状态与逐批记录 | [RM READ status](./03-implementation/rm-read/status.md)、[9 月 worklog](./03-implementation/rm-read/worklog-2026-09.md)、[RC Phase B 实施记录](./03-implementation/phase-b-performance-data-path.md)、[RM READ probe 实现](./03-implementation/rm-read/urma-transport-lab-rm-read-probe-implementation.md) |
| [04-performance](./04-performance/README.md) | 环境文档、RC/RM/READ 模式数据、对比与历史资料 | [RC SEND/RECV](./04-performance/rc-sendrecv.md)、[RM SEND/RECV](./04-performance/rm-sendrecv.md)、[RM READ](./04-performance/rm-read.md)；旧数据见 [history](./04-performance/history/) |
| [99-archive](./99-archive/README.md) | 被后续方案覆盖的阶段文档、独立性能汇报与旧台账 | 每篇顶部指向覆盖它的当前文档；技术正文保留。 |

### 阅读顺序

- **设计或实现**：先读对应架构主文档，再读 RM READ [status](./03-implementation/rm-read/status.md)或 RC [Phase B 实施记录](./03-implementation/phase-b-performance-data-path.md)，需要过程细节时查[工作日志](./03-implementation/rm-read/worklog-2026-09.md)。
- **性能或复现**：先读[性能目录约定](./04-performance/README.md)，按模式进入对应数据页并核对每条结果所链的环境文档；旧结果和 Runbook 在 [history](./04-performance/history/) 中追溯。
- **历史问题**：先查[archive](./99-archive/README.md)顶部的覆盖关系；早期数字和“当前状态”只代表文中日期。

## Top TODO

1. 完成 RM READ 故障、撤权竞态和不同部署栈的门禁；目标栈与 source backing 行为先确认，再决定默认实现。见[状态页](./03-implementation/rm-read/status.md)。
2. 取得新低频埋点的真机结果，拆分 source register/offer、READ owner/CQE 与 Storage 消费；新实验先建立环境文档，再登记到 [RM READ 模式页](./04-performance/rm-read.md)。
3. 在同机器、拓扑、Piece/CC、文件系统、绑核与预热条件下，对 READ 与 SEND/RECV 独立分支做 A/B；不要混用单机 RTP 与双机 CTP 数字。
4. RM SEND/RECV 的跨节点正确性、shared RX/credit/fault isolation 和高 fan-out 门禁仍需按[架构主文档](./02-architecture/Dragonfly-URMA-RC-RM-technical-solution-discussion-2026-09-10.md)逐项验证，结果按[性能目录约定](./04-performance/README.md)登记。

## 维护规则

- 架构更新写入对应设计主文档；完成度写入短 status；逐批过程写入带月份的 worklog；新性能 run、口径、勘误写入对应模式页并链接具体环境文档；旧 B7 记录留在 history。不要在汇报稿另建“最新数据”。
- 新实验至少记录 run ID、代码/工具版本、节点与 Provider 栈、拓扑、Piece/CC、文件系统、预热/样本、完整性、fallback 和阶段耗时。不同条件的吞吐不能直接排序。
- 历史材料保留原技术内容；若结论被覆盖，在顶部指向当前文档和证据。B3.2/B3.3 在此次盘点的文件和正文中均未找到独立条目。
