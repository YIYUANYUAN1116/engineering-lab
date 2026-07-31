# Kata Containers：virtio-fs 与块设备深入学习笔记

> 目标：系统理解 virtio-fs、virtio-blk、virtio-scsi、FUSE、VFS、page cache、Kata rootfs 存储路径，以及如何用 fio / iostat / strace / perf 定位性能瓶颈。
>
> 背景：用于分析 Kata + QEMU 场景下 UnixBench 线性度、Execl/FileCopy/ShellScripts 等指标异常，以及 rootfs 走 virtio-fs / virtio-scsi / virtio-blk 的性能差异。

---

## 目录

1. [总体理解：virtio-fs 和块设备的本质区别](#1-总体理解virtio-fs-和块设备的本质区别)
2. [Linux VFS：open/stat/read/write/execve 怎么走](#2-linux-vfsopenstatreadwriteexecve-怎么走)
3. [page cache / dentry cache / inode cache](#3-page-cache--dentry-cache--inode-cache)
4. [FUSE 基本原理](#4-fuse-基本原理)
5. [virtio 基本模型：driver、device、virtqueue](#5-virtio-基本模型driverdevicevirtqueue)
6. [virtio-fs 完整路径：guest driver + virtiofsd + vhost-user](#6-virtio-fs-完整路径guest-driver--virtiofsd--vhost-user)
7. [virtio-blk / virtio-scsi：块设备路径](#7-virtio-blk--virtio-scsi块设备路径)
8. [QEMU block cache / aio / IOThread / multiqueue 调优](#8-qemu-block-cache--aio--iothread--multiqueue-调优)
9. [fio / iostat / strace / perf 实战定位](#9-fio--iostat--strace--perf-实战定位)
10. [Kata 里的 rootfs / image layer / overlayfs / virtio-fs / block device](#10-kata-里的-rootfs--image-layer--overlayfs--virtio-fs--block-device)
11. [AI 沙箱 / Kata 实践选型建议](#11-ai-沙箱--kata-实践选型建议)
12. [完整排查命令清单](#12-完整排查命令清单)
13. [汇报文案模板](#13-汇报文案模板)
14. [参考资料](#14-参考资料)

---

# 1. 总体理解：virtio-fs 和块设备的本质区别

先抓住一句话：

**virtio-fs 是“把宿主机目录以文件系统语义共享给 guest/container”；块设备是“把一块磁盘/镜像/LV 以块语义交给 guest，再由 guest 自己挂文件系统”。**

二者不是同一个层次，所以性能、缓存、一致性、适用场景差异很大。

## 1.1 virtio-fs 路径

以 Kata 容器 rootfs 走 virtio-fs 为例：

```text
容器进程
  ↓ read/stat/open/exec
 guest kernel VFS
  ↓ virtiofs 文件系统驱动
virtqueue / vhost-user-fs
  ↓
host 上的 virtiofsd 进程
  ↓
宿主机目录 / overlayfs / container image layer
  ↓
host 文件系统 / 块设备
```

关键点：

```text
virtio-fs 的 I/O 数据面里有一个 host 用户态进程 virtiofsd。
```

所以 `virtiofsd` 的 CPU、绑核、线程池、cache 模式会明显影响性能。

## 1.2 块设备路径

以 rootfs 走 virtio-scsi / virtio-blk 为例：

```text
容器进程
  ↓ read/write/exec/stat
guest kernel VFS
  ↓ guest ext4/xfs/overlayfs
guest block layer
  ↓ virtio-blk / virtio-scsi driver
virtqueue
  ↓
QEMU / vhost / host block backend
  ↓
host block device / image / devicemapper / LVM / NVMe
```

关键点：

```text
guest 内核自己管理文件系统和 page cache。
host 主要处理块 I/O，不需要每个 stat/open 都让 host 的 virtiofsd 做文件语义处理。
```

## 1.3 文件语义 vs 块语义

### virtio-fs 提供文件系统语义

guest 看到的是一个共享目录，不是一块盘。guest 里执行：

```bash
stat /bin/sh
open /usr/bin/perl
read /lib/xxx.so
readdir /usr/bin
execve /bin/bash
```

这些文件操作会通过 virtio-fs 协议传给 host 侧 `virtiofsd`。`virtiofsd` 再在宿主机目录里做实际的 `open/stat/read/write/getattr/readdir` 等操作。

virtio-fs 擅长：

```text
host 和 guest 共享目录
容器镜像层直接复用 host 文件
不想给每个容器准备完整磁盘
启动快
易于集成 Kubernetes / containerd / overlayfs
```

virtio-fs 比较怕：

```text
大量小文件 stat
大量 open/close
大量 exec
大量 shell 脚本
频繁 metadata 操作
高并发目录遍历
```

对应 UnixBench 容易差的指标：

```text
Execl Throughput
File Copy
Process Creation
Shell Scripts
```

这些测试不是单纯算力，而是会频繁触发 `fork/exec/open/stat/read library`。

### 块设备提供磁盘语义

guest 看到的是类似：

```bash
/dev/vda
/dev/sda
/dev/dm-0
```

然后 guest 自己挂 ext4/xfs/overlayfs。对应用来说，`stat/open/read/write` 先在 guest 内核 VFS 和 page cache 里处理，真正需要落盘时才变成 block request。

块设备擅长：

```text
rootfs 读写
数据库
编译
解压
大量小文件
大量 exec
fio 随机读写
UnixBench file/process/shell 项
```

块设备的问题：

```text
不天然共享 host 目录
每个容器/VM 需要一个独立块设备或快照
镜像管理复杂
动态挂载 volume 不如 virtio-fs 简单
host 不能像普通目录一样直接看到完整文件语义
```

## 1.4 为什么 virtio-fs 的 UnixBench 会崩？

典型现象：

```text
Dhrystone / Whetstone 接近 host
Execl / File Copy / Shell Scripts 极低
virtio-scsi rootfs 明显好很多
```

这说明 CPU 虚拟化本身没大问题，问题集中在文件系统路径和元数据操作。

以 `Execl Throughput` 为例，一次 `execve()` 可能涉及：

```text
查找可执行文件路径
检查权限
读取 ELF header
加载动态链接器
读取共享库
读取 ld.so.cache
触发 page fault
关闭/继承 fd
创建新进程地址空间
```

普通 host 或 guest block rootfs 下，很多 metadata 和 page cache 都在本机内核里命中。

virtio-fs 下，很多操作要变成：

```text
guest VFS
  → virtiofs request
  → virtqueue
  → virtiofsd
  → host syscall
  → host fs
  → 返回 guest
```

一次 shell 脚本测试会重复成千上万次。每次只多几十微秒，总体也会被放大。

## 1.5 virtio-fs cache 模式

常见模式：

```text
virtio-fs cache=auto
virtio-fs cache=metadata
virtio-fs cache=always
virtio-fs cache=none
```

核心思想：**在性能和一致性之间取舍。**

| 模式 | 大致含义 | 性能 | 一致性 | 适合场景 |
|---|---|---:|---:|---|
| `cache=none` | 尽量不缓存，更多请求回 host | 差 | 强 | host/guest 同时改文件 |
| `cache=auto` | 折中，带超时/自动失效 | 中 | 中 | 通用共享目录 |
| `cache=metadata` | 更多缓存元数据 | 中高 | 中 | 大量 stat/open |
| `cache=always` | guest 更激进缓存 | 高 | 弱 | rootfs 只读、host 不改文件 |

如果 virtio-fs 共享目录对 guest 只读，而且 host 不会在容器运行期间修改对应文件，那么 `cache=always` 的一致性风险较低，比较适合只读 rootfs、只读依赖、只读模型。

如果存在：

```text
host 会改共享目录
多个 VM 共享同一目录并写入
容器内写，host 也读写
CI 构建目录频繁变动
```

则 `cache=always` 风险较高。

## 1.6 DAX 不要和 cache=always 混淆

DAX = Direct Access。

普通文件 I/O 通常会经过 page cache；DAX 的目标是绕过 page cache，让应用可以更直接访问持久内存或类似介质上的文件数据。

放到 virtio-fs 里，可以粗略理解为：

```text
普通 virtio-fs:
  guest page cache 里缓存文件内容

virtio-fs DAX:
  guest 可以把 host 侧文件缓存/内存窗口映射进 guest
  减少数据拷贝和 page cache 重复
```

注意：

```text
DAX 主要改善大文件 mmap/read 场景。
DAX 不一定解决所有 metadata 问题。
stat/open/readdir/exec 这种 metadata-heavy 场景，DAX 不一定能救回来。
```

对 UnixBench 的 `execl/filecopy/shell`，DAX 不是决定性万能药。

## 1.7 virtio-blk、virtio-scsi、vhost-user-blk

### virtio-blk

最直接的 virtio 块设备：

```text
guest /dev/vda
  ↓
virtio-blk driver
  ↓
QEMU block backend
  ↓
host image/block device
```

优点：

```text
路径短
性能好
配置简单
适合少量磁盘
```

缺点：

```text
扩展能力不如 SCSI
大量磁盘管理不如 virtio-scsi
高级 SCSI 特性少
```

### virtio-scsi

虚拟 SCSI 控制器，下面可以挂多个磁盘：

```text
guest /dev/sda
  ↓
virtio-scsi driver
  ↓
SCSI layer
  ↓
QEMU backend
```

优点：

```text
适合多盘
支持更多 SCSI 语义
discard/unmap 等能力更完整
扩展性好
```

缺点：

```text
路径比 virtio-blk 稍复杂
单盘极限性能可能略差
需要合理配置 multiqueue / iothread
```

经验：

```text
少量磁盘、极致性能偏 virtio-blk。
很多磁盘或需要完整 SCSI 特性偏 virtio-scsi。
```

### vhost-user-blk / vhost-user-scsi

把 block backend 放到独立用户态进程里，通过 vhost-user 与 QEMU/guest 通信：

```text
guest virtio driver
  ↓
vhost-user
  ↓
外部 storage backend 进程
  ↓
SPDK / 自研存储引擎 / 高性能 NVMe
```

适合：

```text
高性能存储
SPDK
用户态轮询
绕开部分 QEMU block layer 开销
云厂商定制存储
```

代价是复杂度、部署、稳定性、调试成本更高。

---

# 2. Linux VFS：open/stat/read/write/execve 怎么走

## 2.1 VFS 是什么？

**VFS = Virtual File System，虚拟文件系统层。**

可以理解为：

```text
应用程序
  ↓ open/stat/read/write/execve
Linux syscall
  ↓
VFS 统一入口
  ↓
具体文件系统：ext4 / xfs / overlayfs / tmpfs / virtiofs / procfs / sysfs ...
  ↓
块设备 / 网络 / 内存 / FUSE / host 共享目录
```

应用不需要知道下面到底是 ext4、xfs、virtio-fs 还是 tmpfs，只管调用：

```c
open("/bin/bash")
read(fd, buf, size)
stat("/usr/bin/perl")
execve("/bin/sh", ...)
```

VFS 根据路径所在的挂载点判断底层是谁。

## 2.2 VFS 解决的问题

如果没有 VFS，应用可能要：

```text
读 ext4 文件 → 调 ext4_open
读 xfs 文件 → 调 xfs_open
读 tmpfs 文件 → 调 tmpfs_open
读 virtiofs 文件 → 调 virtiofs_open
```

有了 VFS：

```text
对上：给应用统一接口
对下：屏蔽不同文件系统差异
中间：维护缓存、路径解析、权限、文件对象
```

## 2.3 VFS 里最重要的 4 个对象

```text
superblock  超级块
inode       文件本体的元数据
dentry      路径名到 inode 的映射
file        进程打开后的文件对象
```

### superblock

表示一个文件系统实例。例如：

```bash
mount /dev/vdb1 /data
```

内核里会有一个 superblock，表示这个挂载的文件系统实例。

包含：

```text
文件系统类型：ext4 / xfs / virtiofs
块大小
inode 管理信息
根目录
文件系统操作函数
```

### inode

inode 表示文件本体元数据。

它关心：

```text
文件类型：普通文件/目录/符号链接
权限
uid/gid
大小
时间戳
数据块位置
引用计数
文件操作函数
```

但它不关心文件名。

例如硬链接：

```bash
ln /data/a.txt /data/b.txt
```

`a.txt` 和 `b.txt` 可以指向同一个 inode。

### dentry

dentry = directory entry，目录项。

它负责把名字映射到 inode：

```text
/bin/bash 这个名字 → 某个 inode
/usr/bin/python 这个名字 → 某个 inode
/etc/hosts 这个名字 → 某个 inode
```

内核有 dentry cache，也叫 dcache。

UnixBench 里这些项差：

```text
Execl Throughput
Shell Scripts
Process Creation
File Copy
```

大部分都和路径查找相关：

```text
/bin/sh
/bin/bash
/usr/bin/perl
/lib64/ld-linux
/lib/libc.so
```

如果 dentry cache 命中，速度很快；如果需要穿透到底层文件系统，virtio-fs 比块设备慢很多。

### file

进程执行：

```c
fd = open("/tmp/a.txt", O_RDONLY);
```

内核会创建 `struct file`，进程拿到 fd。

关系：

```text
进程 fd table
  fd=3
   ↓
struct file
   ↓
dentry
   ↓
inode
```

`file` 里有：

```text
当前读写位置 offset
打开模式：只读/只写/追加
文件操作函数 file_operations
指向 dentry/inode
```

总结：

```text
inode = 文件本体
dentry = 文件名到 inode 的映射
file = 某个进程打开这个文件后的状态
```

## 2.4 open("/usr/bin/bash") 发生了什么？

流程：

```text
1. 应用进入内核：sys_openat
2. VFS 解析路径 /usr/bin/bash
3. 从根目录 / 开始查找
4. 查 usr 这个 dentry
5. 查 bin 这个 dentry
6. 查 bash 这个 dentry
7. 找到 bash 对应 inode
8. 检查权限
9. 调用具体文件系统的 open 方法
10. 创建 struct file
11. 返回 fd 给进程
```

图示：

```text
open("/usr/bin/bash")
        ↓
      VFS
        ↓
路径解析：/ → usr → bin → bash
        ↓
dcache 查 dentry
        ↓
inode cache 查 inode
        ↓
权限检查
        ↓
具体文件系统 open
        ↓
返回 fd
```

如果路径已经缓存：

```text
/usr/bin/bash 的 dentry 命中
bash 的 inode 命中
```

就会很快。

如果没命中，就要问底层文件系统：

```text
ext4：去读磁盘目录块
virtiofs：发请求给 virtiofsd
overlayfs：查 upper/lower 层
nfs：可能走网络
```

## 2.5 stat 为什么容易测出 virtio-fs 问题？

`stat()` 也要路径解析：

```text
/usr/bin/bash
  ↓
查 / 的 dentry
查 usr 的 dentry
查 bin 的 dentry
查 bash 的 dentry
拿 inode 属性
返回 size/mode/mtime/uid/gid
```

对于 ext4/xfs/block rootfs：

```text
多数 dentry/inode 在 guest 内存里命中
```

对于 virtio-fs：

```text
guest 可能需要向 host 侧 virtiofsd 发 getattr/lookup 请求
```

测试命令：

```bash
find /usr/bin -type f | head -n 5000 > /tmp/files.txt

time while read f; do
  stat "$f" >/dev/null
done < /tmp/files.txt
```

## 2.6 read() 到底读哪里？

```c
fd = open("/tmp/a.txt", O_RDONLY);
read(fd, buf, 4096);
```

`read()` 不一定马上读磁盘。它先查 **page cache**。

```text
应用 read()
  ↓
VFS
  ↓
page cache 有没有这段文件内容？
  ↓ 是
直接从内存拷给应用
  ↓ 否
调用底层文件系统读数据
  ↓
读完放进 page cache
  ↓
拷给应用
```

第一次读：

```text
应用 → VFS → page cache miss → 文件系统 → 块设备/virtiofs → page cache → 应用
```

第二次读：

```text
应用 → VFS → page cache hit → 应用
```

## 2.7 write() 不是立刻落盘

普通写入：

```c
write(fd, buf, 4096);
```

通常是：

```text
应用 write()
  ↓
拷贝到 page cache
  ↓
标记 dirty page
  ↓
write() 返回
  ↓
后台 writeback 线程慢慢刷盘
```

除非使用：

```text
O_SYNC
O_DIRECT
fsync()
fdatasync()
sync
```

对比命令：

```bash
# 可能主要写 page cache
dd if=/dev/zero of=/tmp/a bs=1M count=1024

# 尽量绕过 page cache
dd if=/dev/zero of=/tmp/a bs=1M count=1024 oflag=direct

# 写完强制刷盘
dd if=/dev/zero of=/tmp/a bs=1M count=1024 conv=fsync
```

## 2.8 execve() 为什么放大文件系统问题？

`execve("/bin/sh", ...)` 不只是“运行一个文件”。

它大概会做：

```text
1. 路径查找 /bin/sh
2. 权限检查
3. 读取 ELF header
4. 判断解释器，比如 /lib64/ld-linux-aarch64.so.1
5. 加载动态链接器
6. 动态链接器读取共享库
7. 读取 /etc/ld.so.cache
8. mmap libc/libpthread/libdl 等 so
9. 建立新进程地址空间
10. 跳到入口执行
```

一次 exec 可能触发很多文件访问：

```text
/bin/sh
/lib/ld-linux*.so
/lib/libc.so
/etc/ld.so.cache
/usr/lib/...
```

如果是 shell 脚本，还会多一步：

```text
脚本第一行 #!/bin/sh
  ↓
内核再去 open /bin/sh
```

UnixBench 的：

```text
Execl Throughput
Shell Scripts (1 concurrent)
Shell Scripts (8 concurrent)
Process Creation
```

会疯狂打：

```text
路径查找
open
read
mmap
close
fork
exec
```

这就是 virtio-fs rootfs 容易在这些项上崩的原因。

## 2.9 VFS 在 virtio-fs 和块设备里的位置

### 块设备 rootfs

```text
容器进程
  ↓ open/stat/read/exec
guest VFS
  ↓ ext4/xfs/overlayfs
guest page cache / dentry cache / inode cache
  ↓
guest block layer
  ↓ virtio-blk/virtio-scsi
host
```

关键点：

```text
VFS、dentry cache、inode cache、page cache 都主要在 guest 内部工作。
```

### virtio-fs rootfs

```text
容器进程
  ↓ open/stat/read/exec
guest VFS
  ↓ virtiofs
guest cache
  ↓
virtiofs request
  ↓
host virtiofsd
  ↓
host VFS
  ↓
host overlayfs/ext4/xfs
```

关键点：

```text
guest VFS → 跨 VM 请求 → host 用户态 → host VFS
```

这条链路比块设备长。

---

# 3. page cache / dentry cache / inode cache

## 3.1 一句话区分

```text
dentry cache：缓存“路径名 → inode”的关系
inode cache：缓存“文件元数据”
page cache：缓存“文件内容”
```

以 `/usr/bin/bash` 为例：

```text
路径字符串：/usr/bin/bash

dentry cache：
  "/"       这个名字
  "usr"     这个名字
  "bin"     这个名字
  "bash"    这个名字

inode cache：
  bash 这个文件的权限、大小、uid/gid、mtime、文件类型等

page cache：
  bash 文件真正的数据内容，比如 ELF header、代码段、数据段
```

总结：

```text
open/stat/exec 主要吃 dentry cache + inode cache
read/write/mmap 主要吃 page cache
```

## 3.2 dentry cache：路径名缓存

内核不是直接认识 `/usr/bin/bash` 这个字符串，它要一步一步查：

```text
/
 └── usr
      └── bin
           └── bash
```

每一层都要 lookup：

```text
在 / 下面找 usr
在 /usr 下面找 bin
在 /usr/bin 下面找 bash
```

这些“名字到 inode 的映射”会被缓存到 dentry cache。

### dentry cache 命中

```text
open("/usr/bin/bash")
  ↓
VFS 查 dentry cache
  ↓
/、usr、bin、bash 都命中
  ↓
很快找到 inode
```

### dentry cache 未命中

```text
open("/usr/bin/bash")
  ↓
VFS 查 dentry cache
  ↓
bash 没命中
  ↓
调用底层文件系统 lookup
```

底层不同，代价差很多：

```text
ext4/xfs：读目录块，查目录项
overlayfs：查 upper 层、lower 层
virtio-fs：发 lookup/getattr 请求给 host virtiofsd
nfs：可能发网络请求
```

重点：

```text
dentry cache miss 对 virtio-fs 特别贵。
```

## 3.3 inode cache：文件元数据缓存

inode 保存文件本体元数据：

```text
文件类型：普通文件 / 目录 / symlink
权限：rwx
uid/gid
文件大小
atime/mtime/ctime
link count
数据块位置
文件操作函数
```

`stat /usr/bin/bash` 看到的这些信息基本来自 inode。

如果 inode 已经在内存里：

```text
stat("/usr/bin/bash")
  ↓
路径查找拿到 dentry
  ↓
dentry 指向 inode
  ↓
inode 在缓存里
  ↓
直接返回 size/mode/mtime
```

如果 inode 不在缓存里：

```text
需要底层文件系统读取 inode 信息
```

对块设备 rootfs：

```text
guest 内核从 ext4/xfs 读取 inode
读完缓存到 guest inode cache
```

对 virtio-fs：

```text
guest 可能要问 virtiofsd：
  这个文件大小多少？
  权限是什么？
  mtime 是多少？
  是否还有效？
```

## 3.4 metadata-heavy 是什么意思？

metadata-heavy 指文件内容读写不一定多，但文件元数据操作很多。

典型操作：

```bash
stat file
ls -l
find .
chmod
chown
touch
open/close 小文件
readdir
rename
unlink
mkdir/rmdir
```

典型场景：

```text
npm install
pip install
maven 构建
解压 tar.gz
编译源码
git checkout
shell 脚本
UnixBench execl/process/filecopy
```

这些场景大量时间不是花在大块数据读写，而是花在：

```text
路径查找
权限检查
inode 获取
目录项更新
小文件 open/close
```

这正是 virtio-fs 容易弱的地方。

## 3.5 page cache：文件内容缓存

page cache 缓存文件内容。

第一次读：

```text
page cache 没有 bash 内容
  ↓
底层文件系统读取
  ↓
读到内存 page cache
  ↓
拷贝给应用
```

第二次读：

```text
page cache 已经有 bash 内容
  ↓
直接从内存返回
```

read 简化路径：

```text
应用 read()
  ↓
系统调用进入内核
  ↓
VFS
  ↓
查 page cache
  ↓
命中：直接返回
  ↓
未命中：调用底层文件系统读
```

对于块设备 rootfs：

```text
page cache miss
  ↓
guest ext4/xfs
  ↓
guest block layer
  ↓
virtio-blk/virtio-scsi
  ↓
host block backend
```

对于 virtio-fs：

```text
page cache miss
  ↓
guest virtiofs
  ↓
virtiofsd
  ↓
host VFS
  ↓
host 文件系统
```

## 3.6 三个缓存放一起

以 `cat /usr/bin/bash > /dev/null` 为例：

```text
1. 解析路径 /usr/bin/bash
   ↓
   dentry cache

2. 找到 bash 文件元数据
   ↓
   inode cache

3. 打开文件
   ↓
   struct file

4. 读取文件内容
   ↓
   page cache

5. page cache 未命中时
   ↓
   底层文件系统读取
```

## 3.7 drop_caches

```bash
sync
echo 3 > /proc/sys/vm/drop_caches
```

含义：

```text
sync：先把脏数据刷出去

echo 3：释放 page cache、dentry cache、inode cache
```

常见值：

```text
echo 1 > drop_caches：释放 page cache
echo 2 > drop_caches：释放 dentry cache 和 inode cache
echo 3 > drop_caches：都释放
```

注意：

```text
drop_caches 不是清理应用内存
不是释放所有内存
不是线上调优手段
主要用于测试环境制造冷缓存状态
```

严谨测试 virtio-fs 时要区分：

```text
guest drop cache
host drop cache
both drop cache
no drop cache
```

因为 virtio-fs 场景可能有两边缓存：

```text
guest:
  dentry cache
  inode cache
  page cache

host:
  dentry cache
  inode cache
  page cache
```

路径可能是：

```text
guest app read
  ↓
guest page cache miss
  ↓
virtiofsd
  ↓
host page cache hit
  ↓
返回 guest
```

---

# 4. FUSE 基本原理

## 4.1 FUSE 是什么？

一句话：

```text
FUSE 是“内核负责接住文件系统请求，用户态进程负责真正处理文件操作”的机制。
```

正常 ext4/xfs：

```text
应用
  ↓ open/read/write/stat
VFS
  ↓
ext4/xfs 内核文件系统代码
  ↓
块设备
```

FUSE：

```text
应用程序
  ↓ open/stat/read/write
Linux VFS
  ↓
FUSE kernel driver
  ↓ /dev/fuse
FUSE userspace daemon
  ↓
真实后端：本地目录 / 网络 / 对象存储 / 加密文件 / 自定义逻辑
```

典型 FUSE 文件系统：

```text
sshfs：把远端 SSH 目录挂成本地目录
s3fs：把对象存储挂成本地目录
ntfs-3g：早期常见 NTFS 用户态实现
gocryptfs：加密文件系统
一些分布式文件系统客户端
```

## 4.2 一次 stat 在 FUSE 里怎么走？

```bash
stat /mnt/fuse/a.txt
```

路径：

```text
应用 stat()
  ↓
syscall 进入内核
  ↓
VFS 路径解析
  ↓
发现 /mnt/fuse 是 FUSE 文件系统
  ↓
FUSE kernel driver 生成 getattr 请求
  ↓
请求写到 /dev/fuse
  ↓
用户态 daemon 读到请求
  ↓
daemon 查询真实后端
  ↓
daemon 返回 size/mode/mtime
  ↓
FUSE kernel driver
  ↓
VFS
  ↓
应用拿到 stat 结果
```

一个普通 `stat()` 会多一次：

```text
内核态 → 用户态 daemon → 内核态
```

这是 FUSE 的典型开销来源。

## 4.3 一次 read 在 FUSE 里怎么走？

```bash
cat /mnt/fuse/a.txt
```

路径：

```text
应用 read()
  ↓
VFS
  ↓
先查 page cache
  ↓
命中：直接返回
  ↓
未命中：FUSE kernel driver 生成 read 请求
  ↓
/dev/fuse
  ↓
userspace daemon
  ↓
daemon 从真实后端读数据
  ↓
数据返回 FUSE kernel driver
  ↓
放入 page cache
  ↓
返回给应用
```

read 有两种情况：

```text
page cache 命中：不一定进入 daemon
page cache 未命中：进入 daemon，路径变长
```

## 4.4 FUSE 核心请求类型

```text
LOOKUP      查路径名
GETATTR     查属性，类似 stat
OPEN        打开文件
READ        读文件内容
WRITE       写文件内容
READDIR     读目录
CREATE      创建文件
MKDIR       创建目录
UNLINK      删除文件
RENAME      重命名
SETATTR     修改属性，比如 chmod/truncate
FLUSH       flush
FSYNC       强制同步
RELEASE     close 后释放
```

用户操作与 FUSE 请求：

| 用户操作 | 可能触发的 FUSE 请求 |
|---|---|
| `stat file` | LOOKUP / GETATTR |
| `ls dir` | LOOKUP / READDIR / GETATTR |
| `cat file` | LOOKUP / OPEN / READ |
| `echo x > file` | LOOKUP / CREATE / OPEN / WRITE / FLUSH |
| `rm file` | LOOKUP / UNLINK |
| `mv a b` | LOOKUP / RENAME |
| `chmod 755 file` | LOOKUP / SETATTR |
| `exec /bin/sh` | LOOKUP / OPEN / READ / mmap 相关 |

## 4.5 FUSE 为什么容易慢？

FUSE 慢通常不是因为“它一定读盘慢”，而是因为：

```text
路径长
上下文切换多
数据拷贝多
metadata 请求多
```

普通 ext4：

```text
应用
  ↓
内核 VFS
  ↓
ext4
  ↓
块设备
```

FUSE：

```text
应用
  ↓
内核 VFS
  ↓
FUSE driver
  ↓
用户态 daemon
  ↓
真实后端
```

多出来：

```text
内核和用户态之间的切换
daemon 调度
请求队列
数据拷贝
daemon 本身的处理逻辑
```

metadata-heavy 会被放大：

```text
find /mnt/fuse -type f
```

可能触发大量：

```text
LOOKUP
GETATTR
READDIR
```

如果有 10 万个小文件，就是 10 万次小请求叠加。

## 4.6 daemon 可能成为单点瓶颈

FUSE 真正处理请求的是用户态 daemon。如果 daemon：

```text
单线程
线程池太小
CPU 被绑到不合适的核
和 workload 抢同一个核
被 cgroup 限制
被调度延迟影响
```

整个文件系统都会慢。

这与 `virtiofsd --thread-pool-size=1`、`taskset -pc virtiofsd_pid` 直接相关。

## 4.7 FUSE 的缓存

FUSE 也能缓存：

```text
entry cache：路径名缓存，类似 dentry
attr cache：属性缓存，类似 inode metadata
page cache：文件内容缓存
```

缓存越激进，性能越好；缓存越保守，一致性越好。

如果真实后端文件被别人改了，但内核缓存还认为旧数据有效，就会出现一致性问题。

因此 FUSE 要决定：

```text
缓存多久？
每次 stat 要不要问 daemon？
read 要不要走 page cache？
write 是 write-through 还是 writeback？
```

这就是 virtio-fs 也会有 `cache=none/auto/metadata/always` 的原因。

## 4.8 FUSE 和 virtio-fs 的关系

传统 FUSE：

```text
guest/host 同一个 Linux 系统内：

应用
  ↓
VFS
  ↓
FUSE kernel driver
  ↓ /dev/fuse
userspace daemon
```

virtio-fs：

```text
guest VM 内：

应用
  ↓
guest VFS
  ↓
guest virtiofs/FUSE client
  ↓
virtio queue
  ↓
host virtiofsd
  ↓
host VFS
  ↓
host 文件系统
```

可以把 virtio-fs 理解为：

```text
FUSE 请求不再通过 /dev/fuse 发给本机 daemon，
而是通过 virtio queue 发给 host 上的 virtiofsd。
```

---

# 5. virtio 基本模型：driver、device、virtqueue

## 5.1 virtio 是什么？

一句话：

```text
virtio 是虚拟化里的“半虚拟化设备标准”：guest 里跑 virtio driver，host/QEMU 里提供 virtio device，双方通过 virtqueue 交换请求。
```

不用 virtio 时，虚拟机 I/O 可以模拟真实硬件：

```text
模拟 e1000 网卡
模拟 AHCI/SATA 控制器
模拟 IDE 磁盘
模拟 USB 控制器
```

兼容性好，但慢，因为 QEMU 要模拟很多真实硬件细节：

```text
寄存器行为
中断行为
DMA 行为
设备状态机
历史兼容逻辑
```

virtio 的思路：

```text
guest 不再使用“真实硬件驱动”
而是使用专门为虚拟化设计的 virtio 驱动
host 提供对应的 virtio 虚拟设备
双方用简单、高效的队列通信
```

## 5.2 三大角色

```text
driver
device
virtqueue
```

### driver：guest 里的驱动

例如：

```text
virtio_blk
virtio_scsi
virtio_net
virtiofs
```

负责把 guest 内核请求转换成 virtio 请求。

### device：host/QEMU 侧虚拟设备

可能在：

```text
QEMU 用户态进程里
host kernel vhost 里
host 用户态 vhost-user backend 里
```

例如：

```text
virtio-blk-pci
virtio-scsi-pci
virtio-net-pci
vhost-user-fs-pci
```

### virtqueue：共享队列

```text
virtqueue = guest driver 和 host device 共享的一组环形队列
```

流程：

```text
guest 把请求描述符放进 virtqueue
guest 通知 host
host 从 virtqueue 取请求
host 处理请求
host 把结果放回 virtqueue
host 通知 guest
```

图示：

```text
guest driver                         host device
    |                                      |
    |  放请求到 virtqueue                  |
    | -----------------------------------> |
    |                                      | 处理 I/O
    |  从 virtqueue 收完成结果              |
    | <----------------------------------- |
```

## 5.3 virtio-blk 读 4K 路径

```text
应用 read()
  ↓
guest VFS
  ↓
guest ext4/xfs
  ↓
guest block layer
  ↓
virtio-blk driver
  ↓
把请求放进 virtqueue
  ↓
通知 QEMU/host device
  ↓
host 读取后端文件/块设备
  ↓
把结果写回 guest 内存
  ↓
更新 virtqueue used ring
  ↓
中断/通知 guest
  ↓
guest block layer 完成 bio
  ↓
应用拿到数据
```

virtio 的关键不是“没有开销”，而是避免模拟复杂真实硬件，用共享内存队列传请求。

## 5.4 virtqueue 里放什么？

virtqueue 放 descriptor。

```text
descriptor = 一段 guest 内存的地址 + 长度 + 方向
```

virtio-blk 读请求可能有三段：

```text
1. 请求头：读还是写？sector 是多少？
2. 数据 buffer：读出来的数据放到哪里？
3. 状态字节：成功还是失败？
```

## 5.5 split virtqueue

经典 virtqueue 由三部分组成：

```text
Descriptor Table
Available Ring
Used Ring
```

流程：

```text
guest driver
  ↓ 填 descriptor
Descriptor Table
  ↓ 把 descriptor id 放入
Available Ring
  ↓ host device 取走处理
Used Ring
  ↓ host device 放完成项
guest driver 收完成
```

packed virtqueue 是优化版，把可用和已用信息压到同一个 ring 结构里，减少内存访问和缓存开销。

## 5.6 notification

virtqueue 是共享内存。请求放进去后还要通知对方。

```text
guest：我放了新请求，你来处理
host：我处理完了，你来看结果
```

guest 通知 host：

```text
写某个 MMIO/PCI 配置区域
触发 eventfd
QEMU/host 收到通知
```

host 通知 guest：

```text
注入虚拟中断
MSI-X interrupt
virtio interrupt
```

通知和中断本身有开销，所以需要：

```text
批量提交请求
批量完成请求
减少通知
中断合并
多队列
vhost
polling
iothread
```

fio 里的 `iodepth`、`numjobs` 会影响 virtqueue 压力。

## 5.7 QEMU、vhost、vhost-user

普通 virtio：

```text
guest virtio driver
  ↓
virtqueue
  ↓
QEMU 里的 virtio device emulation
  ↓
host backend
```

vhost：把 virtqueue 数据面处理从 QEMU 挪到更高效的位置。

```text
vhost-kernel：数据面在 host kernel
vhost-user：数据面在 host 用户态独立进程
```

virtio-fs 是典型 vhost-user：

```text
guest virtiofs driver
  ↓
virtqueue
  ↓
vhost-user-fs-pci
  ↓
host virtiofsd
  ↓
host shared-dir
```

## 5.8 virtio-fs 和 virtio-blk 的 payload 不同

```text
virtio = 运输通道
virtqueue = 运输队列
payload = 运的货
```

不同设备运的货不同：

```text
virtio-blk 运的是：块 I/O 请求
virtio-scsi 运的是：SCSI 命令
virtio-fs 运的是：FUSE 文件系统请求
virtio-net 运的是：网络包
```

不能只说“都是 virtio，所以性能差不多”。

真正差异来自：

```text
上层语义不同
请求粒度不同
缓存位置不同
host 后端不同
线程模型不同
一致性要求不同
```

---

# 6. virtio-fs 完整路径：guest driver + virtiofsd + vhost-user

## 6.1 整体结构

```text
容器进程
  ↓ open/stat/read/write/execve
guest VFS
  ↓
guest virtiofs 文件系统驱动
  ↓
FUSE 请求：LOOKUP / GETATTR / OPEN / READ / WRITE / READDIR
  ↓
virtqueue
  ↓
vhost-user-fs 设备
  ↓
host virtiofsd
  ↓
host VFS
  ↓
host shared-dir / overlayfs / ext4 / xfs / nvme
```

拆成三段：

```text
guest 内部：应用 → VFS → virtiofs driver
跨 VM 通道：virtqueue / vhost-user
host 侧：virtiofsd → host VFS → shared-dir
```

## 6.2 virtiofsd 是什么？

```text
virtiofsd = host 侧的 FUSE server / 文件系统 daemon
```

它负责：

```text
接收 guest 发来的文件系统请求
在 host 的 shared-dir 上执行真实文件操作
把结果返回给 guest
```

例如 guest 里：

```bash
stat /bin/bash
```

如果 `/bin/bash` 在 virtio-fs rootfs 上，host 侧 virtiofsd 可能处理：

```text
LOOKUP /bin
LOOKUP /bin/bash
GETATTR /bin/bash
```

## 6.3 vhost-user 是什么？

普通 virtio 可能是：

```text
guest driver
  ↓
virtqueue
  ↓
QEMU 里的 virtio device
```

virtio-fs 通常是：

```text
guest virtiofs driver
  ↓
virtqueue
  ↓
QEMU vhost-user-fs-pci 设备壳子
  ↓
host virtiofsd 处理数据面
```

QEMU 主要负责：

```text
创建 VM
暴露 virtio-fs PCI 设备
协商 virtqueue
连接 virtiofsd
```

真正文件请求处理主要在 `virtiofsd`。

## 6.4 一次 stat 怎么走？

```text
1. 应用调用 stat()
2. 进入 guest kernel
3. guest VFS 解析路径
4. 发现路径位于 virtiofs mount 上
5. virtiofs driver 生成 FUSE LOOKUP / GETATTR 请求
6. 请求放进 virtqueue
7. 通知 host 侧 virtiofsd
8. virtiofsd 在 host shared-dir 上执行 lookup/stat
9. host VFS 返回文件属性
10. virtiofsd 把 size/mode/mtime/uid/gid 等返回 guest
11. guest VFS 返回给应用
```

图：

```text
stat("/usr/bin/bash")
        ↓
guest VFS
        ↓
virtiofs driver
        ↓
FUSE LOOKUP / GETATTR
        ↓
virtqueue
        ↓
virtiofsd
        ↓
host VFS
        ↓
host shared-dir
```

重点：

```text
stat 看起来很轻，但在 virtio-fs 下可能需要跨 VM 找 host。
```

## 6.5 一次 open/read 怎么走？

```bash
cat /usr/bin/bash >/dev/null
```

路径：

```text
1. open("/usr/bin/bash")
   ↓
   LOOKUP / OPEN

2. read(fd)
   ↓
   先查 guest page cache

3. guest page cache 命中
   ↓
   直接返回应用

4. guest page cache 未命中
   ↓
   生成 FUSE READ 请求
   ↓
   通过 virtqueue 发给 virtiofsd

5. virtiofsd 在 host 上 read 文件
   ↓
   host 可能命中 host page cache

6. 数据返回 guest
   ↓
   guest 放入 page cache
   ↓
   返回应用
```

两层缓存：

```text
guest page cache
host page cache
```

## 6.6 一次 write 怎么走？

```bash
echo hello > /tmp/a.txt
```

如果 `/tmp` 在 virtio-fs 上，可能触发：

```text
LOOKUP /tmp
CREATE a.txt
OPEN a.txt
WRITE "hello"
FLUSH
RELEASE
可能还有 SETATTR / FSYNC
```

路径：

```text
应用 write()
  ↓
guest VFS
  ↓
virtiofs driver
  ↓
FUSE WRITE 请求
  ↓
virtqueue
  ↓
virtiofsd
  ↓
host write()
  ↓
host page cache / host filesystem
```

如果应用调用 `fsync(fd)`：

```text
guest FSYNC
  ↓
virtiofsd fsync
  ↓
host fsync
  ↓
底层存储刷盘
```

## 6.7 一次 execve 怎么走？

```bash
/bin/sh -c true
```

底层可能有：

```text
LOOKUP /bin/sh
GETATTR /bin/sh
OPEN /bin/sh
READ ELF header
LOOKUP /lib64/ld-linux-xxx.so
OPEN /lib64/ld-linux-xxx.so
READ / mmap 动态链接器
LOOKUP /etc/ld.so.cache
OPEN /etc/ld.so.cache
LOOKUP libc.so
OPEN libc.so
mmap libc.so
```

如果都在 virtio-fs rootfs 上，就会变成大量：

```text
guest VFS
  ↓
virtiofs FUSE request
  ↓
virtqueue
  ↓
virtiofsd
  ↓
host VFS
```

`Execl Throughput` 差不一定是 fork 慢，而是 exec 过程中的文件访问被放大。

## 6.8 virtio-fs cache 模式深入

virtio-fs 的缓存控制，本质是在回答：

```text
guest 能不能相信自己的缓存？
```

缓存包括：

```text
entry cache：文件名到 inode/nodeid 的映射
attr cache：文件属性，比如 size/mode/mtime
page cache：文件内容
```

### cache=none

```text
尽量少信任 guest 缓存
更多请求回 host 确认
一致性强
性能差
metadata 请求多
```

适合 host/guest 都可能修改共享目录、多方共享写。

### cache=auto

```text
折中缓存
缓存一段时间
过期后重新确认
性能中等
一致性中等
```

适合通用场景。

### cache=metadata

```text
更积极缓存文件元数据
stat/open/readdir 可能改善
metadata-heavy workload 可能更好
文件内容缓存仍相对谨慎
```

适合大量 stat/open 且文件内容变化不频繁。

### cache=always

```text
更信任 guest 缓存
尽量减少回 host 确认
性能最好
一致性风险最大
```

适合只读 rootfs、只读依赖目录、只读模型目录、host 不会在运行期间修改文件。

## 6.9 DAX 在 virtio-fs 中的位置

普通 virtio-fs read：

```text
guest read
  ↓
guest page cache miss
  ↓
virtiofsd read
  ↓
host page cache
  ↓
数据拷贝回 guest
```

DAX 思路：

```text
把 host 侧文件内容映射到 guest 可访问的内存窗口
guest 通过 mmap/page fault 更直接访问
```

可能改善：

```text
大文件读
mmap
只读文件访问
动态库映射
模型文件映射
```

但不一定解决：

```text
LOOKUP
GETATTR
READDIR
OPEN/CLOSE metadata
```

对 UnixBench：

```text
Execl Throughput：DAX 可能帮助动态库 mmap，但 metadata/open/getattr 仍在
File Copy：大量 create/write/metadata，DAX 帮助有限
Shell Scripts：大量 exec/open/stat，DAX 不是万能药
```

## 6.10 thread-pool-size 的影响

如果 virtiofsd 只有 1 个 worker：

```text
guest 多个 vCPU 并发请求
  ↓
virtiofsd 单线程排队
  ↓
延迟增加
  ↓
vCPU 等 I/O
  ↓
UnixBench 分数崩
```

对这些指标影响尤其大：

```text
Shell Scripts (8 concurrent)
File Copy
Execl Throughput
Process Creation
```

建议测试：

```text
thread-pool-size = 1
thread-pool-size = 2
thread-pool-size = 4
thread-pool-size = 8
thread-pool-size = vCPU 数
```

观察：

```text
execl
filecopy
shell scripts
virtiofsd CPU 使用率
上下文切换
run queue
```

线程池不是越大越好，太大可能导致上下文切换、锁竞争、NUMA 跨节点访问。

## 6.11 queue-size / num-queues

```text
queue-size：一个 virtqueue 能挂多少请求
num-queues：有多少个请求队列
```

队列太小：

```text
guest 并发请求很多
  ↓
virtqueue 容量不足
  ↓
请求排队
  ↓
延迟变高
```

队列更多：

```text
多个 vCPU / 多线程 workload
  ↓
可以分散到多个队列
  ↓
减少竞争
```

但需要 host 侧 virtiofsd 能并发处理，否则 guest 侧队列更多，host 侧还是慢。

## 6.12 NUMA 和绑核

如果：

```text
guest vCPU 绑在 NUMA node0
virtiofsd 绑在 NUMA node2
shared-dir 底层 NVMe 中断在 node1
```

每次文件请求都可能跨 NUMA。

合理思路：

```text
guest vCPU
QEMU emulator/iothread
virtiofsd
底层存储中断
```

尽量在同一个 NUMA node 或至少不要乱跑。

检查：

```bash
numactl --hardware
lscpu -e=CPU,NODE,SOCKET,CORE,ONLINE
ps -T -p <qemu_pid> -o pid,tid,psr,comm
ps -T -p <virtiofsd_pid> -o pid,tid,psr,comm
taskset -pc <qemu_pid>
taskset -pc <virtiofsd_pid>
```

## 6.13 overlayfs + virtio-fs

Kata rootfs 很多时候不是单纯 host 目录，而是：

```text
container image layer
  ↓
overlayfs merged dir
  ↓
virtio-fs shared-dir
  ↓
guest rootfs
```

路径：

```text
guest VFS
  ↓
virtiofs
  ↓
virtiofsd
  ↓
host VFS
  ↓
host overlayfs
  ↓
lower/upper/work dirs
  ↓
host ext4/xfs/block
```

overlayfs 自身也有：

```text
upper layer
lower layer
copy-up
whiteout
lookup upper/lower
```

写 lower 层已有文件可能触发 copy-up：

```text
修改 lower 层已有文件
  ↓
overlayfs copy-up 到 upper
  ↓
再写 upper
```

因此 virtio-fs + overlayfs 对小文件、metadata、写入更不友好。

---

# 7. virtio-blk / virtio-scsi：块设备路径

## 7.1 块设备路径

以 block rootfs 访问 `/usr/bin/bash`：

```text
容器进程
  ↓ open/stat/read/execve
guest VFS
  ↓
guest ext4 / xfs / overlayfs
  ↓
guest dentry cache / inode cache / page cache
  ↓
guest block layer
  ↓
virtio-blk 或 virtio-scsi driver
  ↓
virtqueue
  ↓
QEMU / vhost / host backend
  ↓
host image / LV / raw block device / NVMe
```

重点：

```text
open/stat/exec 这些文件语义，大部分在 guest 内核里解决。
```

只有需要真正读写磁盘块时，才跨 VM 进入 virtio 设备。

## 7.2 与 virtio-fs 对比

virtio-fs rootfs：

```text
open("/usr/bin/bash")
  ↓
guest VFS
  ↓
virtiofs
  ↓
FUSE LOOKUP / GETATTR / OPEN
  ↓
virtqueue
  ↓
virtiofsd
  ↓
host VFS
```

block rootfs：

```text
open("/usr/bin/bash")
  ↓
guest VFS
  ↓
guest ext4/xfs
  ↓
guest dentry/inode cache
```

只有 page cache miss 时：

```text
read file content
  ↓
guest block layer
  ↓
virtio-blk / virtio-scsi
  ↓
host backend
```

本质差异：

```text
virtio-fs：文件系统语义跨 VM
块设备：文件系统语义留在 guest，块 I/O 跨 VM
```

## 7.3 guest 块设备层次

```text
应用
  ↓
VFS
  ↓
文件系统 ext4/xfs
  ↓
page cache
  ↓
block layer
  ↓
I/O scheduler
  ↓
virtio-blk / virtio-scsi
  ↓
virtqueue
```

查看：

```bash
lsblk -f
findmnt -T /
cat /proc/mounts
```

常见：

```text
virtio-blk：/dev/vda、/dev/vdb
virtio-scsi：/dev/sda、/dev/sdb
```

不绝对，但经常如此。

## 7.4 virtio-blk

路径：

```text
guest block layer
  ↓
virtio-blk driver
  ↓
virtqueue
  ↓
QEMU virtio-blk device
  ↓
host backend
```

请求简单：

```text
读/写
sector
数据 buffer
status
```

不关心：

```text
文件名
目录
inode
权限
路径查找
```

优点：

```text
路径短
模型简单
单盘场景性能好
配置简单
开销相对低
```

缺点：

```text
传统上扩展多盘能力不如 virtio-scsi
SCSI 命令语义少
高级存储能力不如 virtio-scsi 方便
老版本多队列/iothread 能力可能有限
```

## 7.5 virtio-scsi

路径：

```text
guest block layer
  ↓
guest SCSI layer
  ↓
virtio-scsi driver
  ↓
virtqueue
  ↓
QEMU virtio-scsi device
  ↓
host backend
```

优点：

```text
适合挂很多盘
更接近标准 SCSI 设备模型
支持更多 SCSI 命令语义
discard/unmap 等能力更容易表达
适合复杂存储后端
```

缺点：

```text
路径比 virtio-blk 稍复杂
单盘极限性能未必一定更好
配置项更多
调优时要考虑 controller、target、LUN、queue、iothread
```

经验：

```text
少量盘，追求简单直接：virtio-blk
多盘、复杂存储、需要 SCSI 语义：virtio-scsi
```

## 7.6 read/open/stat/write 路径

### read page cache 命中

```text
应用 read()
  ↓
guest VFS
  ↓
guest page cache 命中
  ↓
直接返回
```

不进入 virtio-blk/virtio-scsi。

### read page cache 未命中

```text
应用 read()
  ↓
guest VFS
  ↓
guest ext4/xfs
  ↓
page cache miss
  ↓
guest block layer
  ↓
virtio-blk / virtio-scsi
  ↓
virtqueue
  ↓
QEMU / host backend
  ↓
host 存储
  ↓
数据返回 guest
  ↓
填充 guest page cache
  ↓
返回应用
```

### stat/open

热缓存：

```text
guest dentry cache 命中
guest inode cache 命中
  ↓
直接返回
```

冷缓存：

```text
guest ext4/xfs 需要读取目录项和 inode
  ↓
guest block layer
  ↓
virtio block request
```

即使缓存冷，跨 VM 的也是读目录块/inode table block，不是 LOOKUP/GETATTR。

### write

```text
应用 write()
  ↓
guest VFS
  ↓
guest ext4/xfs
  ↓
写入 guest page cache
  ↓
标记 dirty page
  ↓
write() 返回
  ↓
后台 writeback
  ↓
guest block layer
  ↓
virtio-blk / virtio-scsi
  ↓
host backend
```

如果调用 fsync/O_SYNC/sync，才强制往下刷。

## 7.7 块设备后端类型

可能是：

```text
raw file
qcow2 file
LVM LV
devicemapper thin device
直接 block device
NVMe namespace
```

### raw file

```text
host 文件系统上的普通文件
```

简单、开销较低，但仍经过 host 文件系统和 host page cache。

### qcow2

支持快照、压缩、稀疏，但元数据复杂，随机写可能额外开销，通常不如 raw/LV。

### LVM LV / devicemapper

host 上的逻辑块设备。

优点：

```text
更接近裸块设备
适合快照/thin provisioning
绕开普通文件路径
性能通常更稳
```

缺点：

```text
管理复杂
需要存储池
排障门槛高
```

Kata block rootfs 很多时候与 devicemapper snapshotter、LVM、thin pool 相关。

---

# 8. QEMU block cache / aio / IOThread / multiqueue 调优

## 8.1 主要调什么？

块设备链路：

```text
guest block layer
  ↓
virtio-blk / virtio-scsi
  ↓
virtqueue
  ↓
QEMU block backend
  ↓
host page cache / host filesystem / raw block device / NVMe
```

需要重点理解：

```text
cache 模式：QEMU 怎么使用 host page cache
aio 模式：QEMU 怎么提交异步 I/O
IOThread：谁来处理 QEMU 里的 I/O 事件
multiqueue：guest 和 host 能不能并行处理多个 I/O 队列
```

## 8.2 QEMU cache 模式

`cache=` 控制 QEMU 访问 host 后端存储时怎么使用 host page cache。

常见：

```text
cache=writeback
cache=none
cache=writethrough
cache=directsync
cache=unsafe
```

### cache=writeback

路径：

```text
guest write()
  ↓
guest page cache
  ↓
virtio block
  ↓
QEMU
  ↓
host page cache
  ↓
write() 返回完成
  ↓
host 后台再刷盘
```

优点：

```text
写入延迟低
可以利用 host page cache
普通 workload 性能不错
```

缺点：

```text
guest page cache + host page cache 双缓存
host crash 时，未落盘数据有风险
测试结果容易被 host cache 影响
```

### cache=none

核心：

```text
QEMU 用 O_DIRECT 打开后端，尽量绕过 host page cache。
```

路径：

```text
guest write/read
  ↓
guest page cache
  ↓
virtio block
  ↓
QEMU O_DIRECT
  ↓
host filesystem / block device
```

优点：

```text
减少双缓存
更适合数据库/高 I/O
更接近真实块设备表现
更适合 fio direct I/O 测试
```

缺点：

```text
小 I/O 对齐更敏感
无法利用 host page cache
某些文件后端/慢盘可能反而变差
```

### writethrough / directsync / unsafe

```text
writethrough：写入更保守，一致性更强，性能较差
directsync：O_DIRECT + 同步写，性能通常更差
unsafe：忽略或弱化 flush，快但 crash 风险大，生产慎用
```

## 8.3 cache 模式怎么选？

| 场景 | 建议 |
|---|---|
| 普通测试，看整体表现 | `cache=writeback` 和 `cache=none` 都测 |
| 高并发块 I/O | 优先看 `cache=none` |
| 数据库 | 常用 `cache=none` |
| 只关心启动和热缓存 | `cache=writeback` 可能更好看 |
| 强一致写 | `writethrough` / `directsync` |
| 性能极限但不管数据安全 | `unsafe`，仅实验 |
| Kata rootfs 性能排查 | 重点比较 `none` vs `writeback` |

建议记录：

```text
virtio-scsi + cache=none
virtio-scsi + cache=writeback
virtio-blk + cache=none
virtio-blk + cache=writeback
```

## 8.4 aio 模式

`aio` 控制 QEMU 怎么向 host 提交异步 I/O。

常见：

```text
aio=threads
aio=native
aio=io_uring
```

### aio=threads

```text
QEMU 收到 I/O
  ↓
丢给 worker thread
  ↓
worker thread 做 blocking read/write
  ↓
完成后通知 QEMU
```

优点：兼容性好、对后端要求低、配置简单。

缺点：线程上下文切换，高 IOPS 下开销可能大，线程池可能瓶颈。

### aio=native

通常指 Linux native AIO，常和 `cache=none` 搭配。

```text
QEMU 用 Linux native AIO 提交 direct I/O
```

优点：减少 worker thread blocking，适合 direct I/O，高并发块 I/O 可能更好。

缺点：对 O_DIRECT/对齐/后端支持敏感，配置不对可能回退。

### aio=io_uring

Linux 较新的异步 I/O 接口。

优点：高并发 I/O 潜力好，减少系统调用/上下文切换，更适合现代内核和 NVMe。

缺点：依赖内核和 QEMU 版本，兼容性要验证。

经验组合：

```text
cache=writeback + aio=threads
cache=none + aio=native
cache=none + aio=io_uring
```

## 8.5 IOThread

```text
IOThread = QEMU 里专门处理某些设备 I/O 的线程。
```

没有 IOThread：

```text
virtio block I/O
  ↓
QEMU 主线程/默认事件循环处理
```

有 IOThread：

```text
virtio block I/O
  ↓
指定 IOThread 处理
```

价值：

```text
避免 QEMU 主线程太忙
设备 I/O 互相隔离
多 vCPU 高 I/O 时减少单线程瓶颈
方便绑核
```

可以把 vCPU 绑一组核，把 IOThread 绑另一组核，减少抢核。

## 8.6 multiqueue

单队列：

```text
所有 vCPU / 所有进程 I/O
  ↓
一个 virtqueue
  ↓
一个处理路径
```

多队列：

```text
vCPU0 / job0 → queue0
vCPU1 / job1 → queue1
vCPU2 / job2 → queue2
vCPU3 / job3 → queue3
```

好处：

```text
减少队列锁竞争
减少跨 vCPU 唤醒
提升高并发 IOPS
更适合多核 guest
```

但不是越多越好：

```text
队列多了，管理开销也增加
host IOThread 不够，队列多也没用
底层盘不够快，队列多只会排队
CPU 亲和性乱，可能跨 NUMA 更差
```

建议从：

```text
num-queues=1
num-queues=2
num-queues=4
num-queues=8
```

开始测。

## 8.7 IOThread 和 multiqueue 的关系

只有 multiqueue：

```text
guest 有多个 virtqueue
  ↓
但 host 都由一个 IOThread 处理
  ↓
host 单线程仍可能瓶颈
```

只有多个 IOThread：

```text
host 有多个 IOThread
  ↓
但设备只有一个 virtqueue
  ↓
请求入口仍然单队列
```

理想：

```text
多个 virtqueue
  ↓
映射到多个 IOThread
  ↓
IOThread 分散绑核
  ↓
底层存储支持并发
```

## 8.8 fio 参数如何触发这些能力

```text
iodepth=1：每个 job 只挂一个 I/O，更像测延迟
iodepth=32：更容易打满 virtqueue / IOThread

numjobs=1：一个进程发 I/O
numjobs=4/8：多进程并发，更容易利用 multiqueue

bs=4k：IOPS 压力，队列/通知/CPU 开销明显
bs=1M：吞吐压力，底层带宽更关键

direct=0：guest page cache 参与
direct=1：尽量绕过 guest page cache，更接近块设备路径
```

## 8.9 现象判断

### QEMU 某线程 100%，host 磁盘不忙

可能：

```text
QEMU/IOThread 单线程瓶颈
virtqueue 处理瓶颈
aio=threads 开销
队列没有分散
```

尝试：

```text
启用 IOThread
增加 multiqueue
cache=none + aio=native/io_uring
绑核
```

### host 磁盘 %util 100%，await 高

可能：

```text
底层盘/存储打满
后端太慢
qcow2 元数据开销
thin pool 压力
```

尝试：

```text
换 raw/LV/block backend
用更快存储
减少 fsync
检查 host 其他 I/O
```

### guest fio 很快，UnixBench filecopy 仍差

可能：

```text
fio 测的是大块/direct I/O
UnixBench filecopy 是小 buffer + metadata
guest 文件系统/overlayfs/journal 是瓶颈
测试目录仍在 virtio-fs
```

检查：

```bash
findmnt -T /tmp
findmnt -T /root/byte-unixbench-6.0.1/UnixBench
```

---

# 9. fio / iostat / strace / perf 实战定位

## 9.1 定位原则

不要一上来就盲改 Kata/QEMU 配置。

流程：

```text
1. 先确认测试目录到底在哪个文件系统
2. 再判断慢的是 metadata、数据读写、fsync、还是 CPU/线程
3. 再决定是调 virtiofsd、cache、IOThread、multiqueue，还是换块设备
```

## 9.2 先确认路径

```bash
findmnt -T / -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /tmp -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /usr/bin/bash -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /root/byte-unixbench-6.0.1/UnixBench -o TARGET,SOURCE,FSTYPE,OPTIONS
```

看 `FSTYPE`：

```text
virtiofs：这个路径走 virtio-fs
ext4/xfs：大概率走块设备
overlay：还要看 overlay 的 lower/upper/work 在哪里
```

## 9.3 判断 workload 类型

| 类型 | 典型操作 | 常用工具 |
|---|---|---|
| metadata | `stat/open/readdir/ls/find` | `stat/find/strace/perf` |
| 小文件读写 | `open/read/write/close` 很频繁 | `fio` 小文件、`strace` |
| 大块吞吐 | 大文件顺序读写 | `fio/dd/iostat` |
| sync 写 | `fsync/fdatasync/O_SYNC` | `fio --fsync=1`、`strace` |

## 9.4 metadata 测试

```bash
find /usr/bin -type f | head -n 5000 > /tmp/files.txt

time while read f; do
  stat "$f" >/dev/null
done < /tmp/files.txt
```

冷缓存：

```bash
sync
echo 3 > /proc/sys/vm/drop_caches

time while read f; do stat "$f" >/dev/null; done < /tmp/files.txt
```

热缓存：

```bash
time while read f; do stat "$f" >/dev/null; done < /tmp/files.txt
```

解释：

```text
第一遍慢，第二遍快：dentry/inode cache 有效
两遍都慢：可能每次都穿透到底层，virtio-fs/cache 策略/一致性影响大
virtio-fs 比 block 慢很多：metadata/FUSE/virtiofsd 是核心瓶颈
```

## 9.5 open/read 小文件测试

```bash
time while read f; do
  head -c 4 "$f" >/dev/null 2>&1
done < /tmp/files.txt
```

触发：

```text
路径查找
open
read 少量内容
close
```

如果 virtio-fs 很慢，说明不是纯磁盘吞吐问题，而是 open/read/close 请求频率太高。

## 9.6 大文件顺序吞吐

```bash
dd if=/dev/zero of=/tmp/bigfile bs=1M count=1024 conv=fsync

sync
echo 3 > /proc/sys/vm/drop_caches

time dd if=/tmp/bigfile of=/dev/null bs=1M
```

或者：

```bash
fio --name=seqread \
  --directory=/tmp \
  --rw=read \
  --bs=1M \
  --size=1G \
  --numjobs=1 \
  --iodepth=16 \
  --runtime=60 \
  --time_based \
  --group_reporting
```

如果大文件顺序读差距不大，但 stat/open/exec 差距很大，说明主要是 metadata，不是纯吞吐。

## 9.7 direct I/O 测块设备真实能力

随机读：

```bash
fio --name=randread-direct \
  --directory=/tmp \
  --direct=1 \
  --rw=randread \
  --bs=4k \
  --size=1G \
  --numjobs=4 \
  --iodepth=32 \
  --runtime=60 \
  --time_based \
  --group_reporting
```

随机写：

```bash
fio --name=randwrite-direct \
  --directory=/tmp \
  --direct=1 \
  --rw=randwrite \
  --bs=4k \
  --size=1G \
  --numjobs=4 \
  --iodepth=32 \
  --runtime=60 \
  --time_based \
  --group_reporting
```

看：

```text
IOPS
平均延迟
P95/P99 延迟
带宽
```

## 9.8 fsync 测试

```bash
fio --name=fsync-write \
  --directory=/tmp \
  --rw=write \
  --bs=4k \
  --size=256M \
  --numjobs=1 \
  --iodepth=1 \
  --fsync=1 \
  --runtime=60 \
  --time_based \
  --group_reporting
```

如果差，重点查：

```text
guest 文件系统 journal
QEMU cache 模式
flush/fua 处理
host fsync
底层盘同步写能力
```

## 9.9 iostat 判断是否打到底层

```bash
iostat -x 1
```

guest 和 host 都跑。

重点：

```text
r/s, w/s：IOPS
rkB/s, wkB/s：吞吐
await：平均 I/O 等待时间
aqu-sz：平均队列长度
%util：设备忙碌程度
```

判断：

```text
guest await 高，host 磁盘也高：底层存储可能打满
guest await 高，host 磁盘不高：QEMU/virtqueue/IOThread/配置可能是瓶颈
guest 几乎没 I/O，但测试慢：可能卡在 metadata、CPU、锁、virtiofsd、page cache 或应用层
host 磁盘高，guest 不高：可能有其他 host workload 干扰
```

## 9.10 strace 看 syscall 类型

看一次 shell：

```bash
strace -f -e trace=execve,openat,newfstatat,read,mmap,close /bin/sh -c 'true'
```

统计：

```bash
strace -f -c /bin/sh -c 'for i in $(seq 1 100); do /bin/true; done'
```

重点看：

```text
openat
newfstatat
execve
mmap
read
close
fsync/fdatasync
futex
```

如果 `openat/newfstatat` 占比高：metadata/path lookup 是重点。

如果 `fsync/fdatasync` 占比高：同步写是重点。

如果大量 `futex`：可能是锁竞争/线程等待。

## 9.11 perf 看 CPU 时间

guest 看 execl：

```bash
perf record -g -- /bin/sh -c 'for i in $(seq 1 5000); do /bin/true; done'
perf report
```

可能方向：

```text
path lookup
vfs_open
do_execveat_common
load_elf_binary
mmap
page fault
fuse/virtiofs 函数
```

host 看 virtiofsd：

```bash
pid=$(pidof virtiofsd)
perf top -p $pid

perf record -g -p $(pidof virtiofsd) -- sleep 30
perf report
```

host 看 QEMU：

```bash
pid=<qemu_pid>
perf top -p $pid
top -H -p $pid
```

## 9.12 top/pidstat 看线程瓶颈

```bash
top -H -p <qemu_pid>
pidstat -t -p <qemu_pid> 1

top -H -p $(pidof virtiofsd)
pidstat -t -p $(pidof virtiofsd) 1
```

判断：

```text
virtiofsd 单线程 100%：thread-pool-size 或单请求路径瓶颈
QEMU 某 IOThread 100%：block 后端处理线程瓶颈
vCPU 线程 100%，virtiofsd/QEMU 不高：guest 内 CPU/系统调用/锁/benchmark 本身
都不高但慢：可能在等待 I/O、锁、cgroup、调度、NUMA
```

---

# 10. Kata 里的 rootfs / image layer / overlayfs / virtio-fs / block device

## 10.1 两个 rootfs

Kata 里容易混两个 rootfs：

```text
VM rootfs：
  guest 虚机自己的系统根文件系统
  里面跑 kata-agent、init、基础系统

Container rootfs：
  容器镜像展开后的根文件系统
  也就是容器进程看到的 /
```

图：

```text
Kata VM
├── VM rootfs
│   ├── /sbin/init
│   ├── kata-agent
│   └── guest 基础环境
│
└── container rootfs
    ├── /bin/sh
    ├── /usr/bin/...
    ├── /lib/...
    └── 你的容器文件系统
```

UnixBench 受影响的是 container rootfs，不是 VM rootfs。

## 10.2 Kubernetes 到 Kata 链路

```text
kubelet
  ↓
containerd
  ↓
snapshotter 准备镜像 rootfs
  ↓
containerd-shim-kata-v2
  ↓
kata-runtime
  ↓
QEMU / cloud-hypervisor / firecracker
  ↓
guest VM + kata-agent
  ↓
kata-agent 在 guest 里创建容器进程
```

存储关键点：

```text
snapshotter 准备好的 container rootfs
  ↓
Kata 怎么交给 guest VM？
```

常见两种：

```text
virtio-fs 共享目录
block device passthrough
```

## 10.3 普通容器 rootfs

不用 Kata，普通 runc 容器一般是：

```text
container image layers
  ↓
snapshotter
  ↓
overlayfs merged dir
  ↓
runc chroot/pivot_root
  ↓
容器进程看到 /
```

overlayfs 结构：

```text
lowerdir：镜像只读层
upperdir：容器可写层
workdir：overlayfs 工作目录
merged：最终给容器看的 rootfs
```

## 10.4 Kata 默认 virtio-fs rootfs

Kata 需要让 guest 访问 host 准备好的 container rootfs。

常见方式：

```text
host overlayfs merged dir
  ↓
virtio-fs shared-dir
  ↓
guest 里 mount
  ↓
kata-agent 把它作为 container rootfs
```

路径：

```text
host:
  image layers
    ↓
  overlayfs merged rootfs
    ↓
  /run/kata-containers/shared/sandboxes/<sid>/...

跨 VM:
  virtio-fs

guest:
  virtiofs mount
    ↓
  container rootfs
    ↓
  容器进程 /
```

优点：

```text
简单
和 containerd overlayfs snapshotter 集成自然
host 仍然以目录方式管理 rootfs
volume/configmap/secret 也容易共享
不用为每个容器准备块设备
```

问题：

```text
container rootfs 的文件操作跨 VM
```

例如：

```text
guest VFS
  ↓
virtiofs
  ↓
FUSE LOOKUP / GETATTR / OPEN / READ
  ↓
virtqueue
  ↓
virtiofsd
  ↓
host VFS
  ↓
host overlayfs
```

再叠加 host overlayfs：

```text
guest VFS
  → virtiofs/FUSE
  → host virtiofsd
  → host VFS
  → host overlayfs
  → host FS
```

## 10.5 block rootfs 模式

不用 host overlayfs merged 目录通过 virtio-fs 共享，而是把 container rootfs 放到块设备 snapshot 上：

```text
container image
  ↓
block-based snapshotter
  ↓
host block device snapshot
  ↓
virtio-scsi / virtio-blk hotplug to VM
  ↓
guest mount block device
  ↓
container rootfs
```

图：

```text
host:
  devicemapper thin snapshot / LV / block device
    ↓
  /dev/dm-X

跨 VM:
  virtio-scsi / virtio-blk

guest:
  /dev/sdX 或 /dev/vdX
    ↓
  mount ext4/xfs
    ↓
  container rootfs
```

## 10.6 devicemapper snapshotter

overlayfs snapshotter 产物是目录。

devicemapper snapshotter 产物更像块设备 snapshot。

```text
每个容器 rootfs 是一个 thin snapshot block device。
```

Kata 可以：

```text
发现 rootfs 是 block device
  ↓
把它作为磁盘热插进 guest
  ↓
guest 里 mount 成 container rootfs
```

## 10.7 block rootfs 为什么性能好？

容器里访问 `/bin/sh`：

```text
guest VFS
  ↓
guest ext4/xfs
  ↓
guest dentry/inode/page cache
  ↓
guest block layer
  ↓
virtio-scsi / virtio-blk
```

文件系统语义留在 guest。

```text
open/stat/exec：大量在 guest cache 里解决
真正跨 VM：只有 block read/write
```

所以 virtio-scsi rootfs 后：

```text
Execl Throughput 恢复
File Copy 恢复
Shell Scripts 恢复
```

是合理的。

## 10.8 block rootfs 的代价

```text
需要 block-based snapshotter
存储池管理复杂
thin pool 容量/元数据要维护
每个容器可能对应块设备
热插拔和 guest mount 流程更复杂
host 不像目录那样直接查看文件
volume/configmap/secret 仍可能需要其他共享机制
```

生产常见折中：

```text
rootfs 用 block
volume/configmap/secret 用 virtio-fs 或其他共享机制
```

## 10.9 container rootfs 和 volume 要分开看

至少有两类访问：

```text
1. rootfs 访问：/bin /usr /lib /etc
2. volume/workspace 访问：/data /workspace /tmp configmap secret
```

它们可以走不同路径：

```text
rootfs：block device
workspace：virtio-fs
/tmp：tmpfs
configmap/secret：virtio-fs 或 agent 注入
```

检查具体目录：

```bash
findmnt -T /
findmnt -T /tmp
findmnt -T /workspace
findmnt -T /bin/sh
findmnt -T /root/byte-unixbench-6.0.1/UnixBench
```

## 10.10 为什么 UnixBench 特别受 rootfs 影响？

UnixBench 不只是当前目录读写文件。

它会用到：

```text
/bin/sh
/usr/bin
/lib
/lib64
/etc/ld.so.cache
perl
make
shell
动态库
```

所以即使测试目录在块设备上，只要 `/bin`、`/lib` 还在 virtio-fs rootfs，`Execl` 和 `Shell Scripts` 仍然可能慢。

检查：

```bash
findmnt -T /bin/sh -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /lib -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /usr/bin/perl -o TARGET,SOURCE,FSTYPE,OPTIONS
```

File Copy 还要检查工作目录：

```bash
pwd
findmnt -T . -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /tmp -o TARGET,SOURCE,FSTYPE,OPTIONS
```

## 10.11 Kata 存储典型组合

### 默认 virtio-fs rootfs

```text
host overlayfs snapshotter
  ↓
overlayfs merged rootfs
  ↓
virtio-fs
  ↓
guest container rootfs
```

特点：集成简单，共享方便，metadata-heavy 性能可能差。

### block rootfs

```text
host devicemapper snapshotter
  ↓
rootfs block device
  ↓
virtio-scsi / virtio-blk
  ↓
guest mount
  ↓
container rootfs
```

特点：性能更接近 VM 本地盘，rootfs exec/filecopy/shell 表现好，管理复杂。

### rootfs block + volume virtio-fs

```text
rootfs：virtio-scsi / virtio-blk
workspace/input/output：virtio-fs
```

兼顾 rootfs 性能和文件共享。

### rootfs block + /tmp tmpfs

```text
rootfs：block
/tmp：tmpfs
workspace：virtio-fs or block
```

临时文件非常快，减少 File Copy / package install 压力。

## 10.12 为什么 pod 写 8 核，Kata 可能有 9 核/更多线程？

Pod CPU 是给容器 workload/vCPU 的限制，但 Kata sandbox 还有 host 侧辅助进程和线程：

```text
containerd-shim-kata-v2
QEMU main thread
QEMU vCPU threads
QEMU emulator thread
QEMU IOThread
virtiofsd worker threads
vhost threads
```

8 vCPU 不代表 host 上只有 8 个线程参与。

绑核时要分组：

```text
vCPU threads：给 workload
virtiofsd：给文件服务端
IOThread/QEMU：给块设备/设备模拟
shim：管理进程，通常不重
```

---

# 11. AI 沙箱 / Kata 实践选型建议

## 11.1 AI 沙箱常见行为

```text
启动解释器
运行 shell
pip install
npm install
解压依赖
读取大量小文件
写临时文件
执行用户代码
生成输出
```

这些大多是：

```text
metadata-heavy
small-file-heavy
exec-heavy
write-heavy
```

因此如果 rootfs、`/tmp`、依赖缓存都走 virtio-fs，会很容易慢。

## 11.2 推荐组合

```text
rootfs：
  virtio-blk / virtio-scsi

/tmp：
  tmpfs 或 guest 本地块设备

包缓存：
  guest 本地块设备
  /root/.cache/pip
  /root/.npm
  /root/.m2

workspace：
  如果要 host 直接共享，走 virtio-fs
  如果只在 guest 内运行，走块设备

input：
  virtio-fs readonly 可以接受

output：
  小量结果可以 virtio-fs
  大量中间结果 block，结束时拷出
```

更精炼：

```text
系统依赖路径 /bin /usr /lib /etc：block rootfs
高频临时路径 /tmp：tmpfs 或 block
包缓存 /root/.cache/pip /root/.npm /root/.m2：block
用户工作区 /workspace：按是否需要 host 实时共享选择 virtio-fs 或 block
输入数据：readonly virtio-fs 可以
输出结果：小量 virtio-fs，大量中间产物先 block，结束后拷出
```

## 11.3 对 UnixBench 线性度的优化优先级

### 第一优先级：rootfs block

```text
把 /bin /usr /lib /etc 从 virtio-fs 路径移走
```

因为 `Execl`、`Shell` 主要依赖这些路径。

### 第二优先级：/tmp 或 UnixBench 工作目录不要走 virtio-fs

```text
File Copy
fstime
临时文件
```

可以试：

```text
/tmp → tmpfs
UnixBench 工作目录 → block
```

### 第三优先级：如果必须 virtio-fs，调 cache 和 virtiofsd

```text
cache=metadata
cache=always 只读场景
thread-pool-size
virtiofsd 绑核
NUMA 对齐
```

### 第四优先级：block 侧调 QEMU

```text
cache=none/writeback
aio=native/io_uring/threads
IOThread
multiqueue
virtio-blk vs virtio-scsi
```

---

# 12. 完整排查命令清单

## 12.1 guest：确认路径

```bash
findmnt -T / -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /tmp -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /bin/sh -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /usr/bin/bash -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /usr/bin/perl -o TARGET,SOURCE,FSTYPE,OPTIONS
findmnt -T /root/byte-unixbench-6.0.1/UnixBench -o TARGET,SOURCE,FSTYPE,OPTIONS
lsblk -f
cat /proc/mounts | grep -E 'virtiofs|overlay|ext4|xfs'
```

## 12.2 guest：virtio driver

```bash
lspci | grep -i virtio
lsmod | grep -E 'virtio_blk|virtio_scsi|virtio_pci|virtio_ring|virtiofs|fuse'
```

## 12.3 guest：metadata 测试

```bash
find /usr/bin -type f | head -n 5000 > /tmp/files.txt

time while read f; do stat "$f" >/dev/null; done < /tmp/files.txt

time while read f; do head -c 4 "$f" >/dev/null 2>&1; done < /tmp/files.txt
```

## 12.4 guest：缓存测试

```bash
sync
echo 3 > /proc/sys/vm/drop_caches

time while read f; do stat "$f" >/dev/null; done < /tmp/files.txt

time while read f; do stat "$f" >/dev/null; done < /tmp/files.txt
```

## 12.5 guest：strace exec

```bash
strace -f -e trace=execve,openat,newfstatat,read,mmap,close /bin/sh -c 'true'

strace -f -c /bin/sh -c 'for i in $(seq 1 1000); do /bin/true; done'
```

## 12.6 guest：fio

```bash
fio --name=randread \
  --directory=/tmp \
  --direct=1 \
  --rw=randread \
  --bs=4k \
  --size=1G \
  --numjobs=4 \
  --iodepth=32 \
  --runtime=60 \
  --time_based \
  --group_reporting

fio --name=randwrite \
  --directory=/tmp \
  --direct=1 \
  --rw=randwrite \
  --bs=4k \
  --size=1G \
  --numjobs=4 \
  --iodepth=32 \
  --runtime=60 \
  --time_based \
  --group_reporting

fio --name=seqread \
  --directory=/tmp \
  --direct=1 \
  --rw=read \
  --bs=1M \
  --size=4G \
  --numjobs=1 \
  --iodepth=16 \
  --runtime=60 \
  --time_based \
  --group_reporting

fio --name=fsync-write \
  --directory=/tmp \
  --rw=write \
  --bs=4k \
  --size=256M \
  --numjobs=1 \
  --iodepth=1 \
  --fsync=1 \
  --runtime=60 \
  --time_based \
  --group_reporting
```

## 12.7 guest/host：iostat

```bash
iostat -x 1
```

## 12.8 host：Kata 进程

```bash
ps -ef | grep -E 'qemu|virtiofsd|containerd-shim-kata' | grep -v grep
```

## 12.9 host：QEMU 参数

```bash
qpid=<qemu_pid>
tr '\0' ' ' < /proc/$qpid/cmdline > /tmp/qemu.cmd

grep -oE 'vhost-user-fs[^ ]*|virtio-blk[^ ]*|virtio-scsi[^ ]*|scsi-hd[^ ]*|cache=[^, ]+|aio=[^, ]+|iothread[^, ]*|num-queues=[^, ]+|queue-size=[^, ]+' /tmp/qemu.cmd
```

## 12.10 host：virtiofsd 参数和线程

```bash
vpid=$(pidof virtiofsd)
tr '\0' ' ' < /proc/$vpid/cmdline
ps -T -p $vpid -o tid,psr,pcpu,comm
top -H -p $vpid
pidstat -t -p $vpid 1
taskset -pc $vpid
```

## 12.11 host：QEMU 线程

```bash
ps -T -p <qemu_pid> -o tid,psr,pcpu,comm
top -H -p <qemu_pid>
pidstat -t -p <qemu_pid> 1
taskset -pc <qemu_pid>
```

## 12.12 host：NUMA

```bash
numactl --hardware
lscpu -e=CPU,NODE,SOCKET,CORE,ONLINE
```

## 12.13 guest 一键检查脚本

```bash
echo "===== mount targets ====="
for p in / /tmp /bin/sh /usr/bin /lib /root/byte-unixbench-6.0.1/UnixBench; do
  echo "--- $p ---"
  findmnt -T "$p" -o TARGET,SOURCE,FSTYPE,OPTIONS 2>/dev/null || true
done

echo "===== block devices ====="
lsblk -o NAME,MAJ:MIN,SIZE,ROTA,TYPE,MOUNTPOINT,FSTYPE

echo "===== virtio modules ====="
lsmod | grep -E 'virtio|fuse' || true

echo "===== mounts grep ====="
grep -E 'virtiofs|overlay|ext4|xfs' /proc/mounts
```

## 12.14 host 一键检查脚本

```bash
echo "===== kata processes ====="
ps -ef | grep -E 'qemu|virtiofsd|containerd-shim-kata' | grep -v grep

echo "===== qemu cmd ====="
qpid=$(pgrep -f 'qemu-system' | head -n1)
if [ -n "$qpid" ]; then
  tr '\0' ' ' < /proc/$qpid/cmdline > /tmp/qemu.cmd
  grep -oE 'vhost-user-fs[^ ]*|virtio-blk[^ ]*|virtio-scsi[^ ]*|scsi-hd[^ ]*|cache=[^, ]+|aio=[^, ]+|iothread[^, ]*|num-queues=[^, ]+|queue-size=[^, ]+' /tmp/qemu.cmd
fi

echo "===== virtiofsd cmd ====="
vpid=$(pidof virtiofsd)
if [ -n "$vpid" ]; then
  tr '\0' ' ' < /proc/$vpid/cmdline
  echo
  ps -T -p $vpid -o tid,psr,pcpu,comm
fi
```

---

# 13. 汇报文案模板

## 13.1 现象描述

```text
在 Kata + QEMU 场景下，UnixBench 的 CPU 类指标如 Dhrystone、Whetstone 与宿主机接近，但 Execl Throughput、File Copy、Shell Scripts、Process Creation 等指标明显下降。说明 vCPU 算力不是主要瓶颈，问题集中在文件系统路径、metadata 操作和 exec-heavy workload。
```

## 13.2 原理分析

```text
virtio-fs rootfs 下，容器内 open/stat/read/exec 等文件操作会经过 guest VFS、virtiofs driver，转换为 FUSE LOOKUP/GETATTR/OPEN/READ/WRITE 等请求，通过 virtqueue/vhost-user 发送到 host 侧 virtiofsd，再由 virtiofsd 在 host shared-dir 或 overlayfs 上执行真实文件操作。

因此，metadata-heavy 和 exec-heavy 场景会产生大量跨 VM 文件系统请求，容易被 virtiofsd、FUSE 请求路径、host overlayfs、cache 一致性策略、CPU 调度和 NUMA 影响。
```

## 13.3 对比块设备

```text
block rootfs 下，guest 内部使用 ext4/xfs 管理容器 rootfs，open/stat/exec 的路径查找、inode 属性、page cache 大多在 guest 内完成，跨 VM 传输的是 block request，而不是文件系统语义请求。

因此，block rootfs 能更充分利用 guest dentry cache、inode cache、page cache，在 Execl、Shell Scripts、File Copy 等 UnixBench 指标上通常明显优于 virtio-fs rootfs。
```

## 13.4 测试设计

```text
建议分别设计 metadata、small file、large sequential I/O、direct I/O、fsync I/O 测试，并同时观察 guest/host iostat、virtiofsd/QEMU 线程 CPU、QEMU 参数、virtiofsd 参数、rootfs 和 /tmp 的挂载类型。

重点确认：
1. /、/tmp、/bin/sh、/usr/bin、UnixBench 工作目录分别在哪个文件系统。
2. virtiofsd 的 cache 模式、thread-pool-size、CPU 亲和性。
3. block rootfs 的 QEMU cache、aio、IOThread、num-queues、后端类型。
```

## 13.5 优化建议

```text
第一优先级：将 container rootfs 从 virtio-fs 切到 block rootfs，优先使用 virtio-scsi 或 virtio-blk，避免 /bin、/usr、/lib、/etc 等高频 exec 路径走 virtio-fs。

第二优先级：将 /tmp、包缓存、编译目录、UnixBench 工作目录放到 tmpfs 或 guest block 设备，减少 File Copy、解压、package install 对 virtio-fs 的压力。

第三优先级：如果必须使用 virtio-fs，可在只读场景下评估 cache=metadata 或 cache=always，并调整 virtiofsd thread-pool-size、CPU 亲和性和 NUMA 分布。

第四优先级：block 侧继续评估 cache=none/writeback、aio=native/io_uring/threads、IOThread、multiqueue、virtio-blk vs virtio-scsi，并结合 fio 与 iostat 定位是否存在 QEMU/IOThread 或底层存储瓶颈。
```

## 13.6 最终结论

```text
virtio-fs 的价值在于共享 host 目录、集成简单、方便处理 volume/configmap/secret 等场景，但它把文件系统语义跨 VM 传给 host virtiofsd，metadata-heavy、exec-heavy、小文件密集场景容易出现明显性能损耗。

块设备 rootfs 把文件系统语义留在 guest 内，跨 VM 的只是块 I/O，更适合作为 Kata 性能敏感场景、AI 沙箱、编译、解压、package install、UnixBench 等 workload 的 rootfs。

推荐组合是：rootfs 走 block，/tmp 和包缓存走 tmpfs/block，需要 host 共享的 input/output/workspace 再走 virtio-fs。
```

---

# 14. 参考资料

- Kata Containers storage architecture：`https://github.com/kata-containers/kata-containers/blob/main/docs/design/architecture/storage.md`
- Kata Containers virtio-fs howto：`https://github.com/kata-containers/documentation/blob/master/how-to/how-to-use-virtio-fs-with-kata.md`
- Kata Containers with Firecracker：`https://github.com/kata-containers/kata-containers/blob/main/docs/how-to/how-to-use-kata-containers-with-firecracker.md`
- virtio-fs project：`https://virtio-fs.gitlab.io/`
- virtio-fs design：`https://virtio-fs.gitlab.io/design.html`
- QEMU virtiofsd documentation：`https://qemu-stsquad.readthedocs.io/en/doc-updates/tools/virtiofsd.html`
- QEMU virtio devices：`https://www.qemu.org/docs/master/system/devices/virtio/index.html`
- QEMU virtio-blk / virtio-scsi configuration：`https://www.qemu.org/2021/01/19/virtio-blk-scsi-configuration/`
- QEMU multiple IOThreads：`https://www.qemu.org/docs/master/devel/multiple-iothreads.html`
- QEMU block drivers：`https://www.qemu.org/docs/master/system/qemu-block-drivers.html`
- Linux VFS documentation：`https://docs.kernel.org/filesystems/vfs.html`
- Linux pathname lookup：`https://www.kernel.org/doc/html/latest/filesystems/path-lookup.html`
- Linux DAX documentation：`https://docs.kernel.org/filesystems/dax.html`
- Linux virtio documentation：`https://docs.kernel.org/driver-api/virtio/virtio.html`
- Linux virtiofs documentation：`https://docs.kernel.org/filesystems/virtiofs.html`
- Linux FUSE documentation：`https://www.kernel.org/doc/html/next/filesystems/fuse.html`
- fio documentation：`https://fio.readthedocs.io/en/latest/fio_doc.html`
- iostat man page：`https://man7.org/linux/man-pages/man1/iostat.1.html`
- perf record man page：`https://man7.org/linux/man-pages/man1/perf-record.1.html`
