# Dragonfly URMA 真实 Provider 验证 Runbook

更新日期：2026-09-09。本文只把真实 UMDK provider 结果标记为实验证据；编译、mock
或本地故障注入不等价于真机验证。

> **RM-only 分支补充说明（2026-09-07）**：本文已有 B5/B6/B8 结果和部分 lane/Jetty 操作描述是
> RC 历史证据。`urma-rm-prototype` 当前使用 process-wide shared RM endpoint + per-peer imported
> target，不能把逻辑 `lane_id`/`peer_id` 当作 per-peer native Jetty/JFR。B7 inventory 和 manifest 必须
> 显式冻结 `transportMode=rm`、TP type、64 KiB message requirement 与
> `peerGuaranteedRxCredits=0`。

> **当前 RM0 观察（用户初测，尚未归档）**：单节点 `urma_perftest` RM 可运行，跨节点 RM 未跑通。
> 在精确命令、完整输出、两端 `urma_admin show --all`/`show topo` 和 EID/TP 参数归档前，这不是
> `[实验验证]`。跨节点最小 RM probe 是 Dragonfly dual case 的强制前置门禁；单节点成功不能替代它。

> 2026-09-07 局部日志：跨节点 RM+CTP `send_bw` 首批 128 个 WR 返回 `CR status 4`，按当前 UMDK
> 枚举是 `URMA_CR_LOC_ACCESS_ERR`，不是 timeout/RNR；B7 单节点 Dragonfly RM 则在 Parent
> `urma_import_jetty=-1` 后 `early eof` 并回退 TCP。RM 分支现已支持 `tpType: rtp|ctp`，并在 wire/import
> 层拒绝 TP 不一致；该实现仍待真机验证。应优先做同 binary 的 RTP/CTP 矩阵，未确认前不修改 RM gate。

> 当前执行状态：B5/B6 正常路径、budget pressure、多 lane required-first admission，以及 B8 同 lane
> 并发 Piece/native RX window 已完成真实 provider 验证；`pwritev` 后单任务峰值为 63.51 Gbps，
> L8/CC8/TX128 的纯 URMA 均值为 134.25 Gbps。第 5 节的矩阵保留为历史验证方法，不再表示 B5/B6
> “尚未开始”。下一轮先用当前 binary 新增的 TX window acquire timing 重跑 L8/TX128，再按证据优化
> allocator；同时补齐反向 SEND_IMM probe 和第 7 节 outstanding/fault shutdown 矩阵。

B7 自动化工具位于 `/home/yuan/workspace/dev/dragonfly-urma-tools/urma-b7/`：`discover/plan` 负责只读发现和拓扑冻结，
`probe-provider` 负责隔离执行并归档 `urma_perftest` 前置矩阵，
`prepare/run/cleanup` 默认 dry-run，只有显式 `--execute` 才操作远端。执行路径使用 run-scoped
YAML/socket/storage/port/origin，按 owner marker、PID cmdline 和精确路径限制启停与清理；使用方法见
该目录的 `README.md`。双节点 RM `run --execute` 只有在 inventory 的 cross-node probe 为 `passed`
时才放行；诊断性越过门禁必须显式添加 `--allow-unvalidated-urma`，且不能将所得结果当作正式验收。

157、158 当前统一使用 `/home/y30083740/dragonfly`：RM 被测仓库是
`dragonfly-client-urma-rm`，`dragonfly-client-urma-private` 仅保留为 RC A/B 基线，工具仓库是
`dragonfly-urma-tools`。原 158 的 `/home/yzd/dragonfly` 路径已失效。当前顶层目录清单没有旧 inventory
依赖的 `config/`，所以真机执行前还必须恢复/迁移 parent、child 和 scheduler YAML；`discover` 会把缺失
配置、错误 RM 分支或缺少 release binary 标记为 `incomplete`。

B7 工具本身同时支持 `--profile rm` 和 `--profile rc`，但 profile 分别启动两个独立 checkout。prepare
manifest 固化 profile、repo、transport 和 RC/RM 各自的 cross-node probe，run/cleanup 不允许隐式切换。
Dragonfly 的 `urma-rm-prototype` 仍保持 RM-only。

## 1. 已知可用环境

`urma-transport-lab/tcp-urma-file-transfer` 在 2026-08-24 确认的跨节点组合：

```text
Parent          90.91.177.158
Child           90.91.177.157
device          udmac0d1e2
transport       RTP + RC
EID index       1
provider chunk  65536 bytes
```

上机前先用 `urma_perftest send_bw -p 1` 或 demo 复验。该环境必须使用 `eidIndex: 1`，
不能盲用配置示例中的默认 0。

### 1.1 B7 provider 前置探测

先检查 dry-run 生成的两端命令，再显式执行。`--server-address` 必须填写 server 的 URMA EID 地址，不能
填写或推断 SSH 管理地址：

```bash
cd /home/y30083740/dragonfly/dragonfly-urma-tools/urma-b7

# RM 默认覆盖 RTP 和 CTP。
python3 b7.py probe-provider --profile rm --mode dual \
  --server-address 90.91.177.158 --run-id rm-provider-001
python3 b7.py probe-provider --profile rm --mode dual \
  --server-address 90.91.177.158 --run-id rm-provider-001 --execute

# 单节点闭环；仍传本节点的 URMA EID。
python3 b7.py probe-provider --profile rm --mode single --host node1 \
  --server-address 90.91.177.158 --run-id rm-loopback-001 --execute

# RC 对照默认只运行 RTP。
python3 b7.py probe-provider --profile rc --mode dual \
  --server-address 90.91.177.158 --run-id rc-provider-001 --execute
```

证据保存在 `results/<run-id>/provider-probe.json`，包含精确 argv、shell 展示、两端 stdout/stderr/退出码、
耗时、解析后的 `Failed CR status` 和 `discover` 环境快照。工具始终显式传 inventory 的
`--eid_idx 1`。`urma_perftest -O` 表示 priority，并非 opcode；默认不指定，由 perftest/provider 按 TP
选择。只有复现旧命令时才添加 `--priority 6`。默认消息为 4096 bytes，保证 RM+CTP 与 RM+RTP 使用共同
合法尺寸；64 KiB 能力另在 RTP/Dragonfly 分片路径验证。

## 2. 构建

正常闭环使用不带 failpoint 的生产形态：

```bash
export UMDK_INCLUDE_DIR=/usr/include/ub/umdk/urma
export UMDK_LIB_DIR=/usr/lib64
export LD_LIBRARY_PATH=/usr/lib64

cargo build --release -p dragonfly-client \
  --features urma --bin dfdaemon --bin dfget
```

中途失败验证或 transport-only 分层基准必须显式构建验证 feature：

```bash
cargo build --release -p dragonfly-client \
  --features urma-test-failpoints --bin dfdaemon --bin dfget
```

`urma-test-failpoints` 会传递到 storage crate 并自动包含 `urma`。只启用 `urma` 的
二进制不会读取 failpoint 或 `DF_URMA_PERFORMANCE_PROFILE` 环境变量。两节点上保留：

```bash
sha256sum dfdaemon dfget
ldd dfdaemon | grep -E 'urma|not found'
```

### 2.1 24 GiB 分层吞吐对照

B7 已提供两个同参数 case：

- `fanout-piece16-cc8-post1-in16-l8-tx128-transport-only-tmpfs`；
- `fanout-piece16-cc8-post1-in16-l8-tx128-crc32-pwrite-tmpfs`。

两者都是 L8、每 lane CC8、3 个 measured batch × 8 GiB，总传输量 24 GiB，并强制 storage 位于
tmpfs。transport-only 仍验证 SEND_IMM/CQE、精确长度、Done 和 lease recycle，但刻意跳过 Child CRC32
和 pwrite，输出不能用于 SHA 完整性结论；runner 会要求每个 1 GiB/16 MiB task 恰有 64 个 profile
completion。正常 case 必须继续满足 origin/Parent/Child SHA 一致。

执行每组前检查 `df -h /dev/shm`，执行后立即按 manifest cleanup；A/B 至少交替两轮，不能把连续未
cleanup 的结果放入同一比较。PR #1945 的 RDMA concurrency 是单个共享 `FI_EP_RDM` endpoint 上的
并发 Piece，不是多 lane，因此 L8×CC8 只回答 URMA 的总饱和能力；公平机制对照还需使用单 lane 多
Piece case。

## 3. dfdaemon 配置

Parent：

```yaml
storage:
  server:
    ip: 90.91.177.158
    tcpPort: 4005
    urma:
      enable: true
      port: 4008
      device: udmac0d1e2
      eidIndex: 1
      fabricTag: validation-domain
      maxRegisteredBytes: 40MiB
      txRegisteredBytes: 8MiB
      maxInflightChunks: 64
      postListSize: 1
      pipelineDepth: 2
      mmapContent: true
      maxConcurrentTransfers: 16
      transferTimeout: 30s

download:
  protocol: urma
```

Child 只做 downloader 时可设 `enable: false`，但 `device/eidIndex/fabricTag` 仍必须配置：

```yaml
storage:
  server:
    ip: 90.91.177.157
    tcpPort: 4005
    urma:
      enable: false
      port: 4008
      device: udmac0d1e2
      eidIndex: 1
      fabricTag: validation-domain
      maxRegisteredBytes: 40MiB
      txRegisteredBytes: 8MiB
      maxInflightChunks: 64
      postListSize: 1
      pipelineDepth: 2
      maxConcurrentTransfers: 16
      transferTimeout: 30s

download:
  protocol: urma
```

`fabricTag` 必须相同；本地 device 名可不同。TCP 4005 用于 discovery/fallback，TCP
4008 用于 persistent lane control，bulk bytes 走 UB。Parent 选择仍依赖 Dragonfly
scheduler，两个完全孤立的 dfdaemon 不能代替集群闭环。

## 4. 正常闭环与 lane 复用

1. 准备不小于 1 GiB 的确定性文件，记录 SHA-256。
2. Parent 先通过 `dfget` 下载相同 URL，使 Storage 持有完整 task。
3. Child 清空该 task/输出后下载同一 URL，带 `--disable-back-to-source`。

```bash
# Parent
dfget http://ORIGIN/dragonfly-urma-1g.bin \
  -O /tmp/parent-1g.bin --overwrite

# Child
dfget http://ORIGIN/dragonfly-urma-1g.bin \
  -O /tmp/child-1g.bin --overwrite --disable-back-to-source

sha256sum /tmp/parent-1g.bin /tmp/child-1g.bin
```

dfdaemon 使用 debug 日志。以下结构化事件是验收证据：

```text
created cached urma client
reusing cached urma client
urma peer lane established                 role=client/server lane_id=N
urma client selected peer lane             lane_id=N reused_session=true/false
urma piece request on peer lane             lane_id=N piece_number=M
start upload piece content over urma        lane_id=N piece_number=M
urma piece finished on peer lane            lane_id=N
aborting/dropping/closing urma peer lane     lane_id=N
```

至少 10 Piece 的验收条件：

```text
两端 peer lane established 各 1 次
至少 10 个 Piece request/finish
同一端所有 Piece 使用同一 lane_id
首 Piece reused_session=false，后续 Piece=true
abort/drop=0
TCP fallback=0
CQE/protocol/digest error=0
最终 SHA-256 一致
```

### 4.1 2026-08-29 首次并发建连问题与复验要求

`[实验确认]` 修复前的 1 GiB/256 Piece 跨节点测试出现：

```text
peer lane established       8
reused_session=false        8
reused_session=true       248
lane 1..7                   各传 1 Piece
lane 8                      传 249 Piece
最终 SHA-256                一致
```

原因是同一 parent 的 8 个初始 Piece 并发执行 cache check、各自创建 `UrmaClient`，再相互覆盖
cache；不是 provider、CQE 或内容错误。代码已增加 per-parent client initialization singleflight，
并用 client generation 防止旧请求的迟到失败删除新 cache。纯测试已覆盖 8 路同 parent 串行、
不同 parent 独立 gate，完整 `dragonfly-client --features urma --lib` 为 62 passed。

`[实验确认]` 2026-08-29 使用修复后二进制重跑 1 GiB/256 Piece，结果严格变为：

```text
peer lane established       client 1 / server 1
reused_session=false        1
reused_session=true       255
唯一 lane_id                lane 1，256 Piece
```

通用 grep 中的 `all pieces are collected, abort all tasks` 是 Piece collector 正常结束日志，不能计为
URMA abort。URMA transport error 和 lane lifecycle 应分开统计。

### 4.2 idle timeout 语义与跨任务复验

`[实验确认]` 上述传输完成后，旧代码约 30 秒出现：

```text
urma peer connection retired ... URMA control operation timed out: receive Piece Request
```

原因是 server 错把 active operation 的 `transferTimeout=30s` 用于空闲等待下一 Piece，而 client
准备缓存 peer Session 420 秒。当前代码已拆开两个边界：

```text
active control/window timeout     transferTimeout（当前 30s）
client cached Session idle        420s
server idle wait                  420s + transferTimeout grace（当前 450s）
```

server idle 到期现在返回正常状态，记录 `urma peer session idle timeout` 后执行
`closing urma peer lane`，不进入 abort/transport error。纯测试新增
`stalled_idle_request_is_a_normal_expiry`；storage URMA 测试 34 passed，client feature-on 测试
62 passed。

`[待验证]` 补两轮跨任务测试：

1. 完成任务 A 后等待 40 秒，再下载任务 B：仍使用原 lane，第二个任务首 Piece
   `reused_session=true`，双端不新建 lane；
2. 完成后保持空闲超过 450 秒：server 出现正常 idle/close 日志且没有 connection-retired error；
   下一任务由 client 淘汰超过 420 秒的 cache 并新建一个 lane，不发生 Piece TCP fallback。

### 4.3 2026-08-29 非整 Piece/chunk 尾部验证

`[实验确认]` 在 1 GiB 输入后追加 12,345 bytes，构造不能被 4 MiB Piece 或 64 KiB chunk
整除的文件：

```text
source length   1,073,754,169 bytes（1 GiB + 12,345）
normal Piece    4,194,304 bytes
URMA chunk         65,536 bytes
tail              12,345 bytes
SHA-256          f745a08a4e0dfc3c2e0ce22bc036b99721e269f4fd8c73680765c54b2c49f19a
```

Parent 先回源，Child 使用 `--disable-back-to-source` 从 Parent 下载。source、Parent 输出和 Child
输出三者 SHA-256 完全一致；Child 日志确认该任务通过 `lane_id=2` 且后续 Piece
`reused_session=true`，以下错误审计无输出：

```text
fallback
cqe/completion error
digest/protocol error
transfer/urma failed
```

因此 Phase A 的非整 Piece、非整 chunk 尾部长度、写入和 digest 闭环标记为 PASS。后续留档时再
额外保存以下精确证据，便于直接审计最后的短 Piece，而不只依赖最终文件 hash：

```bash
grep -E 'piece_number=256|length 12345|piece_id=.*-256' /tmp/dfdaemon-child.log
grep -E 'piece_number=256|length 12345|piece_id=.*-256' /tmp/dfdaemon-parent.log
```

## 5. B5/B6 批处理与注册预算验证（已执行的基础矩阵）

B5/B6 的基础矩阵和后续 required-first admission 已在真实 provider 上完成。以下表格保留为复现与扩展
矩阵；新增 case 仍应先跑默认兼容基线，再逐项只改一个变量：

| 组 | `postListSize` | `pipelineDepth` | 注册预算 | 目的 |
|---|---:|---:|---|---|
| baseline | 1 | 2 | 40 MiB / TX 8 MiB | 与 B4 行为对照 |
| post-list | 4、8、16 | 2 | 同 baseline | linked post correctness、吞吐和 CPU 校准 |
| single-window | 8 | 1 | 同 baseline | 验证单窗口上限和无 overlap 路径 |
| optional pressure | 8 | 2 | 缩小但仍容纳两个方向各一窗 | 第二窗失败后 ring/pipeline=1，传输仍成功 |
| required pressure | 8 | 2 | 无法容纳 negotiated required window | 明确 BUSY/fallback，不出现无界 owned buffer |
| multi-peer | 8 | 2 | baseline 与 pressure 各一轮 | 独立 lane 均有进展，无资源泄漏/长期饥饿 |

`maxRegisteredBytes` 是 process 预注册总量；`txRegisteredBytes` 是固定 TX 分区，RX 使用余量。两者按
64 KiB slot 向下换算，且 TX/RX 各至少一个 slot。`pipelineDepth=2` 时，单 window 最大 slot 数是方向
slot 数的一半。B6 初测时同一 persistent Session 仍顺序处理 Piece；B8 已通过 transfer multiplexing 和
`SEND_IMM` identity 支持同 lane 并发 Piece，不能再用这一历史限制解释当前结果。

### 5.1 B8 与当前性能复测要求

当前并发性能 case 必须额外验证：

- Piece identity 使用 `(lane_id, transfer_id)`；native RX window identity 使用
  `(lane_id, transfer_id, window_start_chunk)`；
- 同 lane 至少两个 transfer 的 native RX window 同时 outstanding，SEND_IMM window/Piece 汇总闭合；
- JFR/JFS aggregate depth、native permits、required/optional pressure、ring/window1 fallback 均入档；
- L8/CC8/TX128 复测必须每轮保存 evidence 后执行 manifest-owned cleanup，并交替运行 A/B，避免遗留
  storage 目录、公共 seed hard link 和主机负载污染趋势；
- 当前 HEAD 还必须汇总 `tx_required_acquire_ns`、`tx_required_pool_acquire_ns`、
  `tx_optional_acquire_ns` 和 `tx_optional_pool_acquire_ns` 的 count/mean/p50/p95/p99/max，以区分 owner
  command 排队和 allocator 本体成本。

每一组至少覆盖：非整 chunk 尾部、连续 10 Piece、normal/persistent/persistent-cache、mmap 与 reader
fallback。post-list 组还必须注入或构造 partial post、单 WR error、flush/断链，确认只消费成功提交前缀，
未提交后缀可回收，所有已提交 WR 均由 CQE/error 路径退休。flush/断链时额外确认：Jetty 和其 owned
shared JFR 都成功进入 ERROR；recv JFC 只排空真实 WR completion；send JFC 的
`WR_FLUSH_ERR_DONE` 通过 `local_id` 命中正确 lane，且不会 poison 其他健康 lane。

采集以下 Prometheus series（完整名称含现有 namespace/subsystem 前缀）：

```text
dragonfly_client_urma_registered_bytes{direction="tx|rx"}
dragonfly_client_urma_budget_pressure_total{direction="tx|rx",stage="required|optional"}
```

同时保存两端 Piece finished 日志中的 `tx_windows`、`tx_ring_depth`、`tx_overlap_windows`、
`tx_second_lease_fallback`、`tx_fill_ns`、`tx_send_wait_ns`，以及 RX Storage/backpressure 时间。验收要求：

- baseline 的 registered bytes 为配置折算后的固定 TX/RX 分区，运行中不无界增长；
- 正常双窗口组 optional pressure 不增长；压力组 optional 增长且 depth 1 fallback 可见；
- required pressure 只在第一窗口无法取得时增长，并触发可分类 fallback；
- 完成、失败、取消和 shutdown 后 outstanding WR、lease、slot、credit 均归零；
- post-list 从 1 增大后的性能结论必须同时报告 E2E、source-fill、send-wait/CQ、Storage 和两端 CPU。

## 6. 真实 completion 后故障注入

只在 Child 的 validation binary 上设置：

```bash
export DF_URMA_FAIL_AFTER_RECV_WINDOWS=3
dfdaemon --config /path/to/child.yaml --log-level debug --log-dir /tmp/dfdaemon-urma
```

值必须是正整数。failpoint 在第 N 个 window 内的真实 RECV CQE 全部完成后触发；
非最终 window 会先交给 Storage，确保 fallback 验证的是“已有 partial Piece”。如果
N 命中最终 window，数据仍被 Done gate 扣留，不会让 Storage 误判成功。

验收日志顺序：

```text
urma real-provider receive failpoint armed windows=3
injecting urma failure after real receive completions completed_windows=3 lane_id=N
dropping/aborting urma peer lane lane_id=N
streaming urma piece failed while writing, restarting over tcp
```

同时必须确认 TCP 4005 仍可用、最终 SHA-256 正确，且 partial URMA bytes 没有与 TCP
内容拼接。测完立即 unset 环境变量，并换回只启用 `urma` 的二进制：

```bash
unset DF_URMA_FAIL_AFTER_RECV_WINDOWS
```

## 7. shutdown 和结果记录

在有 outstanding transfer 时向 dfdaemon 发 `SIGTERM`，记录 capability clear、lane
abort/drain、Fabric shutdown 和进程退出时间。重启后再跑一次正常 Piece，确认 provider
资源可重建。

至少同时建立两个 lane，再只中断其中一个：被中断 lane 必须等“outstanding WR=0 + send flush-done”
后才删除 Jetty/JFR；另一 lane 应继续完成 Piece。日志中不应出现 unknown native Jetty、receive-JFC
flush-done、提前 delete 或 Fabric 被预期的 flush completion poison。随后对无 outstanding WR 的空闲
lane 执行关闭，确认 owner 仍会继续 poll 到 fake flush-done，而不是因 outstanding=0 提前删除。

每轮保留：节点/IP/device/EID、二进制 SHA-256、UMDK/provider 版本、完整两端
日志、输入/输出 SHA-256、Piece 数、唯一 lane_id 数、fallback 数、CQE error 数，以及本轮 B5/B6
配置、上述 metrics 起止值和 Piece timing 摘要。
