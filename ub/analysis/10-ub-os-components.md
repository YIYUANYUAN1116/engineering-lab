# UB OS Component 组件

## 1. 文档目标

按组件列出层次、运行态、职责、依赖和接口；PDF 没有说明的内容保持未知。

## 2. 主要来源

OS 参考设计 §2-7；高阶服务参考设计 §7.2.2。[参考设计确认]

## 3. 核心概念

| 组件 | 所属层级 | 内核态/用户态 | 主要职责 | 上游依赖 | 下游依赖 | 对外接口 | 来源 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| ubfi | Device Mgmt / UBus Driver | 内核态 | 解析 BIOS UBRT、注册平台设备、生成 Bus Controller/UMMU 等节点 | OS 设备模型 | BIOS/UBRT | 平台设备注册 | OS §3.2，PDF 13 |
| ubus | Device Mgmt / UBus Driver | 内核态 | UB 枚举、设备创建/管理、错误、热插拔、中断基础服务 | UB 设备驱动 | Bus Controller | 总线/驱动接口 | OS §3.2，PDF 13-16 |
| vendor_ubus | Device Mgmt / 厂商适配 | 内核态 | 厂商差异化硬件交互 | ubus | 厂商硬件 | [文档未说明] | OS §3.2，PDF 13 |
| ubcore | Communication / URMA core | 内核态 | 建链，Jetty/Segment 资源申请与状态管理 | uburma/liburma | UDMA driver | 南北向核心能力 | OS §5.3.2，PDF 30 |
| uburma | Communication | 内核通道/框架（PDF措辞） | 用户态库与内核核心层的消息通信框架 | liburma | ubcore | [文档未说明] | OS §5.3.2，PDF 30 |
| liburma | Communication / URMA | 用户态 | Jetty、Segment、数据面北向 API | 应用 | uburma/ubcore | `urma_*` API | OS §5.3.2-5.3.3，PDF 30-34 |
| UMS | Communication / Socket | 内核态为主 | 对接 Socket 抽象，使用 URMA Jetty 或 LD/ST 加速 | Socket 应用/兼容层 | uburma/ubcore | AF_UB 或 `ums_run` | OS §5.5，PDF 38-39 |
| liburpc | Communication / URPC | 用户态 | 统一远程过程调用 API | 应用 | liburma/ubcore | `urpc_*` | OS §5.4，PDF 34-38 |
| libummu | Memory Mgmt | 用户态 | TokenID 申请/释放并维护权限表 | 用户/管理软件 | ummu-core/driver | `ummu_*` | OS §4.2、§4.3.4，PDF 18、24 |
| ubutils | Device Mgmt 工具 | 用户态 | 拓扑、配置空间查询和配置 | 管理员 | UBus Driver | 命令行工具 | OS §3.2，PDF 13 |
| uvs | UBS Virt / Virt OVS | [文档未说明] | 虚拟网络 TP 层协商建链 | Virt OVS | UB 网络 | 文档仅提 CLI 为相邻模块 | Service Core §7.2.2，PDF 23 |
| ubagg | [文档未说明] | [文档未说明] | [文档未说明] [待源码确认] | [文档未说明] | [文档未说明] | [文档未说明] | 四份 PDF 未检出 |
| libumq | [文档未说明] | [文档未说明] | [文档未说明] [待源码确认] | [文档未说明] | [文档未说明] | [文档未说明] | 四份 PDF 未检出 |
| libcdma | [文档未说明] | [文档未说明] | [文档未说明] [待源码确认] | [文档未说明] | [文档未说明] | [文档未说明] | 四份 PDF 未检出 |
| ubctl | [文档未说明] | [文档未说明] | [文档未说明] [待源码确认] | [文档未说明] | [文档未说明] | [文档未说明] | 四份 PDF 未检出 |

## 4. 架构或机制

OS 参考设计把组件归入四块：Device Mgmt、Memory Mgmt、Communication、Virtualization，RAS 横跨硬件/固件、既有 OS、UB 新增组件与厂商组件。[参考设计确认] 来源：OS §2、§7.2，PDF 第 10-11、49-50 页。

## 5. 关键对象与关系

- URMA 路径：应用 → liburma → uburma/ubcore → UDMA driver；`urma_admin` 是管理工具而非数据面库。[参考设计确认]
- URPC 路径：应用 → liburpc/kurpc → ubcore/liburma → UDMA。[参考设计确认]
- UMS 路径：Socket 兼容 → UMS → 内核 URMA Jetty 或 LD/ST。[参考设计确认]

## 6. 文档明确结论

OS 文档使用“参考设计”描述模块和接口，不能仅据该文档断言当前仓库已实现或模块名与源码目录完全一致。[参考设计确认]

## 7. 文档未说明内容

任务列出的 4 个组件未出现；`uvs` 只在 Service Core Virt 文档出现，不能据此把它归为基础 OS 通信栈。[文档未说明] [文档间存在差异]

## 8. 待确认问题

所有模块的 Kconfig、包名、进程名、稳定 ABI、许可证与版本兼容矩阵均待源码确认。[待源码确认]
