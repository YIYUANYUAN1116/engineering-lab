# PMEM、NVDIMM、virtio-pmem 与 DAX 学习笔记

> 本文用于系统学习 PMEM、NVDIMM、virtio-pmem、DAX 及其在 Linux、QEMU、Cloud Hypervisor 和 Kata Containers 中的实现关系。
>
> 默认源码基线：
>
> - Kata Containers：`kata-containers/kata-containers`，`main`
> - Linux Kernel：`torvalds/linux`，`main`
> - QEMU：上游实现
> - Cloud Hypervisor：上游实现
>
> 本文重点不是记忆名词，而是建立从 CPU 地址访问、Linux I/O、持久内存，到 VMM 和 Kata 的完整调用链。

---

# 1. 总体知识地图

PMEM、NVDIMM、virtio-pmem、DAX 很容易混淆，是因为它们分别位于不同层级。

```text
硬件 / Host backing
→ CPU 地址空间
→ Linux 内存管理
→ Linux 块设备 / 文件系统
→ PMEM / DAX
→ 虚拟化设备模型
→ Guest kernel
→ Kata runtime / shim
→ Kata Agent
→ Container mount namespace
```

先给四个概念定层：

```text
PMEM
  = 一种持久、CPU 可寻址的存储资源属性

NVDIMM
  = 一种向操作系统呈现 PMEM 的设备模型

virtio-pmem
  = 另一种通过 virtio 向 Guest 呈现 PMEM 的设备模型

DAX
  = 文件系统 / 应用直接访问 PMEM 的访问模式
```

所以：

```text
PMEM 是资源
NVDIMM / virtio-pmem 是设备呈现方式
DAX 是访问方式
```

在虚拟化环境中，两条典型链路为：

```text
QEMU NVDIMM：
Host image
→ memory-backend-file
→ QEMU NVDIMM
→ ACPI NFIT
→ Guest acpi_nfit
→ libnvdimm
→ nd_pmem
→ /dev/pmem0

virtio-pmem：
Host image
→ VMM mmap
→ virtio-pmem
→ Guest virtio_pmem
→ libnvdimm
→ nd_pmem
→ /dev/pmem0
```

---

# 2. 阶段一：CPU、内存与地址空间

这一阶段解决一个最基础的问题：

> 程序访问一个地址时，CPU 最终访问到哪里？

## 2.1 虚拟地址与物理地址

普通 Linux 进程使用的是虚拟地址：

```c
int *p = malloc(sizeof(int));
*p = 100;
```

`p` 可能是：

```text
0x7f3a12345000
```

这是进程虚拟地址，不是物理内存条上的真实地址。

地址转换路径：

```text
Process Virtual Address
→ CPU MMU
→ Page Table
→ Physical Address
→ CPU Cache / Memory
```

不同进程可以拥有相同虚拟地址：

```text
Process A VA 0x400000 → PFN 100
Process B VA 0x400000 → PFN 900
```

原因是每个进程有自己的页表。

## 2.2 Page 与 PFN

Linux 通常按 Page 管理内存，常见页面大小为 4 KiB：

```bash
getconf PAGE_SIZE
```

PFN：Page Frame Number，物理页框号。

对于 4 KiB 页面：

```text
Physical Address = PFN × 4096 + offset
```

这和后面的 DAX 直接相关，因为 DAX 的核心动作之一就是：

```text
把 PMEM 对应的 PFN 映射进进程页表
```

## 2.3 Page Table 与 PTE

页表维护：

```text
Virtual Page Number
→ Physical Frame Number
```

PTE 除了 PFN，还包含：

- Present
- Read / Write
- User / Supervisor
- Executable
- Dirty
- Accessed

所以只读保护最终也会体现在页表权限上。

## 2.4 MMU 与 TLB

MMU 位于 CPU 中，负责运行时地址转换。

CPU 访问地址时大致执行：

```text
Virtual Address
→ TLB lookup
→ hit：得到 Physical Address
→ miss：Page Table Walk
```

TLB 缓存的是地址转换结果，而不是文件数据。

## 2.5 Page Fault

`malloc()` 分配 1 GiB，并不一定立即分配 1 GiB 物理内存。

```text
malloc
→ 建立虚拟地址范围
→ 第一次访问某页
→ Page Fault
→ Linux 分配物理页
→ 建立 PTE
```

因此需要区分：

```text
VmSize = 虚拟地址范围
VmRSS  = 当前真正驻留的物理页
```

验证：

```bash
grep -E 'VmSize|VmRSS' /proc/<PID>/status
```

## 2.6 普通文件 mmap

普通文件：

```c
mmap(fd, ...)
```

通常不是直接映射磁盘，而是：

```text
Process VA
→ Page Table
→ Page Cache Page
```

第一次访问时可能：

```text
Page Fault
→ Page Cache miss
→ Filesystem
→ Block I/O
→ Device
→ Page Cache
→ 建立 PTE
```

所以：

```text
mmap ≠ DAX
mmap ≠ 自动绕过 Page Cache
```

## 2.7 CPU Cache 与 Page Cache

必须严格区分：

| 项目 | CPU Cache | Page Cache |
| --- | --- | --- |
| 管理者 | CPU 硬件 | Linux 内核 |
| 粒度 | Cache Line，常见 64B | Page，常见 4 KiB |
| 缓存对象 | 内存地址内容 | 文件数据 |
| DAX 是否绕过 | 否 | 是 |

因此：

```text
PMEM + DAX
→ 绕过 Linux Page Cache
→ 不绕过 CPU Cache
```

## 2.8 虚拟机的两级地址转换

虚拟机中多一层：

```text
Guest Virtual Address (GVA)
→ Guest Page Table
→ Guest Physical Address (GPA)
→ KVM Stage-2
→ Host Physical Address (HPA)
```

还经常会看到：

```text
HVA = Host Virtual Address
```

例如 QEMU mmap Guest memory 后：

```text
QEMU HVA
→ Host Page Table
→ HPA
```

所以 PMEM 所谓“直接映射”也仍然存在页表和 Stage-2 转换。

---

# 3. 阶段二：Linux 传统文件 I/O 与 Page Cache

完整传统读取路径：

```text
Application
→ read()
→ VFS
→ ext4/xfs
→ Page Cache
→ block layer
→ device driver
→ device
```

## 3.1 fd、struct file、dentry、inode

```text
fd
→ struct file
→ dentry
→ inode
```

理解：

```text
inode  = 文件对象和元数据

dentry = 文件名 / 路径组件与 inode 的关系

file   = 一次 open() 产生的打开实例
```

所以两个 fd 可以指向同一个 inode，但拥有不同文件偏移。

## 3.2 Page Cache

Page Cache 使用 DRAM 缓存文件数据。

```text
inode
└── address_space
    ├── index 0 → page A
    ├── index 1 → page B
    └── index 2 → page C
```

普通 `read()`：

缓存命中：

```text
Page Cache
→ copy_to_user
→ user buffer
```

缓存未命中：

```text
Device
→ Page Cache
→ copy_to_user
→ user buffer
```

## 3.3 普通 write

```text
User Buffer
→ write()
→ Page Cache
→ Dirty Page
→ writeback
→ Filesystem
→ Block Layer
→ Device
```

所以：

```text
write() 返回
≠ 已经写到磁盘
≠ 已经掉电安全
```

查看脏页：

```bash
grep -E 'Dirty|Writeback' /proc/meminfo
```

## 3.4 fsync

`fsync(fd)` 的意义比“刷 Page Cache”更广。

它可能涉及：

- 文件数据
- inode / extent 等必要元数据
- journal commit
- block device flush
- 虚拟设备 flush
- Host backing flush

因此：

```text
write
→ 可见

fsync
→ 请求满足文件系统定义的持久化语义
```

## 3.5 Block Layer 与 bio

Linux block layer：

```text
Filesystem
→ bio
→ request / blk-mq
→ driver
→ device
```

bio 描述：

- READ / WRITE
- block device
- sector
- memory pages
- length
- flags

传统存储主要是“提交 I/O 请求”。

## 3.6 virtio-blk

Guest：

```text
Guest Filesystem
→ Guest Page Cache
→ Guest block layer
→ virtio_blk
→ virtqueue
→ QEMU / VMM
→ Host storage
```

所以 virtio-blk 的数据真正通过 virtqueue 传输。

## 3.7 普通 mmap

普通 mmap：

```text
Application VA
→ Page Fault
→ Page Cache
→ 必要时 Block I/O
→ PTE 映射 Page Cache Page
```

与 `read()` 的主要区别：

```text
read：
Page Cache → copy_to_user → User Buffer

mmap：
Page Cache → PTE → User VA
```

mmap 省掉了一次显式复制，但仍然使用 Page Cache。

## 3.8 O_DIRECT

O_DIRECT：

```text
User Buffer
→ Filesystem Direct I/O
→ Block Layer
→ Device
```

绕过 Page Cache，但仍然是块 I/O。

因此：

```text
O_DIRECT = Direct I/O
DAX      = Direct Address Access
```

二者不是同一个概念。

---

# 4. 阶段三：文件系统、mmap、O_DIRECT 与 DAX

## 4.1 文件系统做什么

块设备本身只有类似：

```text
read sector N
write sector N
```

并不知道：

```text
/data/a.txt
```

ext4/xfs 在其上建立：

```text
superblock
inode
directory entry
extent
journal
free-space bitmap
```

## 4.2 extent

extent 将文件逻辑块映射到设备物理块：

```text
logical block 0-99
→ physical block 9000-9099
```

这意味着即使使用 DAX，文件系统仍需：

```text
file offset
→ extent lookup
→ PMEM address / PFN
```

所以：

```text
DAX ≠ 绕过文件系统
```

## 4.3 普通 mmap 与 DAX mmap

普通 mmap：

```text
Process VA
→ PTE
→ DRAM Page Cache Page
→ Filesystem
→ Block Layer
→ Device
```

DAX mmap：

```text
Process VA
→ PTE
→ PMEM PFN
```

DAX 主要绕过：

- 文件 Page Cache
- 传统文件数据 block I/O
- Page Cache writeback

DAX 没有绕过：

- VFS
- inode
- dentry
- extent
- 文件权限
- CPU Cache
- 页表 / TLB
- page fault

## 4.4 DAX Page Fault

第一次访问 DAX mmap 区域：

```text
CPU Page Fault
→ VMA
→ Filesystem DAX fault handler
→ File offset
→ Extent lookup
→ PMEM PFN
→ Install PTE
→ Return to userspace
```

后续：

```text
CPU load/store
→ TLB / PTE
→ PMEM
```

## 4.5 fsdax 与 devdax

Filesystem DAX：

```text
/dev/pmem0
→ ext4/xfs
→ file
→ mmap
→ PMEM
```

Device DAX：

```text
/dev/dax0.0
→ mmap
→ raw PMEM address range
```

对比：

| 项目 | fsdax | devdax |
| --- | --- | --- |
| 接口 | 普通文件 | DAX 字符设备 |
| 文件系统 | 有 | 无 |
| inode / directory | 有 | 无 |
| Page Cache | 绕过 | 不使用 |
| 空间管理 | 文件系统 | 应用 |

Kata Guest image 更接近 `fsdax` 路径，因为 Guest rootfs 需要 ext4 文件语义。

## 4.6 virtio-fs DAX 不等于 PMEM DAX

PMEM fsdax：

```text
Guest PMEM block device
→ Guest ext4
→ DAX mapping
```

virtio-fs DAX：

```text
Host file
→ virtio-fs DAX window
→ Guest mapping
```

两者虽然都叫 DAX，但文件系统位置和底层设备模型完全不同。

---

# 5. 阶段四：PMEM、libnvdimm、region、namespace

Linux 官方文档对 PMEM 的核心定义是：

```text
PMEM = 一段 system physical address range，
其中 writes are persistent。
```

同时由 PMEM 组成的 block device 具备 DAX 能力。

Linux 源码 / 文档：

```text
Documentation/driver-api/nvdimm/nvdimm.rst
```

## 5.1 Linux PMEM 对象链

```text
provider
→ nvdimm bus
→ region
→ namespace
→ nd_pmem
→ /dev/pmem0
```

## 5.2 Region

Region 是一段可管理的 PMEM System Physical Address 范围。

一个 region 可能来自多个物理 NVDIMM 的 interleave。

所以：

```text
nmem / NVDIMM
→ 更偏设备对象

region
→ 更偏系统物理地址范围
```

## 5.3 Namespace

Namespace 是从 region 中划分出来的一段逻辑使用空间。

```text
region0 = 128 GiB
├── namespace0.0 = 64 GiB
├── namespace0.1 = 32 GiB
└── free = 32 GiB
```

Namespace 不是磁盘分区。

完整层级：

```text
PMEM region
→ namespace
→ /dev/pmem0
→ GPT / MBR
→ /dev/pmem0p1
→ ext4
```

所以：

```text
namespace
≠ partition
```

## 5.4 nd_pmem

Linux PMEM block driver：

```text
drivers/nvdimm/pmem.c
```

它同时接入：

```text
block device
DAX
libnvdimm
```

不使用 DAX 时：

```text
Application
→ Page Cache
→ bio
→ nd_pmem
→ memcpy
→ PMEM address
```

Linux `pmem.c` 中普通 PMEM block I/O 最终会在内存地址和 bio page 之间复制数据，而不是像 NVMe 一样向设备控制器提交真实 DMA 命令。

因此：

```text
PMEM
≠ 必然 DAX
```

可以有：

```text
PMEM + Buffered I/O
PMEM + fsdax
PMEM + devdax
```

## 5.5 `/dev/pmem0` 为什么是块设备

PMEM 本质是可直接寻址地址范围，但 Linux 为了兼容 ext4/xfs/mkfs/fsck 等传统生态，会通过 `nd_pmem` 同时提供 block device 接口。

所以 `/dev/pmem0` 是一种特殊 block device：

```text
既能接受 block bio
又能提供 DAX 可映射 PFN
```

## 5.6 PMEM Namespace Mode

常见概念：

```text
raw
sector
fsdax
devdax
```

其中：

```text
fsdax
→ /dev/pmem0
→ ext4/xfs
→ mmap DAX

devdax
→ /dev/dax0.0
→ application mmap
```

---

# 6. 阶段五：QEMU NVDIMM 与 virtio-pmem

两者都是让 Guest 获得 PMEM，但发现路径不同。

## 6.1 QEMU NVDIMM

```text
Host image
→ QEMU memory-backend-file
→ QEMU virtual NVDIMM
→ Guest Physical Address Range
→ ACPI NFIT
→ Guest acpi_nfit
→ libnvdimm
→ nd_pmem
→ /dev/pmem0
```

`memory-backend-file` 的作用是：

```text
Host file
→ mmap
→ QEMU HostMemoryBackend / MemoryRegion
```

NVDIMM device 再把它作为虚拟内存设备放入 Guest GPA 空间。

Guest 普通 PMEM 数据访问不是 virtqueue 请求：

```text
Guest Process VA
→ Guest Page Table
→ PMEM GPA
→ KVM Stage-2
→ Host Memory Backend
```

## 6.2 ACPI NFIT

Guest 需要知道某段 GPA 是 PMEM，而不是普通 DRAM。

QEMU NVDIMM 主要通过：

```text
ACPI NFIT
```

向 Guest 描述持久内存资源。

因此 QEMU NVDIMM 对 firmware / ACPI 支持更敏感。

## 6.3 virtio-pmem

Linux 驱动：

```text
drivers/nvdimm/virtio_pmem.c
```

源码注释明确说明：

```text
Discovers persistent memory range information from host
and registers the virtual pmem device with libnvdimm core.
```

Guest 驱动匹配：

```text
VIRTIO_ID_PMEM
```

然后读取：

```text
start
size
```

或者 virtio shared memory region。

之后：

```text
virtio_pmem
→ nvdimm_bus_register()
→ nvdimm_pmem_region_create()
→ libnvdimm
→ nd_pmem
→ /dev/pmem0
```

## 6.4 为什么两者最后都是 `/dev/pmem0`

因为 `/dev/pmem0` 不是 VMM 直接创建。

```text
QEMU NVDIMM
           ┐
           ├→ libnvdimm → nd_pmem → /dev/pmem0
virtio-pmem
           ┘
```

所以仅看 `/dev/pmem0` 无法判断来源。

需要查看：

```bash
lsmod | grep -E 'nfit|virtio_pmem|nd_pmem'
dmesg | grep -Ei 'nfit|virtio.*pmem|pmem'
readlink -f /sys/class/block/pmem0/device
```

## 6.5 virtio-pmem 普通数据为什么不走 virtqueue

virtio-blk：

```text
bio
→ virtio_blk
→ virtqueue
→ VMM
```

virtio-pmem DAX 数据：

```text
CPU load/store
→ PMEM GPA mapping
```

virtio-pmem 只创建一个主要用于持久化协调的：

```text
flush_queue
```

也就是说：

```text
普通数据访问：地址映射
Flush：virtqueue
```

这是 virtio-pmem 与 virtio-blk 最本质的区别之一。

---

# 7. 阶段六：Kata Containers 中的 PMEM 调用链

## 7.1 必须先区分三种 rootfs / volume

### Guest rootfs

```text
Host kata-containers.img
→ Hypervisor device
→ Guest device
→ Guest /
```

### Container rootfs

常见：

```text
Host containerd snapshot
→ shared directory
→ virtio-fs
→ Guest shared mount
→ Kata Agent
→ Container /
```

### Kubernetes Volume

例如：

```text
PVC / hostPath / emptyDir / block volume
→ 独立挂载链
```

所以：

```text
Guest image 使用 PMEM
≠ Container rootfs 使用 PMEM
```

## 7.2 Kata QEMU 架构入口

源码：

```text
src/runtime/virtcontainers/qemu_arch_base.go
```

`qemuArch` 定义：

```text
appendImage()
appendBlockImage()
appendNvdimmImage()
```

所以 Kata 对 Guest image 的主要选择是：

```text
image
├── NVDIMM
└── Block Device
```

当前 Kata QEMU image 路径没有把 QEMU virtio-pmem 作为第三个 image 分支。

## 7.3 ARM64 关键分支

源码：

```text
src/runtime/virtcontainers/qemu_arm64.go
```

核心逻辑：

```go
func (q *qemuArm64) appendImage(ctx context.Context, devices []govmmQemu.Device, path string) ([]govmmQemu.Device, error) {
    if !q.disableNvdimm {
        return q.appendNvdimmImage(devices, path)
    }
    return q.appendBlockImage(ctx, devices, path)
}
```

即：

```text
disableNvdimm = false
→ appendNvdimmImage()

disableNvdimm = true
→ appendBlockImage()
```

因此：

```text
disable_image_nvdimm
```

不仅仅是“是否启用 DAX”，而是在改变 Guest image 的虚拟设备类型。

## 7.4 QEMU NVDIMM image 路径

概念调用链：

```text
configuration.toml
→ HypervisorConfig
→ qemuArchBase.disableNvdimm
→ appendImage()
→ appendNvdimmImage()
→ memory-backend-file + NVDIMM
→ QEMU
→ ACPI NFIT
→ Guest acpi_nfit
→ libnvdimm
→ nd_pmem
→ /dev/pmem0
→ /dev/pmem0p1
→ Guest rootfs
```

## 7.5 QEMU block fallback

```text
disable_image_nvdimm=true
→ appendBlockImage()
→ virtio-blk / virtio-scsi
→ Guest /dev/vda or /dev/sda
→ Guest rootfs
```

## 7.6 `block_device_driver` 与 `disable_image_nvdimm`

二者作用域不同。

```text
disable_image_nvdimm
→ 主要决定 Guest image 是否使用 NVDIMM

block_device_driver
→ 主要决定普通 block-backed device 使用 virtio-blk / virtio-scsi 等
```

例如：

```toml
disable_image_nvdimm = false
block_device_driver = "virtio-blk"
```

可以出现：

```text
Guest image
→ NVDIMM
→ /dev/pmem0

额外 block volume
→ virtio-blk
→ /dev/vdX
```

## 7.7 Cloud Hypervisor 历史路径

历史版本 Kata + CLH：

```text
Guest image
→ CLH PmemConfig
→ Cloud Hypervisor virtio-pmem
→ Guest virtio_pmem
→ libnvdimm
→ /dev/pmem0
```

需要注意：Kata 的配置字段历史上仍叫：

```text
disable_image_nvdimm
```

但在 CLH backend 中历史上实际控制的是 virtio-pmem。

这是历史抽象命名，不代表 CLH 使用 QEMU 式 NVDIMM。

## 7.8 当前 Kata main + CLH

当前 Kata main 已禁用 CLH image PMEM 路径，Guest image 使用只读 block device。

因此不能拿当前 main 的 CLH 行为直接代表 Kata 3.26 及更早版本。

## 7.9 Kata Agent 不负责 Guest rootfs 初始挂载

时间顺序：

```text
VMM 启动
→ Guest kernel
→ Guest rootfs 挂载
→ Kata Agent 启动
→ Container rootfs / Volume 挂载
→ Container process
```

Guest rootfs 是 kernel 启动阶段完成，不是 Kata Agent 再把 `/dev/pmem0p1` 挂成 `/`。

## 7.10 容器为什么还能看到 PMEM 设备

Guest 内核已经注册：

```text
major/minor
→ pmem block device
```

容器内执行：

```bash
mknod /dev/pmem0 b 259 0
```

只是创建一个设备节点入口：

```text
Container /dev/pmem0
→ Guest Kernel block device 259:0
```

`mknod` 不会创建：

- NVDIMM
- PMEM region
- namespace
- block device

这些都已经由 Guest kernel 完成。

---

# 8. 阶段七：持久化语义与安全边界

这一阶段需要同时研究两条链：

```text
Durability / Persistence

Read-only / Security
```

二者不是同一问题。

## 8.1 Visibility、Ordering、Durability

需要严格区分：

```text
Visibility
→ 当前 CPU / 进程是否能看到新数据

Ordering
→ 多个写入以什么顺序被其他 CPU / 持久层观察

Durability
→ 掉电后数据是否仍存在
```

所以：

```text
Visible
≠ Ordered
≠ Durable
```

## 8.2 DAX store 为什么不等于持久化

DAX mmap：

```text
Application VA
→ PMEM PFN
```

但写入后可能只是：

```text
CPU Store Buffer
→ L1/L2/L3 Cache
```

因此：

```text
DAX 绕过 Page Cache
≠ 绕过 CPU Cache
≠ 每一次 store 都立刻掉电安全
```

## 8.3 Persistence Domain

Persistence Domain 表示：

```text
掉电后，系统保证哪些位置中的数据还能进入持久介质
```

常见概念：

```text
ADR
eADR
```

具体平台必须查硬件能力，不能默认 CPU Cache 一定属于 persistence domain。

## 8.4 虚拟 PMEM 的持久化链更长

真实 PMEM：

```text
CPU
→ Memory Controller
→ PMEM Media
```

Kata 虚拟 PMEM：

```text
Guest CPU
→ Guest PMEM GPA
→ KVM
→ Host Mapping
→ Host File
→ Host Filesystem
→ Host Block Device
```

所以 Guest 认为它是 PMEM，不代表 Host 真的是 NVDIMM。

## 8.5 virtio-pmem flush queue

virtio-pmem 的普通数据：

```text
Guest load/store
→ PMEM GPA
```

但 Guest 无法仅凭 CPU store 确认 Host backing 已经 `fsync()`。

所以需要：

```text
Guest fsync / nvdimm_flush
→ virtio-pmem flush_queue
→ VMM
→ Host backing fsync
→ ACK
```

因此 virtio-pmem virtqueue 的主要意义是：

```text
持久化协调协议通道
```

而不是普通文件数据搬运通道。

## 8.6 DAX 下 fsync 仍然有意义

DAX 去掉 Page Cache，并没有去掉：

- filesystem metadata
- journal
- inode
- extent
- persistence ordering
- VMM backing flush

所以：

```text
DAX
≠ fsync 无意义
```

---

# 9. 只读安全边界

必须区分：

```text
Container filesystem RO
↓
Guest filesystem RO
↓
Guest block device RO
↓
VMM virtual device RO
↓
Host backing RO
```

## 9.1 Filesystem mount ro

```bash
mount -o ro /dev/pmem0p1 /mnt/pmem
```

只说明：

```text
通过该 mount point 的正常 filesystem write 会被拒绝
```

不代表：

```text
/dev/pmem0p1 本身不可写
```

## 9.2 Block device ro

检查：

```bash
lsblk -o NAME,RO
blockdev --getro /dev/pmem0
cat /sys/class/block/pmem0/ro
```

如果 block device 仍可写，那么 raw block access 可以绕过 filesystem ro。

例如：

```text
Filesystem path
→ VFS
→ ext4
→ mount ro check

Raw block path
→ /dev/pmem0p1
→ block driver
```

第二条路径根本不经过该 filesystem mount。

所以：

```text
Filesystem RO
≠ Block Device RO
```

## 9.3 VMM read-only

更强的安全边界应该在 Hypervisor / Host：

```text
QEMU memory-backend-file readonly
QEMU NVDIMM unarmed
CLH readonly disk
```

这符合 Kata 的安全模型：

```text
Container / Guest
→ 不可信

Hypervisor / Host
→ 安全边界
```

## 9.4 Host backing read-only

最底层还可以检查：

```text
Host file open flags
Host file permissions
SELinux / AppArmor
Host filesystem
```

理想的 defense-in-depth：

```text
Container RO
+
Guest filesystem RO
+
Guest block RO
+
VMM device RO
+
Host backing RO
```

## 9.5 为什么 Kata PMEM 漏洞危险

如果 Guest rootfs：

```text
/dev/pmem0p1
→ ext4
→ Guest / mounted ro
```

但容器能够获得：

```text
/dev/pmem0p1 raw block access
```

那么容器可能：

```text
绕过 Guest rootfs mount ro
→ 直接修改 PMEM block device
→ 修改 Host backing image
```

因此：

```text
Guest / 是 ro
```

不能证明 Guest image 安全。

真正关键的是：

```text
Guest PMEM block device 是否 RO
VMM backing 是否 RO
```

---

# 10. 性能分析时应建立的统一模型

后续性能实验应该固定区分以下场景：

| 场景 | 设备 / 共享模型 | 文件系统位置 | Page Cache | DAX |
| --- | --- | --- | --- | --- |
| virtio-fs | FUSE + virtiofsd | Host | 有自己的缓存模型 | 可选 virtio-fs DAX |
| virtio-blk | virtqueue block | Guest | Guest Page Cache | 否 |
| PMEM without DAX | nd_pmem block | Guest | Guest Page Cache | 否 |
| PMEM + fsdax | PMEM direct mapping | Guest | 无文件数据 Page Cache | 是 |
| tmpfs | DRAM filesystem | Guest | DRAM | 不适用 |

## 10.1 virtio-fs

```text
Guest VFS
→ FUSE
→ virtqueue
→ virtiofsd
→ Host VFS
→ Host filesystem
```

## 10.2 virtio-blk

```text
Guest VFS
→ Guest ext4
→ Guest Page Cache
→ Guest block layer
→ virtio-blk
→ VMM
→ Host storage
```

## 10.3 PMEM without DAX

```text
Guest VFS
→ Guest ext4
→ Guest Page Cache
→ bio
→ nd_pmem
→ memcpy
→ PMEM mapping
```

## 10.4 PMEM + DAX

```text
Guest VFS
→ Guest ext4 DAX
→ Page Fault
→ PMEM PFN
→ User Page Table
→ CPU load/store
```

## 10.5 为什么 virtio-fs vs PMEM+DAX 不能直接归因于 DAX

因为同时变化了：

- FUSE
- virtiofsd
- Guest / Host boundary
- Filesystem location
- Page Cache
- block layer
- DAX
- metadata cache

所以如果观察到：

```text
open_close：PMEM+DAX ≫ virtio-fs
```

不能简单得出：

```text
DAX 导致 10 倍提升
```

正确实验应该增加：

```text
PMEM without DAX
```

从而隔离：

```text
PMEM + ext4 without DAX
vs
PMEM + ext4 + DAX
```

这才更接近 DAX 本身的增益。

---

# 11. 推荐验证命令

## 11.1 Guest 设备

```bash
lsblk -o NAME,MAJ:MIN,TYPE,SIZE,FSTYPE,RO,MOUNTPOINTS
findmnt -o TARGET,SOURCE,FSTYPE,OPTIONS /
```

## 11.2 PMEM 驱动来源

```bash
lsmod | grep -E 'nd_pmem|libnvdimm|nfit|virtio_pmem'
dmesg | grep -Ei 'nvdimm|nfit|virtio.*pmem|pmem'
readlink -f /sys/class/block/pmem0/device
```

## 11.3 Read-only 状态

```bash
blockdev --getro /dev/pmem0
blockdev --getro /dev/pmem0p1
cat /sys/class/block/pmem0/ro
findmnt /mnt/pmem
```

## 11.4 QEMU Host 参数

```bash
QEMU_PID=$(pgrep -f 'qemu-system' | head -n1)

tr '\0' '\n' </proc/${QEMU_PID}/cmdline \
  | grep -Ei 'memory-backend|nvdimm|virtio-pmem|virtio-blk|readonly|unarmed|kata-containers'
```

## 11.5 QEMU backing mmap

```bash
grep -E 'kata-containers\.img' /proc/${QEMU_PID}/maps
ls -l /proc/${QEMU_PID}/fd | grep kata-containers.img
```

## 11.6 Page Cache / Dirty

```bash
grep -E 'Cached|Dirty|Writeback' /proc/meminfo
vmstat 1
iostat -x 1
```

---

# 12. 源码阅读入口

## Kata Containers

### QEMU 架构入口

```text
src/runtime/virtcontainers/qemu_arch_base.go
```

重点：

```text
qemuArch
appendImage
appendBlockImage
appendNvdimmImage
qemuArchBase.disableNvdimm
```

### ARM64

```text
src/runtime/virtcontainers/qemu_arm64.go
```

重点：

```text
appendImage()
```

### QEMU Runtime

```text
src/runtime/virtcontainers/qemu.go
```

### QEMU 参数生成

```text
src/runtime/pkg/govmm/qemu/qemu.go
```

重点搜索：

```text
NVDIMM
MemoryBackendFile
ReadOnly
unarmed
```

### Cloud Hypervisor

```text
src/runtime/virtcontainers/clh.go
```

重点搜索：

```text
DisableImageNvdimm
PmemConfig
DiskConfig
readonly
```

## Linux Kernel

### libnvdimm 文档

```text
Documentation/driver-api/nvdimm/nvdimm.rst
```

### PMEM block driver

```text
drivers/nvdimm/pmem.c
```

重点：

```text
pmem_submit_bio
pmem_do_read
pmem_do_write
nvdimm_flush
```

### virtio-pmem driver

```text
drivers/nvdimm/virtio_pmem.c
```

重点：

```text
VIRTIO_ID_PMEM
virtio_pmem_probe
flush_queue
nvdimm_bus_register
nvdimm_pmem_region_create
```

---

# 13. 最终知识闭环

最终应形成下面这张图：

```text
PMEM
│
│ 一段持久的系统物理地址范围
│
├── NVDIMM
│   │
│   ├── QEMU memory-backend-file
│   ├── virtual NVDIMM
│   ├── ACPI NFIT
│   └── Guest acpi_nfit
│
└── virtio-pmem
    │
    ├── virtio device
    ├── PMEM start/size
    ├── Guest virtio_pmem
    └── flush_queue

            ↓

       libnvdimm
            ↓
         region
            ↓
        namespace
            ↓
        nd_pmem
            ↓
       /dev/pmem0
            ↓
       /dev/pmem0p1
            ↓
          ext4
       ↙        ↘
Page Cache      fsdax
  ↓               ↓
bio           PMEM PFN
  ↓               ↓
nd_pmem       User PTE
```

再放进 Kata：

```text
Kata configuration
→ HypervisorConfig
→ VMM image device selection
→ QEMU NVDIMM / block / historical CLH virtio-pmem
→ Guest kernel
→ libnvdimm / virtio_blk
→ Guest rootfs
→ Kata Agent
→ Container rootfs / volumes
```

最后再加上两条必须始终检查的链：

```text
Persistence：
CPU store
→ CPU Cache
→ Guest PMEM
→ Guest flush
→ VMM
→ Host backing
→ Host filesystem
→ Host storage

Read-only：
Container FS RO
→ Guest FS RO
→ Guest block RO
→ VMM device RO
→ Host backing RO
```

只要这三张链路能够独立画出来并解释清楚，PMEM、NVDIMM、virtio-pmem、DAX 就基本不会再混淆。

---

# 14. 后续学习建议

下一步可以继续深入：

1. Linux DAX fault 源码：ext4 / fs/dax 如何把文件 offset 转成 PFN。
2. QEMU `memory-backend-file`、`MemoryRegion`、KVM memory slot 的真实地址映射流程。
3. QEMU NVDIMM 的 ACPI NFIT 生成流程。
4. virtio-pmem flush 请求从 Guest 到 VMM 的完整代码调用链。
5. Kata Guest image builder 中 PMEM/DAX metadata、MBR 和 `nd_pfn_sb` 的生成方式。
6. Kata `virtio-fs`、`virtio-blk`、PMEM without DAX、PMEM+DAX 的统一性能实验矩阵。
7. Kata PMEM 只读安全问题中，从 Container raw block write 到 Host image backing 的完整攻击/修复链路。
