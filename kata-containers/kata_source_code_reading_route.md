# Kata Containers 源码查看路线文档

> 目标：围绕 Pod 创建、containerd-shim-kata-v2、virtcontainers、QEMU、virtio-fs、block rootfs、agent 这条主链路看源码。适合分析 Kata + QEMU + virtio-fs/block/pmem 的性能瓶颈。

---

## 0. 先建立整体认知

Kata 的核心思想是：对外仍表现为容器 runtime，但容器 workload 实际运行在轻量 VM 内。containerd 通过 shim v2 调用 Kata runtime；Kata runtime 启动 hypervisor、启动 VM、通过 ttRPC/VSOCK 与 guest 内的 kata-agent 通信。

建议先看：

```text
docs/design/architecture/README.md
src/runtime/README.md
src/runtime/virtcontainers/README.md
```

重点理解三个环境：

```text
Host
  ├─ containerd
  ├─ containerd-shim-kata-v2
  ├─ qemu
  └─ virtiofsd

Guest VM root
  └─ kata-agent

Container environment inside VM
  └─ workload process
```

---

## 1. 启动入口：containerd-shim-kata-v2

### 文件

```text
src/runtime/cmd/containerd-shim-kata-v2/main.go
```

### 核心代码

```go
func main() {
    shimapi.Run(types.DefaultKataRuntimeName, shim.New, shimConfig)
}
```

### 作用

这是 Go 版 shim 的入口。containerd 启动的不是 `kata-runtime`，而是 `containerd-shim-kata-v2`。这里通过 `shimapi.Run()` 注册 Kata shim v2 service。

### 继续跳转

```text
shim.New
  ↓
src/runtime/pkg/containerd-shim-v2/service.go
```

---

## 2. shim service：接 containerd Task API

### 文件

```text
src/runtime/pkg/containerd-shim-v2/service.go
```

### 核心方法

```go
func New(ctx context.Context, id string, publisher cdshim.Publisher, shutdown func()) (cdshim.Shim, error)
```

### 作用

创建 shim service 对象，保存 sandbox id、namespace、container map、事件通道等。这个 service 实现 containerd shim v2 的 TaskService。

关键字段：

```go
type service struct {
    sandbox vc.VCSandbox
    containers map[string]*container
    config *oci.RuntimeConfig
    id string
    hpid uint32
}
```

理解：

```text
一个 Pod 对应一个 shim service
一个 shim service 里维护一个 Kata sandbox
一个 sandbox 对应一个 VM
一个 VM 里可以有多个 container
```

---

## 3. Create 请求：判断是 sandbox 还是普通容器

### 文件

```text
src/runtime/pkg/containerd-shim-v2/create.go
```

### 核心方法

```go
func create(ctx context.Context, s *service, r *taskAPI.CreateTaskRequest) (*container, error)
```

### 作用

这是 containerd 调 Kata 创建容器时的主入口。它主要做：

1. 解析 containerd 传来的 rootfs。
2. 读取 OCI config.json。
3. 判断容器类型：PodSandbox、SingleContainer、PodContainer。
4. 加载 Kata runtime 配置。
5. 根据 rootfs 类型决定是否在 host 侧 mount。
6. 调 `katautils.CreateSandbox()` 或 `katautils.CreateContainer()`。

### 核心代码

```go
containerType, err := oci.ContainerType(*ociSpec)
runtimeConfig, err := loadRuntimeConfig(s, r, ociSpec.Annotations)
```

```go
switch containerType {
case virtcontainers.PodSandbox, virtcontainers.SingleContainer:
    sandbox, _, err := katautils.CreateSandbox(...)
    s.sandbox = sandbox

case virtcontainers.PodContainer:
    _, err = katautils.CreateContainer(...)
}
```

### 对你当前任务最重要的代码

```go
func checkAndMount(s *service, r *taskAPI.CreateTaskRequest) (bool, error) {
    if katautils.IsBlockDevice(m.Source) && !s.config.HypervisorConfig.DisableBlockDeviceUse {
        return false, nil
    }
    ...
    doMount(r.Rootfs, rootfs)
}
```

### 作用解释

这段决定 rootfs 走哪条路径：

```text
普通 overlay/rootfs
  → host 侧 mount 到 bundle/rootfs
  → 再通过 shared fs 共享进 VM

block backed rootfs + disable_block_device_use=false
  → 不在 host 侧 mount
  → 直接把 block device 插进 VM
```

这就是你分析 `virtio-fs` 和 `virtio-scsi/virtio-blk` 差异时的关键入口。

---

## 4. 配置加载：configuration-qemu.toml 怎么进代码

### 文件

```text
src/runtime/pkg/containerd-shim-v2/create.go
src/runtime/pkg/katautils/config.go
src/runtime/config/configuration-qemu.toml.in
```

### 核心方法

```go
func loadRuntimeConfig(s *service, r *taskAPI.CreateTaskRequest, anno map[string]string) (*oci.RuntimeConfig, error)
```

### 配置优先级

源码逻辑大致是：

```text
OCI annotation config_path
  ↓
containerd shimv2 options
  ↓
KATA_CONF_FILE 环境变量
  ↓
default configuration path
```

### 重点配置字段

你现在主要看这些：

```toml
shared_fs = "virtio-fs"
virtio_fs_daemon = "..."
virtio_fs_cache = "..."
block_device_driver = "virtio-scsi"
disable_block_device_use = false
rootfs_type = "ext4"
image = "..."
initrd = "..."
memory_size = ...
default_vcpus = ...
sandbox_cgroup_only = ...
```

---

## 5. katautils.CreateSandbox：把 OCI + runtime config 转成 SandboxConfig

### 文件

```text
src/runtime/pkg/katautils/create.go
```

### 核心方法

```go
func CreateSandbox(ctx context.Context, vci vc.VC, ociSpec specs.Spec, runtimeConfig oci.RuntimeConfig, rootFs vc.RootFs, containerID, bundlePath string, disableOutput, systemdCgroup bool) (_ vc.VCSandbox, _ vc.Process, err error)
```

### 作用

把 containerd/OCI 层的信息转换成 virtcontainers 能理解的 `SandboxConfig`。

### 核心代码

```go
sandboxConfig, err := oci.SandboxConfig(ociSpec, runtimeConfig, bundlePath, containerID, disableOutput, systemdCgroup)
```

```go
sandboxConfig.HypervisorConfig.SharedPath = vc.GetSharePath(containerID)
```

```go
sandbox, err := vci.CreateSandbox(ctx, sandboxConfig, func(ctx context.Context) error {
    PreStartHooks(ctx, ociSpec, containerID, bundlePath)
    CreateRuntimeHooks(ctx, ociSpec, containerID, bundlePath)
    return nil
})
```

### 作用解释

这里是桥梁层：

```text
containerd request / OCI spec / configuration.toml
  ↓
oci.SandboxConfig
  ↓
virtcontainers.SandboxConfig
  ↓
virtcontainers.CreateSandbox
```

---

## 6. virtcontainers.CreateSandbox：真正创建 sandbox/VM/container

### 文件

```text
src/runtime/virtcontainers/api.go
```

### 核心方法

```go
func CreateSandbox(ctx context.Context, sandboxConfig SandboxConfig, factory Factory, prestartHookFunc func(context.Context) error) (VCSandbox, error)
```

### 内部主流程

```go
s, err := createSandbox(ctx, sandboxConfig, factory)
s.createNetwork(ctx)
s.setupResourceController()
s.startVM(ctx, prestartHookFunc)
s.getAndStoreGuestDetails(ctx)
s.createContainers(ctx)
```

### 作用解释

这是主生命周期：

```text
创建 Sandbox 对象
  ↓
准备网络
  ↓
准备 cgroup/resource controller
  ↓
启动 VM
  ↓
和 agent 通信获取 guest 信息
  ↓
创建容器
```

---

## 7. Sandbox 结构：Kata 的核心运行时对象

### 文件

```text
src/runtime/virtcontainers/sandbox.go
```

### 核心结构

```go
type Sandbox struct {
    devManager api.DeviceManager
    factory Factory
    hypervisor Hypervisor
    agent agent
    fsShare FilesystemSharer
    containers map[string]*Container
    network Network
    config *SandboxConfig
    state types.SandboxState
}
```

### 作用

Sandbox 是 Kata 内部最重要的对象：

```text
Sandbox
  ├─ Hypervisor：QEMU/CloudHypervisor/Firecracker/remote
  ├─ Agent：guest 内 kata-agent 通信代理
  ├─ FilesystemSharer：virtio-fs/none/nydus
  ├─ DeviceManager：block/vfio/device 管理
  ├─ Network：网络 endpoint 管理
  └─ Container map：Pod 内多个容器
```

看 Kata 源码时，不要只看某一个方法，要围绕 `Sandbox` 这个对象追字段。

---

## 8. newSandbox：初始化 hypervisor、agent、fsShare、deviceManager

### 文件

```text
src/runtime/virtcontainers/sandbox.go
```

### 核心方法

```go
func newSandbox(ctx context.Context, sandboxConfig SandboxConfig, factory Factory) (*Sandbox, error)
```

### 核心代码

```go
agent := getNewAgentFunc(ctx)()
hypervisor, err := NewHypervisor(sandboxConfig.HypervisorType)
network, err := NewNetwork(&sandboxConfig.NetworkConfig)
```

```go
fsShare, err := NewFilesystemShare(s)
s.devManager = deviceManager.NewDeviceManager(sandboxConfig.HypervisorConfig.BlockDeviceDriver, ...)
```

```go
s.hypervisor.CreateVM(ctx, s.id, s.network, &sandboxConfig.HypervisorConfig)
s.agent.init(ctx, s, sandboxConfig.AgentConfig)
```

### 作用解释

这里还没有真正启动 VM，而是准备 VM 对象和 QEMU 参数。

```text
NewHypervisor
  → 根据配置创建 qemu/clh/fc 等 hypervisor 实现

NewFilesystemShare
  → 根据 shared_fs 创建 virtio-fs/none 等共享文件系统逻辑

NewDeviceManager
  → 根据 block_device_driver 初始化设备管理器

hypervisor.CreateVM
  → 生成 QEMU 配置，但不一定启动 QEMU 进程
```

---

## 9. startVM：启动 QEMU，并通知 agent 启动 sandbox

### 文件

```text
src/runtime/virtcontainers/sandbox.go
```

### 核心方法

```go
func (s *Sandbox) startVM(ctx context.Context, prestartHookFunc func(context.Context) error) error
```

### 核心代码

```go
return s.hypervisor.StartVM(ctx, VmStartTimeout)
```

```go
if err := s.agent.startSandbox(ctx, s); err != nil {
    return err
}
```

### 作用解释

这一步才是真正启动虚拟机：

```text
s.hypervisor.StartVM
  → 启动 qemu 进程
  → 等 QMP 就绪
  → VM boot

s.agent.startSandbox
  → 通过 ttRPC/VSOCK 调 guest 内 kata-agent
  → agent 准备 sandbox 环境
```

---

## 10. QEMU 实现：qemu struct 和 CreateVM

### 文件

```text
src/runtime/virtcontainers/qemu.go
src/runtime/virtcontainers/qemu_amd64.go
src/runtime/pkg/govmm/qemu/
```

### 核心结构

```go
type qemu struct {
    arch qemuArch
    virtiofsDaemon VirtiofsDaemon
    qemuConfig govmmQemu.Config
    config HypervisorConfig
    nvdimmCount int
}
```

### 核心方法

```go
func (q *qemu) CreateVM(ctx context.Context, id string, network Network, hypervisorConfig *HypervisorConfig) error
```

### CreateVM 主要做什么

```text
q.setup
  ↓
getQemuMachine
  ↓
buildNUMATopology
  ↓
cpuTopology / memoryTopology
  ↓
buildDevices
  ↓
createPCIeTopology
  ↓
createVirtiofsDaemon
  ↓
保存 q.qemuConfig
```

### 核心代码

```go
machine, err := q.getQemuMachine()
smp := q.cpuTopology(effectiveNUMANodes)
memory, err := q.memoryTopology()
devices, ioThread, kernel, err := q.buildDevices(ctx, kernelPath)
```

```go
qemuConfig := govmmQemu.Config{
    Name: fmt.Sprintf("sandbox-%s", q.id),
    Path: qemuPath,
    Machine: machine,
    SMP: smp,
    Memory: memory,
    Devices: devices,
    Kernel: *kernel,
    QMPSockets: qmpSockets,
}
```

### 对性能分析最有价值的点

```go
if q.config.SharedFS == config.VirtioFS || q.config.SharedFS == config.VirtioFSNydus {
    q.setupFileBackedMem(&knobs, &memory)
}
```

含义：virtio-fs 需要 shared memory/file-backed memory。这会影响内存后端、NUMA、hugepage、VM 模板等行为。

---

## 11. QEMU StartVM：启动 qemu 进程和 virtiofsd

### 文件

```text
src/runtime/virtcontainers/qemu.go
```

### 核心方法

```go
func (q *qemu) StartVM(ctx context.Context, timeout int) error
```

### 核心代码

```go
qmpConn, err := q.setupEarlyQmpConnection()
```

```go
if q.config.SharedFS == config.VirtioFS || q.config.SharedFS == config.VirtioFSNydus {
    err = q.setupVirtiofsDaemon(ctx)
}
```

```go
qemuCmd, reader, err := govmmQemu.LaunchQemu(q.qemuConfig, newQMPLogger())
err = q.waitVM(ctx, qmpConn, timeout)
```

### 作用解释

启动顺序大致是：

```text
准备 VM 运行目录
  ↓
准备 QMP socket
  ↓
如果 shared_fs=virtio-fs，先启动 virtiofsd
  ↓
govmm LaunchQemu
  ↓
通过 QMP 等待 VM ready
```

这条线适合你对照实际机器上的：

```bash
ps -ef | grep qemu
ps -ef | grep virtiofsd
ls -l /run/vc/vm/<sandbox-id>/
```

---

## 12. virtiofsd：Kata 共享文件系统性能瓶颈重点

### 文件

```text
src/runtime/virtcontainers/virtiofsd.go
```

### 核心接口

```go
type VirtiofsDaemon interface {
    Start(context.Context, onQuitFunc) (pid int, err error)
    Stop(context.Context) error
    Mount(opt MountOption) error
    Umount(mountpoint string) error
}
```

### 核心方法

```go
func (v *virtiofsd) Start(ctx context.Context, onQuit onQuitFunc) (int, error)
```

### 核心代码

```go
args := []string{
    "--syslog",
    "--cache=" + v.cache,
    "--shared-dir=" + v.sourcePath,
    fmt.Sprintf("--fd=%v", FdSocketNumber),
}
```

### 作用解释

Kata 在 host 上启动一个 virtiofsd，把 host 的共享目录暴露给 guest。容器 rootfs 如果走 virtio-fs，本质上会经过：

```text
container workload
  ↓
guest virtio-fs mount
  ↓
virtiofs device
  ↓
host virtiofsd
  ↓
host shared dir / overlay rootfs
```

所以你之前测到 `stat/read_small` 慢，是很合理的，因为 metadata 操作会频繁穿越 guest-host 边界。

---

## 13. block rootfs / virtio-scsi 线索

### 文档

```text
docs/design/architecture/storage.md
```

### 关键结论

如果使用 block-based graph driver，Kata 会用 `virtio-scsi` 把 workload image/rootfs 以块设备方式共享进 VM。

普通情况下，如果没有 block-based graph driver，Kata 用 `virtio-fs` 共享 workload image/rootfs。

### 源码入口

```text
src/runtime/pkg/containerd-shim-v2/create.go
  └─ checkAndMount

src/runtime/virtcontainers/qemu.go
  └─ buildDevices
  └─ appendSCSIController

src/runtime/pkg/device/
src/runtime/pkg/device/drivers/
```

### 核心判断

```go
if katautils.IsBlockDevice(m.Source) && !s.config.HypervisorConfig.DisableBlockDeviceUse {
    return false, nil
}
```

意思是：如果 rootfs source 是块设备，并且没有禁用 block device use，就不要 host mount，后续走设备直通/热插入。

这就是你要分析 `virtio-fs vs virtio-scsi/blk` 的核心分叉。

---

## 14. runtime-rs 新链路：Rust 版重点看哪里

Kata 主仓现在同时有 Go runtime 和 Rust runtime-rs。你如果当前环境是 Go 版 `containerd-shim-kata-v2`，先看 Go 链路；如果配置/二进制用了 runtime-rs，再看 Rust。

### Rust shared fs 入口

```text
src/runtime-rs/crates/resource/src/share_fs/mod.rs
```

### 核心 trait

```rust
pub trait ShareFs {
    async fn setup_device_before_start_vm(...);
    async fn setup_device_after_start_vm(...);
    async fn get_storages(&self) -> Result<Vec<Storage>>;
    async fn stop(&self) -> Result<()>;
}
```

```rust
pub trait ShareFsMount {
    async fn share_rootfs(&self, config: &ShareFsRootfsConfig) -> Result<ShareFsMountResult>;
    async fn share_volume(&self, config: &ShareFsVolumeConfig) -> Result<ShareFsMountResult>;
    async fn umount_rootfs(&self, config: &ShareFsRootfsConfig) -> Result<()>;
}
```

### shared_fs 分发

```rust
match shared_fs.as_str() {
    "inline-virtio-fs" => ShareVirtioFsInline,
    "virtio-fs" => ShareVirtioFsStandalone,
    "virtio-fs-nydus" => ShareVirtioFsNydus,
    _ => unsupported,
}
```

### 作用解释

Rust 版把共享文件系统抽象得更清晰：

```text
ShareFs
  ├─ inline-virtio-fs
  ├─ standalone virtio-fs
  └─ virtio-fs-nydus
```

如果你研究 `virtio-fs-nydus`，这条 Rust 路线更重要。

---

## 15. 建议源码查看顺序

### 第一轮：只看主链路

```text
1. docs/design/architecture/README.md
2. src/runtime/README.md
3. src/runtime/cmd/containerd-shim-kata-v2/main.go
4. src/runtime/pkg/containerd-shim-v2/service.go
5. src/runtime/pkg/containerd-shim-v2/create.go
6. src/runtime/pkg/katautils/create.go
7. src/runtime/virtcontainers/api.go
8. src/runtime/virtcontainers/sandbox.go
9. src/runtime/virtcontainers/qemu.go
10. src/runtime/virtcontainers/virtiofsd.go
```

### 第二轮：围绕性能看存储

```text
1. docs/design/architecture/storage.md
2. create.go / checkAndMount
3. qemu.go / buildDevices
4. qemu.go / createVirtiofsDaemon
5. virtiofsd.go / Start / args
6. device manager / block driver
7. runtime-rs share_fs 模块
```

### 第三轮：结合你自己的测试现象

你现在可以按问题反推源码：

```text
stat/read_small 慢
  → virtiofsd.go
  → qemu.go setupVirtiofsDaemon
  → shared path
  → guest mount

block/scsi 是否生效
  → create.go checkAndMount
  → RootFs.Source 是否 block device
  → disable_block_device_use
  → qemu.go appendSCSIController

pmem/dax 是否生效
  → qemu arch appendImage / nvdimm
  → guest mount 参数
  → /proc/mounts / findmnt

UnixBench 线性度差
  → vCPU 配置
  → qemu.go cpuTopology
  → cgroup/sandbox_cgroup_only
  → virtiofsd 单进程/队列/metadata 开销
```

---

## 16. 推荐 grep/rg 命令

```bash
rg "func create\(" src/runtime/pkg/containerd-shim-v2
rg "CreateSandbox" src/runtime
rg "checkAndMount" src/runtime
rg "DisableBlockDeviceUse" src/runtime
rg "BlockDeviceDriver" src/runtime
rg "VirtioSCSI" src/runtime
rg "SharedFS" src/runtime
rg "VirtioFS" src/runtime
rg "createVirtiofsDaemon" src/runtime
rg "setupVirtiofsDaemon" src/runtime
rg "LaunchQemu" src/runtime
rg "StartVM" src/runtime/virtcontainers
rg "appendSCSIController" src/runtime
rg "appendImage" src/runtime/virtcontainers
rg "NVDIMM|nvdimm|pmem|dax" src/runtime
```

---

## 17. 最小调用链总图

```text
containerd
  ↓ shim v2 grpc
tcontainerd-shim-kata-v2 main.go
  ↓ shimapi.Run(..., shim.New, ...)
service.New
  ↓
create.go:create
  ↓
loadSpec / loadRuntimeConfig / checkAndMount
  ↓
katautils.CreateSandbox
  ↓
oci.SandboxConfig
  ↓
virtcontainers.CreateSandbox
  ↓
createSandboxFromConfig
  ↓
createSandbox / newSandbox
  ↓
NewHypervisor / NewFilesystemShare / NewDeviceManager
  ↓
qemu.CreateVM
  ↓
qemu.buildDevices / createVirtiofsDaemon / qemuConfig
  ↓
sandbox.startVM
  ↓
qemu.StartVM
  ↓
setupVirtiofsDaemon
  ↓
govmmQemu.LaunchQemu
  ↓
qemu.waitVM
  ↓
agent.startSandbox
  ↓
s.createContainers
  ↓
container.create
  ↓
agent.createContainer
  ↓
s.StartContainer
  ↓
container.start
  ↓
agent.startContainer
  ↓
workload running inside VM
```

---

## 18. 你当前任务的阅读重点

你的任务是分析 Kata + QEMU UnixBench 线性度瓶颈，所以优先看：

```text
create.go / checkAndMount
qemu.go / CreateVM
qemu.go / StartVM
virtiofsd.go / Start / args
storage.md
runtime-rs share_fs
```

不要一开始陷入网络、policy、confidential computing、GPU、factory、remote hypervisor。那些可以后看。

最关键的问题是：

```text
1. rootfs 到底是通过 virtio-fs 共享，还是 block device 插入？
2. shared_fs=virtio-fs 时 QEMU 内存后端怎么变？
3. virtiofsd 参数是什么？cache 是什么？shared-dir 是哪？
4. block_device_driver=virtio-scsi 时有没有真正走到 block rootfs？
5. 你的 UnixBench 差项是不是集中在文件元数据和小文件读？
```
