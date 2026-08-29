# Dragonfly URMA Adaptation

本目录记录 Dragonfly `dfdaemon` 适配 UMDK/URMA 的实施分析、阶段设计和验证状态。

## 当前源码基线

- Dragonfly 根仓库：`/home/yuan/workspace/dev/dragonfly`，`main`，`39c586d5`。
- Dragonfly client 子仓库：`/home/yuan/workspace/dev/dragonfly/client`，本地候选分支
  `urma-p2p`，`1ccc7d1`。该提交包含 libfabric RDMA Piece 传输，但不属于当前 Dragonfly
  main 固定的正式能力。
- URMA demo：`/home/yuan/workspace/dev/urma-transport-lab`，`6150f21`。
- UMDK：`/home/yuan/workspace/cloud-native/umdk`。

## 文档索引

1. [当前架构决策：URMA 作为 Dragonfly Storage 内部传输后端](./architecture-storage-aligned-revision.md)
2. [历史架构决策：独立 transport crate 方案（已取代）](./architecture-decision.md)
3. [阶段 A：最小 Dragonfly Piece over URMA 闭环实施分析](./phase-a-minimal-piece-over-urma.md)
4. [RDMA/URMA 上传下载路径对比与进度台账](./rdma-urma-upload-download-path-comparison.md)
5. [A0：transport core 机械迁移状态（历史）](./phase-a-a0-transport-core-migration-status.md)
6. [A1：UrmaEngine owner thread 骨架状态（历史）](./phase-a-a1-engine-owner-status.md)
7. [真实 Provider 验证 Runbook](./real-provider-validation-runbook.md)
8. [Phase B：URMA production 性能数据路径](./phase-b-performance-data-path.md)

## 证据口径

本文档统一区分：

- `[源码确认]`：由当前本地 Dragonfly、URMA demo 或 UMDK 源码确认；
- `[实验确认]`：已在真实 UB/UDMA 环境运行验证；
- `[设计决定]`：本适配阶段建议采用、尚未实现；
- `[待验证]`：需要代码、真机或故障注入才能确认。

证据冲突时优先级为：真实实验、当前源码、历史设计文档。
