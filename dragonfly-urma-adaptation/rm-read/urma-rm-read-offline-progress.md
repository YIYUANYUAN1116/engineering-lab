# RM READ 离线实现进度

更新时间：2026-09-17。

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

## 第四批：owner registry、双侧预算与隔离回收

- 新增独立于设备的 `urma/read_owner.rs`，以 registry 实例 ID、单调递增 owner 序号和 peer generation
  标识资源；拒绝跨 registry ID、旧 generation 和序号回绕。peer drain 完成之前不能启用下一 generation。
- native 创建前先 reserve，预留同样计入预算；创建成功或不确定结果再 attach ownership bundle。
  即使 shutdown、drain 或 quarantine 已发生，迟到的创建结果仍能 attach，但不能重新变为 Active。
- 总额度、source/destination 分区及每个 peer 的双向额度均检查字节数和条目数；方向之间不借用保留
  空间，避免 Child 占满 Parent 的进展空间。接入方必须按实际保留的 allocation/backing 大小计费。
  条目数表示 ownership bundle/创建预留数，不等同于 provider Segment 数或 WR/JFS credit。
- Retiring 和 Quarantined 始终占用原额度。隔离字节数或条目数达到阈值后停止新 admission；已接纳
  的操作仍可进入隔离并超过该停止阈值，但不会突破原有总额度。达到阈值不触发强制释放或逐出。
- `reap_with` 保留部分 cleanup 进度；Pending 不回收，失败进入 quarantine 并允许重试。只有匹配
  当前 owner 的 `VerifiedRetirement` 才会释放 bundle 和预算，错误 proof 不允许释放其他 owner。
  proof 构造为 unsafe，要求覆盖整个 bundle；该接口本身不能产生硬件退休或 Parent 撤权证据。
- 尚未 attach 的预留只有已知未创建资源时才能退还；若已经进入 quarantine，后续确认无资源的
  创建结果还需匹配 proof 才能解除预留。未知 post/register 结果不能当作普通失败退还额度。
- shutdown 关闭 admission、标记 peer drain，并保留所有未决预留、owner 和隔离项，直到全部 reap。
  registry Drop 不析构未回收的 bundle，仅作为避免提前释放的最后防线。生产 owner loop 必须持续
  持有并驱动该 registry，不能销毁后重建来绕过预算。
- 提供总量、方向、peer、reserved/retiring/quarantine 使用量快照；当前按有界条目扫描计算，尚未
  接入 metrics、配置、异步额度等待/唤醒、缓存或 native source/import/WR 的具体清理适配器。
- 新增 13 项测试，覆盖双向字节/条目保留、失败 admission 不改变记账、隔离阈值、cleanup 重试、
  shutdown 迟到结果、peer generation、错误 proof、预留释放和 Drop 保留 owner。URMA 模块共 147 项通过。

本批完成管理层基础，不代表已经把 source/import/WR 接入生产 Runtime，也未接通 READ wire 协议。

## 第五批：Parent native source 与 registry 绑定

- 新增 `urma/read_source_owner.rs`，为 `ReadOwnerRegistry<SourceOwner<K>>` 提供同步 owner-thread
  注册入口。按 backing 实际长度 reserve 后才调用 native register；预算拒绝和注册前拒绝均返回
  backing，已建立的无资源预留即时退还。注册成功 attach，注册不确定则 attach 并 quarantine。
- descriptor 只允许经 Active owner 导出；retire、peer drain、shutdown 或 quarantine 后禁止继续导出。
  已发出的 descriptor 不会因此自动撤权，仍必须完成下述清理。
- 引入分别绑定 owner ID 的 `UnregisterPermit` 与 `SourceRevoked`，unsafe 构造分别要求 provider
  unregister 前置条件和独立撤权/失败注册回滚证据。两种证明不能由 TCP 消息、超时或普通 unregister
  成功直接构造；代码没有增加自动生成这些证明的机制。
- 已成功注册的 source 按 unregister → 等待撤权证明 → token/backing release 顺序清理。成功的
  unregister 阶段被保存，release 重试不再重复 unregister。任一清理失败均隔离并保留全部预算。
- 不确定注册没有可用 registration handle，不尝试伪造 unregister；仅在独立证明 grant/pin 已回滚后
  进入 release。若 shim wrapper 也丢失，现有 native API 仍会拒绝 release 并持续保留，不能猜测回收。
- release 成功才释放 backing/keepalive，随后向 registry 提交整个 source bundle 的退休证明并退还额度。
  source 不包含 Child WR 或 consumer worker；不能把这一退休证明用于 Child bundle。
- 新增 5 项离线测试：清理替身覆盖 unregister 成功后等待证明、unregister/token-release 失败重试、
  不确定注册回收与错误身份拒绝；真实 Rust adapter 使用关闭的 runtime 验证预算/注册前拒绝及 backing
  归还。URMA 模块共 152 项通过。成功 native 注册到清理的真机流程未执行。

本批仅绑定 Parent source。Child import/WR、destination buffer、CQE identity 路由和生产 Runtime
尚未接入。当前 source 专用 registry 不能与另一个各自占用完整 process budget 的 Child registry 并用；
后续需使用统一 ownership bundle/共享预算管理两侧。生产 READ dispatch 继续关闭。

## 第六批：Child native ownership 与逐 WR 退休校验

- 新增 `urma/read_child_owner.rs`。`NativeChild` 持有 destination Segment、imported Segment 和
  keepalive，并借用 Jetty/Target；`ChildOwner` 持有该 bundle 和有界 outstanding WR 表，可由现有
  registry attach/reap 管理。构造为 unsafe，要求已预留实际 allocation 预算、确认精确范围和独占访问。
- 按 Piece 内连续、不重叠的 local/remote offset 发起单 WR READ；outstanding 达限时拒绝新 post。
  每次 post 分配进程内单调、不复用的 READ context，溢出拒绝。此 context 尚未并入 SEND/RECV 的
  production completion namespace，不能直接在现有 shared poll loop 中混用。
- native Posted 和 Uncertain 均保留 WR；Uncertain 停止后续 post 并将 attempt 标记失败，匹配 CQE
  到达后仍可 drain。已知 Rejected 无 WR 可退休；丢失 handle 等无法解释的错误保留整个 bundle，
  不提供 timeout/reset 式解除隔离接口。注册层仍需由 owner loop 将错误映射为 quarantine。
- `ReadRetired` 是 unsafe 的 provider 退休证据边界；先验证 opcode、context flags、error/flush
  语义后才能构造，再由 owner 检查 owner ID（含 peer generation）、Jetty、context 和请求长度。
  未知、重复或不匹配完成不会消费另一个 WR，并阻止后续 post；真实 native CQE decoder 尚未实现。
- 取消/registry retirement 停止新 post，已有 WR 仍可经 reap 回调处理完成。pending 未清空时不调用
  unimport 或 buffer close；清空后按 unimport → buffer close → drop keepalive → registry 退还预算
  顺序清理。清理失败保留进度，buffer close 重试不重复已成功的 unimport。
- 本批是 cleanup-only：`read_succeeded` 仅描述全部计划字节的 READ 成功，不交付 CPU 指针或 Storage
  lease，不替代 ReadDone/Done。当前没有 consumer worker，因此回收路径不能直接套用于未来
  consumer-held buffer，后续必须延长其预算和生命周期。
- 新增 5 项资源替身测试，覆盖乱序/重复/错误身份完成、uncertain post、shutdown drain、unimport/
  buffer close 重试、已知拒绝与缺失 handle 的保留。完整 URMA 模块 157 项通过；native adapter
  已编译链接，未在真机执行 READ/CQE。

接入限制：当前 NativeChild 借用独占 Jetty，适合验证单 owner 生命周期，尚不支持生产多 Piece 共用
Jetty 的调度；destination allocation/import 的自动 admission 入口、Parent/Child 统一 registry、
shared JFS permits、CQE router、取消 wire 和 Storage 交付均待接入。不能分别创建两个拥有完整
process allowance 的 registry 并宣称已实现全进程预算。

## 第七批：双向统一 registry 与 Child 创建 admission

- 新增 `urma/read_owners.rs`，以 `ReadBundle::Source/Child` 将两侧 adapter 放入同一张 registry，
  共享总额度、方向保留、peer generation、quarantine 触发线与 shutdown 生命周期。Parent 注册/
  descriptor/reap 通过 `SourceSlot` 复用原实现；不再需要为两侧各配置一份完整 READ process allowance。
- 新增同步 `create_child` factory 入口：检查 Piece/allocation 大小、Jetty ID 和 outstanding 配置，
  reserve 实际 allocation bytes 后才执行创建。预算或 shutdown 拒绝时 factory 不会运行。
- factory 区分 Ready、已完全回滚的 Rejected、持有部分资源的 Uncertain，以及无法重建 owner 的 Lost。
  Rejected 退还预留；Uncertain attach 并 quarantine；Lost 保留 ownerless 隔离预留及额度，不能由
  普通 reap 释放。factory 是 unsafe native 集成边界，必须准确声明资源状态，不允许把 cleanup
  失败归类为 Rejected。具体 Segment allocation/import factory 尚未在生产调用点安装。
- 统一 post 仅访问 Active Child；错误保守地关闭该 attempt 的 dispatch 并隔离。completion 可以访问
  Retiring/Quarantined owner，继续执行完整身份校验并 drain；错误 completion 保留资源并隔离。
  普通 admission 拒绝目前也按 attempt 失败处理，尚无等待/重试调度。
- `NativeChild` 改为持有 `Rc<RefCell<JettyHandle>>` 和 `Rc<TargetHandle>`，post 时才短暂借用 Jetty；
  消除单个 Piece 整个生命周期独占 Jetty 的限制。Segment import 仅需共享 Target 引用，原生依赖计数
  仍由 shim 维护。该改动提供单 owner-thread 共享持有能力，不等同于完成 shared JFS credit、公平调度
  或生产 Fabric 接线。
- 新增 6 项测试：同表 source 预留和 Child 同时计费、无额度/无效配置/shutdown 不运行 factory、
  部分创建失败保留、Lost 持续隔离、shutdown 后精确完成，以及混合 registry 的 Parent preflight
  拒绝归还 backing。新增测试使用资源替身或关闭的 runtime；没有执行真实 allocation/import/READ。

仍需完成：具体 native Child 创建/部分失败回收 factory、shared JFS/per-peer WR permits、真实 CQE
解码与路由、生产 Runtime/Session 接线及 Storage lease。统一表只是 READ 管理模块，尚未计入生产
SEND/RECV pool 或其他已有内存额度，也未把两种数据面同时启用。

## 第八批：native Child factory、注册失败保留与 WR 额度

- 新增 READ 专用 `dfurma_read_buffer_create` / `SegmentHandle::create_read_buffer`。沿用 local-only
  注册，但 register 返回 NULL 后保留 allocation、wrapper 和 runtime Segment 计数；以 Uncertain
  返回，普通 close 拒绝回收。未证明失败注册已完成 pin rollback 前不释放或复用，不改动原 SEND/RECV
  allocator 的失败路径。正常 READ buffer 的 unregister 失败同样保留内存和计数，允许重试。
- `ReadOwners::create_native_child` 在统一 byte admission 内调用 native factory：验证 Piece/descriptor
  长度及实际 Jetty ID，创建 destination，再 import。成功结果自动包装 WR credit adapter；普通 import
  失败尝试关闭 destination，关闭失败则保留为 Uncertain。import 失败无资源的结论仍依赖该 provider
  的失败回滚门禁，不以离线测试代替证明。
- register 不确定不继续 import；成功却丢失 buffer handle 的异常保留共享 native owner/keepalive，
  并保留 ownerless 隔离预留。import 成功却丢失 handle 时也禁止普通清理，继续保留 bundle。
- 新增 `read_wr_credit.rs`：一个 owner-thread 共享额度表同时约束 JFS 总 outstanding 和每个 peer
  generation 的 outstanding。`CreditedChild` 在 native post 前取得一份额度，Posted/Uncertain 将
  额度绑到 WR；只有匹配的退休路径完成 native 计数更新后才归还。明确未接受的 post 立即归还。
  缺失 WR handle 的不确定错误保留额度，避免把可能已提交的容量再次使用。
- WR 的意外 Drop 不退还额度；byte registry 的 retire/quarantine 不退还 WR 额度。当前额度耗尽
  返回已知拒绝并使 attempt 失败，尚未实现公平排队、有界异步等待或 BUSY 重试。生产接入必须让
  同一 Jetty 的全部 READ 使用同一额度实例，容量来自实际可用 JFS depth；若与 SEND 并用还必须统一
  两者的物理 WR 容量，目前没有接通这种并用模式。
- 新增 3 项 Rust credit 测试，覆盖双 peer 共享/单 peer 上限、已知拒绝、Uncertain 与丢失 handle。
  URMA 模块共 166 项通过。C provider-call 替身增加 2 组：真实 shim 的 destination 注册失败保留/
  unregister 重试，以及新 buffer → import → READ → retirement → unimport → buffer close 模拟闭环；
  共 10 组通过，`-Wall -Wextra -Werror` 编译通过。

本批已完成具体 native factory 与显式 WR 额度封装，但没有 production Runtime 调用点或真实 CQE
解码。credit、byte admission 与 native 生命周期的行为由离线测试覆盖，不代表 provider rollback、
CQE retirement、pin/unpin 或跨节点 READ 已经验证。没有增加释放不确定 destination 的猜测式恢复接口。

## 接入边界

已有 native import/READ 封装，但 production Runtime/Session 不调用它们，没有修改 wire version 或发布
RM_READ capability。当前 RM SEND/RECV 数据路径继续运行；查询到非零 max_read_size 不会自动启用 READ。

Parent native 外部内存注册/export 及分阶段 backing owner 已完成离线封装，见第三批。尚未接
Storage 的具体保活/不可变内容租约和真实硬件撤权证明。双侧预算与 peer generation registry
已完成独立管理模块，并绑定 Parent native source；Child native ownership adapter 已完成，
两侧已纳入统一 registry、native Child 创建 factory 和 WR credit adapter；真实 CQE 路由
和生产 Runtime 尚未接入。
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

累计验证结果（第八批重新编译并执行 C shim 替身测试）：

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check`、`git diff --check` | PASS |
| `cc -Wall -Wextra -Werror -fsyntax-only`，使用本地 UMDK include | PASS |
| 直接编译 `read.rs` 纯状态测试 | 12 passed / 0 failed |
| 第一批 storage feature-on `--lib urma::` 测试 | 130 passed / 0 failed，含新增 12 项 |
| 第二批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 132 passed / 0 failed |
| 第三批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 134 passed / 0 failed |
| 第四批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 147 passed / 0 failed，含新增 13 项 registry 测试 |
| 第五批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 152 passed / 0 failed，含新增 5 项 source adapter 测试 |
| 第六批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 157 passed / 0 failed，含新增 5 项 Child owner 测试 |
| 第七批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 163 passed / 0 failed，含新增 6 项统一 owner 测试 |
| 第八批 storage feature-on 构建、链接及 `--lib urma::` 测试 | 166 passed / 0 failed，含新增 3 项 WR credit 测试 |
| C READ shim provider-call 替身测试 | 10 组场景 PASS，含新增 READ buffer 失败保留与完整模拟生命周期 |
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

- 将 native factory 与共享 WR credit 实例接入生产 owner loop，补公平调度及有界等待；
- READ operation owner、generation registry 与 production Runtime post/flush 集成；
- 将双侧预算接入配置、实际 allocation/Storage lease 与 admission 等待；实现 `ExportedPieceLease`、BufferReady 和完整取消协议；
- R0/R1 真机 capability、撤权/重用/授权边界验证；
- 通过门禁后再接 file mmap、三类 Piece Storage 和性能验证。

## 单机 RM 测试方法（2026-09-17，待上机执行）

单机测试应分为 provider loopback 和 Dragonfly 双进程两个层次。两者都在同一台机器运行，但
结论不同：前者只证明 UMDK/provider 的 RM 本地回环，后者才观察 Dragonfly 配置、Jetty import、
控制面和数据路径。当前 `urma-read-prototype` 的 READ dispatch 仍关闭；要测已有 RM SEND/RECV，
使用工具固定的 `urma-rm-prototype` checkout。

### B7 测试目录约定

B7 inventory 的单机默认目录已统一为：

```text
/home/y30083740/dragonfly-b7/origin   # seed 与每轮 origin hard link
/home/y30083740/dragonfly-b7/storage  # parent/child storage 与 output
/home/y30083740/dragonfly-b7/run      # 配置、socket、pid、daemon/dfget 日志与磁盘快照
```

`storageClass=tmpfs` 的 case 也使用上述 `storage` 根目录；运行前需要把该根目录（或其挂载点）
挂载为 tmpfs，prepare 会检查文件系统类型。旧 manifest 中的 `/dev/shm/dragonfly-b7` 仍可按兼容
规则清理，但新 run 不会再生成该路径。

`origin.directory` 是文件系统路径；`origin.baseUrl` 仍为 `http://141.61.17.196:8080`，因此
目标机上的 HTTP origin 服务必须把上述 `origin` 目录作为 document root（B7 不会修改 nginx 或
其他 HTTP 服务配置）。若服务仍提供 `/var/www/dragonfly`，prepare 会生成无法下载的 URL，需先
调整服务 document root 或本地 inventory。B7 的路径安全检查、磁盘快照和 cleanup 均从 inventory
读取这三个根目录，并保留旧根目录仅用于回收历史 manifest。

本次离线检查：`python3 -m unittest -v test_b7.py` 共 99 项通过；`b7.py plan --profile rm
--mode single` 已确认 seed、run、storage 和日志均落在上述目录。没有执行真实 provider 或
Dragonfly 进程。

### A. 先做只读环境检查

在目标机器上确认 device、EID index、URMA 工具和库：

```bash
command -v urma_admin urma_perftest
urma_admin show --all
urma_admin show topo
ldconfig -p | grep -E 'liburma(_common)?'
```

`--server-address` 必须填 `eidIndex` 对应的 URMA EID，不能填 SSH 管理 IP。当前 B7 inventory
默认使用 `device=udmac0d1e2`、`eidIndex=1`；如果本机实际值不同，先修改一份本地 inventory，
不要直接套用旧的 `90.91.177.158`。保留上述命令原始输出和 `uname -a`、`sha256sum $(command -v urma_perftest)`。

### B. RM provider 同机回环

推荐用 B7 自动归档；它会在同一节点通过两个 SSH 会话启动 server/client，默认依次覆盖 RTP 和 CTP：

```bash
cd /home/yuan/workspace/dev/dragonfly-urma-tools/urma-b7
python3 b7.py probe-provider --profile rm --mode single --host node1 \
  --server-address <本机URMA-EID> --size 4096 --iterations 1000 \
  --run-id rm-loopback-20260917
python3 b7.py probe-provider --profile rm --mode single --host node1 \
  --server-address <本机URMA-EID> --size 4096 --iterations 1000 \
  --run-id rm-loopback-20260917 --execute
cat results/rm-loopback-20260917/provider-probe.json
```

如果不使用 B7，在同机两个终端分别执行等价命令。server 不带 `-S`，client 带本机 EID：

```bash
# terminal 1
urma_perftest send_bw -d udmac0d1e2 --eid_idx 1 --tp_aware --ctp \
  -p 0 -j true -n 1000 -s 4096
# terminal 2
urma_perftest send_bw -d udmac0d1e2 --eid_idx 1 --tp_aware --ctp \
  -p 0 -j true -n 1000 -s 4096 -S <本机URMA-EID>
```

去掉 `--ctp` 可单独测 RM+RTP；不要添加 `-O`，除非需要复现旧 priority。`--ctp` 的 provider
probe 上限是 4096 bytes，RTP 可再测 65536；8 KiB/64 KiB 结果不能反推 CTP 的能力。
记录每个 TP 的 return code、completion status、耗时和 `urma_admin show --all/topo` 快照。

### C. Dragonfly 单机双实例

先 dry-run 检查端口和路径，再执行；单机模式会在同一节点规划 parent/child 两套端口（默认
44000/44008 与 44100/44108）、socket、storage 和 origin link：

```bash
cd /home/yuan/workspace/dev/dragonfly-urma-tools/urma-b7
python3 b7.py plan --profile rm --mode single --host node1 \
  --run-id rm-dragonfly-single-20260917
python3 b7.py prepare --profile rm --mode single --host node1 \
  --run-id rm-dragonfly-single-20260917 --case smoke-post1-pipe1 --execute
python3 b7.py run --manifest results/rm-dragonfly-single-20260917/manifest.json --execute
python3 b7.py cleanup --manifest results/rm-dragonfly-single-20260917/manifest.json --execute
```

执行前必须确认 inventory 中 parent/child YAML、`urma-rm-prototype` release `dfdaemon/dfget`、
scheduler/origin 已在目标机存在；当前记录的 `/home/y30083740/dragonfly/config` 曾缺失，缺失时
先恢复配置再运行。`prepare/run/cleanup --execute` 会创建、启动和删除本轮隔离资源，必须使用新
run ID，不能覆盖未完成 manifest。

### D. 如何判定结果

provider JSON 中 RTP/CTP 两个 case 都是 `passed`，只表示同机 RM provider loopback 通过。Dragonfly
还要检查 `evidence`、parent/child transfer log 和 manifest：

```bash
rg -n 'URMA|RM|fallback|import_jetty|early eof|Piece|error' \
  results/rm-dragonfly-single-20260917/evidence \
  /home/y30083740/dragonfly-b7/run/rm-dragonfly-single-20260917/{parent,child}/*.log
sha256sum /home/y30083740/dragonfly-b7/storage/rm-dragonfly-single-20260917/{parent,child}/output.bin
```

必须区分三种结果：

1. `provider passed`：同机 UMDK RM provider 成功；
2. `Dragonfly RM passed`：日志和指标显示正常 URMA RM Piece/transfer，且内容校验通过；
3. `TCP fallback/unsupported`：例如 `import_jetty=-1`、`early eof` 或明确 fallback，内容成功也
   只能记为 fallback，不能记 RM PASS。当前已知单机 Dragonfly RM 曾在 `import_jetty` 后回退 TCP，
   所以 B/C 两层结果必须分别归档。

单机成功不能替代跨节点 provider/RM 验证，也不能证明 READ、撤权、token 重用或跨节点 EID 路由。
真机结果应保存到 `results/<run-id>/provider-probe.json`、manifest 和 evidence，再回填 provider ledger。

以上生产集成均未由当前八批基础代码完成；本地编译、纯状态或 provider-call 替身测试不计为真实
provider 验证。下一步优先接真实 READ command/completion 路由，并补齐 owner loop 调度及控制面状态机，
继续保持 production READ dispatch 关闭，直到 R0/R1 门禁具备证据。

## 第九批：真实 CQE 路由与 owner loop 轮询接口（2026-09-17）

`urma-transport-lab` 单机真实 provider probe 已通过 64 MiB / 1 MiB 分片的 RM/RTP READ：Child
收到 64 个 send-side Jetty CQE，`user_ctx_valid=true`，内容校验、owner retirement、BUSY
unimport 和 clean shutdown 均通过。该 provider 的成功 READ CQE 为 `opcode=0`、
`completion_len=0`，且 `remote_id`、`imm_data` 无效。因此 Dragonfly 路由不使用 opcode、
completion length 或 remote identity 判别 READ。

本批实现：

- READ `user_ctx` 使用独立前缀 `[0xffff][0xff][0x52][sequence:32]`。`0x52` 位于现有
  `WrToken.operation` 字段且不是 SEND/RECV 的 1/2，未知或迟到的 READ CQE 不会误解码为现有 WR；
- `ReadOwners` 在 native post 返回后建立 `context -> owner/jetty/request_length` 路由。
  provider 返回 uncertain、可能已经接受的 WR 同样保留路由并隔离 owner，等待真实 CQE；
- READ CQE 只接受 work-request event、send JFC、`user_ctx_valid=true`、`is_recv=false`、
  `is_jetty=true` 和匹配的 local Jetty。请求长度取 post 时保存的 route；status 决定成功或失败；
- wrong queue、wrong Jetty、未知或重复 context 都 fail closed。能够精确匹配的 error CQE 会退休
  native WR、归还 WR credit，并把 transfer 保持在失败状态；
- `CompletionRouter::poll_once_with_read` 在仅有 READ outstanding 时也轮询 send JFC，并在普通
  SEND/RECV `WrToken` 解码前调用 READ sink；receive JFC 也经过 READ sink 以拒绝错误队列 CQE；
- shutdown/drained 判定同时要求 READ route 表为空，避免 registry 已清空但 CQE ownership 仍悬空。

验证：`cargo fmt --all`、`git diff --check` 通过；使用本地 UMDK 执行
`cargo test --offline -p dragonfly-client-storage --features urma --lib urma::`，169 passed / 0 failed。
新增测试覆盖真实 provider 的 opcode/length 形态、重复 CQE、错误队列/Jetty 隔离，以及 uncertain
post 在 error CQE 到达前持续持有 route 和 WR。

当前仍未发布 production READ capability，也没有 READ fabric command、Runtime 内的具体
`ReadOwners` 实例或 Storage lease。下一步需要统一生产 `UrmaJetty` 与 native READ owner 当前使用的
Jetty handle 所有权形态，把具体 READ owner 容器放入 fabric owner thread，再让其调用本批的
`poll_once_with_read`；随后接 peer generation、shutdown/flush 和公平 post 调度。

## 第十批：生产 Runtime owner 容器与原生依赖共享（2026-09-17）

本批把第九批的 READ-aware completion 接口接入生产 `UrmaRuntime` 和 fabric owner loop，但仍不开放
READ capability、配置或 fabric command：

- 生产 shared RM Jetty 的 native handle 改为 owner-thread 内的 `Rc<RefCell<JettyHandle>>`；普通
  SEND/RECV 和 READ 使用同一个 Jetty/JFS，不再为 READ 建第二套 endpoint；
- `PeerTarget` 的 imported target 改为 `Rc<TargetHandle>`。未来 Child READ owner 可同时持有 Jetty
  和 target，PeerTarget close 在引用未释放时 fail closed，防止 target unimport 和 peer ID generation
  重用早于 READ import/WR 退休；
- Jetty close 同样检查 READ 引用；只要 Child owner 仍持有 Jetty，endpoint、JFC 和 native runtime
  就不能继续关闭；
- `UrmaRuntime` 新增具体类型
  `ReadOwners<(), CreditedChild<NativeChild<()>>>` 的 disabled/active owner-thread 状态。当前启动固定为
  disabled，尚无协商或配置能切到 active；
- `UrmaRuntime::poll_once` 现在始终调用 `poll_once_with_read`，`outstanding()` 同时统计普通 WR 和 READ
  route。fabric owner loop 因此会在只有 READ outstanding 时继续 poll send JFC；
- peer 创建/retire 已接 READ generation registry；peer reap 要求该 generation 的 READ entries 清空；
- shutdown 先停止 READ admission，再把 READ outstanding/drained 纳入普通 drain、endpoint flush、超时
  报错和最终资源关闭门禁。未完成 READ cleanup 时不会关闭 target、Jetty、JFC 或 native runtime。

新增无 provider 的生命周期测试，验证 READ clone 会阻止 target/Jetty close，释放 clone 后可以重试
关闭。使用本地 UMDK 执行 storage URMA lib 测试：170 passed / 0 failed；`cargo fmt --all -- --check`
和 `git diff --check` 通过。完整非 lib `cargo check` 仍被现有 `dragonfly-api` build script 向只读 Cargo
dependency source 写 `src/descriptor.bin` 阻断，与本批代码无关。

当前 active variant 只确定生产所有权和调度位置，没有创建入口。下一步应加入显式、默认关闭的 READ
runtime 配置和统一 JFS admission，使 SEND 与 READ 共享真实 send depth；然后实现 owner-thread Child
create/post/retire command，使用本批提供的 shared Jetty/target，并继续保持 wire capability 不发布。

## 第十一批：READ-only JFS admission 与 Child owner command（2026-09-17）

本批按用户确认将该分支定义为 READ-only bulk data 分支，不再要求保留 SEND/RECV 数据面兼容。实现
选择相应调整为独占而非共享：

- 新增显式 `ReadRuntimeConfig`，包含统一 source/destination byte budget、per-peer READ outstanding
  上限和 destination buffer alignment。配置参与 process-shared Fabric identity，配置不一致不能复用
  同一个 native runtime；
- READ-only 启用时，effective JFS depth 不再受旧 TX slot 数量约束，完整分配给
  `ReadWrCredits`；per-peer limit 不能超过实际 JFS depth；
- READ-only runtime 明确拒绝旧的 receive-window、send-credit 和 registered SEND command，避免两套
  WR 记账同时使用同一个 JFS。旧接口暂时保留在源码中供后续删除，但不属于 READ-only active path；
- startup gate 要求 provider `max_read_size > 0`，READ buffer alignment 必须是至少 4096 的二次幂，
  byte budget 继续由统一 `ReadOwners` 验证；
- fabric owner thread 新增 `CreateReadChild`、`PostReadChild` 和 cleanup-only `RetireReadChild` command。
  Create 在 owner thread 内完成 destination byte reservation、buffer allocation、remote Segment import、
  shared Jetty/PeerTarget retention 和 JFS credit绑定；
- Create 的 descriptor/token authentication 与 transfer generation 证明仍由未来 wire/session 状态机提供，
  因而 facade 保持显式 `unsafe`；quarantined create 会把 owner ID 返回给调用者，不能因错误丢失 cleanup
  身份；
- Post 返回 provider `user_ctx` 并立即进入第九批建立的 CQE route。Retire 当前只用于失败、取消和
  shutdown cleanup；成功数据尚未形成 Storage lease，不能调用该接口冒充业务完成。

验证新增 READ-only 配置测试：旧模式 send depth 受 TX slots 约束，READ-only 模式取得完整 provider JFS
depth，且 READ 配置会改变 shared Fabric identity。使用本地 UMDK 执行 storage URMA lib 测试：
171 passed / 0 failed；`cargo fmt --all -- --check`、`git diff --check` 通过。

当前 server/client 配置适配层尚未构造 `ReadRuntimeConfig`，所以线上启动仍不会进入 active READ-only
状态；wire capability 也仍未发布。下一步是实现 Parent source register/descriptor/revoke owner commands，
随后定义 DFUR READ Offer/BufferReady/ReadDone/Done 状态机，把 Child create/post 与 Parent grant 串起来。

补充：READ-only startup 不再创建旧 registered TX/RX slot pool，避免在 Piece-sized READ destination/source
预算之外重复 pin SEND/RECV 内存。READ owner loop 使用不依赖 `UrmaBufferPool` 的 send-JFC poll；除
READ CQE和 endpoint lifecycle CQE 外，任何普通 WR CQE都作为 READ-only 协议错误处理。duplex Jetty、
send/recv JFC/JFR 对象仍按已验证 provider 约束创建，但不会 post legacy SEND/RECV WR。

## 第十二批：Parent source owner-loop 生命周期（2026-09-17）

本批把已经离线验证的 Parent `ReadSource` owner 接入 `UrmaRuntime` 与 fabric owner command 队列，但仍未
接入 DFUR wire/session，也未开启生产 READ：

- 新增 `ReadSourceRequest`、opaque `ReadSourceId`、`ReadSourceOffer` 和 admission 结果；source 注册、
  descriptor 导出和 token 交付都在唯一 native owner thread 串行完成；
- 注册成功但 descriptor 导出失败时，不释放 source/backing，而是先停止该 owner 的正常 dispatch，保留
  id、完整预算和 native ownership 供后续清理；已知未开始注册的 rejection 才允许立即释放 backing；
- 新增 `RegisterReadSource`、`RetireReadSource`、`UnregisterReadSource`、
  `ReleaseReadSourceAfterRevoke` fabric commands，并将清理命令放入 urgent queue；
- 清理保持三阶段边界：`retire` 只停止 descriptor 使用；`unregister` 需要 provider drain permit 且仍保留
  backing/预算；只有独立证明旧 generation/token 的远端访问已经终止且不能恢复后，才允许 release；
- `ReadDone`、TCP EOF 和 timeout 本身仍不能构造 unregister/revocation proof。wire 状态机必须先完成
  Child accepted WR drain + unimport，再由 Parent 的 provider 撤权门禁提供独立证明；
- unsafe facade 明确承担 exact Piece immutable backing、peer/transfer generation 认证和 provider proof；
  command handler 本身不能绕过这些入口构造证明。

验证：`cargo test --offline -p dragonfly-client-storage --features urma --lib urma::` 在修正本机 UMDK
`liburma_common` 搜索目录后通过，171 passed、0 failed；`cargo fmt --all` 与 `git diff --check` 通过。

下一步：定义 READ-only DFUR capability 和 `BufferReady/SegmentOffer/ReadDone/Done`、
`Cancel/CancelDrained/Cancelled` 的 pointer-free wire DTO，先实现纯状态机与 generation/tombstone 测试，
再把本批 Parent command 和上一批 Child command 接入 session。

## 第十三批：READ-only DFUR wire DTO 与纯状态机（2026-09-17）

本批新增独立 `read_protocol`，仍未挂入现有生产 listener/session：

- 定义 DFUR READ wire version 5；READ frame 使用独立 envelope codec，version 4 会 fail closed，不会被
  READ peer 当作兼容的 SEND/RECV 协议；
- 定义 READ capability：`max_read_size`、`max_jfs_sge`、`descriptor_version`，协商后的 READ 上限取双方
  `max_read_size` 最小值，零值和 descriptor version 不匹配直接拒绝；
- 定义 pointer-free `BufferReady`、`SegmentOffer`、`ReadDone`、`Done`、`Cancel`、
  `CancelDrained`、`Cancelled` DTO；SegmentOffer 显式携带 descriptor 字段、token、非零
  `segment_generation` 和 `effective_max_read_size`，自定义 Debug 不输出 token；
- 所有 frame 使用 `peer_generation + transfer_id + metadata_generation` 的完整基础身份；Offer 后的 frame
  还必须匹配非零 `segment_generation`，不存在用零 generation 匹配已发布 export 的路径；
- Parent 状态机强制 `BufferReady -> register/publish Offer -> ReadDone -> revoke -> Done`；Done 只能在
  source 安全释放后产生。Offer 后取消必须等待同 generation 的 `CancelDrained`，且 accepted/retired WR
  计数完全相等后才能进入 revoke；
- Child 状态机覆盖正常 READ、Offer 前取消、Offer 后取消和迟到 Offer。Cancel 后迟到 Offer 只产生
  `DrainLateOffer`，不能进入 `StartRead`；关闭 import 并退休全部 accepted WR 后才能发 CancelDrained；
- 增加有界 tombstone，按完整 transfer identity 和可选 segment generation 去重 terminal；容量淘汰后的
  古老重复 frame 回到 unknown/fail-closed 路径，不会无限增长。

新增 9 个定向测试覆盖 DTO round-trip、token 日志脱敏、READ version gate、capability 协商、正常生命周期、
Offer 前/后取消、迟到 Offer、错误 generation/长度/WR 计数以及 tombstone 边界。完整 URMA 测试结果：
180 passed、0 failed；`cargo fmt --all` 与 `git diff --check` 通过。

下一步：把 READ capability 和 version 5 握手接入 rendezvous，建立 READ 专用 lane control dispatcher，随后
用 adapter 把 Parent/Child 状态机 action 映射到上一批 owner-loop command。接线阶段仍保持 feature/config
gate，直到单机真实 session 跑通后再替换当前生产 SEND/RECV session。

## 第十四批：version 5 lane handshake 与 READ dispatcher（2026-09-17）

本批在 `read_control` 中接通 version 5 DFUR envelope 的 lane handshake 和 transfer dispatcher，仍由独立
模块承载，尚未替换生产 client/server session：

- 新增 READ lane capability，固定 READ 数据面并协商 transport type、RTP/CTP、fabric tag、
  `max_read_size`、`max_jfs_sge` 和 descriptor version；兼容检查返回双方较小的有效 READ 长度；
- 新增 `Connect/Connected` version 5 handshake，双方交换 capability、Jetty descriptor，并要求 server
  原样回显非零 `session_generation`；descriptor 空值、超长、capability 不匹配或错误 generation 均拒绝；
- wire identity 从两端无法共享的本地 `PeerTarget generation` 调整为握手协商的 64-bit
  `session_generation`。两端本地 PeerTarget generation 继续由 runtime owner 使用，session adapter 后续
  显式绑定二者，不能假设两端本地 generation 数值相同；
- 增加不回绕的 process-level session generation allocator；耗尽时停止 admission，而不是复用旧 identity；
- `ReadLaneControl` 使用单 reader/single writer task，在一个 lane 内按完整
  `session_generation + transfer_id + metadata_generation` 路由多个 Piece；注册受 semaphore 上限约束；
- send 和 receive 两侧都验证完整 identity。错误 session generation、同 transfer 不同 metadata
  generation、未知 transfer 都会关闭 lane，不能路由到相邻 Piece；
- 正常 finish 和非正常 drop 都写入有界 tombstone；匹配的重复 `Done/Cancelled` 被吸收，不影响 sibling
  transfer。已退休且 tombstone 仍在的完整 transfer identity 不能重新注册；
- dispatcher 只接受第十三批 READ frame codec，不接受 version 4 SEND/RECV frame。

新增 4 个 dispatcher/handshake 测试，覆盖 version 5 handshake round-trip、generation echo、交错 transfer
路由、错误 generation/重复注册、retired terminal 重发与 sibling 隔离。完整 URMA 测试结果：
184 passed、0 failed；`cargo fmt --all` 与 `git diff --check` 通过。

下一步：增加 READ session adapter。Child 侧把 `BufferReady/SegmentOffer` 映射到
`create_read_child/post_read_child`，按 `max_read_size` 滑动提交并等待 owner CQE；Parent 侧把
`RegisterSource/RevokeSource` action 映射到 source owner commands。第一步使用内存 backing，Storage mmap
lease 和生产 listener 切换继续保持关闭。
