# 性能测试文档

本目录按**测试环境、传输模式、跨模式对比、历史记录**组织。新测试先建立环境文档，再把结果写入对应模式页。每一条结果都必须明确链接到本目录 `environments/` 下的**具体 Markdown 文件**；只写机器名、设备名或“同上”不够。

## 目录导航

| 位置 | 职责 |
|---|---|
| [environments/](./environments/) | 测试环境快照。每份文档固定一套机器、网络/Provider、软件栈、存储与测试拓扑；环境变化时新建文件；已被 run 引用的环境快照只补勘误，不覆盖原配置。当前尚无环境文档。 |
| [rc-sendrecv.md](./rc-sendrecv.md) | RC SEND/RECV 的测试记录。 |
| [rm-sendrecv.md](./rm-sendrecv.md) | RM SEND/RECV 的测试记录。 |
| [rm-read.md](./rm-read.md) | RM READ 的测试记录。 |
| [comparisons/](./comparisons/) | 基于各模式页已有结果编写的对比分析。当前尚无对比文档。 |
| [history/](./history/) | 旧台账、旧测试方法和阶段性对比，供追溯；其中的结论按当时条件和后续勘误阅读。 |

历史资料入口：[真实 Provider 台账](./history/real-provider-performance-ledger.md)、[验证 Runbook](./history/real-provider-validation-runbook.md)、[1 GiB TCP/URMA 对比](./history/dragonfly-tcp-urma-1g-e2e-performance-comparison.md)。B7 是旧台账中的历史验证阶段，不作为新文件的命名或分组依据。

## 新数据怎么记录

1. **先确定环境**：在 `environments/` 中选定或新建一份环境文档。记录节点角色与拓扑、CPU/NUMA、网络设备与 TP、内核及 UMDK/uburma/ubcore/liburma 版本、Dragonfly/测试工具提交、文件系统与存储、绑核和配置差异。环境变更若影响结果可比性，应新建环境文件，并在记录中说明。
2. **再写模式页**：RC SEND/RECV、RM SEND/RECV、RM READ 分别写到对应文件。每个 run 或表格数据行都要给出环境文档链接、run ID、日期、代码/工具版本、workload（文件与 Piece 大小、并发、预热、样本数）、成功/完整性/fallback、指标口径和原始证据位置。表格不得只在整节开头写一次环境链接；每行须能独立追溯环境。
3. **最后做对比**：在 `comparisons/` 中引用模式页的具体 run，并列出各 run 的环境文档。只有拓扑、软件栈、存储、workload 和统计口径可比时才计算提升比例；不一致处明确写出，不能把历史单机 RTP 与双机 CTP 结果直接做 A/B。

单条记录可从下面的最小格式开始，实际文件名以已建立的环境文档为准：

```markdown
### YYYY-MM-DD · run ID · 测试问题

- 环境：[具体环境名称](./environments/<environment-id>.md)
- 版本：Dragonfly 提交；测试工具提交；Provider/驱动构建
- 负载：拓扑、文件/Piece 大小、并发、预热与有效样本数
- 正确性：成功/尝试数、校验、fallback、retained owner
- 结果：E2E 墙钟、吞吐及所用统计口径；必要的阶段耗时
- 证据：manifest、日志或结果文件的位置
- 结论与边界：只说明本 run 支持的判断
```

新模式页为空时表示**尚未按上述格式登记结果**，不能用 `history/` 的数字填补空白。要采用旧结果，应先确认其环境文档和原始证据，再以可追溯的新记录写入模式页；旧文档继续保留在 `history/`。
