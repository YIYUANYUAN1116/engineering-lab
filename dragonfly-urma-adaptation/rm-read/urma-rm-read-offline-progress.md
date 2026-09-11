# RM READ 离线实现进度

更新时间：2026-09-11。

设计入口：[RM READ 整体方案与路线图](./dragonfly-urma-rm-read-design-and-roadmap.md)。

本文已从代码仓库 `docs/` 迁移到工程文档的 `rm-read/` 目录，后续以此处为唯一进度记录。
下列源码路径和命令均相对于 Dragonfly `urma-read-prototype` 工作区根目录。

当前没有目标机器。按用户确认先推进可离线验证的代码；设计中的 R0/R1 仍作为启用 native READ 和
接入生产数据路径的门禁。本批属于 R2 的部分基础准备，不代表 R0、R1 或 R2 整体完成。

## 第一批：切片与纯状态记账

- C shim → Rust FFI → Runtime capability 增加 `max_read_size`、`max_write_size`，保持设备返回的字节值；
  不把 SEND `max_msg_size` 或外部“256M”建议当作 READ 能力。
- `urma/read.rs` 提供独立于设备的 READ limit 协商和惰性切片规划。零 limit 关闭 READ，检查两侧
  Segment/allocation 范围、Piece offset、地址溢出、进程可表示长度和单 SGE `u32` 长度。
- `ReadProgress` 跟踪一个 transfer 的保留 post batch、实际 accepted prefix 和未完成 slice。
  per-transfer outstanding 上限约束记账大小，不按全部计划 slice 预分配。
- 取消禁止新 post，但保留尚未返回结果的 post batch 和已接受 WR；post/CQE 错误阻止成功完成，
  已知 WR 继续 drain。重复、未知、长度不匹配 CQE 或不一致的 post 结果不能证明安全 drain。
- 每条 READ 都按独立 CQE 计账，支持乱序完成。全部计划字节成功覆盖与 local drain 分开判断。

## 第二批：Child native import 与单 WR READ

- `ffi/read.rs` 和 C shim 增加显式、无指针的 READ Segment DTO。当前格式仅表示 pinned、non-cacheable、
  READ-only、plain-token 且无 provider extension 的 Segment；它还不是 DFUR wire 协议。
- import 前验证版本、权限、token policy、非零长度、地址溢出、UASID 位宽及 remote EID/UASID 与
  PeerTarget 一致，查询设备 RM/READ/SGE 能力并约束单 READ 长度。不支持跨 context 的 source Segment。
- imported Segment 持有 native PeerTarget 依赖并计入 runtime Segment 数；有 outstanding READ 时
  unimport 返回 BUSY。unimport 失败保留 handle 和计数，成功后才归还依赖。
- 单条 READ 使用一个 remote source SGE、一个 local destination SGE，逐 WR 开启 completion。
  本地 allocation、remote Segment、PeerTarget、Jetty 和 Runtime 都持有 outstanding 计数。
- post 结果分为 `Posted`、`Rejected`、`Uncertain`：只有 provider 明确通过 `bad_wr` 指出该 WR 未被
  接受时才回滚；错误但无法确认是否接受时返回 retained WR，必须等待 CQE/经验证的 retirement 或隔离。
- Rust `post_read` 和 READ WR retirement 为显式 unsafe 接口，要求调用者证明目标内存独占及匹配
  completion；DTO/token 的 Debug 隐藏 token 和访问 descriptor 的敏感字段。
- 增加直接包含实际 shim 实现的 C provider-call 替身测试，检查 descriptor 拒绝、SGE 方向、post
  failure/uncertainty、依赖计数和 unimport 失败重试。模拟 payload copy 不证明 DMA 或 provider 行为。

## 第三批：Parent 外部内存注册与 backing 生命周期

- `ffi/read/source.rs` 新增 `ReadBacking<K>` / `ReadSource<K>`，持有独占 bytes 或只读 mmap，以及
  后续 Storage lease/预算凭据。注册期间不暴露可释放或修改的 backing。
- C shim 注册调用使用精确外部 VA/length、READ-only、plain token、pinned、non-cacheable；shim 不
  分配或释放外部 backing，也不把范围自动扩展到 Piece 外。
- 显式分配并持有 token ID，避免走当前 UMDK 对自动 token ID 的 unregister 失败后 free 路径。
  token 在独立确认撤权且 release 成功之前不归还；这不是 token 重用安全已被真机验证的结论。
- 注册结果区分 `Registered`、`Rejected` 和 `Uncertain`。Rejected 只覆盖注册调用之前的失败；一旦
  调用过 register 而返回 NULL，就保留 source wrapper、token、backing 和 runtime Segment 计数，
  不假设 provider 已完成 grant/pin rollback。缺少 native handle 时禁止伪造 unregister 成功。
- exporter 通过 `urma_get_seg_ctx()` 获取上下文，以 `urma_put_seg_ctx()` 释放；检查结构长度、全部
  可表示属性、exact VA/length 和 UASID 位宽。拒绝扩展、non-pin、cacheable、额外权限或范围变化。
- 生命周期拆成三步：`unregister()` 停止后续 descriptor export；native 成功后仍保留 wrapper/token/
  backing；只有调用者提供独立撤权依据后，才可使用 unsafe `release_after_revoke()` 归还 backing。
  token free 失败继续持有资源，禁止重复 unregister 已被 native 消费的对象。
- `ReadSource::Drop` 不调用 unregister/free，也不释放 backing 或 keepalive。这个保守保留策略只防止
  提前回收；生产集成仍需 owner registry 负责 quarantine、持续计费、上限与后续 reap，不能靠 Drop
  泄留代替有界资源管理。
- C 替身测试覆盖 source register → descriptor → import → READ → unimport → unregister → release
  的模拟闭环，以及不支持的 context、unregister/token-free 重试、注册失败保留。Rust 测试覆盖 source
  Drop 保留 backing/keepalive、注册前拒绝归还 backing。

当前独立撤权依据尚不可由代码自动产生。ReadDone、TCP EOF、超时或 unregister 返回成功都不能直接
用作 unsafe release 的依据；没有真机证据时这些接口保持与 production dispatch 断开。

## 接入边界

已有 native import/READ 封装，但 production Runtime/Session 不调用它们，没有修改 wire version 或发布
RM_READ capability。当前 RM SEND/RECV 数据路径继续运行；查询到非零 max_read_size 不会自动启用 READ。

Parent native 外部内存注册/export 及分阶段 backing owner 已完成离线封装，见第三批。尚未接
Storage 的具体保活/不可变内容租约、双侧预算、generation registry 和真实硬件撤权证明。
不能把持有泛型 keepalive 的能力视为已经实现 `ExportedPieceLease` 业务集成。

`ReadProgress` 是纯记账模块，不是 native owner/RAII guard。后续接入必须满足：

1. 外层 registry 先验证 peer/transfer/Segment generation 和原生 CQE identity，才按 slice index 路由；
2. production owner 持有 buffer、imported Segment、PeerTarget 和 native WR，不能因 Rust future/drop
   而提前释放。当前 shim 已有 native 依赖计数，但 Runtime quarantine/reap 与 generation registry 尚未接入；
3. post 前同时取得 shared JFS、per-peer 和 byte permits，并受 shim 最大 post-list 长度约束；
4. owner 串行处理 native post 返回与 CQE，先 commit accepted prefix，再处理对应完成；
5. `locally_drained()` 只表示本 transfer 不会继续 post 且已接受 WR 全部退休，不证明 Parent 撤权；
6. `read_succeeded()` 不允许直接发布 Storage lease；仍需 unimport、ReadDone/Done terminal gate；
7. 真实 READ `completion_len`、error/flush CQE 语义由 probe 确认后才能映射到本模块；
8. 记账进入 uncertain 后不提供猜测式 clear/reset。资源隔离或经验证的 native retirement 由外层负责。

## 离线验证入口

本批结果：

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check`、`git diff --check` | PASS |
| `cc -Wall -Wextra -Werror -fsyntax-only`，使用本地 UMDK include | PASS |
| 直接编译 `read.rs` 纯状态测试 | 12 passed / 0 failed |
| 第一批 storage feature-on `--lib urma::` 测试 | 130 passed / 0 failed，含新增 12 项 |
| 第二批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 132 passed / 0 failed |
| 第三批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 134 passed / 0 failed |
| C READ shim provider-call 替身测试 | 8 组场景 PASS，含 source 回收、注册失败保留和 Child READ |
| native READ / 跨节点 provider / 撤权 | 未执行 |

纯状态测试无需 UMDK、设备或整个 workspace 构建：

```bash
rustc --edition=2021 --test dragonfly-client-storage/src/urma/read.rs \
  -o /tmp/dragonfly-urma-read-unit-tests
/tmp/dragonfly-urma-read-unit-tests
```

完整 URMA 模块测试使用本地 UMDK build：

```bash
env UMDK_INCLUDE_DIR=/home/yuan/workspace/cloud-native/umdk/src/urma/lib/urma/core/include \
  UMDK_LIB_DIR=/home/yuan/workspace/cloud-native/umdk/build-urma/lib/urma/core \
  LD_LIBRARY_PATH=/home/yuan/workspace/cloud-native/umdk/build-urma/lib/urma/core:/home/yuan/workspace/cloud-native/umdk/build-urma/common \
  cargo test --offline -p dragonfly-client-storage --features urma --lib urma::
```

当前 `dragonfly-api` 的 build script 会在 Cargo dependency source 内生成 `src/descriptor.bin`；
只读缓存沙箱会阻断该构建。需要正常可写构建环境，不应为绕过此问题修改业务依赖或关闭 URMA 检查。
该本地 UMDK build 的 `liburma.so` 还依赖 `common/liburma_common.so.SOVERSION`，链接和运行时均需
包含上述 common 路径；只配置 core 路径会出现 `ub_str_to_u*` undefined reference。

C shim 替身测试单独运行，不由 Cargo 自动发现；链接真实 liburma 以解析其余 shim 符号，但测试替换了
query/token/register/export/import/unimport/unregister/post 调用，不打开设备：

```bash
env LD_LIBRARY_PATH=/home/yuan/workspace/cloud-native/umdk/build-urma/lib/urma/core:/home/yuan/workspace/cloud-native/umdk/build-urma/common \
  cc -Wall -Wextra -Werror \
  -I /home/yuan/workspace/cloud-native/umdk/src/urma/lib/urma/core/include \
  dragonfly-client-storage/tests/urma_read_shim_test.c \
  -L /home/yuan/workspace/cloud-native/umdk/build-urma/lib/urma/core -lurma \
  -o /tmp/dragonfly-urma-read-shim-test
env LD_LIBRARY_PATH=/home/yuan/workspace/cloud-native/umdk/build-urma/lib/urma/core:/home/yuan/workspace/cloud-native/umdk/build-urma/common \
  /tmp/dragonfly-urma-read-shim-test
```

## 后续工作

- source/import/WR owner registry：generation、quarantine/reap、持续计费及 shutdown 依赖顺序；
- READ operation owner、generation registry 与 production Runtime post/flush 集成；
- 双侧 byte budget、`ExportedPieceLease`、BufferReady 和完整取消协议；
- R0/R1 真机 capability、撤权/重用/授权边界验证；
- 通过门禁后再接 file mmap、三类 Piece Storage 和性能验证。

以上生产集成均未由当前三批基础代码完成；本地编译、纯状态或 provider-call 替身测试不计为真实
provider 验证。下一步优先完成 owner registry 与预算，以便把保守保留的资源纳入可观测、有界的管理。
