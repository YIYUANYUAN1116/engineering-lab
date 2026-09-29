# CubeSandbox 学习文档

> Cube Sandbox 是腾讯云开源的 AI Agent 安全沙箱服务，基于 RustVMM + KVM 构建，主打「Instant、Concurrent、Secure、Lightweight」，可在 60ms 内创建具备完整服务能力的硬件隔离沙箱，单实例内存开销 <5MB。

**官方仓库**: https://github.com/TencentCloud/CubeSandbox
**中文文档**: 仓库内 `docs/zh/` 目录

---

## 目录

- [1. 背景：为什么 AI Agent 需要沙箱](#1-背景为什么-ai-agent-需要沙箱)
- [2. 核心特性](#2-核心特性)
- [3. 性能基准](#3-性能基准)
- [4. 总体架构](#4-总体架构)
- [5. 核心组件一览](#5-核心组件一览)
- [6. 关键技术原理](#6-关键技术原理)
- [7. 网络模型与安全体系](#7-网络模型与安全体系)
- [8. 一次沙箱创建的完整旅程](#8-一次沙箱创建的完整旅程)
- [9. 部署与快速开始](#9-部署与快速开始)
- [10. SDK 使用示例（E2B 兼容）](#10-sdk-使用示例e2b-兼容)
- [11. 版本演进](#11-版本演进)
- [12. 学习路径与参考资源](#12-学习路径与参考资源)

---

## 1. 背景：为什么 AI Agent 需要沙箱

大语言模型生成的代码本质上**不可信**。当 AI Agent 需要执行 LLM 生成的代码、操作浏览器、调用外部服务时，必须有一个「安全笼子」来隔离风险。

**核心风险维度：**

| 风险维度 | 典型场景 | 潜在影响 |
| --- | --- | --- |
| 代码执行风险 | LLM 生成恶意操作（`rm -rf /`、反向 shell） | 宿主机被攻陷，波及同租户 |
| 数据外泄 | Agent 向未授权外部 API 发送隐私数据 | 合规违规、业务损失 |
| 凭据滥用 | Agent 窃取/滥用 API Key、Token | 资源盗用、服务中断 |
| 供应链攻击 | `pip install` 恶意包，执行挖矿/后门 | 横向移动、持久入侵 |
| 逃逸攻击 | 利用内核漏洞从容器逃逸到宿主机 | 整个集群被攻陷 |

**传统方案的矛盾：**

- **Docker 容器**：启动快（~200ms），但**共享宿主机内核**，一旦内核有漏洞即可逃逸，隔离不足；
- **传统虚拟机（QEMU）**：隔离强（独立内核），但冷启动需 2–3 秒，内存开销大，密度低。

CubeSandbox 的目标是用 **KVM MicroVM + eBPF + CoW + 资源池** 同时解决「快、密、安全」三者不可兼得的问题。

---

## 2. 核心特性

### 2.1 Instant：60ms 冷启动

端到端冷启动平均 <60ms，比主流方案快 2.5–50 倍。

**秘诀**：预快照模板 + 快照克隆。沙箱不是从零启动操作系统，而是从一个"已经启动好的快照"直接恢复，再叠加资源池化（pre-provisioning）与 Copy-on-Write 克隆，跳过一切冷启动开销。

### 2.2 Concurrent：高密度部署

- VMM 自身基础内存开销仅 **5MB**（传统 QEMU 约 30MB，约为其 1/6）；
- 结合内核共享（kernel sharing）与 XFS reflink 写时复制，**单台服务器可运行数千个沙箱实例**；
- **AutoPause/AutoResume**：空闲沙箱自动休眠，来请求时毫秒级唤醒，进一步降低成本。

### 2.3 Secure：多层硬件级安全隔离（六层纵深防御）

1. **硬件隔离**——每个沙箱是独立 KVM MicroVM，运行专属 Linux 内核，无共享内核逃逸面；
2. **网络隔离**——eBPF 虚拟交换机 CubeVS 在内核态做沙箱间隔离，默认拒绝私网/链路本地地址段，支持逐沙箱放行/拒绝策略；
3. **出口管控**——L7 代理 CubeEgress 拦截所有出向 HTTP/HTTPS 流量，域名必须显式放行；
4. **凭据保险箱（Credential Vault）**——API Key 通过请求头重写注入，密钥从不进入沙箱，模型与沙箱代码都看不到明文；
5. **Seccomp 加固**——Hypervisor 只保留最小系统调用白名单；
6. **API 认证**——CubeAPI 支持可插拔的认证回调。

### 2.4 Lightweight：极致轻量

基于 Rust 重写并极限裁剪的运行时（aggressively trimmed runtime），配合 CoW 内存复用，单实例开销 <5MB。

### 2.5 E2B 无缝迁移

原生兼容 E2B SDK 接口，**只改一个 URL 环境变量**即可从 E2B Cloud 迁移，零业务代码改动。

### 2.6 快照 · 克隆 · 回滚

v0.3.0 引入 CubeCoW 存储引擎（基于 XFS FICLONE/reflink），实现百毫秒级的：
- **事件级快照**：运行中随时打检查点；
- **即时克隆**：从任意状态分叉（fork）探索；
- **回滚**：恢复到任意已保存状态。

---

## 3. 性能基准

| 指标 | Docker 容器 | 传统 VM | CubeSandbox |
| --- | --- | --- | --- |
| 隔离级别 | 低（共享内核 Namespaces） | 高（独立内核） | 极高（独立内核 + eBPF） |
| 启动速度* | ~200ms | 秒级 | 毫秒级（<60ms） |
| 内存开销 | 低（共享内核） | 高（完整 OS） | 低（极限裁剪，<5MB） |
| 部署密度 | 高 | 低 | 极高（单机数千实例） |
| E2B SDK 兼容 | ✗ | ✗ | ✅ 完全兼容（Drop-in） |

> \* 完整 OS 启动时长。测试基于裸金属环境。

**冷启动时延实测（裸金属）**：
- 单并发：平均 **60ms**；
- 50 并发创建：平均 **67ms**，**P95 90ms**，**P99 137ms**——整体稳定在百毫秒级。

**内存开销**：基于 ≤32GB 规格沙箱实测；更大规格下开销略有上升，但幅度极小（蓝色为沙箱规格，橙色为对应基础内存开销，随规格扩大仅少量增长）。

---

## 4. 总体架构

CubeSandbox 采用**分层模块化架构**，从上到下分为三个平面：

```
┌─────────────────────────────────────────────────────────────┐
│ 管理面 (Management Plane)                                    │
│   CubeAPI (REST 网关, Rust)                                  │
│   CubeMaster (集群编排调度, Go)                              │
│   CubeOps (节点指标与运维)                                   │
├─────────────────────────────────────────────────────────────┤
│ 节点面 (Node Plane)                                         │
│   Cubelet (节点本地沙箱生命周期代理)                          │
│   network-agent (TAP 池 / PortMapping / 策略分发)            │
│   CubeProxy (TLS 终结与请求路由)                             │
├─────────────────────────────────────────────────────────────┤
│ 沙箱面 (Sandbox Plane)                                      │
│   CubeShim (containerd shim v2 桥接, Rust)                  │
│   Hypervisor (KVM MicroVM 管理, Rust / Cloud Hypervisor fork)│
│   cube-agent (Guest 内 PID1 初始化守护, Rust)                │
│   envd (用户环境服务)                                        │
└─────────────────────────────────────────────────────────────┘
```

**数据流方向**：客户端 SDK → CubeAPI（REST）→ CubeMaster（gRPC 调度）→ Cubelet（节点本地）→ CubeShim → Hypervisor（KVM）→ Guest 内 cube-agent/envd。

---

## 5. 核心组件一览

| 组件 | 语言 | 职责 |
| --- | --- | --- |
| **CubeAPI** | Rust (axum) | E2B 兼容的 REST API 网关；鉴权、限流、日志；映射到内部 CubeMaster RPC |
| **CubeMaster** | Go | 集群编排调度器；节点元数据、集群状态、模板与实例生命周期；含 TemplateCenter（OCI 镜像 → rootfs 管线）与 cubemastercli 管理 CLI |
| **CubeProxy** | Lua/nginx | 反向代理，按 Host/路径把请求路由到对应沙箱实例，负责 TLS 终结 |
| **Cubelet** | Go | 节点本地调度，管理本节点全部沙箱的完整生命周期；经 cbri 插件化容器运行时接口，支持 containerd（chi 插件），管理持久卷与快照 |
| **CubeShim** | Rust | containerd shim v2 桥接；通过 ttrpc + vsock 与 Guest 内 cube-agent 通信；支持快照/回滚 |
| **Hypervisor** | Rust | Cloud Hypervisor 定制 fork；CPU/内存分配、PCI/ACPI 模拟、virtio 设备（网络/块/控制台）、virtiofsd 文件共享 |
| **cube-agent** | Rust | Guest 内 init 守护（PID 1）；基于 rustjail/OCI 管理容器进程，经 vsock 暴露接口 |
| **CubeVS** | C/eBPF | 内核态 eBPF 虚拟交换机；L4 转发、IP/域名过滤、SNAT、会话跟踪 |
| **CubeEgress** | Lua/OpenResty | L7 出口安全网关；HTTPS 透明拦截、凭据注入、访问审计 |
| **CubeCoW** | 存储引擎 | XFS reflink 写时复制；O(1) 快照与克隆 |

---

## 6. 关键技术原理

### 6.1 快照克隆（Snapshot Cloning）

- 沙箱模板是一个**已经启动完成的系统快照**（内存 + rootfs）；
- 创建新沙箱 = 从快照恢复，而非完整引导 OS，从而跳过内核初始化、服务启动等所有冷启动开销；
- 配合资源池（pre-provisioning）进一步摊薄成本。

### 6.2 Copy-on-Write 存储（CubeCoW）

- 基于 **XFS FICLONE（reflink）** 内核能力，实现 O(1) 快照和克隆；
- 多个沙箱共享同一底层数据块，写入时才复制，实现极低内存/磁盘开销与高密度。

### 6.3 TAP 设备池（TAP Pooling）

- network-agent 预创建 500+ 个 TAP 设备；
- 沙箱创建时**免 TAP 初始化**，减少 59ms+ 的创建时间。

### 6.4 内核共享（Kernel Sharing）

- 多个 MicroVM 共享相同的内核页，进一步降低每实例内存开销。

### 6.5 RustVMM + 极限裁剪

- 基于 Rust 的 Cloud Hypervisor fork + 裁剪的 Guest 内核；
- 只保留最小系统调用白名单（Seccomp），缩小攻击面。

---

## 7. 网络模型与安全体系

网络数据面分**沙箱入口（Ingress）**与**沙箱出口（Egress）**双向：

- **L4 转发与策略执行**：内核态 eBPF 完成（高性能、低 CPU 开销）；
- **L7 路由与深度检查**：用户态 nginx/openresty 完成。

### 7.1 CubeVS（eBPF 虚拟交换机）

- **设计目标**：传统容器网络方案（Linux Bridge、OVS、iptables NAT）每包处理开销随租户数增长，无法支撑千级并发；CubeVS 用 eBPF 把策略执行放进内核，每个 TAP 拥有独立策略 trie（LPM Trie），互不影响；
- **能力**：CIDR 策略 IP 过滤、基于 DNS 的域名策略过滤、SNAT、会话跟踪、stateless 端口映射转换。

### 7.2 CubeProxy（入口网关）

基于 nginx/openresty 的反向代理，通过 HTTP 头进行高性能路由，负责 TLS 终结。

### 7.3 CubeEgress（L7 出口安全网关）

- **按需流量引导**：eBPF 将出向流量调度到 L7 代理；
- **HTTPS 透明拦截**：动态生成证书；
- **L7 策略引擎**：域名白名单放行、越权出站当场拦截；
- **凭据托管与注入**：请求头重写注入，Key 不进沙箱、不进模型上下文、不落日志；
- **访问审计**：全量访问留审计日志，便于合规；
- **可扩展性**：通过 Lua 扩展。

---

## 8. 一次沙箱创建的完整旅程

一次 `Sandbox.create()` 请求的完整链路：

```
SDK
  → CubeAPI（REST 网关，鉴权/限流）
  → CubeMaster（gRPC 调度，选择计算节点）
  → Cubelet（本地调度）
      → CubeCoW 克隆模板 rootfs
      → CubeShim（containerd shim v2）
      → Hypervisor 从内存快照恢复 MicroVM（KVM）
      → CubeVS 挂 TAP 设备并附加网络过滤器
      → Guest 内 cube-agent/envd 就绪
  ← 返回沙箱 ID（毫秒级）
```

全程毫秒级完成，返回沙箱后 SDK 通过 HTTPS 访问 `<port>-<sandboxID>.cube.app` 域（经 CubeProxy）与沙箱内服务通信。

---

## 9. 部署与快速开始

### 环境要求

- **x86_64 Linux 环境，支持 KVM**；
- 支持单机部署与多机集群扩展。

### 部署方式选择

| 方式 | 说明 | 推荐度 |
| --- | --- | --- |
| PVM（云服务器部署） | 普通云服务器上部署，无需裸金属或嵌套虚拟化 | ⭐ 推荐 |
| 裸金属（Bare Metal） | 最佳性能 | 推荐 |
| Dev-Env（QEMU 虚机） | 一次性 OpenCloudOS 9 虚机，无 KVM 权限时体验 | ⚠️ 不推荐（性能差） |

### 一键安装（在线）

```bash
curl -sL https://github.com/TencentCloud/CubeSandbox/raw/master/deploy/one-click/online-install.sh | bash
```

源码构建：`git clone` + `make`。v0.5 起支持 Terraform 一键集群部署与 ARM64 原生支持。

### 四步上手

1. **准备服务器**：PVM 或裸金属，确认 KVM 可用；
2. **安装 Cube Sandbox**：一键脚本或源码构建；
3. **创建沙箱模板**：OCI 镜像一键转模板，或从 Template Store 安装官方预置镜像；
4. **运行第一段 Agent 代码**：用 E2B SDK 创建沙箱并执行代码。

### Web 控制台

安装完成后浏览器访问：

```
http://<控制节点 IP>:12088
```

推荐三步走：**Overview** 确认节点 Ready 且资源有余量 → **Template Store** 安装预置模板（或 Templates 已有 READY 模板可跳过）→ **Sandboxes → + New sandbox** 选 READY 模板创建，进入详情页查看实时日志。

---

## 10. SDK 使用示例（E2B 兼容）

### 环境准备

```bash
export CUBE_TEMPLATE_ID=<模板ID>      # 沙箱镜像模板，必填
export E2B_API_URL=http://<host>:3000 # Cube API 地址（创建沙箱用），必填
export E2B_API_KEY=e2b_000000         # SDK 非空校验用，填任意字符串

pip3 install e2b-code-interpreter
```

### 创建沙箱

```python
import os
from e2b_code_interpreter import Sandbox

template_id = os.environ["CUBE_TEMPLATE_ID"]

with Sandbox.create(template=template_id) as sandbox:
    info = sandbox.get_info()
    print("sandbox info:", info)
```

> `with` 块结束时沙箱自动销毁，避免资源泄漏；也可手动调用 `sandbox.kill()`。

### 执行 Python 代码

```python
with Sandbox.create(template=template_id) as sandbox:
    result = sandbox.run_code('print("hello cube")')
    print(result.stdout)   # 标准输出列表
    print(result.stderr)   # 标准错误列表
    print(result.error)    # 执行异常（None 表示成功）
    print(result.results)  # 富文本输出（图表、HTML 等）
```

流式回调：`sandbox.run_code(python_code, on_stdout=lambda data: print(data))`，适合实时打印 Agent 执行过程输出。

### 执行 Shell 命令

```python
with Sandbox.create(template=template_id) as sandbox:
    result = sandbox.commands.run("echo hello cube")
    print(result.stdout)
```

`commands.run` 面向系统命令（echo、curl、uname 等），`run_code` 面向 Python 解释器并支持富文本结果。

### 典型场景

- **代码解释器（Code Interpreter）**：Agent 生成代码 → 沙箱执行 → 返回图表、PDF、HTML 结果；
- **浏览器自动化 / 网页 Agent**：在隔离环境中安全打开、渲染和操作网页；
- **强化学习（RL）训练**：为 SWE-Bench 等训练任务批量创建廉价、隔离的执行环境；
- **长时有状态服务**：把持久化开发环境、Web 服务甚至数据库直接跑在沙箱里（结合快照/回滚）。

---

## 11. 版本演进

| 版本 | 发布时间 | 核心内容 |
| --- | --- | --- |
| v0.1.0 | 2026-04-20 | 初始开源发布；生产就绪（已在腾讯云生产环境验证，单机可分钟级拉起上万沙箱） |
| v0.3.0 | — | 引入 **CubeCoW 快照引擎**：事件级快照、即时克隆、回滚；模板系统（OCI 镜像转模板、模板商店、跨节点分发） |
| v0.4.0 | — | 更安全的出口 + 更易运维：**Credential Vault**（Key 不进沙箱）、Dashboard（版本矩阵与模板健康检查） |
| v0.5.0 | — | **AutoPause/AutoResume**、Terraform 一键集群部署、**ARM64 原生全栈支持**、网络策略强化（逐沙箱流量 token、策略路由出口） |
| v0.6.0 | 2026-07-24 | 见官方 changelog |
| v0.7.0 | 2026-08-28 | 见官方 changelog |

---

## 12. 学习路径与参考资源

### 建议学习路径

1. **入门**：通读本文档 + 官方 README/README_zh → 体验一键安装 + Web 控制台（:12088）创建第一个沙箱；
2. **动手**：跑通 `examples/` 下的 E2B SDK 示例（create / exec_code / cmd / 文件读写 / 暂停恢复 / 网络策略）；
3. **架构**：精读 `docs/architecture/overview.md`（架构总览）与 `docs/architecture/network.md`（网络模型）；
4. **安全**：深入 CubeVS（eBPF）、CubeEgress（L7 代理与凭据注入）的设计与实现；
5. **存储**：研究 CubeCoW 快照引擎（XFS reflink）与模板体系；
6. **调优**：阅读核心操作性能基准测试报告（裸金属）与 PVM 云服务器测试报告。

### 官方资源链接

- GitHub 仓库：https://github.com/TencentCloud/CubeSandbox
- 快速开始：`docs/guide/quickstart.md`
- 部署指南：`docs/guide/bare-metal-deploy.md` / `docs/guide/pvm-deploy.md`
- 模板指南：`docs/guide/templates.md`
- 快照/克隆/回滚：`docs/guide/snapshot-rollback-clone.md`
- WebUI 指南、安全代理指南、AgentHub（OpenClaw 数字助手）文档均在 `docs/` 下

### 常见术语速查

| 术语 | 含义 |
| --- | --- |
| E2B | Execute Base，AI Agent 代码执行沙箱服务，CubeSandbox 兼容其 SDK |
| MicroVM | 轻量虚拟机，独立内核但裁剪精简，介于容器与传统 VM 之间 |
| RustVMM | 基于 Rust 的虚拟化运行时（此处指 Cloud Hypervisor fork） |
| CoW | Copy-on-Write 写时复制，快照/克隆/内存复用的底层机制 |
| reflink | XFS 文件系统 FICLONE，实现块级共享与 O(1) 克隆 |
| eBPF | 内核态可编程数据路径，CubeVS 网络策略的执行基础 |
| shim v2 | containerd 的容器运行时桥接接口 |
| vsock | 宿主机与 Guest 之间的高速虚拟套接字通道 |
