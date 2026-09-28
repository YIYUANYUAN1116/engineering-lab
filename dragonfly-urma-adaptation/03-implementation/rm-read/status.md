# RM READ 实现状态

更新：2026-09-28。本页只记录当前判断。协议与权限边界以[RM READ 设计主文档](../../02-architecture/dragonfly-urma-rm-read-design-and-roadmap.md)为准；既有实验数据与口径见[历史 Provider 台账](../../04-performance/history/real-provider-performance-ledger.md)为准；逐批实现、命令和历史勘误见[9 月工作日志](./worklog-2026-09.md)。

## 已完成项

- `urma-read-prototype` 以 RM + READ 作为 bulk data 路径，TCP 承担控制面；该分支不以 SEND/RECV 作为生产 bulk 路径。
- Child 使用每 Piece 的 registered destination lease，READ CQE 和 terminal gate 完成后交给 Storage，消费完成后回收；destination pool 已在样本中命中。
- Parent 对每 Piece 导出独立 Segment/token；page-aligned Storage 映射可直接注册，并持有映射直至安全撤权。异常路径保留 owner 和预算。
- 32 MiB Piece 默认一条 32 MiB READ WR；owner 命令支持批量提交同一 Piece 的多条 WR，native shim 仍逐条 post，CQE 逐 WR 确认。
- 双机 CTP 正常路径已完成 CRC、写入和内容校验，128/128 Piece 成功，fallback 0。

## 未完成项

- 失败、撤权竞态和不同部署栈组合的完整门禁尚未完成；控制超时、TCP EOF 或普通 unregister 返回值不能单独作为撤权证明。
- READ 与 SEND/RECV 尚无相同机器、拓扑、Piece/CC、文件系统、绑核和预热条件下的严格 A/B。
- 新增的低频时间字段尚无真机结果；B7 汇总脚本尚未展示这些字段。

## 阻塞与前置条件

- 目标部署的内核、uburma/ubcore、liburma 构建及 source backing 行为尚需确定。196 与 198 的 file-backed 注册表现不同，198 作为 Parent 的产品路径和同会话 file/anon 对照尚未闭环，因此尚不能确定跨栈默认 source backing。

## 下一步

1. 在目标栈核对模块与 liburma 构建，完成 198 Parent 的 exact-Piece file-backed 与 anonymous staging 对照；保持独立 token/generation 和撤权边界。
2. 用已有低频字段拆分 READ owner 查询/post/轮询、source register/offer、READ、Storage pwrite/CRC/writeback/回收，再选择优化点。
3. 在相同 workload 下用独立 SEND/RECV 分支与 READ 分支做 A/B；区分完整 dfget、Piece 稳态和 transport-only 口径。

## 关键实验结果

| 实验 | 已成立的结论与边界 |
|---|---|
| read-src-013 → 014，同轮绑核、相同 case | direct mmap source 的 aggregate 从 3965.9 增至 5391.5 MiB/s；Parent source register p50 从 24.42 降至 1.29 ms。只支持这次 source 路径优化。 |
| read-src-016，双机 CTP、32 MiB Piece、CC16 | aggregate 5766.8 MiB/s；READ CQE p50 约 2.60 ms/片，pwrite p50 约 32.93 ms/片。READ 与 pwrite 存在重叠，不能将阶段时间相加当作批次墙钟。 |
| 196/198 file-backed 探针 | 196 的 32×32 MiB 与 1×1 GiB 注册约 45.46/46.24 ms；198 约有 17.7 ms/次固定等待。两者栈与 backing 条件不同，不支持 task 级 Segment 决策。 |

完整 run、样本、限制和后续勘误见[历史 Provider 台账](../../04-performance/history/real-provider-performance-ledger.md)；此表只作状态摘要。
