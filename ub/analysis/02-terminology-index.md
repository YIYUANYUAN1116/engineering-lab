# UB 术语索引

## 1. 文档目标

统一记录 PDF 明确出现的术语，并保留文档缺失与命名差异。

## 2. 主要来源

基础规范 §1.5、§8-11；OS 参考设计 §2-7 与附录 A；高阶服务参考设计 §1.4、§2-7；白皮书 §2、§4。[规范确认] [参考设计确认] [白皮书确认]

## 3. 核心概念

| 术语 | 中文名 | 英文名 | 所属层级 | 文档定义 / 定位 | 来源 | 需进一步确认 |
| --- | --- | --- | --- | --- | --- | --- |
| UB | 灵衢 | UnifiedBus | 基础协议 | 面向超节点的互联协议 | 基础规范 §1.5，PDF 12 | 否 |
| UnifiedBus | 灵衢 | UnifiedBus | 基础协议 | UB 全称；不是 Ubuntu 或业务模块 | 同上 | 否 |
| 超节点 | 超节点 | SuperPoD | 系统架构 | 多类组件池化、平等协同，逻辑上构成一台计算机 | 白皮书 §2，PDF 5 | 否 |
| UB 协议栈 | 灵衢协议栈 | UB protocol stack | 协议 | 物理、链路、网络、传输、事务、功能六层，另含 UMMU/UBFM | 基础规范 §2.2，PDF 15-16 | 否 |
| Entity | 实体 | Entity | 资源/通信对象 | 设备分配自身资源的基本单元、UB 域内通信对象 | 基础规范 §1.5，PDF 9 | 否 |
| EID | Entity 身份标识 | Entity Identifier | 资源/寻址 | 唯一标识 Entity 通信对象身份；规范为 128-bit 唯一 ID 空间，可在线路中使用简短格式 | 基础规范 §1.5、§10.3.1，PDF 9、295-296 | 否 |
| Jetty | 通信码头 | Jetty | 功能层/URMA | URMA 基本通信单元，承载异步事务下发与执行 | 基础规范 §1.5、§8.2.2，PDF 10、245-250 | 否 |
| Segment | 内存段 | Memory Segment | 功能层/内存 | 连续地址内存，内存事务基本对象；规范由 UBMD 标识 | 基础规范 §1.5、§8.2.1，PDF 10、244-245 | 否 |
| UBMD | UB 内存描述符 | UB Memory Descriptor | 内存管理 | EID、TokenID、UBA 的组合 | 基础规范 §9.3，PDF 264 | 否 |
| UBA | UB 地址 | UB Address | 内存管理 | Home 提供给 User 的内存访问地址 | 基础规范 §1.5，PDF 11 | 否 |
| URMA | 统一远程内存访问 | Unified Remote Memory Access | 功能层/API | 异步访问编程模型/高速通信库，支持异步内存访问和双边消息 | 基础规范 §1.5、§8.1，PDF 12、244 | API 细节待源码确认 |
| UMS | 灵衢内存语义 socket | UB Memory based Socket | OS 通信 | 对接 Socket 抽象层，内核态使用 URMA Jetty 或 LD/ST 语义加速 | OS 参考设计 §5.5、附录 A，PDF 38-39、57 | 待源码确认 |
| UMMU | UB 内存管理单元 | UB Memory Management Unit | 内存管理/硬件 | 在 Home 侧执行地址映射和访问权限校验 | 基础规范 §1.5、§9.4，PDF 12、264-265 | 实现待确认 |
| UVS | 统一虚拟交换 | Unified Virtual Switch | UBS Virt | 支持虚拟网络 TP 层协商建链 | 高阶服务参考设计 §7.2.2，PDF 23 | 实现待源码确认 |
| UBoE | 以太网承载 UB | UB over Ethernet | 承载/跨域 | 通过以太/IP 网络承载 UB 事务，实现跨 UB Domain 互通 | 基础规范 §1.5、§2.1，PDF 12、14 | 具体部署待确认 |
| UBFM | 灵衢互连结构管理器 | UB Fabric Manager | 管理面 | UB Domain 管理者，管理互连、通信和计算资源 | 基础规范 §2.2、§10.1，PDF 16、292 | 产品实现待确认 |
| ubcore | 文档未给中文名 | ubcore | OS/URMA 内核核心 | 建链及 Jetty、Segment 等资源申请和状态管理 | OS 参考设计 §5.3.2，PDF 30 | 待源码确认 |
| uburma | 文档未给中文名 | uburma | OS/URMA 通道 | 用户态库与内核态核心层的消息通信框架 | OS 参考设计 §5.3.2，PDF 30 | 具体装载形态待源码确认 |
| liburma | URMA 用户态库 | liburma | 用户态库 | 提供 Jetty、Segment 与数据面 API | OS 参考设计 §5.3.2，PDF 30 | ABI 待源码确认 |
| UB Service Core | 灵衢系统高阶服务 | UB Service Core / UBS Core | 高阶服务 | OS 之上的五类集群级服务 | 高阶服务参考设计 §1.4、§2.2，PDF 6、8-9 | 交付状态待确认 |
| UBS Engine | 灵衢系统高阶集群引擎服务 | UB Service Core Engine | 高阶服务/控制面 | 池化资源管理、调度、北向 API 与高可用参考实现 | 高阶服务参考设计 §3，PDF 10-12 | 待源码确认 |
| UBS Memory | 能力分类名 | 文档写作 UBS Mem | 高阶内存服务 | 任务分类名；PDF 正式组件名为 UBS Mem | 高阶服务参考设计 §1.4、§4，PDF 6、13-15 | [文档间存在差异] |
| UBS Communication | 能力分类名 | 文档写作 UBS Comm | 高阶通信服务 | 任务分类名；PDF 正式组件名为 UBS Comm | 高阶服务参考设计 §1.4、§5，PDF 6、16-18 | [文档间存在差异] |
| UBS IO | 灵衢系统高阶 IO 服务 | UB Service Core IO | 高阶 IO | 全局数据读写缓存和 HBM Direct 参考能力 | 高阶服务参考设计 §6，PDF 19-21 | 待源码/环境确认 |
| UBS Virt | 灵衢系统高阶虚拟化服务 | UB Service Core Virt | 高阶虚拟化 | Virt OVS、Virt Optimizer、Virt VM | 高阶服务参考设计 §7，PDF 22-25 | 待源码确认 |

## 4. 架构或机制

`Entity --EID--> 通信身份`；`Segment --UBMD{EID,TokenID,UBA}--> Home 内存`；`Jetty + SQ/RQ/CQ/EQ --> URMA 异步事务`；`UMMU --> Home 侧翻译与鉴权`。[规范确认]

## 5. 关键对象与关系

- EID 与网络地址不同：EID 标识 Entity 身份，网络地址绑定拓扑位置；Entity 迁移后需要重新绑定网络地址。[规范确认] 来源：基础规范 §10.3.1，PDF 第 295-296 页。
- Token 可指内存 TokenID/TokenValue 或 Jetty 的上下文索引/TokenValue，不能只写成一个无类型“令牌”。[规范确认] 来源：基础规范 §1.5、§11.4，PDF 第 10、351-353 页。

## 6. 文档明确结论

任务列出的 `ubagg`、`libumq`、`libcdma`、`ubctl` 未在四份 PDF 文本中出现；不得按名称补定义。[文档未说明] [待源码确认]

## 7. 文档未说明内容

“UB RPC”“UB 消息队列”不是四份 PDF 中稳定出现的正式组件名；文档明确的是 URPC、消息事务以及部分库名。[文档未说明]

## 8. 待确认问题

UVS 全称、`uburma` 的精确用户/内核边界、未出现组件的职责均需以源码和头文件确认。[待源码确认]
