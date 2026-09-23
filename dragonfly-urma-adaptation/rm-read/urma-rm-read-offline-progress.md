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

## 第十五批：READ normal-path session adapter 与 Child progress（2026-09-20）

本批开始把第十三/十四批状态机与第十一/十二批 owner-loop command 串接，仍保持 gated、未接生产
client/server listener：

- `ChildOwner` 新增只读 progress snapshot，记录 Piece 长度、accepted/retired bytes、accepted/retired WR、
  outstanding、posting stopped、failed 和整 Piece成功状态；计数只在 native post 已接受和匹配 CQE 已退休
  的位置推进；
- progress 通过 `ReadOwners -> UrmaRuntime -> FabricCommand` 在 native owner thread 查询，session 不直接访问
  owner 或 CQE route；snapshot 会校验 bytes/WR 单调关系和 `outstanding = accepted - retired`；
- 新增 `ChildTransportSession` normal path：发送 BufferReady、校验 SegmentOffer、创建 native Child，按协商
  `effective_max_read_size` 和 per-transfer outstanding 上限滑动提交 READ，轮询 owner progress，并设置整体
  completion deadline；
- Child 只有在 `accepted_bytes == retired_bytes == piece_length`、全部 WR 退休且无失败后才形成 ReadDone。
  transport-only adapter 在 ReadDone 前先 retire Child、关闭 import 和 destination，符合
  `stop post -> reap CQE -> unimport -> ReadDone` 顺序；
- 当前 adapter 故意不返回 destination bytes：尚无 Storage lease handoff，成功测试路径会关闭 buffer，只能
  作为 transport lifecycle 验证，不能记 Dragonfly Piece业务成功；
- 新增 `ParentSourceSession` normal path：等待 BufferReady、在 owner thread 注册 source、转换 descriptor/token
  为脱敏 wire Offer、发布非零 segment generation、校验 ReadDone，并先 retire source；
- Parent 的 terminal 接口为显式 unsafe `revoke_and_finish`。只有调用方已经拿到 provider unregister 和独立
  远端撤权证明时，才执行 unregister、release 和 Done；ReadDone/EOF/timeout 仍不能调用该接口；
- source ID 与 segment generation 保存在 session 内，finish 时再次比对，不能把其他 transfer 的 revoke
  结果用于当前 Done；source request 的 peer ID 也必须匹配当前 lane；
- native `ReadToken` 增加 consuming wire extraction，值仍不会进入 Debug、错误或指标。

新增 progress snapshot 断言和 session progress invariant 测试。完整 URMA 测试结果：186 passed、0 failed；
`cargo fmt --all` 与 `git diff --check` 通过。

当前限制：normal path adapter 尚未接生产 listener；Parent/Child 取消和中途 control failure 还需要返回并保留 cleanup owner，不能启用；Child 成功 destination 尚未转换为 Storage lease。下一步先补齐取消/错误路径的 retained-owner outcome，再实现 destination lease 提取与单机 memory-to-memory session harness。

## 第十六批：取消/错误路径 retained-owner outcome（2026-09-20）

本批补齐第十五批遗留的取消与 control failure 路径，仍保持 gated、未接生产 listener：

- `read_protocol` 补两个竞态转移：Parent 在 `Offered` 相位收到无 generation 的 `Cancel`（Child
  已取消但尚未消费迟到 Offer）进入 `WaitingCancelDrain`，不撤权；`WaitingCancelDrain` 相位收到
  无 generation 的 `CancelDrained`（Child 在看到迟到 Offer 前已 drain）只继续等待，必须等到匹配
  generation 且 WR 计数相等的声明才允许进入 release；
- Child 失败路径重构为 `run_inner` + `cancel_after_failure`：失败时先按状态机发送
  `Cancel(segment_generation)`，best-effort wire 交换不决定本地 cleanup；随后读取 progress 并执行
  cleanup-only retirement。`retire_read_child_for_cleanup` 返回 drained 才继续
  `CancelDrained` -> 等待 `Cancelled` -> finish；返回 Pending 或错误则返回
  `ChildTransportFailure { retained_child }`，由调用方保留 owner id 供后续 cleanup owner pass；
- Child 在 `WaitingDone`（ReadDone 已发）之后的失败不做取消交换：destination 已证明关闭，仅返回
  失败；未创建 native owner 的失败不返回 retained id；
- Parent `publish_and_wait_read_done` 区分三种 release 触发：`ReadDone`（Success）、匹配
  generation 的 `CancelDrained`（Cancelled）、其余 frame 只切换等待目标。注册 Quarantined、
  Offer 发布失败、等待期 control failure、retire 失败均返回
  `ParentTransportFailure { retained_source }`，其中 Offer 未发布时 generation 为 `None`；
- `revoke_and_finish` 按 `ParentTerminal` 发送 `Done` 或 `Cancelled`，terminal 与 pending 不一致
  直接拒绝；ReadDone/CancelDrained/EOF/timeout 仍不能构造 provider 撤权证明；
- `run_inner` 成功改为返回 terminal generation，由拥有 session 的调用点执行 `control.finish`；
  future 被 drop 而未走 cancel 路径时，native owner 仍由 registry fail-closed 保留，但跳过 wire
  取消交换。

新增 3 项测试：Child Offer 后取消必须全量 drain 后才能收 `Cancelled`（含重复 terminal）、Parent
无 generation Cancel/CancelDrained 竞态序列、drainable 计数要求 accepted==retired。完整 URMA
测试结果：189 passed、0 failed；`cargo fmt --all -- --check` 与 `git diff --check` 通过。

下一步：实现 destination lease 提取与单机 memory-to-memory session harness，随后接生产
listener 切换实验。

## 第十七批：destination lease 三阶段流与单机 session harness（2026-09-20）

本批完成 Child destination lease 的提取、发布与回收全链路，并交付单机 harness：

- `read_child_owner`：新增 `LeaseSpan`（CPU 指针 + 长度）与 `ChildResources::Lease` 关联类型及
  `extract_lease`/`close_lease`；`NativeChild.local` 改为 `Option<SegmentHandle>`（lease 提取后
  移出，post 拒绝重复提取）。`ChildOwner` 新增三阶段方法：
  - `drain_for_lease`：停发布 + 关 import，保留注册缓冲与全额预算，永不返回 Retired；
  - `publish_lease`：READ 完全成功且 import 已关后提取 CPU span，预算保持扣减；
  - `recycle_lease`：关闭 lease 并完成普通 reap 释放预算；`reap` 在已发布未回收时保持
    `Pending`，fail-closed；
- 三层贯通：`read_owners` 新增 `drain_child_for_lease`/`publish_child_lease`/`recycle_child_lease`
  （recycle 走 registry 的 `Retiring`/`Quarantined` 状态门禁）；runtime 新增 `ReadLeaseSpan` DTO
  （仅指针跨界，`unsafe impl Send` 附安全论证）与三个 owner-thread 命令；fabric 新增
  `DrainReadChildForLease`/`PublishReadChildLease`（业务队列）与 `RecycleReadChildLease`
  （urgent 队列）及组合入口 `recycle_published_child_lease`（retire + recycle，`Ok(true)` 证明
  全量释放）；
- `read_session` 成功路径改为 lease 流：`drive_reads` 成功 → `drain_read_child_for_lease` →
  `publish_read_child_lease` → 发 `ReadDone` → 收 `Done`，返回 `ChildTransportSuccess` 携带
  `PublishedChildLease { child_id, span }`。publish 之后的失败路径 owner 保留（内容已 final，
  调用方仍可消费后通过 retained id 回收）；
- memory-to-memory harness `read_session_harness`：测试可执行文件拉起独立 Child 进程，使 Parent/Child
  分别拥有 provider context 和 RM Jetty；控制面走 loopback TCP version-5 协议并交换真实 descriptor，
  验证 handshake → 双向 import → source 注册 → READ → drain → publish → CPU 消费 span 内容比对 →
  recycle 预算释放 → lane/fabric shutdown。门禁：`#[ignore]` +
  `URMA_TEST_DEVICE`/`URMA_TEST_EID_INDEX` 环境变量。

新增测试：ChildOwner 三阶段 lease 生命周期（drain→publish→retire 后 reap 仍 Pending→recycle
释放）、全量 URMA 测试 192 passed、0 failed、1 ignored（harness 门禁）。

真机首跑（2026-09-21）：设备 `udmac0d1e2` 上 fabric 启动、lane 建链、握手均通过，
`register_read_source` 返回 `Status(-5)`（-EIO）。根因：本仓库 shim 直接注册调用方
`Vec<u8>` 堆地址（仅 16B 对齐），provider 的 `urma_register_seg` 拒绝非页对齐 VA 返回
NULL；lab probe shim 则用 `posix_memalign(4096)` + memcpy 注册私有对齐副本。修复：
`shim.c` 的 `dfurma_read_source_register` 改为内部 4096 对齐拷贝注册（结构体新增
`memory` 字段，token 分配失败与 VA 溢出回滚均释放副本，`release_after_revoke` 释放），
Rust 侧 `ReadSource::register` SAFETY 注释同步更新为"synchronous copy"契约。

后续真机复核（2026-09-21）：原单进程 harness 的两个 lane 实际复用 process-wide RM Jetty，
Parent/Child descriptor 中的 jetty id 相同；`import_jetty` 因把本地 Jetty 导入自身 context 返回
`-EPERM`。此前记录的“全链路 passed”不能作为有效的双端 provider 证据，现予以撤销。harness 已改为
两个 OS 进程和两个独立 provider context，待按本文末命令复跑。

下一步：接生产 listener 切换实验。

## 第十八批：生产 Parent/Child 接线与审查修复（2026-09-21）

本批把 version 5 READ lane 接到 Dragonfly 的 Parent listener、Child downloader 和 Storage finish，随后按
代码审查结果补齐安全门禁与失败回收：

- Parent listener 通过 DFUR envelope version 区分 READ lane，动态接收 `BufferReady`，解析三类 Piece
  metadata，注册 source 并发送 `SegmentOffer`；Child 缓存每个 Parent 的 READ lane，取得 destination
  lease 后直接执行 positional write、CRC32 校验、metadata commit，READ 失败回到整 Piece TCP fallback；
- 修复 Storage blocking worker 捕获 `*const u8` 导致 `--features urma` 无法编译的问题。跨线程只传递地址
  整数，lease future 在 worker join 前保持 owner；future 被取消时 armed lease fail-closed 保留 owner；
- `UrmaReadPieceLease` 增加 armed/disarmed 状态，成功 recycle 后不再错误打印“未回收 lease”告警；
- Child retained owner 明确区分 `Cleanup` 和 `PublishedLease`。`ReadDone` 发送失败、`Done` 丢失、terminal
  校验失败和 `control.finish` 失败均保留正确 cleanup 身份；客户端保存并在后续 transfer/close 重试，
  不再把 retained id 转成普通错误后丢失；
- client/server 在握手失败、缓存淘汰、lane error 和 listener lane 退出时调用 `close_lane`。Parent transfer
  失败后注册过的 source 按 lane 保存，lane 关闭后才在 provider gate 下重试 unregister/release；
- source shim 仍使用 `posix_memalign(4096) + memcpy` 的临时 staging 实现，但 Rust 调用方 Piece buffer 只在
  同步注册调用期间存在；注册完成后立即释放 caller memory，只保留 Storage keepalive。活跃 source 不再
  同时长期持有两份整 Piece 内存。该路径仍有一次 source-to-registration copy，尚未达到 R4 的正常路径
  零拷贝验收条件；
- READ source token 和 Child transfer id 改为 checked allocator，耗尽时停止 admission，不再回绕到零或
  复用旧 identity；新增 token exhaustion 测试；
- 新增 `read.providerRevocationValidated`，默认 `false`。未显式声明部署 provider 已通过独立撤权门禁时，
  READ runtime 可以用于 gated harness，但 discovery 的 `read_port` 保持 0，生产 READ lane 不发布。
  `ReadDone/CancelDrained` 本身仍不构成撤权证明。

离线验证：

```text
cargo test --offline -p dragonfly-client-storage --features urma --lib urma::
191 passed, 0 failed, 1 ignored

cargo fmt --all -- --check
passed

git diff --check
passed

cargo check --offline -p dragonfly-client --features urma
passed（将只读 registry 中的 dragonfly-api 复制到 /tmp，并通过临时 crates.io patch 运行）
```

直接执行完整 check 仍会被上游 `dragonfly-api` build script 写只读 Cargo dependency source 的
`src/descriptor.bin` 阻断；使用内容相同的 `/tmp` 可写副本后，完整 `dragonfly-client --features urma`
已经通过，并额外发现、修复了 `PieceKind` 未公开 re-export 和 `UrmaReadPieceLease` 缺少受约束 `Sync`
实现两处只有顶层 client 编译才会暴露的问题。

下一步先在真机验证 provider source revoke gate（旧 descriptor/token 在 unregister 后必须不可访问，并覆盖
outstanding READ 与 revoke 竞争），通过后才设置 `providerRevocationValidated: true` 做单机生产链路验证。

## 第十九批：真机 harness 双进程修正（2026-09-21）

真机执行旧 harness 时，Child `import_jetty` 返回 `-EPERM`，日志同时显示
`local_jetty_id == remote_jetty_id`。根因是 runtime 按进程共享一个 RM endpoint；在同一
`UrmaFabric` 上连续 `create_lane` 只创建两个逻辑 peer，返回的仍是同一个本地 Jetty descriptor。
provider 拒绝 self-import 是合理行为，与用户权限和 READ capability 无关。

`read_session_harness` 已改为父进程监听 loopback TCP并拉起同一测试可执行文件作为 Child。两个进程分别
启动 `UrmaFabric`、创建 RM endpoint，通过生产 version-5 handshake 交换真实 descriptor，再执行完整
READ session。后续发现该 provider 不支持同 EID loopback 后，测试名改为
`rm_read_memory_to_memory_session`，并增加下文的双节点角色：

```bash
export UMDK_INCLUDE_DIR=/usr/include/ub/umdk/urma
export UMDK_LIB_DIR=/usr/lib64
export LD_LIBRARY_PATH=/usr/lib64:${LD_LIBRARY_PATH:-}

URMA_TEST_DEVICE=udmac0d1e2 \
URMA_TEST_EID_INDEX=1 \
cargo test --release -p dragonfly-client-storage --features urma \
  rm_read_memory_to_memory_session -- --ignored --nocapture --test-threads=1
```

离线完成 `cargo fmt --all -- --check`、`git diff --check`，并在本地 UMDK headers/library 下完成 harness
测试可执行文件的编译链接。由于开发机没有真实 provider，本批的运行结果仍待目标机回填。该 harness 只证明
受控 Child 的协作式 unregister 生命周期，不能替代旧 descriptor/token 撤权测试；
`providerRevocationValidated` 必须继续保持 `false`。

同机双进程复跑（2026-09-21）：Jetty 已变为两个独立对象（Parent 1073、Child 1072），但
`udmac0d1e2` provider 仍在同一 EID 的 remote Jetty import 上返回 `-EPERM`。因此该设备不支持
RM loopback；增加进程隔离只能解决 self-import，不能把同一个物理 EID 变成合法远端。与此同时，双节点
`urma_perftest read_bw` 已通过 256 MiB、256 iterations，平均带宽 48073.96 MB/s，证明跨节点 RM READ
与 256 MiB capability 正常。

harness 新增显式双节点角色，并用第二条 TCP 连接回报 Child 已完成内容校验、lease recycle 和 shutdown；
Parent 收到完成标记后才关闭 lane，避免 `Done` 写入与控制连接关闭竞态。先在两端完成编译，然后运行：

Parent 节点（先启动，`141.61.17.196` 替换为 Parent 控制网 IP）：

```bash
URMA_TEST_DEVICE=udmac0d1e2 \
URMA_TEST_EID_INDEX=1 \
URMA_TEST_ROLE=parent \
URMA_TEST_CONTROL_ADDR=0.0.0.0:31912 \
cargo test --release -p dragonfly-client-storage --features urma \
  rm_read_memory_to_memory_session -- --ignored --nocapture --test-threads=1
```

Child 节点：

```bash
URMA_TEST_DEVICE=udmac0d1e2 \
URMA_TEST_EID_INDEX=1 \
URMA_TEST_ROLE=child \
URMA_TEST_CONTROL_ADDR=141.61.17.196:31912 \
cargo test --release -p dragonfly-client-storage --features urma \
  rm_read_memory_to_memory_session -- --ignored --nocapture --test-threads=1
```

控制端口只传 version-5 descriptor/control frame 和测试完成标记，Piece 数据由 URMA READ 搬运。两端均
显示 `passed` 才算 Dragonfly RM READ session 真机路径通过。同机无显式角色的模式仅保留给支持 RM
loopback 的 provider，不作为当前 `udmac0d1e2` 的验收方法。

双节点首跑继续在 import 阶段返回 `-EPERM`，descriptor 显示 harness 使用 `tp_type=Rtp`；对应成功的
`urma_perftest read_bw` 命令实际带 `--ctp`。根因是 harness 的 `harness_fabric` 硬编码 RTP，而生产
`UrmaServer` 虽已支持 `tpType: ctp`，默认值仍是 RTP。修复如下：

- harness 改用 `TpType::Ctp`，双端 capability/descriptor 会协商并编码 CTP；
- 本 RM READ 分支的生产配置默认 `tpType` 改为 `ctp`，仍允许显式配置 `tpType: rtp`；
- downloader 与 server 已经从同一 `storage.server.urma.tpType` 映射到 runtime，无需额外接线；
- 配置测试 5 passed，harness 测试可执行文件编译链接通过，fmt 与 diff check 通过。

下一次双节点运行应先确认错误上下文显示 `tp_type=Ctp`；若仍在 import 阶段失败，再对比两端 EID、
provider profile 和 perftest 的完整参数，不再回退到 RTP。

CTP 双节点复跑显示 Parent 已完成 import 并进入 `accept_transfer`，但 Child import Parent 返回
`-EPERM`；Parent 随后的 `READ accept timed out` 是 Child 未能发送 `BufferReady` 的连锁结果。对照成功的
perftest 后确认它使用的是 `--tp_aware --ctp` 路径：先按 local/peer EID 调 `urma_get_tp_list`，再将本地
`tp_handle` 和随机 tx PSN 传给 `urma_import_jetty_ex`。Dragonfly shim 此前只设置 CTP priority 和
`rjetty->tp_type`，最终仍调用普通 `urma_import_jetty`，并没有实现 TP-aware CTP import。

修复 `dfurma_jetty_import`：UB/CTP 分支构造 RM+CTP `urma_get_tp_cfg_t`，以本地 Jetty EID 和 descriptor
中的远端 EID取得唯一 TP handle，再调用 `urma_import_jetty_ex`；RTP 继续使用普通 import。CTP 不需要像
RTP TP-aware 模式交换 peer TP handle，与 perftest 行为一致。离线 URMA 测试结果：191 passed、0 failed、
1 ignored；harness 测试可执行文件编译链接、fmt 和 diff check 通过。待双节点再次验证真实 import 和 READ。

同时调整 harness 与生产 server 的握手顺序：Parent 创建 lane 后先发送携带本端 descriptor 的
`Connected`，再 import Child；Child 收到 descriptor 后 import Parent。这样双方都已取得 opposite EID，
可以并发执行 `urma_get_tp_list`/extended import，符合 perftest 在双方 TP info 就绪后连接的前置条件。
若任一端 import 失败，TCP 随 lane 失败关闭，另一端仍按 fail-closed 路径退出。

## 第二十批：READ lane 单向 native import（2026-09-21）

提交 `9aab3a5` 后的双节点复跑仍表现为 Parent 已进入 `accept_transfer`，Child 的 CTP
`import_jetty_ex` 返回 `-EPERM`，随后 Parent 报 `READ accept timed out`。后者只是 Child 未完成建链的
连锁错误，不是监听或控制地址问题。

对照 Mooncake 的 `urma_endpoint.cpp` 后确认：Mooncake 交换 peer EID 与 Jetty ID，构造 RM+CTP
`urma_rjetty_t`，并在连接两端各执行一次 import。Mooncake endpoint 同时承载双向 READ/WRITE，因此两端
都需要 remote target；其普通 `urma_import_jetty` 在当前 UMDK 中也只是以零 active TP 配置转调
`import_jetty_ex`。Mooncake 固定 priority 1 是其部署参数，不能覆盖本设备 perftest 明确要求的 CTP
priority 6，也不能替代 Dragonfly 已实现的 `get_tp_list + import_jetty_ex` TP-aware 路径。

Dragonfly 当前 READ lane 的方向固定：Parent 注册并暴露 source，只有 Child import Parent、提交 READ WR
并消费 CQE。Parent import Child 形成的 reverse target 从未被任何 READ 操作使用。真机日志恰好显示这个
无用的 Parent import 先成功，随后真正需要的 Child import 被 provider 拒绝。提交 `987450a` 已从
harness 和生产 `UrmaServer` 删除 Parent 侧 native import，同时保留现有 wire descriptor 双向交换和
capability 协商；Child import 时继续校验 Parent 的 transport、TP type 与 EID，Parent 收到的 Child
descriptor 在固定方向的 READ lane 中不再用于 native 操作。Parent lane 仍提供 peer generation，用于
source owner 的身份绑定和回收。`PeerTarget` 在 Created
状态可进入 drain/close，source register 只依赖本地 Jetty 与有效 peer generation，因此该单向生命周期能
正常注册 source 并安全关闭。

Child 侧继续使用与成功 perftest 对齐的 CTP TP-aware import。离线验证：URMA 模块 191 passed、0 failed、
1 ignored；`cargo fmt --all -- --check` 与 `git diff --check` 通过。下一次双节点复跑使用原命令即可；预期
Parent 不再占用 reverse target，Child 是整条 lane 唯一的 `import_jetty_ex` 调用者。若 Child 仍返回
`-EPERM`，下一步应记录 `get_tp_list` 返回的 TP 数量/handle 和 import active cfg，逐项对照 perftest，
不再把 Parent timeout 当作独立故障。

## 第二十一批：CTP remote Jetty 归一化与分阶段诊断（2026-09-21）

提交 `987450a` 双节点复跑后，唯一的 Child import 仍返回 `-EPERM`，证明无用的 Parent reverse import
不是该 provider 拒绝的根因；Parent `READ accept timed out` 仍是 Child 建链失败后的连锁结果。

继续逐字段对照成功的 `urma_perftest --tp_aware --ctp` 和 Mooncake：普通单设备 perftest 只交换
`urma_jetty_id_t`，Mooncake 也只交换 peer EID 与 Jetty ID；两者都从零构造最小 `urma_rjetty_t`。
Dragonfly 此前交换 `urma_get_rjetty` 返回的完整 descriptor，并将其中 flag/policy 原样传给
`urma_import_jetty_ex`。提交 `853b537` 对没有 provider extension 的 descriptor 只保留 remote
EID/Jetty ID，并从零构造 RM+JETTY+CTP rjetty，与上述两个已知实现对齐；带 extension 的 bonding
descriptor 继续保留完整内容。

同一提交增加 failure-only import diagnostics。错误不再统一显示为 `import_jetty failed`，而会明确标出
`get_tp_list`、`import_jetty_ex` 或普通 `import_jetty` 阶段，并输出 native status、errno、TP count、
TP handle、tx PSN 以及 local/peer EID；bearer token 不进入日志。下一次真机失败日志已经足以判断是 EID
到 TP 的查询失败，还是 kernel/provider 拒绝 extended import。

离线验证：URMA 模块 192 passed、0 failed、1 ignored；release harness 编译链接通过；fmt 与 diff check
通过。真机继续使用原双节点命令。若仍失败，保存 Child 的完整新错误行，并将其中 local/peer EID 与两端
`urma_admin show`（或等价 EID 查询）结果核对。

Child 使用 `853b537` 复跑后，新诊断确认 `urma_get_tp_list` 返回 `status=-1, errno=ENOMEM`，并未进入
`urma_import_jetty_ex`。失败方向为 local EID `00000000003f020000100000df004b00` 到 peer EID
`00000000003f020000100000df000b00`，TP count 仍为输入值 1，handle 为 0。

提交 `da5c239` 增加受限 fallback：只有 `get_tp_list` 明确返回 ENOMEM、且尚未取得 TP handle/创建 target
时，才按 Mooncake 路径调用普通 `urma_import_jetty`，由 provider 自动选择 CTP。其他 TP 查询错误直接失败；
取得 handle 后的 extended import 失败也不重试，避免不确定 native ownership 下重复 import。若自动路径也
失败，错误阶段会显示 `ctp_auto_import_after_get_tp_enomem`。离线 URMA 测试保持 192 passed、0 failed、
1 ignored，release harness 编译链接、fmt 和 diff check 通过。

## 第二十二批：dfdaemon 端到端与 B7 自动化验证（2026-09-21）

使用 B7 集群验证框架（urma-b7）新增 `read` profile，完成 dfdaemon 双节点端到端验证。READ profile 复用
RM 传输模型（transportMode=rm、tpType=ctp），仅注入 `storage.server.urma.read.*` 配置段； Eid 强制
eid0（集群 CTP 资源只绑 eid0，eid1-7 为地址表项，见第二十一批诊断）；provider 探测去掉 `--tp_aware`
与 `-p`，对齐第二十一批验证的自动 CTP 导入路径；crossNodeProbe 直接归档 passed（手动双节点证据）。

联调过程中定位并修复的问题：

1. 端口冲突：parent 44000 被宿主机 cinder-api 以临时源端口占用（Linux 临时端口范围 32768-60999 覆盖
   原端口段），B7 全部监听端口下移至 24000/24100 段。
2. upload server IP：B7 只 patch 各 server 段 port 不 patch ip，child 广播了错误 IP；改为所有 server
   段统一注入节点 IP。
3. 证据误报：B7 的 URMA 证据判定是 SEND/RECV 时代的标志行，READ 路径（`urma READ piece attempt` /
   `urma READ source fully read; revoking export`）互不为子串，增加 READ 联合标记。
4. 隐性 TCP 回退：`read.maxOutstandingPerPeer`（默认 8）语义为 per-peer 跨所有并发 piece transfer
   共享的 WR credit 池，cc8 × 16MiB / 4MiB = 32 需求超过供给，超限 piece 报 `READ WR credits exhausted`
   后回退 TCP，吞吐被 1G 网卡线率钉死在 ~119MiB/s。B7 现注入 256（共享池容量为 send_jfc_depth≈4096，
   安全），并修正配置文档注释。
5. piece length 上限：scheduler 侧 proto 校验 `Download.PieceLength ∈ [4MiB, 64MiB]`（d7y.io/api PGV
   规则）为硬约束，B7 校验保持 64MiB 对齐 fail-fast；>64MiB 需 fork d7y.io/api 与 dragonfly-api 双端
   重建，暂不立项。

端到端结果（全部 READ 数据面、零 TCP 回退、证据完整）：

```text
read-smoke-post1-pipe1（4MiB × 256 串行）   ~40MB/s        p50 e2e 103.7ms/piece
read-piece16-cc8-post1-pipe2（16MiB × cc8）  900MiB/s      142ms/piece（8 并发槽各 ~112MiB/s）
read-piece64-single（64MiB × 1 次）          507MiB/s      dfget 全程 126ms，startToFirst 123ms
```

分析：单 piece 控制面固定开销约 100ms 量级（child/parent 每 piece 整块注册/注销 MR、parent source
拷贝进页对齐注册缓冲、双端 digest），与裸 perftest CTP read_bw ~48GB/s 相差约 90×。吞吐 = 并发 ×
piece 大小 / 固定开销，piece 越大摊销越充分。下一批优化目标：注册缓冲池化复用，消除每 piece 的
register/unregister 与重复页表操作。

B7 侧离线验证：110 passed。新增 read 三个 case（smoke / cc8 / 64m single）与 `urmaRead` case 级
覆盖（预算、perPeer、quarantine、maxOutstandingPerPeer、maxReadSize）。

## 第二十三批：READ destination 注册缓冲池化（2026-09-21）

目标：消除每 piece 的 `dfurma_read_buffer_create` / `dfurma_segment_delete`（分配+注册 → 注销+释放）
固定开销。基线显示单 piece 控制面 ~100ms，其中注册/注销是主要组成；池化后同尺寸 piece 直接复用
已注册缓冲，不再触碰 provider MR 生命周期。

实现（dragonfly-client-storage）：

1. 新增 `urma/read_buffer_pool.rs`：owner 线程内 `ReadBufferPool`，按 allocation 长度分段
   （BTreeMap<u64, Vec<SegmentHandle>>）保留已注册缓冲。`take` 命中即复用、未命中回落
   `create_read_buffer`；`put` 超过保留上限返回 `Err(buffer)` 由调用方真实注销。
2. 保留上限 = destination budget（默认 1GiB）。池化字节不再计入 owner registry 预算，最坏
   pinned = in-flight（≤ destinationBytes）+ pool（≤ destinationBytes）= 2GiB，与已验证的
   memlock 上限一致。
3. `NativeChild` 持有 `Rc<RefCell<ReadBufferPool>>`（与 `RuntimeReadState::Active` 共享），
   `close_buffer` / `close_lease` 统一走 `release_local`：健康缓冲入池，uncertain 创建
   （`buffer_uncertain`）的缓冲永不复用、只走真实注销。
4. import 失败的提前清理路径同样归还池（本地缓冲健康，与远端描述符无关）。
5. Drop 顺序天然正确：`read` 字段声明在 `native` 之后，runtime 关闭时池内缓冲先于 native
   runtime 释放。
6. `Runtime::create_read_child` → `ReadOwners::create_native_child` → `NativeChild::create`
   透传池句柄。

离线验证：`cargo check --features urma` 通过；全工作区 `cargo test --features urma` lib 全绿
（storage 307 = 原 305 + 池行为单测 2），仅 dragonfly-client-util 3 个历史 doctest 失败（文档
伪代码被 doctest 编译，与本次无关）。本机 UMDK 源码树（workspace/cloud-native/umdk）+ 头文件目录
`src/urma/lib/urma/core/include` + `build-perftest` 产物可完成离线 urma 编译与单测，无需真机。

待办：真机 B7 基线对比（read-piece16-cc8 / read-piece64-single），观察 p50 per piece e2e 是否
从 ~100-142ms 显著下降；parent source 侧池化同理可做，视 destination 收益决定。

### 第二十三批勘误（评审修正，2026-09-22）

1. **shutdown 生命周期（真 bug，已修）**：`UrmaRuntime::shutdown_inner()` 成功分支显式
   `native.close()` 时，`RuntimeReadState::Active` 仍持有带 pooled `SegmentHandle` 的池；
   `SegmentHandle` 带 Drop，Runtime 后续 drop 会对已关闭 runtime 发起 `dfurma_segment_delete`。
   修复：shutdown 成功分支在 native.close() 之前显式 `ReadBufferPool::drain()` 并逐个 close，
   失败计入 shutdown failures。poisoned 分支不清理（native 未关闭，依赖既有 drop 顺序保障，
   read 字段声明在 native 之前 drop）。
2. **pinned 上限表述修正**：2GiB 仅是 child destination 子系统上界（in-flight ≤ destinationBytes
   + pool ≤ destinationBytes）。进程级 registered/pinned 还需叠加 parent source 注册
   （≤ sourceBytes）与 RM legacy buffer_pool，不能宣称全局 2GiB 上限。
3. **方案 A 可行性修正**：READ WR 已支持 `local_offset`（`ReadRequest`/`post_read` 直传），
   child 侧"大 MR + range allocator"技术上无需改 provider/shim。当前选 free-list 池是改动
   最小的决策，非技术不可行；真正复杂点在 parent 侧 exact-Piece descriptor/token/revoke 隔离。
4. **复用资格不变量固化（测试补充）**：复用资格并非只看创建态 `buffer_uncertain`，生命周期
   退休证明已由结构保证——`close_buffer` 唯一入口 `reap()` 被
   `lost_handle/pending 非空/published` 三重门拦截（PostUncertain 的 WR 先入 pending，
   WrongProof 的身份不匹配 WR 不移除 pending，均永久阻塞 reap）；`close_lease` 唯一入口
   `recycle_lease` 要求 `read_succeeded` + import 已关。新增两个 mock 测试固化：
   `uncertain_post_blocks_pool_entry_until_cqe_retires_the_wr`、
   `failed_read_retires_into_the_pool_release_path`（CQE 即退休证据，失败 piece 缓冲可安全入池）。
5. **第一轮真机验证重点**：pool hit/miss、实际 register/unregister 次数、registered bytes、
   Piece E2E p50/p95、aggregate throughput，据此评估 Child registration 贡献再决定 parent
   source 池化。

### 第二十三批补充：第一轮真机验证观测埋点与监控脚本（2026-09-22）

代码埋点（debug 级，B7 `--log-level debug` 可采集）：

1. pool 四标记（`read_buffer_pool.rs` / `read_child_owner.rs`）：
   - `urma READ pool miss; registering destination`（注册，带 length/retained_bytes）
   - `urma READ pool hit`（复用命中）
   - `urma READ pool returned`（入池，带 retained_bytes）
   - `urma READ pool bypass; unregistering destination`（真实注销）
2. parent source 计时：`urma READ source fully read; revoking export` 增加 `source_e2e_ns`
   字段（`server/urma.rs`，start 标记到 fully read 的单 piece source 侧 E2E）。
3. child piece E2E 复用既有 `finished dragonfly urma READ piece attempt` 的
   `child_piece_e2e_ns`（piece.rs，debug 级）。

监控脚本：`urma-b7/read_pool_monitor.py`
- 用法：`python3 read_pool_monitor.py results/<runId>/`（递归找 daemon 日志）或显式传日志
  文件；`--json` 输出机器可读。
- 输出：register/miss、hit/hit-rate、returned、unregister/bypass、per-length 注册与命中
  分解（extra registers beyond first）、peak/final retained bytes、piece E2E p50/p95/mean、
  log span 聚合吞吐、tcp fallback 计数；parent 侧 pieces served、source E2E p50/p95、
  retained warnings。
- 已用合成日志自测解析路径。

验证：storage urma 309 passed；`cargo check --features urma` 通过。
真机流程：同步代码 → 两节点 `cargo build --release --features urma` → B7 跑
read-piece16-cc8-post1-pipe2 / read-piece64-single → `read_pool_monitor.py results/<runId>/`
对照第二十二批基线（16MiB cc8：900MiB/s、p50 142ms；64MiB 单次：507MiB/s、126ms）。

### 第二十三批真机结果：destination 池化第一轮验证（read-pool-001，2026-09-22）

配置：read-piece16-cc8-post1-pipe2，4GiB 任务（warmup + 3×1GiB sample），cc8 × 16MiB × 256 pieces。

池行为（符合设计预期）：
- 全程注册仅 8 次（= cc8 并发度），warmup 后三个 sample hit rate 均 100%
- unregister (bypass) = 0；peak retained = 128MiB（= 8×16MiB working set），无泄漏
- warmup 8 miss / 56 hit（87.5%），首 8 个并发 piece 建池后完全复用

性能对比基线（池化前，第二十二批）：
- piece E2E p50：142ms → ~86ms（-39%，三 sample 84.8~86.2ms）
- 聚合吞吐（单 sample，log span）：~900MiB/s → ~1545MiB/s（+72%）
- 数据完整性：256/256 成功，0 失败，0 TCP 回退；parent retained warnings = 0
- 注意：child.log 全程聚合吞吐 158.9MiB/s 是 log span 含 sample 间隔的失真值，以单 sample 为准

剩余瓶颈转移：child p50 86ms，parent source E2E p50 ~77ms。parent 每 piece
mmap/read → 拷贝 16MiB → 注册 source MR → 等 ReadDone → 注销/revoke 成为下一优化目标，
方向为 parent source 缓冲池化（与 destination 池对称）。

### 第二十三批补充：parent source 侧 Stage 0a + 0b（2026-09-22）

方案分析结论：parent per-piece 注册成本 = copy#1（我们侧 Vec，含首触缺页）+ shim 内部
posix_memalign(16MiB) + memcpy(copy#2，首触缺页) + urma_alloc_token_id + urma_register_seg
（pin 4096 页）+ unregister（unpin）+ token 释放 + free。注册内存归 shim 所有且与
per-piece token/revoke 1:1 绑定，不能照搬 child 侧 Rust 层池化；exact-Piece 隔离
（revoke 后远端不可再读）为安全红线，持久 slot 复用会扩大授权面为 slot-window。

本轮实施（Stage 0a + 0b）：
1. Stage 0a — Mapped 直通消 copy#1：`ReadSourceMemory::Mapped` 改持 `content::MappedPiece`
   （storage map_upload_piece 为 piece 精确映射），READ source 路径 Mapped 分支跳过
   16MiB Vec 拷贝直接传 shim（shim 同步拷贝后 backing 即弃，munmap 时机安全），
   Reader 分支保持 Bytes。长度校验保留。
2. Stage 0b — source 生命周期分阶段计时：
   - `PendingSourceRevoke` 新增 `register_ns`（fabric.register_read_source：
     aligned copy + token + MR 注册）与 `wait_read_done_ns`（offer 后等 ReadDone）
   - `fully read` 日志行新增 `source_open_ns / source_copy_ns / register_ns /
     wait_read_done_ns`
   - 新标记 `urma READ source revoke finished`（`revoke_ns`：unregister + revoke 证明 +
     token 释放）
   - `read_pool_monitor.py` parent 段输出五阶段 p50/p95（open/copy/register/wait/revoke）
3. 真机判据（用户定版）：Stage 0b 数据若显示 alloc/free/first-touch 占比明显 → Stage 1
   shim backing-memory pool（shim 内部按长度分段缓存已释放注册内存，无 API 变化，
   安全模型不变）；若 register/unregister（pin/unpin）本身才是大头 → Stage 1 收益有限，
   才重新评估 persistent registered slot（trusted 门控）。

验证：storage urma 309 passed；`cargo check --workspace` 通过；监控脚本合成日志自测通过。

### 第二十三批真机结果：parent source 分阶段计时（read-src-001，2026-09-22）

配置同 read-pool-001（16MiB × cc8 × 256 pieces）+ Stage 0a Mapped 直通。

Stage 0a 验证：copy p50 = 0.0ms（n=256），copy#1 完全消除；open p50 0.05ms。
child 侧 p50 88.96ms、单 sample 吞吐 1440~1479MiB/s（与 read-pool-001 的 86.76ms /
1501~1557MiB/s 同量级，噪声范围内），0 回退 0 失败。

parent 分阶段（p50 / p95，n=256）：
- open            0.05ms / 0.11ms
- copy            0.0ms  / 0.0ms   （Stage 0a 生效）
- register       43.57ms / 75.85ms ← 主导项
- wait ReadDone  35.47ms / 71.4ms
- revoke/unreg    3.85ms / 81.68ms（p50 便宜，p95 长尾与 register p95 同源）

分析：
1. register_ns 含 owner 线程排队（cc8 并发），按吞吐反推单次实际执行 ≈5-10ms，
   但全链路（排队+执行）仍是当前主导项。
2. register 内部 = alloc+首触缺页（估 10-20ms）+ copy#2（3-4ms）+ token（1ms）+
   urma_register_seg pin（估 20-30ms），尚未实测拆分。
3. 按定版判据，register/unregister 是大头 → Stage 1 收益有限。但 Stage 1 vs
   persistent slot 的最终选择先做 shim 亚阶段计时（posix_memalign/memcpy/token/
   register_seg 四段 clock_gettime + getter 入日志）把决策数据化，预期 pin 占大头。
   → 该预期已被第二十五批真机数据推翻（pin 仅 0.18ms），Stage 1/2 双双否决。

### 第二十四批：Parent source register 亚阶段计时（离线实现，2026-09-22）

目的：把 `register_ns`（第二十三批 p50 43.57ms / p95 75.85ms）拆成 alloc / copy#2 /
token / MR pin 四段，为 Stage 1（shim backing-memory pool）与 Stage 2（persistent
registered slot）的选择提供数据。

实现：
1. C shim（`ffi/shim.c`）：
   - `struct dfurma_read_source` 新增 `register_alloc_ns / register_copy_ns /
     register_token_ns / register_seg_ns`
   - `dfurma_read_source_register` 用 `clock_gettime(CLOCK_MONOTONIC)` 分别包住
     `posix_memalign`、`memcpy`（copy#2，含首触缺页）、`urma_alloc_token_id`、
     `urma_register_seg`；失败分支在返回前即记好已发生的阶段
   - 新增 `dfurma_read_source_register_stages(source, out)`：仅观测，NULL 参数返回
     -EINVAL，不参与任何生命周期/准入/撤权判断
2. `ffi/shim.h`：新增 `dfurma_read_source_stages_t` DTO 与 getter 声明；bindgen 自动生成。
3. Rust 透传：
   - `ReadSourceStages` + `ReadSource::register_stages()`（`ffi/read/source.rs`）
   - `SourceOwner::register_stages()`、registry `source_register_stages()`（owner 线程读）
   - `ReadOwners::source_register_stages()` → `ReadSourceOffer.register_stages`
     （runtime.rs 侧读取失败按 Default 归零，绝不使 transfer 失败）
   - `PendingSourceRevoke.register_stages` → `server/urma.rs`
     `urma READ source fully read; revoking export` 新增 `register_alloc_ns /
     register_copy_ns / register_token_ns / register_seg_ns`
4. `read_pool_monitor.py`：parent 段 register 阶段下单列四段 p50/p95（旧日志无这些
   字段时自动跳过，兼容第二十三批日志）。
5. C 替身测试新增 `test_source_register_stage_timings`：NULL 参数拒绝、四段之和不超过
   同一单调时钟的外层窗口、至少一段非零、且读取计时不影响 unregister/release 生命周期
   （11 组通过）。

验证（离线，UMDK build-perftest）：storage urma 309 passed（基线不变）；C shim 替身
测试 11 组 PASS；监控脚本合成日志自测输出四段 p50/p95 正常。

真机步骤：两节点 `cargo build --release --features urma` → B7 跑
read-piece16-cc8-post1-pipe2 → `read_pool_monitor.py results/<runId>/`，与 read-src-001
（register p50 43.57ms / p95 75.85ms）对照，用四段占比做 Stage 1 vs Stage 2 判据。

### 第二十五批：register 亚阶段真机结果 → Stage 1 / Stage 2 双双否决（2026-09-22）

真机：`read-src-002`（read-piece16-cc8-post1-pipe2，parent.sample-001~003 + parent.log，
256/64 片，全 0 retained warning；三个 sample 与汇总高度一致，数据可信）。

观测（parent source 侧，p50/p95，ms）：

| 阶段 | p50 | p95 | 占 register p50 |
| --- | --- | --- | --- |
| register（含 owner 线程排队 + 往返） | 43.39 | 74.92 | 100% |
| ├ alloc（`posix_memalign`） | 0.00 | 0.01 | 0% |
| ├ copy#2（shim `memcpy` 16MiB） | 2.16 | 2.35 | 5.0% |
| ├ token id | 9.06 | 9.26 | 20.9% |
| ├ MR pin/register（`urma_register_seg`） | 0.18 | 0.33 | 0.4% |
| ├ shim 内小计 | 11.40 | 11.95 | 26.3% |
| └ 差值（排队 + dispatch + 往返） | 31.99 | 62.97 | 73.7% |

其余未变：stage open 0.05 / stage copy(Bytes) 0.0 / stage wait ReadDone 35.24 /
stage revoke 3.81（p95 81.45）；parent source E2E p50 79.57；child 侧
1467.1 MiB/s、piece p50/p95 88.44/167.54ms（与 read-pool-001 持平）。

结论与判据（用户定版判据的实测落点）：
1. **Stage 1（shim backing-memory pool）否决**：alloc 已归零，首触缺页确认落在 copy#2
   内（2.16ms ≈ 16MiB memcpy 带宽，缺页可忽略）。池化最多省 alloc + register_seg
   ≈ 0.18ms（0.4%），且 copy#2 无论是否池化都省不掉。
2. **Stage 2（persistent registered slot）否决**：其针对的 pin 成本实测仅 0.18ms（原估
   20-30ms 完全不成立），0.4% 收益换 trusted 门控 + 授权面扩大 + 预算模型 slot 常驻化
   不成立。
3. **新瓶颈排名**（两者同源，都在 owner 线程上）：
   a. **token id 分配 9.06ms/片**（占 shim 内 79%、占 register 21%），p95≈p50 说明是
      **固定成本**而非抖动；UMDK 链路 `udma_u_alloc_tid_common` → `ummu_allocate_tid`
      （libummu，不在源码树内）+ `urma_cmd_alloc_token_id` 内核 ioctl
      （`src/urma/hw/udma/udma_u_tid.c` L27-L59）。
   b. **owner 线程排队 ~32ms p50 / ~63ms p95**（74%）。`register_read_source` 走
      **非紧急** `submit`（`fabric.rs` RegisterReadSource 分支），而 retire/unregister
      走 `submit_urgent`；cc8 下数据面占满该单线程，8 条 register 串行排队。
4. **归因修正**：`register_ns` 是异步命令往返（排队 + dispatch + shim + 回包），shim 内
   小计 < register_ns 属预期，不是 instrumentation 缺陷；四段 p50/p95 极紧（抖动全在
   排队侧）。

E2E 意义：parent source E2E p50 79.57ms 占 child piece p50 88.44ms 的 90%
（register 43.4 + wait 35.2 + revoke 3.8 ≈ 82.5 与 source E2E 吻合）→ **parent register
路径已是端到端延迟主杠杆**，child 侧不再是限制。

候选下一步（本轮按用户决定仅记录、不改代码）：
- A（推荐）token id 池化/预分配：按 slot 持有已分配 token，撤销完成后才归还，逐片只换
  `token_value`（现请求里的 u32）；256 次分配降到 ≤8 次，预期 register p50 −9ms 起，
  且 owner 线程单次占用 11.4→2.3ms 会同步压缩排队。安全模型不变：slot 仍随 source 撤销、
  逐片 value 轮换、exact-Piece 隔离保持。
- B 先拆 owner 线程排队：在 owner 线程命令处理入口加 enqueue/dispatch 时间戳，把 32ms
  拆成纯排队与 dispatch，再评估 `RegisterReadSource` 提为 urgent（需防数据面饥饿）。
- C 先定位 `ummu_allocate_tid` 9ms 固定成本成因（需 UMDK/libummu 侧确认），再定池化形态。

→ 候选 A 的详细设计见第二十六批；设计过程中对 A 做了安全复核，**"复用 tid" 形态与
exact-Piece 红线冲突，已改判为"预取 fresh tid"形态**，并另立 P0 既有隔离输入复核项。

### 第二十六批：候选 A（token id 池化）实施方案与安全复核（设计，2026-09-22）

#### 26.1 收益模型（为什么值得做）

owner 线程是单线程串行执行所有 fabric 命令（`fabric.rs` `FabricCommand::RegisterReadSource`
→ `runtime.register_read_source`）。按 read-src-002 反推其占用：

- 每片 shim 内占用 11.40ms（token 9.06 + copy#2 2.16 + seg 0.18 + alloc 0.00）
- 每 sample 64 片 × 11.40ms ≈ 730ms ≈ 整段 700ms wall（1024MiB / 1467MiB/s）
- ⇒ **owner 线程被 token 分配单独吃掉约 79% 的占用，是 parent 路径的实际串行瓶颈**；
  32ms 排队（register p50 43.39 − shim 11.40）正是这个占用造成的，不是独立成本

因此把 9.06ms 移出 owner 线程后预期：register p50 43.4 → ~3ms，source E2E 79.6 → ~40ms；
child piece p50 88.4 → ~48ms（`wait ReadDone` 是否同步下降待测，见 26.6）。

#### 26.2 安全复核：为什么"复用 tid"形态被否决

原始设想是"池化复用 tid，逐片只换 token_value"。读完 UMDK 后该形态与 exact-Piece 红线冲突：

1. **tid 是硬件侧的远端访问键，不是纯本地句柄**：`udma_u_segment.c` 的 register 路径执行
   `ummu_grant(udma_tid->tid, seg->va, seg->len, perm, &seg_attr)`，即 tid→(VA,len,perm)
   的映射写在硬件里；`ummu_api.h` 的 `ummu_seg_attr` 只有 `token/e_bit/p_bit`，**没有
   per-peer 绑定**。
2. **UMDK 自己的仿真器明确警告 tid 必须与段一一对应**（`test/urma/dt/simulator/urma_sim_ummu.c`
   的假 `ummu_allocate_tid` 注释）：固定 tid 会让不同 token 注册的段"共享同一远端访问键，
   tid→段关联冲突：远端读写错地址、注销破坏他 token 映射"。
3. **token_value 只保证 process-unique，不保证不可预测**：`server/urma.rs` 的
   `NEXT_READ_TOKEN = AtomicU32::new(1)` 顺序自增（注释只说"process-unique so a stale offer
   can never arm a new export"）。
4. **子节点本来就拿到了 tid**：wire 描述符里带 `token_id`（shim `dfurma_read_source_descriptor`
   `out->token_id = seg->token_id`）。复用后子节点对后续片只需猜 value（= 自己那片的 value + k，
   全局计数器小步推进），加上同尺寸片 VA 大概率被复用（每片 `posix_memalign(4096, len)`
   后 free，同尺寸 mmap 常回到同一地址）⇒ **跨片读成立**。

结论：复用 tid 会把 exact-Piece 隔离的输入从 `(fresh tid, unique value)` 降级为只有
"顺序可猜的 value"，**否决**。若将来要做，必须先完成 26.5 的 P0（value 不可预测化）并单独评审。

#### 26.3 采用的形态：A7「预取 fresh tid」池（off-path prefill）

关键区分：红线要求的是"**每个 source 拿到一个从未被任何其他 source 使用过的 tid**"，
而不是"必须在 register 的关键路径上现调 `urma_alloc_token_id`"。因此：

**不变量（与今日完全一致，安全模型零变化）**
- 池中只存**从未注册过任何段**的 token id；一个 tid 只绑定一个 live source
- `retire` 仍 `urma_free_token_id`（已用 tid **绝不回流**）——这是与 A1 的本质差别
- 仍逐片 fresh VA + fresh tid + process-unique value

**收益来源**：不减少总分配次数（仍 1 片 1 次），只把 9ms 从 owner 线程关键路径搬到后台预取线程；
owner 线程单次 register 占用 11.4 → 2.34ms，排队随之塌陷。

**两个必须先验证的前提（否则只是"搬家"）**
- P-A：9ms 是否**可重叠**（等待型而非串行资源型）。后台线程需产出 1 token/11ms、单次 9ms
  → 利用率 82%；若 9ms 是串行资源，一个后台线程即成新瓶颈，收益不成立。
- P-B：`urma_alloc_token_id` 能否在**非 owner 线程**调用（`ctx->dev_fd` 上的 ioctl + libummu
  内部状态），同时 owner 线程在跑 register_seg；register_seg 仍严格只走 owner 线程。
- 归因提示：register_seg 全路径（`ummu_grant` + register_seg ioctl）仅 0.18ms ⇒ **ioctl 本身不慢**，
  9ms 几乎全在 `ummu_allocate_tid`（libummu 不在源码树内，需真机 strace/探针确认）。

#### 26.4 实施清单

**C shim（`ffi/shim.c` / `ffi/shim.h`）**
- `struct dfurma_runtime` 新增（首次引入跨线程共享状态，故加锁）：
  ```c
  #define DFURMA_READ_TOKEN_PREFILL_MAX 64
  pthread_mutex_t token_lock;
  urma_token_id_t *token_pool[DFURMA_READ_TOKEN_PREFILL_MAX];
  size_t token_pool_count;
  uint64_t token_acquire_hits;    /* 命中池的次数 */
  uint64_t token_alloc_on_path;   /* 回落 on-path 的 provider 分配次数 */
  uint64_t token_alloc_prefill;   /* 预取线程的 provider 分配次数 */
  ```
- `dfurma_read_source_register`：`urma_alloc_token_id` 换成
  ```c
  static urma_token_id_t *dfurma_read_token_acquire(dfurma_runtime_t *rt)
  {
      urma_token_id_t *tid = NULL;
      pthread_mutex_lock(&rt->token_lock);
      if (rt->token_pool_count > 0) {
          tid = rt->token_pool[--rt->token_pool_count];
          rt->token_acquire_hits++;
      } else {
          rt->token_alloc_on_path++;
      }
      pthread_mutex_unlock(&rt->token_lock);
      /* 池空时回落；9ms 的 ioctl 故意放在锁外，避免阻塞预取线程 */
      return tid != NULL ? tid : urma_alloc_token_id(rt->context);
  }
  ```
  `register_token_ns` 语义变为"仅回落路径的耗时"，steady state 应 ≈0。
- `dfurma_read_source_release_after_revoke`：**不改**（仍 `urma_free_token_id`），仅补注释说明
  "已用 tid 不得回流"。
- 新增 FFI：
  - `int dfurma_runtime_read_token_prefill(dfurma_runtime_t *rt, uint32_t target);`
    （只走 `token_lock` 与 `urma_alloc_token_id`，**不碰** segment_count/jetty 等 owner 线程字段；
    幂等，返回到池中的净增数或负 errno）
  - `int dfurma_runtime_read_token_stats(dfurma_runtime_t *rt, dfurma_read_token_stats_t *out);`
    → `{pool_count, pool_cap, acquire_hits, alloc_on_path, alloc_prefill}`
- `dfurma_runtime_close`：在现有 `segment_count == 0` 校验**之后**、`urma_delete_context`
  **之前**排空池（对池内每个 tid `urma_free_token_id`），仅成功路径销毁 mutex。
  `segment_count == 0` 恰好可证"无 source 仍持有 tid"（计数在 release 才递减）。
- `bindgen`：随 `shim.h` 自动生成。

**Rust**
- `ffi/runtime.rs`：`prefill_read_tokens(target) -> Result<u32>`、`read_token_stats()`。
- 新增预取 worker（建议置于持有 READ 侧 `NativeRuntime` 所有权的对象上，与 owner 线程句柄同作用域，
  以便在 `Drop` 中强制"先 join worker、再 close runtime"）：
  - 唤醒通道：`sync_channel(1)`；register 命中/消费后 `try_send(())`（满了就丢，池空自动回落，
    不阻塞、不 panic）；worker 循环 `while rx.recv().is_ok() { prefill(cap) }`，启动时先 `prefill(cap)` 预热
  - 跨线程只调用 `prefill` 一个入口，用带 `unsafe impl Send` 的原始指针包装 + 文档化生命周期论证
    （`dfurma_read_source_*` 一族仍是 `!Send/!Sync`，不受影响）
  - prefill 返回负值只记 debug 日志，绝不影响 transfer

**日志与监控**
- `server/urma.rs` 的 revoke 日志行追加累积字段：`token_pool_hits` / `token_alloc_on_path` /
  `token_alloc_prefill`
- `read_pool_monitor.py`：parent 段输出命中率与"每片增量"（末值 − 首值 / 片数），旧日志无字段自动跳过

**测试**
- C 替身（现有 11 组 → 13 组）：
  - 12 `test_read_token_prefill_and_acquire_accounting`：prefill 填池 → acquire 全命中
    （hits>0、on_path==0）；逐片 register/unregister/release 后已用 tid **不进池**；
    恒等式 `allocs == frees + pooled + in_flight`
  - 13 `test_read_token_prefill_fallback_and_close`：池空回落 on-path；`segment_count>0` 时 close
    仍 `-EBUSY`；排空后 close 成功且 `frees == allocs`（含池内预取项）
- Rust：worker 协议测试（`trait TokenPrefill` 注入计数替身）——通知→预取→退出、prefill 失败不
  panic、通道满不影响 register
- 离线：`cargo test -p dragonfly-client-storage --features urma`（309 基线不变）+ C 13 组 +
  `cargo check --workspace`

#### 26.5 前置探针（第 0 步，无代码改动，真机）

- (0a) 打印 `dev_cap.feature.bs.muti_seg_per_token_id`（见 26.7 A12）；若为 1 且语义等价于
  "每段独立远端键"，可能是不必新增线程的正规路径
- (0b) 9ms 归因：对 parent 跑 `strace -f -c -e trace=ioctl` 或按调用计数对比 register_seg 的 ioctl
  耗时，确认 9ms 落在 `ummu_allocate_tid` 而非 ioctl
- (0c) 重叠性 P-A：~20 行探针，两个线程对同一 ctx 各调 `urma_alloc_token_id` N 次，
  比较串行与并行的总耗时/p50；并行总耗时 ≈ 串行/N ⇒ 可重叠，A7 成立
- (0d) P-B：同探针中与 owner 线程并发跑 register_seg，观察是否报错/串行化；如不可并发，
  A7 需改由"预取 + 回落"退化为纯预热（收益大幅缩水）

**探针实现状态（2026-09-22）**

- 交付物：`/home/yuan/workspace/dev/dragonfly-urma-tools/urma-b7/token_probe.c`（单文件 C，不依赖 B7 框架，
  自开 ctx、不与任何对端通信、退出前释放全部 token/ctx/seg）
- 覆盖：0a `dev_cap.feature` 位打印 + `urma_alloc_token_id_ex(multi_seg=1)` 试探；
  0b 串行 `urma_alloc_token_id/free` 逐次计时（p50/p95/max/mean）与 `register_seg`（fresh tid）对照；
  0c T 线程对同一 ctx 并发 alloc/free，输出 `overlap_ratio = serial_total / parallel_total`；
  0d 后台 alloc storm 期间重测 `register_seg`（对比控制组），并单测"同一 tid 在 unregister 后复用"的
  API 合法性（顺带服务 A1 判断）
- 参数：`token_probe <device_name> <eid_index> <serial_iters> <threads>`，默认
  `udmac0d1e2 0 24 4`（threads 上限 16；`MAX_LIVE_TOKENS 64` 防 TID 耗尽）
- 判据：0c `ratio ≈ threads` ⇒ OVERLAPPABLE（A7 成立，9ms 可移出 owner 线程）；
  `ratio ≈ 1` ⇒ SERIALIZED（A7 只是搬家，需重新界定/A12 优先）
- 离线自检：已用 build-perftest 源码树编译链接通过（需 `-lurma_common -Wl,-rpath-link,.../urma/common`），
  运行至 `urma_get_device_by_name` 返回 `errno=19 ENODEV`（本机无 URMA 硬件，属预期，非探针缺陷）
- 真机命令（必须在 **parent 节点 141.61.17.196** 上跑，即 register 路径所在；跑前确认无 B7 任务在跑）：

```bash
scp token_probe.c root@141.61.17.196:/tmp/
ssh root@141.61.17.196 'cc -O2 -Wall -Wextra -I /usr/include/ub/umdk/urma /tmp/token_probe.c \
  -L/usr/lib64 -lurma -lurma_common -lpthread -o /tmp/token_probe && \
  LD_LIBRARY_PATH=/usr/lib64 /tmp/token_probe udmac0d1e2 0 24 4'
```

- 结论回填：本回合**未取得真机数据**——本机到 141.61.17.196/.198 的 ssh/ping 全部超时
  （sandbox 内外均已验证），故 26.5 的 P-A/P-B 仍待判定，A7 暂不进入编码。

**P0（独立于本优化的既有隔离输入复核，建议不晚于 A 落地）**
- 现行隔离输入为 `(VA, len, tid, token_value)`：VA 同尺寸片间可能被复用、tid 依 UMMU 语义自增
  （可预测）、value 顺序自增（可预测）
- 需确认：真实 libummu 的 tid 是否自增（可预测）；硬件在 value 之外是否存在 per-EID/per-jetty/
  per-import 授权绑定（`ummu_seg_attr` 未见）
- 若结论为"仅靠 value"，则 value 需改为"process-unique 且不可预测"（例如高位保留唯一计数器、
  低位随机），并单独评审——该结论同时决定 A1 是否可重新考虑

#### 26.6 验证判据与回滚

真机 `read-src-003`（read-piece16-cc8-post1-pipe2，对照 read-src-002）：

| 指标 | 今日 | 目标 |
| --- | --- | --- |
| `token_alloc_on_path` 增速 | 1/片 | 预热后 ≈ 0 |
| `token_alloc_prefill` | — | ≈ 片数 |
| 池命中率 | — | ≥ 95% |
| `register_token_ns` p50 | 9.06ms | ≤ 0.5ms |
| `register` p50 | 43.39ms | ≤ 10ms |
| `register_seg_ns` | 0.18ms | 不回退（确认未拖慢 pin） |
| child piece p50 | 88.44ms | ≤ 55ms |
| 吞吐 / retained / tcp fallback | 1467MiB/s / 0 / 0 | 不回退 |

`wait ReadDone`（35.24ms）单独重测：completion 也在 owner 线程处理，若同步下降则 E2E 收益更大；
若不变说明 READ 数据面已是链路限制。

回滚：`DFURMA_READ_TOKEN_PREFILL_MAX = 0`（池容量 0 ⇒ acquire 永远走 on-path，等价今日行为，
worker 不启动），零风险开关。

#### 26.7 未采纳变体

- **A1 复用 tid**：见 26.2，红线冲突，否决（除非先完成 P0）。
- **A12 provider 原生多段 token id 表**：`core/urma_cp_api.c` 的
  `urma_alloc_token_id_ex` 提示存在 `flag.bs.multi_seg` + `dev_cap.feature.bs.muti_seg_per_token_id`
  模式（"token id table mode"）。若设备支持且语义为"一段一键/逐段 value 校验"，可用一个 tid
  覆盖多段，既省分配又不降隔离；语义未知，列为 26.5(0a) 探针项，需 UMDK 侧确认后再评估。
- **A2 纯预热（只在启动时批量预取）**：片数远超池容量，预热只能省掉最初几片，收益可忽略。

#### 26.8 实施顺序

(0) 探针 0a-0d 与 P0 结论 → (1) C shim（池 + prefill + stats + close 排空 + 12/13 组测试）
→ (2) Rust FFI + worker + 生命周期接线 → (3) 日志/monitor → (4) 离线验证（309 + 13 组）
→ (5) 真机 read-src-003 → (6) 文档回填。

**当前状态（据第二十七批更新）：26.4 的 A7 实施清单已作废（0c `overlap_ratio=0.96x` 否决 P-A）；
首选改为 A12（table tid 池），待重跑 0d storm + 新增 0e 判定后进入实现设计。**

### 第二十七批：探针 0a-0d 真机结果 → A7 否决、A12 上位（2026-09-22）

真机命令：`LD_LIBRARY_PATH=/usr/lib64 ./token_probe udmac0d1e2 0 24 4`（parent 侧，eid=0）

**1. 原始输出（关键行）**

```text
0a device features           value=0x000788af muti_seg_per_token_id=1 ctp_en=1 ipourma_en=1 uboe=0 outorder_comp=0
0a device limits             max_jfc=65528 max_jetty=65532 max_jfr=65532
0a/token-id-table-mode     multi_seg=1 ACCEPTED token_id=0xdb00
0b alloc_token_id (serial)      n=24 errors=0 p50= 8.123ms p95= 8.718ms max= 8.773ms mean= 8.159ms
0c alloc simultaneous           threads=4 per_thread=6 serial_total=195.827ms parallel_total=203.540ms overlap_ratio=0.96x
0c verdict                      SERIALIZED
0b register_seg (fresh tid)     n=64 errors=0 p50= 0.146ms p95= 0.295ms max= 0.362ms mean= 0.166ms
0d alloc storm                  concurrent alloc+free=92 while measuring register_seg
0d register_seg (storm)         n=64 errors=64 p50= 8.226ms   ← 无效，见第 3 点
0d register_seg (tid reused)    n=64 errors=0 p50= 0.121ms p95= 0.138ms max= 0.236ms
0d verdict                      tid-reuse errors=0 (API-legal sequentially)
WARN urma_delete_context failed errno=0
```

**2. P-A 判定：否决 A7（预取 fresh tid）**

- `overlap_ratio = 0.96x`（4 线程），24 次分配在 4 线程下**总墙钟与串行一致**（195.8ms vs 203.5ms）⇒
  `urma_alloc_token_id` 的 ~8.1ms 是**全局序列化资源**（libummu `ummu_allocate_tid` 的锁，或内核
  tid 分配器），不是可并行的 CPU 工作
- 0b 复现 read-src-002 的 9.06ms（8.12ms p50 / 8.72ms p95），确认亚阶段计时与探针口径一致
- 推论：off-path 预取**只能把 8ms 从 owner 线程搬到 prefill 线程**，系统总成本不变；prefill 单线程
  吞吐 ≈ 123 tid/s，owner 需求 64 片/约 730ms ≈ 88 tid/s，仅 1.4× 余量；若 register 的内核 ioctl
  与 tid 分配**共享串行化控制路径**，owner 反而会被 prefill 拖慢——这正是重跑 0d 要判定的事
- ⇒ **P-A 失败，A7 不再作为候选**（26.4 的实施清单作废，不改代码）

**3. 0d storm 测量无效（探针缺陷，已修）**

- 现象：`errors=64`（全部 register_seg 失败）且耗时 8.2ms，与"register 只用 token_id、不分配 tid"
  的源码事实矛盾
- 根因：`keep_token()` 在 `live_count == MAX_LIVE_TOKENS`（64）时**先 free 再返回 0**，调用方继续
  用这个已释放的 tid 去 `register_seg`；而控制组 `reg_clean` 恰好把 64 个名额用满，故 storm 轮
  **每一次**都拿到悬垂 tid
- 另发现：reuse 轮的 `reused` tid 未释放 → `urma_delete_context` 失败（`WARN ... errno=0`）
- 修复：[token_probe.c](../../../../dev/dragonfly-urma-tools/urma-b7/token_probe.c) 中
  `MAX_LIVE_TOKENS 64→128`、`probe_register_seg` 入口先 `free_live_tokens()`、失败时打印
  `first failure errno`、run 末尾释放 `reused` ⇒ **0d storm 需重跑**
- 仍有效的一行：`0d register_seg (tid reused)` p50 0.121ms / errors=0 ⇒ **顺序复用同一 tid 合法**

**4. 源码核对：8ms 归属与 0a 的真实含义（新增证据）**

- `hw/udma/udma_u_ops.c:95-97`：`.alloc_token_id = udma_u_alloc_tid`、`.alloc_token_id_ex = udma_u_alloc_tid_ex`
- `hw/udma/udma_u_tid.c:90-100`：
  - `udma_u_alloc_tid`（即 `urma_alloc_token_id`）→ `udma_u_alloc_tid_common(ctx, **MAPT_MODE_TABLE**)`
  - `udma_u_alloc_tid_ex`：`multi_seg != 0` → TABLE；`multi_seg == 0` → **MAPT_MODE_ENTRY**
  - ⇒ **今日每片 `urma_alloc_token_id` 拿到的 tid 本来就是 table（多段）模式**；
    `urma_alloc_token_id` 与 `_ex(multi_seg=1)` 在 udma provider 下等价
- `hw/udma/udma_u_tid.c:27-68`：8ms 在 `ummu_allocate_tid`（libummu，**不在源码树内**）+ 
  `urma_cmd_alloc_token_id` ioctl；结合 0c 的完全串行 ⇒ 全局锁/单一序列化资源
- `hw/udma/udma_u_segment.c:64-89 / 132-168`：`udma_u_register_seg` **不分配 tid**，只用
  `seg_cfg->token_id` 做 `ummu_grant` + `urma_cmd_register_seg`(pin) ⇒ 0b 的 0.146ms 是纯 register 成本
- 文档 `doc/ch/urma/URMA API Guide.ch.md:2296`：`urma_alloc_token_id_ex` 扩展接口"增加 flag 参数
  控制用 table mode 还是 entry mode。**若用 table mode，注册的 seg 地址需按页对齐**"
  ⇒ table 模式的设计语义就是"一个 tid 映射一段页对齐的多段表"，而非"一键一段"

**5. 路线改判**

| 候选 | 状态 | 依据 |
| --- | --- | --- |
| A7 预取 fresh tid | **否决** | 0c `overlap_ratio=0.96x`，9ms 是全局串行资源；只搬家，不改总成本 |
| A2 纯预热 | 否决（沿用 26.7） | 片数远超池容量 |
| **A12 table tid 池** | **上位（首选）** | 0a `muti_seg_per_token_id=1` 且 `multi_seg=1 ACCEPTED`；且**今日路径已是 TABLE 模式**（udma_u_tid.c:92），一 tid 多段是 provider 设计用法，非 hack |
| A1 复用 tid + P0 | 降级为 A12 的实现细节 | table 模式下"复用"不再是越权借用；但 P0（value 不可预测性）与安全评审仍需完成 |

A12 收益模型：启动时分配若干 table tid，每片只做 `register_seg`（0.146ms）+ copy，
**省掉每片 8.1ms 的 alloc**；按 read-src-002 外推 `register` p50 43.39ms → 约 2.3ms + 排队，
owner 线程占用从 ~100% 降到近零。
（原文此处写"N ≈ 并发片数，如 64"，**已撤回**：tid 数量不由 CC 大小倒推，
改由第二十九批 probe 的 `P-SWEEP capacity` + 并发实测决定。）

**6. 仍需证据（新增 0e 探针）**

- **0e（已加入探针）**：一个 table tid 能否**同时**持有 N 段（4 个 4MiB、页对齐、各自 value）——
  分别对 `urma_alloc_token_id`（TABLE）与 `_ex(multi_seg=0)`（ENTRY）各跑一遍做对照，
  输出 `live_ok / failed / first_errno / per-seg register|unregister`
- **本地接受 ≠ 远端正确**：0e 只证明 provider 受理；真正的证明是 child 侧 import 两个同 tid 段并
  分别读取成功（B7 read 用例），这一条必须做，否则 A12 不能落地
  - 后续：已改为**独立双机 probe** `read_multiseg_probe.c`（比 B7 read 用例更聚焦、更快迭代），
    覆盖远端正确性 + token 隔离 + 独立 unregister + stale descriptor fail-closed + capacity sweep，
    见第二十九批
- **P0**（不变）：现行 `(VA, len, tid, value)` 的可预测性复核

**7. 下一步（重跑命令，parent 节点 141.61.17.196）**

```bash
cc -O2 -Wall -Wextra -I /usr/include/ub/umdk/urma /tmp/token_probe.c \
   -L/usr/lib64 -lurma -lurma_common -lpthread -o /tmp/token_probe && \
LD_LIBRARY_PATH=/usr/lib64 /tmp/token_probe udmac0d1e2 0 24 4
```

判读：0d storm 若 `errors=0` 且 p50 仍 ≈0.15ms ⇒ alloc 的锁不阻塞 register，A12 无干扰；
若 p50 ≈8ms ⇒ 两者共享内核锁，A12 需把 register 也考虑进串行区。0e 若 `live_ok=4/4`
且 TABLE 组成功、ENTRY 组失败 ⇒ 两种模式语义确实不同，A12 可进入实现设计。

### 第二十八批：0d storm 重跑 + 0e → A12 可行性确认（2026-09-22）

**1. 重跑输出（探针已修，0d storm 本次有效）**

```text
0b alloc_token_id (serial)      n=24 errors=0 p50= 7.930ms p95= 8.589ms max= 8.629ms mean= 8.014ms
0c alloc simultaneous           threads=4 per_thread=6 serial_total=192.337ms parallel_total=207.527ms overlap_ratio=0.93x
0c verdict                      SERIALIZED
0b register_seg (fresh tid)     n=64 errors=0 p50= 0.147ms p95= 0.232ms max= 0.313ms mean= 0.158ms
0d alloc storm                  concurrent alloc+free=121 while measuring register_seg
0d register_seg (storm)         n=64 errors=0 p50= 8.166ms p95= 8.367ms max= 8.494ms mean= 8.180ms
0d register_seg (tid reused)    n=64 errors=0 p50= 0.123ms p95= 0.128ms max= 0.198ms mean= 0.125ms
0e table(multi_seg=1) x4        live_ok=4/4 failed=0 unreg_failed=0 first_errno=0
0e                              per-seg register=0.106ms unregister=0.072ms
0e entry(multi_seg=0) x4        live_ok=1/4 failed=3 unreg_failed=0 first_errno=0
0e                              per-seg register=0.172ms unregister=0.087ms
done                            （无 WARN urma_delete_context，泄漏已修）
```

**2. 0d storm 有效 ⇒ register_seg 与 alloc 共享串行化控制路径**

- 后台线程狂打 `alloc/free_token_id` 时，**持有效 tid** 的 `register_seg` 从 0.147ms 退化到
  **8.166ms（≈55×）**，`errors=0`（全部成功），分布极紧（p50 8.166 / p95 8.367 / max 8.494）
- 即：`urma_cmd_alloc_token_id` 与 `urma_cmd_register_seg`(pin) 在**共享的串行化控制路径**上排队
  （不是各自独立）；8ms 的 alloc 一旦在跑，register 就得等它
  - 措辞边界（据用户 2026-09-22 指示）：0d 只证明二者**共享串行化控制路径**，
    **不写成"同一把内核/驱动锁"**——本次 probe 未定位到具体锁对象，
    锁/队列/资源三种实现都能产生相同观测；正式表述统一用"共享串行化控制路径"
- 与 0c 合起来，**A7 被双重否决**：
  1. 0c：9ms 不可并行（全局串行），off-path 预取只搬运不消除
  2. 0d：预取线程的 alloc 会**反向抢占 owner 线程的 register**（每次 +8ms），A7 是负收益
- 反向的价值：这也解释了为什么"每片先 alloc 再 register"是最坏组合——同一线程内这两步串行，
  且任何并发 alloc（同进程其它 lane/对端）都会再插一刀

**3. 0e ⇒ TABLE / ENTRY 语义确实不同，A12 本地可行**

- `urma_alloc_token_id`（→ MAPT_MODE_TABLE）：**4 个页对齐段同时挂在一个 tid 上，live_ok=4/4**，
  全部注销成功；每段 register 0.106ms、unregister 0.072ms
- `_ex(multi_seg=0)`（→ MAPT_MODE_ENTRY）：**live_ok=1/4，后 3 段被拒**（`first_errno=0`，
  provider 返回 NULL 但未置 errno）⇒ ENTRY 是"一键一段"，TABLE 是"一键多段表"，两者不是同一路径
- 结合第 27 批的 `udma_u_tid.c:92`（plain alloc 已是 TABLE）⇒ **A12 不需要任何新 API**，
  只要把"每片现分配一个 tid"改成"启动时分配一池，逐片复用"

**4. A12 收益模型（据本轮实测替换 27 批的推算）**

| 项 | 今日（read-src-002） | A12 预分配 tid 池 |
| --- | --- | --- |
| 每片 `token id` | 9.06ms（8.1ms 分配） | **0**（池内取用；仅一次性池成本） |
| 每片 `register_seg` | 0.18ms | 0.106ms（0e 实测） |
| 每片 `unregister` | — | 0.072ms |
| 每片 register 阶段 p50 | 43.39ms | 目标 ≤ 3ms（register+池取用+queue） |

**5. 仍缺的证据（A12 落地前的硬门槛）⇒ 已交由第二十九批的双机 probe**

- **远端正确性**：0e 只证明 provider 本地受理。必须证明 child 侧 import **同一个 tid 下的多个
  不同 (VA, len)** 段后，分别读取落到**正确地址**——静默错址正是 UMMU 仿真器警告的失效模式，
  本地测不出来
- **token 隔离 / 独立 unregister / stale descriptor fail-closed**：同属只能在对端观测的性质
- **table capacity**：单 tid 能同时挂多少段，决定 A12 需要多少 tid；**不预设为 CC 大小**
- 上述四项 + capacity 已一次性实现在 `read_multiseg_probe.c`，见第二十九批
- **P0**（不变）：`(VA, len, tid, value)` 可预测性复核。注：今日路径 alloc→free→alloc 已可能
  拿回同一 tid（probe 未测分配器是否回收号段），故"顺序复用 tid"本身不引入新风险；
  新增的风险面仅是**多片同时在池内共用一个 tid 表**，需在安全评审中明示

**6. A12 实现要点（简版，较 A7 显著简化）**

- 位置仍在 C shim（`dfurma_runtime` 内建 tid 池）：**不引入线程、不跨线程调用、无 FFI worker**，
  A7 的 26.4 清单（预取线程 + 池 + 生命周期）整段作废
- 生命周期（形状已定，容量未定）：`open` 时预分配一池 TABLE tid；piece 取用 → `register_seg`
  → 用 → `unregister_seg` → 归还；`close` 时统一 `free_token_id`
- **tid 数量不预设**（据用户 2026-09-22 指示）：**不由 CC 大小倒推**，改由
  第二十九批 probe 的 `P-SWEEP capacity`（单 tid 同挂段数上限）+ 并发实测共同决定；
  若 capacity ≥ 并发片数，则**一个 tid 即可服务全部片**，池退化为单元素
- 池容量 `DFURMA_READ_TID_POOL_MAX`，置 0 时逐片现分配 ⇒ 完全等价今日行为（零风险回滚）
- 每片仍保留独立 `(VA, len, token_value)`，`unregister` 为范围级（`ummu_ungrant(tid, va, len)`），
  与 0e 的 `unreg_failed=0` 一致

**7. 下一步**

1. **先跑第二十九批的双机 probe**（parent=.196 / child=.198），四项判据全 PASS + 拿到
   `P-SWEEP capacity` 后，才动产品代码
2. 实现 A12（shim 池，容量取实测值）→ 离线 309 + C 测试 → 真机 `read-src-003`
   （复用既有 B7 read 用例，同时验证远端正确性与性能）→ 文档回填

---

## 第二十九批：A12 落地前的双机 probe（2026-09-22）

**1. 结论先行**

- A7 **正式淘汰**；A12 是**第一候选**。0d 证明 `alloc/free_token_id` 与 `register_seg`
  **共享串行化控制路径**，但**不写死为"同一把锁"**（未定位锁对象）
- A12 落产品代码**之前**，必须先在对端证明四项性质；本批交付该 probe
- **tid 数量不预设**：由 probe 的 `P-SWEEP capacity` + 并发实测决定，**不预设做成 CC 大小的 tid pool**

**2. 交付物**

- `urma-b7/read_multiseg_probe.c`（与 `token_probe.c` 同目录，约 1180 行）
- 产品代码**零改动**：自带 context、自有 TCP 控制协议、自有 wire 描述符格式

构建（节点已装 umdk）：

```bash
cd /home/yuan/workspace/dev/dragonfly-urma-tools/urma-b7   # 节点上的同名目录
cc -O2 -Wall -Wextra -I /usr/include/ub/umdk/urma read_multiseg_probe.c \
   -L /usr/lib64 -lurma -lurma_common -lpthread -o read_multiseg_probe
```

**3. 设计要点**

- **parent（source 侧，节点 .196）**：`urma_alloc_token_id` 取**一个** TABLE tid →
  用**同一个 tid** 注册 N 段（各段页对齐、distinct VA、`token_value = 0x10000000+i`、
  各填**可归属的伪随机器** `0xA5A5_00ii_0000_5A5A`）→ capacity sweep → 建 **RM+CTP**
  endpoint → TCP listen/accept → 下发 `wire_header + wire_seg[]`（含 `table_token_id`）→
  命令循环（`CMD_UNREGISTER` / `CMD_EXIT`）
- **child（reader 侧，节点 .198）**：connect → 收描述符 → 建 RM+CTP endpoint →
  注册 **LOCAL_ONLY** 读缓冲（poison 0xEE 预填）→ CTP import jetty →
  每段用**同一个** `hdr.table_token_id` + **各自** `token_value` import → 跑四项验证
- **字段级对齐产品 shim**（保证 probe 结论可迁移）：
  - CTP import 三步（`get_tp_list` + `import_jetty_ex` + `ENOMEM` 回落 auto-import）
    镜像 `dfurma_jetty_import`
  - seg 注册（`token_policy=PLAIN_TEXT` / `access=READ` / `cacheable=NON_CACHEABLE` /
    `token_id_valid=VALID`）镜像 `dfurma_read_source_register`
  - seg import（`attr.bs` 同上 + `token_id = 描述符里的 tid 数值`）镜像 `dfurma_read_segment_import`

**4. 四项判据 + capacity**

| 检查 | 内容 | PASS 判据 |
| --- | --- | --- |
| C-1 remote correctness | N 段共用一个 tid，逐段 READ 并逐字校验 | `ok=n/n`、`wrong_pattern=0`。错址会打印"匹配到了哪个段的模式"（`pattern of seg k`），即**静默错址**可归因 |
| C-2 token isolation | 用同 tid + **别的段的 value** / **伪造 value** 各 import 一次并读 | 两组都必须 fail-closed（import 被拒 / CR 非 SUCCESS / post 失败）；可读到数据即 `TOKEN ISOLATION LEAK` |
| C-3 独立 unregister | parent 注销 index 0 后，child 读 index N-1 | 仍 `cr=SUCCESS` 且字节正确 ⇒ `independent` |
| C-4 stale descriptor | parent 已注销后，child 用**旧描述符**再读 | 必须 fail-closed **且**读缓冲仍全为 poison（`buffer_intact=1`） |
| P-SWEEP capacity | 同一 tid 持续追加注册直到 provider 拒绝 | 打印 `one tid held K simultaneous segments`；**K 即决定 A12 的 tid 数量** |

**5. 运行命令（parent = 141.61.17.196，child = 141.61.17.198）**

```bash
# 注：下表命令为 v1 形态；v2 请用第三十批第 5 节（--sweep 512），且 v1 的输出样例已作废
# parent（先起，会阻塞在 accept）
LD_LIBRARY_PATH=/usr/lib64 ./read_multiseg_probe parent \
    --dev udmac0d1e2 --eid 0 --listen 0.0.0.0:13999 \
    --segments 8 --seg-bytes 1048576 --sweep 64

# child（后起；--segments 只用于本地缓冲，真实段数用 parent 下发的）
LD_LIBRARY_PATH=/usr/lib64 ./read_multiseg_probe child \
    --dev udmac0d1e2 --eid 0 --connect 141.61.17.196:13999
```

- `--segments`/`--seg-bytes` 两侧必须能整除到 4096（页对齐）；child 侧以 parent 下发值为准
- `--eid` 必须为 0（沿用既有硬约束：EID 1-7 缺 CTP transport 资源）
- 退出码：**0 = 四项全 PASS**；1 = 任一 FAIL 或建链/初始化失败
- child 打印 `C-VERDICT A12 PASS|FAIL correctness=.. isolation=.. independent_unregister=.. stale_fail_closed=..`

**6. 预期输出（形如）**

```text
endpoint ready               dev=udmac0d1e2 eid=0 local_jetty_id=... tp_priority=...
P-TID table token id         0x.... (single tid carries every segment)
P-REG 8 segments live        one tid, register_seg each errno=0
P-SWEEP capacity             one tid held K simultaneous segments (first failure at index .. errno=..)
P-WAIT listening             port=13999
C-OFFER received             jetty_id=... table_token_id=0x.... segments=8 seg_bytes=1048576
C-JETTY imported             stage=3 (0=plain,2=auto-ctp,3=import_ex)
C-IMPORT 8 segments          all share table_token_id=0x...., distinct VA/value
C-1 seg=0   cr=SUCCESS bytes=OK            ...  （8 行）
C-1 verdict correctness      ok=8/8 wrong_pattern=0 cr_fail=0 post_fail=0
C-2 foreign-segment-value value=0x10000001 cr=REM_ACCESS_ABORT -> fail-closed
C-2 bogus-value-2        value=0xdeadcf23 cr=REM_ACCESS_ABORT -> fail-closed
C-2 verdict token isolation   fail_closed=2/2
C-3 parent unregistered      index=0 status=0
C-3 seg=7   (still live) cr=SUCCESS bytes=OK -> independent
C-4 stale seg=0   cr=REM_ACCESS_ABORT buffer_intact=1 -> fail-closed
C-VERDICT A12 PASS           correctness=PASS isolation=PASS independent_unregister=PASS stale_fail_closed=PASS
```

**7. 判读分支（写死在这里，避免结果出来再争）**

- **四项全 PASS** ⇒ A12 可进产品代码；`P-SWEEP K` 作为 tid 数量的下界（K ≥ 并发片数时
  单 tid 即可，池退化为单元素）
- **C-1 出现 `bytes=WRONG (pattern of seg k)`** ⇒ **静默错址** ⇒ **A12 立即否决**
  （这正是 UMMU 仿真器警告的失效模式）
- **C-2 未 fail-closed**（`TOKEN ISOLATION LEAK` 或 `bytes=WRONG`）⇒ 说明**远端在 READ 路径
  不校验 token_value**，隔离只靠 `(tid, VA, grant)`。此时不直接否决 A12，但必须记录：
  A12 下多片共享同一 tid，隔离面比今日更窄，需在安全评审中明示（probe 会打印 `C-2 note`）
- **C-3 失败**（注销一段影响到兄弟段）⇒ table tid 的 unregister 并非范围级 ⇒ A12 不可用
- **C-4 未 fail-closed** ⇒ stale descriptor 可越权读 ⇒ **安全阻断项**，A12 否决
- **P-SWEEP 未打印 capacity（`no failure`）** ⇒ 直接加大 `--sweep`（上限 512）重跑，
  直到拿到拒绝点

**8. 状态**

- 本地编译：`-Wall -Wextra` **无告警**通过（唯一提示来自 URMA 头文件自身的
  `zero-size array`，属 `-Wpedantic`）
- 本机无 URMA 硬件：自检输出 `FAIL urma_get_device_by_name(udmac0d1e2) errno=19`（预期），
  参数校验与 `usage` 正常
- **真机双机结果见第三十批**（v1 结果暴露探针缺陷，已改出 v2）

---

## 第三十批：probe v1 真机结果 → 结论只能取 C-1，其余四项判据无效（2026-09-22）

**1. 原始输出（parent=.196 / child=.198，`--segments 8 --seg-bytes 1MiB --sweep 64`）**

```
parent: P-SWEEP capacity              one tid held 64 simultaneous segments
                                      (sweep target 64 reached with no failure; raise --sweep)
child:  C-1 verdict correctness       ok=8/8 wrong_pattern=0 cr_fail=0 post_fail=0
        FAIL no completion for read len=1048576
        C-2 foreign-segment-value value=0x10000001 post/complete failed -> fail-closed
        FAIL no completion for read len=1048576
        C-2 bogus-value-2         value=0xdeadd123 post/complete failed -> fail-closed
        C-2 verdict token isolation   fail_closed=2/2
        C-3 parent unregistered       index=0 status=0
        FAIL no completion for read len=1048576
        C-3 seg=7   (still live) POST FAILED <== unregister disturbed a sibling
        FAIL no completion for read len=1048576
        C-4 stale seg=0   post/complete failed, buffer_intact=1 -> fail-closed
        C-VERDICT A12 FAIL  correctness=PASS isolation=PASS independent_unregister=FAIL stale_fail_closed=PASS
```

**2. 可信结论（只有一条，但很关键）**

- **C-1 remote correctness = PASS，8/8 段、共用同一个 TABLE tid `0xdb00`、各自不同 VA/value，
  全部读到正确字节，`wrong_pattern=0`**
  ⇒ A12 最核心的安全性质（**一 tid 多段不会静默错址**）**已在真机验证**，
  UMMU 仿真器警告的失效模式**未出现**
- P-SWEEP：`held 64` 且"未遇失败"⇒ **capacity ≥ 64**，需加大 `--sweep` 才能拿到上界

**3. 无效判据与根因：`C-3 FAIL` 是探针缺陷，不是硬件性质**

- 形态证据：**第 9 次读之后的每一次读（含用正确 token 的 C-3）都无 completion**。
  C-3 读的是仍未注销的 seg 7、用的是它自己的正确 token，不可能因"注销 index 0"失败；
  正确解释是**承载它的 lane 在第 9 次读时已经失效**
- 两个候选机制（v1 无法区分，因为 v1 既没打印深度、也没为失败读隔离 lane）：
  1. **首个被拒的 READ（错 token）把本地 JFS 挂起**，之后该 jetty 的所有 WR 不再产生 completion
  2. **JFC 在 8 次读之后不再排空**：v1 的成功读次数恰好等于 `--segments`(8)，
     而这台机器 `dev_cap.max_jfc_depth` 到底是多少 **v1 从未打印**；
     `lane.rs` 的 mock 恰为 `max_jfc_depth: 8`，故机制 2 不能排除
- 由于 C-2 的"fail-closed"同样只是"无 completion"，**C-2 token 隔离与 C-4 stale fail-closed
  也一并无效**（v1 里 C-2 被判 PASS 是假阳）
- 教训（与 0d storm 同类）：**一个会把承载它的 lane 打死的检查，不能与其它检查共用 lane，
  也不能把"无 completion"直接当作"远端拒绝"**

**4. probe v2 的修改（已本地编译通过，`-Wall -Wextra` 无告警，1409 行）**

- **lane 抽象**：`struct lane{send_jfc,recv_jfc,jfr,jetty,tjetty}`，`endpoint` 持 `main` lane +
  共享 context/attr/priority；`post_read`/`wait_cr` 改为按 lane 收发
- **每个 poison 读独占一条新建 lane**，用完即 `urma_delete_*` 拆掉 ⇒ 一次挂起污染不到任何其它检查
- **control-before 是归因依据**：新 lane 上先用（合法 VA + 正确 token）读一次，通过后再发 poison 读。
  lane 已被证明健康，poison 读与它的唯一差别就是被投毒的描述符 ⇒ 拒绝**可归因**
- **control-after 仅作观测**：报告 `lane_survived=0/1`，即"一次被拒的 READ 是否把 lane 打死"——
  这是产品级事实（产品需要在 lane 报错时重建）
- **顺序改为 C-1 → C-1b → C-3 → C-4 → C-2**：健康检查全部先行，poison 检查殿后
- **C-2 改成严格 A/B**：poison 读与 control 读**用完全相同的 VA**（都用 seg n-1），
  只有 `token_value` 不同 ⇒ 排除 VA/len 混入
- **新增 C-1b lane 复用诊断**：在 lane0 上继续追加 4 次健康读，直接区分"机制 1"与"机制 2"；
  一个不能连续服务多次健康读的 lane 是 lane 复用上限，而非 token 效应。已并入 A12 判据
- **打印设备能力与实际深度**：`max_jfc/max_jfs/max_jfr/max_jetty/max_read_size` 与
  `send_jfc/recv_jfc/jfr/jfs` 生效深度 ⇒ 机制 2 是否成立一眼可见
- child 现在要求 `n >= 2`（poison 检查需要一个仍然 live 的 control 段）

**5. 下一步（重跑命令，`--sweep` 加大以拿 capacity 上界）**

```bash
# parent @ .196
LD_LIBRARY_PATH=/usr/lib64 ./read_multiseg_probe parent \
    --dev udmac0d1e2 --eid 0 --listen 0.0.0.0:13999 \
    --segments 8 --seg-bytes 1048576 --sweep 512

# child @ .198
LD_LIBRARY_PATH=/usr/lib64 ./read_multiseg_probe child \
    --dev udmac0d1e2 --eid 0 --connect 141.61.17.196:13999
```

判读：

- `endpoint lane depths` 的 `send_jfc=..` 若为 8 之类的小值 ⇒ 机制 2 成立，v1 的 8 次是深度巧合
- `C-1b lane0 extra reads served=4/4` ⇒ 无 lane 复用上限，机制 1/2 均不成立，
  则 v1 的静默需另找原因（此时 C-2/C-3 的结果重新变为可解释）
- `C-1b served<4` ⇒ 存在 lane 复用上限，**A12 的产品实现必须据此决定 lane 的重建策略**

---

## 第三十一批：probe v2 真机结果 → A12 五项全 PASS，已落产品代码（2026-09-22）

**1. 原始输出（v2，parent=.196 / child=.198，`--segments 8 --seg-bytes 1MiB --sweep 512`）**

```
parent: endpoint caps                max_jfc=65528(max_depth 1048576) max_jfs=65532(depth 8192)
                                     max_jfr=65532(depth 32768) max_jetty=65532 max_read_size=268435456
        endpoint lane depths         send_jfc=512 recv_jfc=512 jfr=512 jfs=512
        P-TID table token id         0xdb00 (single tid carries every segment)
        P-REG 8 segments live        one tid, register_seg each errno=0
        P-SWEEP capacity             one tid held 512 simultaneous segments
                                     (sweep target 512 reached with no failure; raise --sweep)
        P-UNREG index=0 status=0     (other segments stay live on the same tid)
child:  C-JETTY imported             stage=3 (0=plain,2=auto-ctp,3=import_ex)
        C-IMPORT 8 segments          all share table_token_id=0xdb00, distinct VA/value
        C-1 verdict correctness      ok=8/8 wrong_pattern=0 cr_fail=0 post_fail=0
        C-1b lane0 extra reads       served=4/4 -> lane keeps draining, no reuse limit seen
        C-3 parent unregistered      index=0 status=0
        C-3 seg=7 (still live)       cr=SUCCESS bytes=OK -> independent
        C-3 verdict independent unregister  PASS
        C-4 stale-descriptor         poison seg=0 control seg=7 refused (no completion)
                                     buffer_intact=1 lane_survived=0 -> attributable
        C-4 verdict stale descriptor PASS
        C-2 foreign-segment-value    value=0x10000001 pos=7 (same VA as the control)
                                     refused (no completion) buffer_intact=1 lane_survived=0 -> attributable
        C-2 bogus-value              value=0xdeadbeef pos=7 (same VA as the control)
                                     refused (no completion) buffer_intact=1 lane_survived=0 -> attributable
        C-2 verdict token isolation  refused_and_attributed=2/2
        C-VERDICT A12 PASS           correctness=PASS lane_reuse=PASS isolation=PASS
                                     independent_unregister=PASS stale_fail_closed=PASS
done exit=0
```

**2. 判定：A12 前置门槛通过**

| 判据 | 结果 | 依据 |
| --- | --- | --- |
| remote correctness | PASS | 8 段共用一个 TABLE tid `0xdb00`，逐段读出各自字节，`wrong_pattern=0`（无静默错址） |
| lane 复用 | PASS | `C-1b served=4/4`，lane0 累计 12 次健康读；实测 `send_jfc=512` |
| token 隔离 | PASS | 同 VA、仅 token 不同 ⇒ 2/2 被拒且可归因 |
| 独立 unregister | PASS | 注销 index 0 后，同 tid 的 seg 7 仍可读 |
| stale fail-closed | PASS | 已注销描述符被拒，缓冲区未被写入（`buffer_intact=1`） |
| table capacity | **≥ 512** | `held 512 ... no failure`，被 `MAX_SEGS=512` 封顶，未取到真实拒绝点 |

**3. 两个候选机制的判决：机制 1 成立，机制 2 否证**

- **机制 2（JFC 不排空 / 深度用尽）被否证**：`C-1b` 在同一 lane0 上继续读 4 次全部成功，累计 12 次；
  实测 `send_jfc=512`（设备 `max_jfc_depth=1048576` 被探针的 `capped_depth()` 夹到 512）。
  v1 里"恰好 8 次读后静默"与 `--segments=8` 相同是巧合，不是 lane 复用上限。
- **机制 1 被证实**：`lane_survived=0` 在 C-4 与 C-2 两例中均出现
  ⇒ **一次被拒的 READ（错 token 或 stale VA）不产生 CR，且永久打死承载它的 jetty**。
- **挂起局限于该 lane**：C-4 的 lane 死后，C-2 两例各自**新建 lane** 的 control-before 均成功
  ⇒ context 与**共享 tid 在新建 lane 上完全可用**。这正是 A12 的硬需求。
- 附带事实：拒绝表现为**无 completion（挂到超时）**，而不是 CR 错误码
  ⇒ 产品在 WR 层无法把"token 不匹配"与"挂死"区分开。

**4. 产品级新事实与既有假设的核对（正向，无需改设计）**

- `lane_survived=0` 恰好证实产品已写下的两条假设：
  [urma.rs](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/client/urma.rs#L417-L420)
  「A failure is sticky until this cached client is retired」与
  [urma_read.rs](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/client/urma_read.rs#L366)
  「retire the cached client; the next client rebuilds a fresh lane」
  ⇒ **"失败粘性 → retire → 重建新 lane"的模型已被硬件证实**，不是新缺陷。
- 由此得到 A12 的实现约束：**共享 tid 必须是 context 作用域，不能挂在 lane 或 source 上** ——
  lane 会因一次被拒 READ 被 retire 重建，source 会随 Piece 注销，而 tid 必须跨越两者存活。

**5. A12 落地（本次代码改动，仅 shim，Rust 零改动）**

改动集中在 [shim.c](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/urma/ffi/shim.c)：

- `struct dfurma_runtime` 新增 `read_token_id`（context 作用域，`NULL` 表示尚未创建）与
  `read_token_id_refs`（持有它的 READ source 数，含 registration 结果 uncertain、永远无法释放的 source）
- 新增 `dfurma_read_token_id_acquire()`：首次使用时 `urma_alloc_token_id()`，之后只递增引用计数
- `dfurma_read_source_register()` 用它取 tid（仍保留 `register_token_ns` 计时，供对比）
- `dfurma_read_source_release_after_revoke()` **不再** `urma_free_token_id()`，只递减引用计数
- `dfurma_runtime_close()`：`read_token_id_refs != 0` 时返回 `-EBUSY`；否则在删 context 之前
  `urma_free_token_id()` 一次（失败则保持可重试的 close 语义）

**为何池会退化为单元素**：实测单 tid 至少承载 512 个同时 live 的段，而本进程同时服务的 Piece 数受
客户端并发（B7 为 cc8/cc32）约束，远小于 512 ⇒ **一个 tid 就够**，不做成按并发量预分配的池
（与第二十八批撤回"预分配 N ≈ 并发片数"一致）。容量不足时的表现是 `register_seg` 失败 ⇒
该片走既有 TCP 回落，不做额外降级逻辑。

**6. 隔离面变化的论证（A12 唯一弱化的就是 tid 这一项）**

- exact-Piece 隔离输入集合是 `(VA, len, tid, token_value)`；A12 只把 `tid` 从"每片新分配"变为"全进程共享"
- `VA` 与 `len` 仍由每片自己的注册决定；`token_value` 仍是**进程唯一**的单调计数
  （[server/urma.rs](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/server/urma.rs#L587-L601)
  的 `NEXT_READ_TOKEN`，注释即"A stale offer can never arm a new export"）
- C-4 已证**未注册的 VA 不存在 grant**，因此一个 stale descriptor（旧 VA + 共享 tid + 旧 token_value）
  既不满足 VA 条件、也不满足 token_value 条件 ⇒ 仍 fail-closed
- 未变的安全缺口（沿用既有记录，非本批引入）：`token_value` 可预测（顺序计数），
  对抗主动猜测者的强度不足；这属于 P0 安全复核项，A12 不改变其结论

**7. 验证**

- `cargo check --features urma`：通过（shim 以 `-Wall -Wextra` 编译，无告警）
- `cargo test --features urma --lib`：**309 passed / 0 failed / 1 ignored**
- 真机回归（待跑）：`read-src-003`（复用既有 B7 read 用例，同时覆盖远端正确性与性能），
  对比 `register_token_ns`（应≈0）与 `register_ns`（p50 43.57ms 中约 9ms 来自 token 分配；
  其余的 owner 线程排队约 32ms 与 A12 无关，不要期待 register 阶段降到个位数毫秒）
- 补测项：capacity 的真实上界（当前被 `MAX_SEGS=512` 封顶，且只用 1MiB 段测得；
  每片 4–64MiB 时是否受 tid 地址空间影响未测）

**8. 状态**

- A12 已落产品代码，**离线编译与单测通过**；真机 `read-src-003` 未跑
- probe 保持为交付物（v2，1422 行），未被产品代码引用

---

## 第三十二批：A12 真机结果 → 判据全达成，register p50 43.39→5.84ms，吞吐 ×4（2026-09-22）

**1. 配置与口径**

- 用例：`read-piece16-cc8-post1-pipe2`（16MiB × cc8 × 64 pieces/sample，共 1024MiB），
  parent=.196 / child=.198，与 `read-src-002` **同用例同参数**
- 与 `read-src-002` 的唯一代码差异：A12（第三十一批的 shim 改动）。
  注意 `read-src-002` 的 child 侧已经包含 destination 池
  （child piece p50 88.44 与 `read-pool-001` 持平），故下表差值**可归因于 A12**

**2. 对照表（p50 / p95，ms，`parent.sample-003` + `child.sample-003`）**

| 指标 | read-src-002 | read-src-003 | 26.6 目标 | 判定 |
| --- | --- | --- | --- | --- |
| `register_token_ns` | 9.06 / 9.26 | **0.00 / 0.00** | ≤ 0.5 | 达成 |
| `register` | 43.39 / 74.92 | **5.84 / 11.76** | ≤ 10 | 达成（超预期） |
| `register_seg_ns`（pin） | 0.18 / 0.33 | 0.17 / 0.20 | 不回退 | 不回退 |
| `register_copy#2` | 2.16 / 2.35 | 2.08 / 2.14 | — | 基本不变 |
| `register_alloc` | 0.00 / 0.01 | 0.00 / 0.00 | — | 不变 |
| `stage wait ReadDone` | 35.24 | **7.19 / 11.87** | 待测 | 同步下降 |
| `stage revoke/unregister` | 3.81 / 81.45 | **0.74 / 9.81** | — | 大幅下降 |
| parent source E2E | 79.57 | **13.30 / 19.23** | 26.1 预测 ~40 | 超预期 |
| child piece E2E | 88.44 / 167.54 | **23.30 / 29.30** | ≤ 55 | 达成 |
| 吞吐 | 1467.1 MiB/s | **5888.4 MiB/s** | 不回退 | ×4.0 |
| failures / tcp fallback / retained warnings | 0 / 0 / 0 | **0 / 0 / 0** | 不回退 | 达成 |

**3. 为什么 register 降幅（−37.5ms）远大于 token 省下的 9.06ms —— 26.1 的模型被证实**

26.1 的推断是：**32ms 排队不是独立成本，而是 owner 线程被 shim 内 11.40ms/片占满后的产物**
（64 片 × 11.40ms ≈ 730ms ≈ 整段 wall）。本轮实测把这条推断验证到底：

- shim 内占用：11.40ms → **2.25ms/片**（copy#2 2.08 + pin 0.17 + alloc 0 + token 0）
- 64 片 × 2.25ms ≈ 144ms，owner 线程**不再饱和** ⇒ 排队塌缩
- 因此 `register` 的残差（排队+dispatch）从 31.99ms → 3.59ms，与"占用驱动排队"一致
- `wait ReadDone`（completion 同样在 owner 线程处理）同步 35.24 → 7.19ms，
  正是 26.6 里"若同步下降则 E2E 收益更大"那一支

**4. 新的瓶颈结构**

- `register` 内：**copy#2 2.08ms 占 92%**，是唯一剩下的实质成本；
  这正是第二十三批判定 Stage 1（backing pool）**无效**的原因（copy#2 无论如何都要付），
  本轮结果与该判定自洽
- parent 已不再是端到端主杠杆：source E2E 13.30ms 仅占 child piece 23.30ms 的 **57%**
  （read-src-002 时是 90%）⇒ child 侧与链路成为共同限制
- E2E 组成自洽：open 0.04 + copy 0 + register 5.84 + wait 7.19 + revoke 0.74 ≈ 13.81 ≈ source E2E 13.30

**5. child destination 池表现（附带观测，非本轮改动）**

- `sample-003`：hit 64 / miss 0（100%），returned 64，peak/final retained 128MiB
- `warmup-001`：miss 8 → hit 56（87.5%），`extra registers beyond first: 7`，
  即池在冷启动时按 cc8 并发把 16MiB 槽位长到 8 个（8 × 16MiB = 128MiB peak），之后全命中 ⇒ 符合设计预期
- `warmup-001` 的 child piece p95 137.5ms / parent source p95 109.57ms 属冷启动抖动，
  稳态看 `sample-003`（p95 29.30ms）
- **待确认的小问题**：`child.sample-003.tasks.log` 的池计数器全为 0（同一 sample 的 `.log` 是 64/100%）。
  两份日志的 piece E2E 数字完全一致，看起来是 monitor 在 `.tasks.log` 口径下未接池计数器，
  属**观测口径问题**，不影响结论；若后续要用 tasks.log 判池，需要修 monitor 口径

**6. 26.6 判据表中需要作废的两行**

`token_alloc_on_path`（目标 1/片→0）、`token_alloc_prefill`、池命中率 ≥95% 三行是
**A7（预取 fresh tid）形态**的指标；A12 是"共享单 tid"形态，没有 prefill、也没有逐片取用，
对应的等价证据就是 `register_token_ns` p50/p95 = 0.00。
回滚开关 `DFURMA_READ_TOKEN_PREFILL_MAX` 随 A7 一并作废，A12 的回滚方式是 revert 本次 shim 改动。

**7. 残留与下一步候选**

- **P0（未做，独立项）**：`token_value` 可预测性复核——A12 把隔离面从 `(VA,len,tid,value)`
  弱化为 `(VA,len,value)`，其强度现在完全落在 value 上（第三十一批第 6 节已论证 stale descriptor
  仍 fail-closed，但主动猜测者的强度未评估）
- capacity 真实上界仍未取到（被 `MAX_SEGS=512` 封顶，且只用 1MiB 段测得；本轮真实负载是
  16MiB 段、cc8、64 片同 tid，未触发任何 register 失败，可视为对 A12 的间接支持）
- 若要继续压 parent：copy#2 已是唯一实质成本（16MiB memcpy ≈ 2.1ms），
  池化/持久 slot 均已被第二十三批否决 ⇒ parent 侧已接近下限
- 若要继续压端到端：下一步杠杆在 child 侧（piece p50 23.30ms 中 parent 只占 13.30ms）

**8. 状态**

- A12 **真机验证通过**：功能 0 失败 / 0 TCP 回落 / 0 retained warning，性能判据全达成
- 一次性 tid 分配（约 9ms）不出现在任何 sample 的 p50/p95 里（runtime 在首个被记录片之前
  就已建池；n=64 下单次离群不会进入 p95），路径上已无 9ms 固定成本

---

## 第三十三批：A12 后隔离面的真机验证 → grant 边界不是屏障，key 相等才是（2026-09-22）

**1. 为什么需要这个探针**

第三十一批论证了"stale descriptor 仍 fail-closed"，第三十二批验证了 A12 的性能收益。
两者都**没有**回答 A12 唯一改动带来的问题：一个 TABLE tid 现在承载全进程所有 Piece 的 grant，
那么**两个共用同一 tid 的 Piece 之间，硬件到底靠什么隔离**？

第三十一批把 exact-Piece 隔离输入集合写成 `(VA, len, token_value)`，那是**推理**，不是实测。
本批把 `(VA, len, tid, value)` 逐维做成反例，用双机真机探针读出结论。

交付物：`/home/yuan/workspace/dev/dragonfly-urma-tools/urma-b7/read_isolation_probe.c`
（1912 行，新建；产品代码零改动，自带 TCP 控制协议与私有 wire 描述符格式）。

**2. 探针设计**

四个假设，每个都有 fail-closed 期望：

| 假设 | 内容 | 反例用例 |
| --- | --- | --- |
| B-1 | value 按 `(tid, VA)` 的 grant 绑定，不按 tid | C-B1：slice3 的 VA + slice0 的 key（两个不同 grant） |
| B-2 | 旧 value 不能授权"已注销并重新注册"的 VA | C-S1（stale VA + 自有 key）、C-V2（同 VA 重注册 + 旧 key） |
| B-3 | 一次 READ 由其起始 grant 限界，而非仅由其实地址限界 | C-B3a（越界进同 key 兄弟）、C-B3c（越界进异 key 兄弟）、C-B3b（grant 缩短后按原长读） |
| B-4 | key 0 是 key，不是旁路（`URMA_TOKEN_NONE == 0` 是 *policy*） | C-V3a / C-V3b / C-V3c |

布局：parent 分配**一段连续 arena**，切成 n 个 page-aligned slice，在**同一个 TABLE tid** 上
分别 `register_seg`，每片自带 pattern 与 value：

- slice0 = slice1 = `V0`（**故意的 value 碰撞**，B-1 要拿它量"碰撞的后果"）
- slice2 = `0`（key 0 可达性）；slice3 = `W`（随后被 parent 用 `W+1` 重注册且**不告知 child**）

连续布局让"越界"有确定落点（越界读必然命中相邻 slice），且每个字节都能归因。

lane 纪律（沿用 probe v2 的教训，本探针强制）：一个被拒 READ 会永久打死承载它的 jetty ⇒
每个用例独占 lane，且都先用一个 control READ 证明 lane 健康，再发被投毒的 READ；
毒读之后再补一个 control，记录"这类拒绝是否也打死 lane"。拒绝分三类报：
`local post rejected` / `remote-cr` / `remote-silent`。

退出码：`0` = B-1..B-4 全部成立。**X-1（可预测性）只作 P0 证据，不进退出码** ——
它量化残差，不是缺陷。

**3. 第一轮真机结果**（.196 parent / .198 child，端口 13997）

关键行（原文）：

```
C-V1 collision-consequence    -> ALLOWED, served slice1's bytes with slice0's descriptor
                                 and their SHARED key => the barrier is key equality
                                 per grant, not the address
C-B1 wrong-key-other-va       -> refused (no completion) buffer_intact=1 lane_survived=0
C-B3a read-past-grant-end     -> ALLOWED but bytes are NOT the addressed slice:
                                 dominant=pattern of slice 0 (8192/16384 words) lane_survived=1
C-S1 stale-va-own-key         -> refused (no completion) buffer_intact=1 lane_survived=0
C-V2 verdict old-key-on-reregistered-VA PASS
C-B3b read-past-shrunk-grant  -> refused (no completion) buffer_intact=0 lane_survived=0
                                 <== refused but the destination was written
C-X1 verdict guessed-key      allowed
C-VERDICT A12-ISOLATION FAIL  b1_per_grant=PASS b2_no_key_reuse=PASS b3_range=FAIL
                              b4_no_zero_bypass=PASS
```

结论：**B-1 / B-2 / B-4 PASS，B-3 FAIL**。所有拒绝形态统一为 `remote-silent`（无 CR）+
`lane_survived=0`，与 probe v2 的机制 1 一致 ⇒ 产品"失败粘性 → retire → 重建 lane"的假设继续成立。

`C-B3a` 的几何是精确的：读 `[slice0.va + 16MiB - 64KiB, +128KiB)`，即
**slice0 尾 64KiB + slice1 头 64KiB**，结果 ALLOWED 且 `lane_survived=1`（毫无拒绝）。
但 `dominant_pattern` 用严格大于选优，8192:8192 平票时固定报 slice 0 ⇒
**光凭这一行无法区分"真越界读到 slice1"与"短传但仍报 SUCCESS"**。这处歧义必须消掉，于是有了第二轮。

**4. 加诊断后的第二轮**

两处改动（仅探针）：

1. 新增 `provenance` 行：把目标缓冲按 8 字节词逐 pattern 计数，并分出 `poison`（未被写）与
   `other`（都不匹配）；挂在"允许但不是自己的 slice"与"拒绝但 `buffer_intact=0`"两个分支上。
2. 新增 `C-B3c`：从 slice1 尾部越界进 **key=0** 的 slice2。B3a 跨进的是同 key 的 slice1，
   分不清 provider 是"按每个覆盖 grant 校 key"还是"只校起始 grant"。`b3` 判据随之变为
   `b3a && b3b && b3c`。

关键行（原文）：

```
C-B3a provenance                slice0=8192 slice1=8192 slice2=0 slice3=0 poison=0 other=0 total=16384 words
C-B3c cross-into-other-key   -> refused (no completion) buffer_intact=0 lane_survived=0
                                 <== refused but the destination was written
C-B3c provenance                slice0=0 slice1=8192 slice2=0 slice3=0 poison=8192 other=0 total=16384 words
C-B3c verdict crossed-grant-key checked
C-B3b provenance                slice1=1048576 poison=1048576 other=0 total=2097152 words
P0-CONCLUSION barrier=key-equality(remote) replay_old_key=refused guessed_key=allowed
              across_grant_key=checked
```

**5. 判读——三条确证**

**（a）C-B3a 是真越界泄露，不是短传。**
128KiB 全部传输完成：前半（8192 词）是 slice0 尾，后半（8192 词）**正是 slice1 头的真实字节**，
整段 `poison=0 other=0`。跨 grant 边界把兄弟 grant 的数据服务出来了。

**（b）key 按「每个覆盖到的 grant」校验。**
C-B3c 携带 `V0` 跨向 key=0 的 slice2 → 拒绝，且 `slice1=8192 poison=8192` 说明中断恰好发生在
越界处（grant 内 64KiB 已落盘，越界半仍是 poison）。
⇒ 越界**只**在「区间覆盖到的每个 grant 的 key 都等于请求 key」时放行。B3a 成功不是因为
边界不设防，而是因为 slice1 被刻意设成与 slice0 同 key。
**泄露的触发条件因此收敛为一条：两段 grant 共享同一个 value。**

**（c）拒绝是边传边判（lazy），不是一次性 range 检查。**
C-B3b 的 `slice1=1048576 poison=1048576 other=0`（16MiB 请求：grant 内 8MiB 已传、
越界 8MiB 未写）与 C-B3c 同形。附带一条产品相关事实：**拒绝 ≠ 无副作用** ——
被拒 READ 已经写进目标缓冲。产品侧无害（失败的片被丢弃并重试，成功的片按其自身长度覆写），
但它否证了"没有 CR 就等于目标未被触碰"这个直觉，故记录在案。

**6. 修正后的隔离模型（本批核心产出）**

> 一个 TABLE tid 承载进程内全部 Piece 的 grant。provider 的访问控制是
> **「tid 表内、覆盖请求区间的每个 grant 的 key 与请求 key 相等」** ——
> **grant 边界不是屏障，tid 也不是。**

与 V-1 完全自洽：同 value 的两个 grant 之间没有任何屏障
（C-V1 用 slice0 的 descriptor + slice1 的 VA 直接读到了 slice1 的字节）。

这解释了 A12 前后**性质上的差别**：A12 之前"一 Piece 一 tid"，越界读最多只能打到同一个 grant，
所以"range 跨 grant 解析"这个平台性质**无从产生后果**；A12 之后一个 tid 装 N 个 grant，
它**第一次成为隔离面上的真实边界**。第三十一批说"只弱化了 tid 一项"仍然成立，
但本批给出了它**为什么重要**的机制：缺的那一项不是"多猜一个数"，而是"少一道边界"。

**7. 产品影响：B-3 不可达，但已是承重假设**

B-3 类越界在**产品 API 上无法表达**，两道守卫均已核实：

1. 请求侧上界：[shim.c](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/urma/ffi/shim.c#L1250-L1255)
   强制 `remote_offset + length <= remote->length`；而
   [read_child_owner.rs](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/urma/read_child_owner.rs#L444-L469)
   的 `remote_offset = posted_bytes`、入场条件 `length <= piece_length - posted_bytes`
   ⇒ 请求区间恒在 `[0, piece_length)`
2. 注册长度 ≡ 公告长度：[shim.c](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/urma/ffi/shim.c#L967-L991)
   为每个 Piece `posix_memalign` 一段**恰好等于 `length`** 的私有拷贝并注册；
   [shim.c](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/urma/ffi/shim.c#L1078-L1080)
   导出 descriptor 时用 `seg->len != source->length` 兜底（否则 `-ERANGE`）

⇒ **B-3 不构成产品缺陷**。但它现在是承重的：上述两条中任何一条被改动（例如让注册长度大于
公告长度、或允许"一次注册多片复用"），第 6 节的平台性质会立刻变成可利用的跨 Piece 读。
**这两条守卫必须作为显式不变量保留，并在任何涉及 READ 源注册/描述符导出的改动中被复核。**

**8. P0：`token_value` 可预测性 —— 从"待复核"升级为"已确认残差"**

C-X1 让 parent 用 `value = old + 1` 重注册且**不告知** child，child 盲猜该值 →
**ALLOWED，读到的是该 grant 自己的字节**。而产品的 value 正是
[server/urma.rs](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/urma/server/urma.rs#L587-L599)
的 `NEXT_READ_TOKEN`：从 **1** 起、每注册 **+1**、进程唯一。
**`old+1` 不是探针编造的场景，就是产品的真实行为。**

攻击链（不需要越界，因此与 B-3 是两条独立路径，且更便宜）：

1. 持有某个 Piece 的 descriptor（含 VA 与旧 value）
2. 等 parent 把这个 VA 重新用于新 Piece（同尺寸重复分配时堆复用同一地址是常态）
3. 试 `value ∈ {old+1, ...}` ⇒ 读到未授权的兄弟 Piece

C-V2 说明"直接用旧 key"被挡住，C-X1 说明"猜下一个 key"挡不住 ⇒
A12 之后唯一的秘密是 value，而这个秘密是**低熵且由单一样本可推导**的。

加固约束：字段只有 32 位
（[urma_types.h](file:///home/yuan/workspace/cloud-native/umdk/src/urma/lib/urma/core/include/urma_types.h#L313-L315)，
`urma_token_t { uint32_t token; }`），且现有语义要求**进程生命周期内永不重复**
（注释："a stale offer can never arm a new export"）。

**9. P0 加固设计（已实施）**

目标：让 value 对**持有旧 descriptor 的 peer**不可预测，同时保持进程内永不重复。

初版方案对比（A 被采纳，形态按下文修正）：

| 方案 | 形态 | 不重复性 | 对"持有一个样本"的预测者 | 判定 |
| --- | --- | --- | --- | --- |
| A 随机 + 已用集合去重 | 每次注册取 32 位随机值，命中已发放集合则重取 | 集合保证 | 不可预测、不可由单个样本推导下一值 | 采纳（形态修正） |
| B 随机起点 + 奇数大步长 | `value_k = start + k*stride (mod 2^32)`，stride 奇数 ⇒ 2^32 次内双射 | 数学保证、零内存 | 拿到**两个**样本即可解出 stride；对单样本攻击者仅剩遍历，**无效** | 否决 |
| C 值域扩到 64 位 | — | — | — | 不可行：`token` 是 32 位 |
| D 回到"每片新 tid" | 即回退 A12 | — | 直接消除该维度 | 否决：等于放弃第三十二批的 ×4 |

**方案 A 的形态修正（实际实现）**：字面的"随机 + 已用集合"有一个长期代价——为了守住
"进程内永不重复"，集合必须保留**全部历史值**，而注册次数 ≈ Piece 传输次数
（实测 5888 MiB/s ÷ 16MiB ≈ **370 次/秒**），即约 128MB/天、3.8GB/月，对长跑节点不可接受。

因此改为**方案 A 的零内存等价形态**：对 32 位 token 空间做**带密钥的双射置换**
（4 轮 Feistel，轮函数为 `RandomState` 播种的 SipHash-1-3，截断 16 位）：

- **不重复由结构保证**：Feistel 对任意轮函数都是双射 ⇒ 不同 index 永不碰撞，
  无需任何账本、无重取循环、内存 O(1)
- **不可预测由密钥保证**：值不能由观察到的样本外推（无密钥）；这正好覆盖
  X-1 的攻击者定义（"手里已有一个样本"），而方案 B 对它无效
- 恰好一个 index 映射到 0（`URMA_TOKEN_NONE` 是 policy 不是 key），跳过该 index 即可，
  与旧实现"从不发放 0"的语义一致
- index 空间耗尽沿用原有失败形态：`checked_add` 返回 None ⇒ 拒绝注册（不环绕重发）

落地位置：[server/urma.rs](file:///home/yuan/workspace/dev/dragonfly/dragonfly-client-urma-read/dragonfly-client-storage/src/server/urma.rs#L587-L660)
（`ReadTokenSpace` + `allocate_read_token`，替换原 `NEXT_READ_TOKEN` 计数器；无新依赖，
`RandomState` 的密钥由 std 从 OS 播种）。

离线验证：`cargo test -p dragonfly-client-storage --features urma` **311 passed / 0 failed / 1 ignored**
（含 3 个 token 测试：空间耗尽不环绕且不发放 0、连续 65536 个 index 全部互不相同、
相邻两次发放不构成 `+1`——最后一条即 X-1 所利用性质的反向回归）。`cargo fmt --check`
与 `cargo clippy --all-targets` 对该文件零告警。

**仍被否决**的替代路线：靠"避免 VA 复用"堵这条路——要保证 16MiB 级私有拷贝的虚拟地址
永不复用需长期保留地址空间（mmap/mprotect 或永不释放），长跑下地址空间会被耗尽，
代价远大于置换 token。

**边界**：本方案只提高**猜测与外推成本**，不改变第 6 节那条平台性质
（同 value 的 grant 之间无屏障）。它与"第 7 节两道守卫必须保留"是一组配套结论，缺一不可。

**10. 状态**

- A12 隔离面**真机验证完成**：B-1 / B-2 / B-4 成立；B-3 被否证并已定性为平台性质
  （产品 API 不可达）
- 探针 `read_isolation_probe.c`（1912 行）为本批交付物；`read_multiseg_probe.c`（v2，1422 行）保持
- **P0 已从"待复核"升级为"已确认残差"，并已完成加固实现**（离线 311 测试通过），
  真机回归见第三十四批（src-004，与 src-003 对照为性能中性、0 回归）
- 待办：capacity 真实上界（被 `MAX_SEGS=512` 封顶，且只用 1MiB 段测得）、
  monitor `tasks.log` 池计数器口径

---

## 第三十四批：P0 加固的真机回归（read-src-004 对照 src-003）（2026-09-22）

**1. 目的与口径**

`read-src-004` = A12 + P0 token 加固（`ReadTokenSpace` 带密钥双射置换，第三十三批第 9 节）。
用例与 src-003 完全相同：`read-piece16-cc8-post1-pipe2`（16MiB × cc8 × 64 片，
warmup 1 + sample 4 = 全量 256 片）。目标是验证加固**不引入性能或正确性回归**。

**2. 对照表**（p50，括号为全量 n=256）

| 指标 | src-002（A12 前） | src-003（A12） | src-004（A12+P0） |
| --- | --- | --- | --- |
| register sub token id | 9.06 ms | 0.00 ms | 0.00 ms |
| register sub copy #2 (shim) | — | 2.08 ms | 2.09 ms (2.09) |
| register sub MR pin | — | 0.17 ms | 0.17 ms |
| stage register | 43.39 ms | 5.84 ms | 6.46 ms (6.68) |
| stage wait ReadDone | 35.24 ms | 7.19 ms | 7.37 ms (7.42) |
| stage revoke/unregister | 3.81 ms | 0.74 ms | 0.78 ms (0.78) |
| source E2E | 79.57 ms | 13.30 ms | 14.08 ms (14.16) |
| child piece p50 / p95 | 88.44 / 167.54 ms | 23.30 / 29.30 ms | 23.26 / 30.53 ms |
| aggregate throughput | 1467.1 MiB/s | 5888.4 MiB/s | 5914.1 MiB/s |
| failures / tcp fallback / retained warnings | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |

**3. 判读**

- **加固性能中性**。register 5.84 → 6.46 ms 的 +0.62 ms 不可能来自 token 改动：
  `next_read_token()` 在 Rust 侧 `publish_and_wait_read_done` 之前调用，是 4 次
  SipHash（亚微秒级），且**不在任何被计时的 stage 内**（`register sub token id` 计的是
  shim 里的 tid 分配，A12 后恒为 0）。真正变化的只有排队残差：
  6.46 − 2.09 − 0.17 ≈ 4.20 ms vs 5.84 − 2.08 − 0.17 ≈ 3.59 ms，即 owner 线程占用抖动。
- child 侧 p50 23.26（src-003 23.30）持平、吞吐 5914.1（5888.4）略升、
  failures / tcp fallback / retained warnings **全 0** ⇒ 端到端无回归。
- `register sub token id p50 0.0` 继续确认 A12 的共享 tid 路径仍在生效；
  `copy #2` 稳定在 2.09 ms（占 register 的 ~89%），与第二十三批否决 Stage 1/2 自洽。
- warmup 的 p95 偏高（child 138.59 / parent 110.72 ms）来自 destination 池冷启动 +
  首片注册，sample 阶段 p95 回到 30.53 / 19.71 ms，属预期形态。

**4. child destination 池**

- sample-001：`register (miss) 0 / hit 64 = 100% / peak 128MiB`
- warmup-001：`register 8 / hit 56 = 87.5%`（8 miss 建池 = cc8，符合设计）
- 全量：`register 8 / hit 248 = 96.9% / returned 256 / peak 128MiB`
- `child.sample-001.tasks.log` 的池计数器仍全为 0（同 sample 的 `.log` 是 64/100%）
  ⇒ 与 src-003 一致，**确认为 monitor 侧口径问题**，不是池行为异常

**5. X-1 的验证方式更正（重要）**

第三十三批第 10 节曾写"用探针复验 X-1 是否已不可复现"，**这是错的**：
`read_isolation_probe` 的 parent **自造 value**（`V0` / `0` / `W` / `old+1` 全是硬编码），
不走产品的 `allocate_read_token()`，所以加固前后它都会照旧报
`C-X1 guessed-key allowed`。它测的是"**value 可预测时会发生什么**"，
不是"**产品当前发放什么**"，因此它不是加固的验证手段。

加固的证据因此改为：

1. 3 个 token 单测——唯一性（连续 65536 个 index 取值互不相同）、不发放 0、
   相邻两次发放不构成 `+1`（即 X-1 所利用性质的反向回归）
2. 本批 End-to-end 回归（src-004）证明替换后**性能与正确性均无回归**

要在硬件上"证伪猜测"需让探针 parent 改用产品分配器并做穷举，收益极低
（等价于穷举 32 位置换，且每次尝试都要重建 lane），故不做。

**6. 状态**

- **P0 加固收口**：离线 311 测试 + src-004 真机回归均通过，无回归
- A12 性能收益保持：child piece p50 23.26 ms、5888–5914 MiB/s（vs A12 前 88.44 ms / 1467 MiB/s）
- 待办不变：capacity 真实上界、monitor `tasks.log` 池计数器口径

---

## 第三十五批：capacity 上界（字节口径）+ monitor 口径收口（2026-09-22）

**1. 为什么旧口径不成立**

旧结论是"一个 TABLE tid 接受 512 个同时存活段"，但那是 512 × 1MiB = **覆盖 512MiB**，
而产品的真实约束不是段数而是**覆盖字节**：`sourceBytes / Download.PieceLength` 个 source
grant 必须同时挂在同一个共享 tid 上（A12 之后进程只有一个 tid）。线上池 `sourceBytes`
= 1GiB ⇒ 16MiB 片时为 64 个 grant、4MiB 最小片时为 256 个 grant，**覆盖都是 1GiB**。
旧测量只到 512MiB，即最坏情况的一半字节，因此**不能**证明线上池可满足。

**2. 探针改动**（`read_multiseg_probe.c`）

- `MAX_SEGS` 512 → 2048；新增 `--sweep-bytes`（默认 2GiB pinned 预算），
  扫描在「provider 首次拒绝 / entry 上界 / 字节预算 / 本地分配失败」四者中最先到达处停止
- `P-SWEEP` 同时报 entries、覆盖字节，并显式打印**被哪个界停下的**
- 加字节预算的原因：`MAX_SEGS` 提高后，老命令 `--sweep 2048 --seg-bytes 16777216`
  会邀请 32GiB 分配；预算是必要的安全伴生改动
- `cc -O2 -Wall -Wextra` 零告警；child 侧无需改参数（n 与段大小由 parent 的 wire header 公告）

**3. 真机结果**（parent=.196，child=.198；两次独立会话）

| parent 命令 | P-SWEEP 结果 | 停止原因 | child |
| --- | --- | --- | --- |
| `--segments 8 --seg-bytes 16777216 --sweep 128` | one tid held **128** segments, **2147483648 bytes** covered | entry bound（无拒绝） | A12 PASS |
| `--segments 8 --seg-bytes 1048576 --sweep 2048` | one tid held **2048** segments, **2147483648 bytes** covered | entry bound（无拒绝） | A12 PASS |

两次 parent 其余输出一致且干净：`P-TID 0xdb00`、`P-REG 8 segments live errno=0`、
`P-UNREG index=0 status=0 (other segments stay live on the same tid)`、`done exit=0`；
child 两次均 `C-VERDICT A12 PASS`（correctness 8/8、lane_reuse、isolation 2/2、
independent_unregister、stale_fail_closed 全 PASS），并确认扫描段被干净回收
（扫描没有污染后续 probe 段）。child endpoint caps：`max_read_size=268435456`、
`max_jfs depth 8192`。

**4. 判读**

- **产品口径已满足，且有余量**：单 tid 覆盖 **≥2GiB** 无拒绝（产品形状 16MiB 段），
  是线上 1GiB 池的 **2×**；entry 侧 ≥2048 个同时存活 grant，是 4MiB 最小片最坏情况
  （256 个）的 **8×**。两个维度都覆盖线上默认配置的最坏情形。
- **精确上界仍未取到，且这次没能区分"按字节受限"与"按 entry 受限"**：两条命令的
  `--sweep × --seg-bytes` 恰好都等于 2GiB 预算，所以两次都是先撞到 entry 上界、
  覆盖字节刚好等于预算，预算判据从未触发。这是推荐命令设计上的失误（两个界重合），
  不是探针缺陷。
- 继续逼近真实上界的代价是 **1:1 的 pinned 内存**（注册即 pin 页），即"要测到的容量"
  本身就是它需要的内存；因此残差风险被限定在**非默认 `sourceBytes > 2GiB`** 的配置上，
  默认配置（1GiB）已被 2× 覆盖。若要把产品形状的证明推到 4GiB，可再跑一条
  `--seg-bytes 16777216 --sweep 256 --sweep-bytes 4294967296`（需 ~4GiB 空闲），
  但这只提高余量倍数，不改变结论方向。
- **容量耗尽的后果是 fail-closed 但带粘性**（非错误数据）：`shim.c` 在
  `urma_register_seg` 失败时置 `registration_uncertain`/`closing` 并**保留共享 tid 的引用计数**
  （共享 tid 不能在其他 source 存活时释放/重分配，这是刻意的），Rust 侧把字节记入
  quarantine；持续耗尽会吃掉 `quarantineBytes` 直到 READ 准入拒绝、child 回落 TCP。
  即吞吐悬崖，不是正确性问题。
- 附带确认：两次独立 parent 进程都拿到同一个 `table_token_id=0xdb00`，说明 ummu 对
  第一个 tid 的分配是确定性的——tid 取值本身无随机性，这也再次说明 P0 加固必须落在
  `token_value` 上（第三十三批第 9 节），而不是 tid。

**5. monitor 口径收口：`.tasks.log` 池计数器全 0 是构造性缺失**

根因链（不需要真机即可闭合）：

1. `*.tasks.log` 是 `b7.py` 的 `filter_task_scoped_log()` 输出，只保留
   `last_task_id(line) ∈ task_ids` 的行
2. 池标记来自 `read_buffer_pool.rs` 的 `debug!(length, retained_bytes, ...)`，
   只有这两个字段，**没有 `task_id`**
3. 它执行在专用 owner 线程 `dragonfly-urma-fabric`（`fabric.rs`，普通 `std::thread`，
   不继承调用方 async 任务的 span）⇒ 该线程上的事件不可能带 `task_id`
4. 而 piece attempt / completion 行在任务 span 内，带 `task_id`
   ⇒ 于是"piece 数字与 `.log` 完全相同、池块全 0、peak 0B"三者自洽

因此 `.tasks.log` **从来就不是池数据的载体**：src-004 的池结论以区间日志为准
（`child.sample-001.log` 64/100%、全量 8 miss / 248 hit 96.9%），**均继续成立**。
工具侧已修 `read_pool_monitor.py`：目录遍历跳过 `*.tasks.log`（顺带消除
range log 与 task log 的 piece 行重复计数），显式传入该文件时打印 note 说明口径；
已用合成样本验证两种行为。

**6. 状态**

- capacity 收口：单 tid 覆盖 ≥2GiB / ≥2048 entries 均无拒绝，线上默认配置最坏情形被 2×（字节）
  与 8×（entry）覆盖；精确上界为 pinned 内存所限、且只对 `sourceBytes > 2GiB` 的
  非默认配置有意义，不再追测
- monitor 口径收口：确认为工具/口径问题，池行为结论不变；工具已修
- 两项待办均关闭；本批无产品代码改动（仅工具：`read_multiseg_probe.c`、`read_pool_monitor.py`）

---

## 第三十六批：review 收口——注册内存硬预算、token PRF 与 shutdown owner 保留（2026-09-22）

本批复核第三十三至第三十五批的产品代码，发现 destination pool 预算与 token 安全论证各有一处需要修正；第三十五批“工具待办关闭”的结论仍成立，但不再代表产品代码已经完全收口。

**1. destination pool 纳入同一份硬预算**

旧实现把回收到 pool 的 registered buffer 从 owner registry 释放，同时允许 pool 自己最多保留一份完整的 `destinationBytes`，因此实际 pinned 上界是 `sourceBytes + active destinationBytes + pooled destinationBytes`，最多比 `totalBytes` 多一份 `destinationBytes`。

修正后保持动态 pool，不静态切半并发容量：

- exact-size hit：pool retained charge 减少、active owner charge 增加，registered 总量不变；
- miss：按 `active destination bytes + retained pool bytes + requested bytes <= destinationBytes` 先淘汰并注销空闲 registration，腾出预算后才创建新 buffer；
- 淘汰时 provider close 失败：句柄重新放回 pool，拒绝本次新建，不能在资源归属不明时继续超配；
- source 上界与 destination 上界之和仍由既有配置校验约束在 `totalBytes` 内，因此总 registered bytes 恢复为配置声明的硬上限。

新增离线回归覆盖 exact hit 的 charge 转移，以及不同 size-class miss 必须先淘汰旧 registration。

**2. token P0 的安全构造修正**

第三十三批使用 `RandomState`/`DefaultHasher` 作为 Feistel 轮函数。它在当前 Rust 实现中是 SipHash-1-3，但公开 API 不承诺内部算法或密码学安全性，因此不能作为 bearer token 的稳定安全契约；“相邻值不是 `+1`”单测也不能证明不可预测。

现改为 4 轮 32-bit Feistel + **四把独立 HMAC-SHA256 轮密钥**：

- 每把 256-bit key 由 `getrandom` 直接从 OS CSPRNG 初始化；初始化失败即拒绝 READ source 注册；
- HMAC-SHA256 是显式依赖，不再依赖标准库未承诺的 HashMap hasher 实现；
- Feistel 双射继续保证进程生命周期内不同 counter index 不碰撞；
- 测试使用固定轮密钥和固定向量，不再用随机实例做概率性断言；
- 安全强度仍受 provider 的 32-bit token 字段限制，必须继续依赖 lane/transfer admission、错误 lane 退役和速率约束，不能描述为超过 32-bit 的认证强度。

因此第三十三批第 9 节和第三十四批第 5/6 节的历史结果应按本节解释：真机结果证明功能与性能中性；不可预测性来自明确的 CSPRNG + HMAC 构造，不由“非 `+1`”测试证明。

**3. shutdown 失败路径保留 native owner**

旧实现先 drain pool；若 `urma_unregister_seg` 持续失败，Rust wrapper 在局部变量 Drop 后丢失，runtime 随后也被 take，可能留下无 owner 的 registered Segment。

修正后：

- `close_all()` 只移除关闭成功的 buffer，失败 handle 留在 pool；
- pool 未排空时不关闭 native runtime；
- owner-thread shutdown 是消费型、外部没有二次重试入口，因此最终失败会有意保留整个 `UrmaRuntime` 到进程退出，并保持 `ACTIVE` fail-closed guard，避免丢失任何仍可被 DMA/provider 引用的 C owner。

**4. 离线验证**

- `cargo fmt --all --check`：PASS
- `git diff --check`：PASS
- `cargo check -p dragonfly-client-storage --features urma --tests --offline`：PASS（使用 UMDK 源码头文件和仅供 check 的占位 lib 路径，不代表完成链接或真机运行）
- 纯 Rust URMA 定向单测（临时 `liburma` 符号 stub，仅用于链接且不执行 provider API）：token 3 passed；destination pool 4 passed
- `cargo test -p dragonfly-client-storage --lib --offline`：109 passed / 0 failed（沙箱外；沙箱内 4 个 sendfile 用例因 `Operation not permitted` 失败，确认是执行环境限制）
- 真机 `--features urma` 回归：待下一次部署后执行；重点观察 pool hit/miss/evict、registered bytes 峰值和 shutdown 日志

**5. 状态**

- 第三十五批的 capacity 与 monitor 工具项保持关闭；
- destination registered-memory 超预算已在代码中修复；
- token P0 已换成具有稳定安全契约的显式 PRF 构造；
- shutdown 不再丢失失败 close 的 native owner；
- 剩余验证只有上述三项的真机回归，不需要重跑 TABLE tid capacity/isolation 探针。

---

## 第三十七批：cc 扫描（read-src-005~008）——前导的 cc 代价与源端串行项（2026-09-22）

`read-src-005~008` 四条 run 覆盖 16MiB 片 cc8/cc16/cc32 与 32MiB 片 cc16，用来把第三十六批遗留的「数据面速率随并发如何变化」问到底。汇总改用本批新增的 `read_attribution_summary.py`：跨 run 三视图（per-run E2E 分解 / parent 数据面 + child Piece E2E，samples 与 warmup 分开池化 / per-batch 启动链），`*.tasks.log` 与 `evidence/<role>.log` 汇总 blob 自动排除。

**0. 先钉死一个先前含糊的口径：cc 不是并发进程数**

`ok/att = 256/256`、`transfer.summary.samples = 3`、`child.warmup-001` 的池计数为 64 片三者共同说明：每个 batch 只有**一个** child dfget 进程下完整的 1GiB 文件（`concurrency` 仍为 1），`cc` 是**同一 lane 上同时 in-flight 的 Piece 数**。表 2 的 `n`（192 / 96）是片线数，不是样本数。此前的 `maxOutstandingPerPeer ≈ cc × ceil(pieceLength / effective_max_read_size)` 估算正是按这个语义写的，本批得到确认。

**1. 配置与正确性（正面结论）**

| run | case | cc | piece | maxRd | effMaxRd | chk | agg MiB/s | start ms | piece ms | tail ms | dfget ms | rate MiB/s | ok/att | fallb |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 005 | read-piece16-cc8-post1-pipe2 | 8 | 16MiB | 16MiB | 16MiB | 1 | 3863.5 | 83.37 | 170.46 | 3.32 | 257.24 | 6007.3 | 256/256 | 0 |
| 006 | read-piece16-cc16-post1-pipe2 | 16 | 16MiB | 16MiB | 16MiB | 1 | 3718.9 | 126.02 | 146.03 | 3.40 | 277.56 | 7012.3 | 256/256 | 0 |
| 007 | read-piece16-cc32-post1-pipe2 | 32 | 16MiB | 16MiB | 16MiB | 1 | 3583.8 | 171.74 | 121.58 | 3.48 | 299.92 | 8422.3 | 256/256 | 0 |
| 008 | read-piece32-cc16-post1-pipe2 | 16 | 32MiB | 32MiB | 32MiB | 1 | 3778.2 | 161.53 | 110.98 | 3.40 | 275.87 | 9226.8 | 128/128 | 0 |

- `effective_max_read_size` 与配置逐条相等，**设备没有夹取**「`maxReadSize = pieceLength`」⇒ 每片 1 WR / 1 rsge / 1 sge 在 16MiB 与 32MiB 上都是设备接受的值；
- `fallbackErrors = 0`，成功/尝试 256/256、256/256、256/256、128/128，quarantine 未触发，tail 恒定 3.3~3.5ms；
- 池计数给了预算语义一个直接旁证：**warmup 的 pool miss 恰等于 cc**（8 / 16 / 32 / 16），sample 全部 hit。即首次必须为 cc 个并发片各准备一个 destination buffer，之后逐片回收复用。这既验证第三十六批第 1 节的动态池语义，也说明 `read-piece64-cc16`（16 × 64MiB = 1GiB = 默认 `destinationBytes`，零余量）必须抬池才安全；
- lane 只在 warmup 建立（`lanes=1`），三个 sample 批次 `lanes=0`（启动链表标 `reused`）⇒ **建链是一次性成本且被 warmup 吸收**，样本批次测的是稳态。

**2. 片阶段收益被前导吃掉（本批主结论）**

- 片阶段速率 6007 → 7012 → 8422 MiB/s：cc ×4 只换来 ×1.40，**次线性**，说明片阶段接近饱和而不是被并发线性加速；
- 前导 83.37 → 126.02 → 171.74ms：边际 5.33 ms/cc（cc≤16）后放缓到 2.86 ms/cc（cc≥16）；
- 净 E2E 单调变差：dfget 257.24 → 277.56 → 299.92ms，aggregate 3863.5 → 3718.9 → 3583.8 MiB/s。
- 因此**对 1GiB 文件提高 cc 是净亏**。以本批数字外推 16GiB 文件：cc8 ≈ 16 × 170.46ms + 83.4ms = 2.81s（5.8 GiB/s），cc32 ≈ 16 × 121.58ms + 171.7ms = 2.12s（7.7 GiB/s）——只有文件足够大、前导被摊薄，cc 才转化为收益。
- 优化第一优先项因此从「降低每片串行成本」前移到**前导的 cc 依赖**与**固有前导**两块。

**3. 每秒片的成本分布：哪一部分真的是串行**

| run | cc | childE2E p50 | srcE2E p50 | register | alloc | copy#2 | pin | token | wait | revoke |
|---|---|---|---|---|---|---|---|---|---|---|
| 005 | 8 | 23.10 | 13.91 | 6.68 | 未采集 | 2.08 | 0.18 | 0.00 | 7.21 | 0.76 |
| 006 | 16 | 43.16 | 25.43 | 11.80 | 未采集 | 2.11 | 0.20 | 0.00 | 12.88 | 2.31 |
| 007 | 32 | 87.10 | 57.47 | 26.45 | 未采集 | 2.12 | 0.20 | 0.00 | 28.16 | 5.02 |
| 008 | 16 | 84.03 | 55.35 | 22.37 | 未采集 | 3.85 | 0.11 | 0.00 | 22.92 | 1.86 |

（`alloc` 列是在本轮汇总工具里新加的，这四条 run 的区间日志当时只带旧字段，需要下一次 run 才有值。）

- childE2E p50 随 cc 线性 +2.67 ms/cc，其中 68% 落在 parent 侧（srcE2E +1.82 ms/cc）；
- parent 侧的增量几乎全部由 `register`(+0.82) + `wait`(+0.87) + `revoke`(+0.18) = 1.87 ≈ srcE2E 的 1.82 构成；
- 而 `copy#2`(2.08→2.12) 与 `MR pin`(0.18→0.20) **完全不随 cc 变化** ⇒ 串行瓶颈既不是 memcpy 带宽也不是 MR pin；
- `token = 0.00` 复现第三十二批的 A12 结论；`revoke` 0.76→5.02ms(+0.18 ms/cc) 说明源端 teardown 与 register 共用同一条串行路径。

**关键区分（wait 不是锁）**：把 `wait` 还原成聚合带宽 `cc × 16MiB / wait_p50` 得到 17.75 / 19.88 / 18.18 GiB/s —— **在 cc ×4 范围内恒定**，所以 `wait` 的增长是「cc 个读共享固定聚合带宽」的应有表现，不能作为锁竞争证据。反观 `register` 与 `revoke`，两者都不含数据传输，其线性增量无法用带宽共享解释，是**真正的串行项**。

由此得到一个尚未被利用的额度：聚合读带宽约 **18 GiB/s（≈150 Gbps）**，而片阶段实测只有 5.9 / 6.9 / 8.2 GiB/s，即**只拿到 33% / 34% / 45%**。剩余额度不是在「带宽不够」，而是在**每秒片关键路径中无法重叠的那部分**（register + revoke + 非重叠的 wait）。

**4. 可复现性**

`read-src-005` 与 `read-src-004` 是同一条 case（`read-piece16-cc8-post1-pipe2`）：片阶段 6001 vs 6007 MiB/s（0.1% 以内）、tail 3.34 vs 3.32ms，唯一漂移是前导 105.92 → 83.37ms（-22.6ms），E2E 因此从 3619.0 升到 3863.5 MiB/s。**数据面速率在两次独立真机上可复现到 0.1%，前导是唯一漂移项**，这是好消息：后续 A/B 只要盯住片阶段即可获得高信噪比，前导则必须成对测量。

**5. 下一步（按判读优先级）**

- **N1（第一刀）TCP 同 cc 扫描** `tcp-piece-cc8/cc16/cc32-post1-pipe2`：若 TCP 的前导也随 cc 线性上升 ⇒ 前导的 cc 依赖在 dfget/scheduler 层、与 URMA READ 无关，优化应投在那里；若 TCP 前导平坦 ⇒ 是 URMA lane 的串行 admission。这是唯一能把前导定性归因的实验，优先级高于任何产品改动。
- **N2 干净 A/B 隔离「单 WR 尺寸」**：固定 32MiB 片，`maxReadSize` 4MiB（8 WR/片）对照 32MiB（1 WR/片）。本批 008 vs 006 同时改了 `pieceLength` 与 `maxReadSize`，因此 9226.8 MiB/s 不能归因于单 WR 更大。
- **N3 cc64**（受 `maxOutstandingPerPeer = 256` 与设备 `max_jfs_depth` 约束，需先确认硬校验通过）：看速率是否饱和、前导是否继续线性，用来定位 E2E 最优 cc。
- **N4** 用新加的 `alloc` 列确认 `register` 的线性增量是否落在 alloc/first-touch：若是，指向 Stage 1（shim backing-memory pool + 复用 registered slot）。

**6. 工具（本批新增/修正）**

- `read_attribution_summary.py`（新增）：跨 run 三视图；`effMaxRd ≠ maxReadSize` 时自动打 note；无 lane 行的批次标 `reused` 并计数 `lanes`；`--json` 可机读；
- `read_pool_monitor.py`：补 `urma READ pool evicted` 的解析与 `evicted from pool` 输出行——第三十六批第 1 节的淘汰路径此前在工具侧完全不可见。

---

## 第三十八批：read-src-009 稳态复跑与下一轮优化门（2026-09-22）

`read-src-009` 复跑 `read-piece32-cc16-post1-pipe2`：RM/CTP、1GiB、32MiB Piece、cc16、每片 1 个 32MiB READ WR，warmup 1 次、sample 3 次。128/128 次 READ 成功，session 首次 1 次、复用 127 次，fallback/transfer error 均为 0。

| run | aggregate MiB/s | start ms | piece ms | tail ms | dfget ms | piece rate MiB/s |
|---|---:|---:|---:|---:|---:|---:|
| read-src-008 | 3778.2 | 161.53 | 110.98 | 3.40 | 275.87 | 9226.9 |
| read-src-009 | 4021.4 | 137.37 | 113.94 | 3.51 | 254.80 | 8987.0 |

009 相对 008 的 aggregate 提升 6.4%，来自前导缩短约 24.2ms；Piece 阶段耗时反而增加 2.7%，对应速率下降约 2.6%，仍在同一个约 9GiB/s 档位。009 的 `startToFirstPiece` 占总时延 53.9%，因此 4.0GiB/s 是 1GiB 文件的 wall-clock 吞吐，不是 READ 数据面的上限。即使把前导完全消掉，按当前 Piece+tail 也只有约 8718MiB/s，后续必须同时优化前导和 Piece 路径。

本批 manifest 只有汇总值，没有 register 的 alloc/copy/pin/wait/revoke 原始分项，不能据此直接选择代码改动。下一轮严格按以下顺序：

1. 用 `read_attribution_summary.py results/read-src-008 results/read-src-009` 读取完整结果目录，确认 009 的 alloc 与 register 排队项；
2. 跑已有 TCP cc8/16/32 对照，判断前导的 cc 依赖来自 dfget/scheduler 还是 URMA admission；
3. 固定 32MiB Piece、cc16、post1、pipe2，只将 `maxReadSize` 从 32MiB 改成 4MiB，严格隔离 1 WR/Piece 与 8 WR/Piece；
4. 只有分项确认源端 register 队列仍占主导后，才进入产品优化：优先评估直接注册 page-aligned `MappedPiece` 并把 mmap 所有权保留到 revoke，消除 shim 的 `posix_memalign + memcpy`；若 provider 不接受文件映射，则改为复用 source backing/registered slot。两条路径都必须保持现有 source budget、token 和 revoke owner 语义。

暂不提高 `maxInflightChunks`：当前 32MiB Piece/32MiB maxReadSize 每片只有一个 WR，该参数不会增加单 Piece 的 READ 并行度；盲目提高 cc 只会继续扩大已经观察到的前导和注册排队。

---

## 第三十九批：RM READ 端到端阶段日志与 CRC/pwrite 重叠（2026-09-23）

本批按范围修正第三十八批的下一步：**不做 dfget/scheduler 调度层优化**。`dfgetToFirstReadStartNs` 只作为外部边界观测，用于从总墙钟中扣出 READ 尚未开始的时间；所有优化决策只落在 URMA RM READ 的 source、协议、WR/CQE、lease 与 Storage consumer 路径。

### 1. Child READ 传输拆分

成功路径新增以下逐 Piece 纳秒计时，并由同一条 `urma READ child finished transfer` 日志输出：

- `retained_cleanup_ns`：进入本次传输前重试遗留 owner；
- `lane_acquire_ns`：复用或建立 READ lane；
- `buffer_ready_send_ns`：发送 BufferReady；
- `segment_offer_wait_ns`：等待 Parent 打开/注册 source 并返回 SegmentOffer；
- `destination_admission_ns`：destination pool 获取/创建、remote Segment import 与 owner admission；
- `read_completion_ns`：READ WR post 到全部 CQE retire，是真正的数据搬运区间；
- `lease_publish_ns`：停止 post、unimport、发布 CPU lease；
- `done_round_trip_ns`：ReadDone 到 Parent revoke 后 Done；
- `session_run_ns` / `read_transfer_total_ns`：协议会话与外层总耗时。

Parent 侧原有 `source_open/register_alloc/register_copy/register_seg/wait_read_done/revoke` 保留，因此下一轮可以把 Child `segment_offer_wait` 与 Parent source 分项互相校验。

### 2. dfget 与 Piece 边界

每个 READ Piece 在进入 downloader 时显式记录带 `task_id` 的 `starting dragonfly urma READ piece attempt`。B7 在原有 `startToFirstPiece/firstToLastPiece/tail` 外新增：

- `dfgetToFirstReadStartNs`：dfget 进程开始到第一个 READ Piece 真正进入下载；
- `firstReadStartToFirstPieceNs`：首个 READ 开始到首片完整提交；
- `firstReadStartToLastPieceNs`：首个 READ 开始到最后一片完整提交。

这些字段只用于切开墙钟时间，不构成调度层优化任务。

### 3. Storage consumer 拆分并优化

RM READ lease 写入日志统一为 `file_open_ns / pwrite_ns / digest_ns / storage_total_ns`，后续还有 `storage_write_ns / recycle_ns / metadata_commit_notify_ns / finish_total_ns`。全部显式携带 `task_id`，可被 B7 的 task-scoped evidence 精确归入 sample。

代码复核同时发现 READ lease 原先是 **pwrite 完成后再算 CRC32**，两者串行；现改为两个 blocking worker 并行读取同一个 immutable registered lease，等两者都 join 后才 recycle。lease/owner 生命周期不变，日志仍分别保留 pwrite 与 CRC CPU 耗时，`storage_total_ns` 反映重叠后的实际墙钟。

### 4. B7 输出

- 新增 `result.transfer.urmaReadStageSummary`，对 measured samples 汇总上述 transport/storage/finish/attempt 分项；
- 每个 `child_transfer` 也保留 task-scoped `urmaReadStages`；
- `read_attribution_summary.py` 新增 `toREAD / READ->1 / READspan` 与 Child RM READ p50 阶段表；
- `read-piece32-cc16-read4-post1-pipe2` 作为 4MiB maxReadSize 严格 A/B 用例；README 已删除调度层优化建议。

### 5. 离线验证

- `cargo fmt --all --check`：PASS；
- `cargo check -p dragonfly-client --features urma --offline`：PASS（只有既有 dead-code warning）；
- B7 全量单测：115 passed；
- 新增定向测试覆盖 dfget→READ start 边界以及 transport/storage/commit 字段解析；
- 合成 manifest 验证 `read_attribution_summary.py` 能输出新阶段表。

下一次真机应先复跑原 32MiB/cc16 case，读取 `urmaReadStageSummary`，再跑 4MiB maxReadSize 对照。优化判断限定为：source register/copy、SegmentOffer wait、destination admission、READ CQE、Done/revoke、CRC/pwrite/recycle/metadata；不进入 scheduler 代码。

提交状态：Dragonfly `294f804`（`perf(urma): attribute read stages and overlap storage work`），B7 `494e676`（`feat(b7): report rm read pipeline stages`）。本地分支分别 ahead 1；当前环境没有 GitHub HTTPS 凭据，自动 push 失败，需在有凭据的开发机推送后再由测试节点拉取。

---

## 第四十批：32MiB/4MiB WR 归因与 direct mmap source（2026-09-23）

### 1. read-src-010 与 read-wr4-001

固定 1GiB 文件、32MiB Piece、cc16、post1、pipe2，只改变 `maxReadSize`：

| run | maxReadSize | WR/Piece | aggregate MiB/s | READ span ms | Piece E2E p50 ms | source E2E p50 ms |
|---|---:|---:|---:|---:|---:|---:|
| read-src-010 | 32MiB | 1 | 3053.0 | 230.68 | 98.45 | 72.34 |
| read-wr4-001 | 4MiB | 8 | 2771.0 | 238.00 | 101.03 | 66.77 |

两条 run 都是 128/128 READ 成功、fallback 0，sample 全部 destination pool hit。8 WR/Piece 没有提高数据面吞吐：aggregate 低 9.2%，READ CQE p50 从 2.17ms 增至 3.31ms，Piece E2E 从 98.80ms 增至 101.38ms。因此当前设备和 32MiB Piece 下继续保留 `maxReadSize = pieceLength`；不再投入 4MiB 分片优化。

两条 run 的 Parent 分项都显示 `register` 约 29~30ms、shim 第二次复制约 4.4ms、`wait_read_done` 约 28~30ms。Child 原 `done_round_trip` 约 39ms，但它混合了发送 ReadDone、本机控制流排队、Parent unregister/revoke 和 Done 回包，不能继续作为一个优化对象。

### 2. 本批代码优化

Parent source 注册增加 direct 路径：

- page-aligned `MappedPiece` 直接作为 `urma_register_seg` 的 VA，不再执行 shim `posix_memalign + memcpy`；
- `ReadSource` 持有整个 mapping，只有 native unregister 成功且独立 revocation 已证明后才释放；
- reader/heap bytes 继续使用原来的 shim-owned 对齐复制路径；
- direct 注册返回非空 handle 且错误时，mapping 与 native owner 一起无限期保留，维持 fail-closed；不会在不确定的 provider 状态下自动退回复制注册；
- Parent 日志新增 `register_direct`，B7 汇总输出 direct/copied 计数。

Child 将 `done_round_trip_ns` 进一步拆为 `read_done_send_ns` 与 `done_wait_ns`，同时保留原总值。B7 新版解析器把新增字段视为旧日志的可选字段，因此可以继续重扫 read-src-010/read-wr4-001，而不会把旧 transport 行判为 malformed。

这轮只消除 URMA source 注册前的额外复制并细化协议阶段，不修改 dfget/scheduler 调度。

### 3. 离线验证

- `cargo fmt --all`：PASS；
- `cargo check -p dragonfly-client --features urma --offline`：PASS（只有既有 dead-code warning）；
- `cargo check -p dragonfly-client-storage --features urma --tests --offline`：PASS；
- B7 全量单测：117 passed；
- 两个仓库 `git diff --check`：PASS；
- direct file-backed mmap 是否被当前 provider 接受、以及真实吞吐收益，仍需双机验证。

### 4. 下一次真机判定门

只复跑 `read-piece32-cc16-post1-pipe2`。预期 measured samples 的 Parent `direct/copied = 96/0`，warmup 为 `32/0`；`register_alloc_ns` 与 `register_copy_ns` 应为 0。若 direct 注册失败，保存 Parent provider 错误与整套结果，不自动改走复制路径。

性能判定优先看：

1. `register`、source E2E 和 Piece E2E 是否下降；
2. `doneTx/doneWait` 中延迟是否主要位于 Done wait；
3. READ span 与 aggregate 是否相对同机、同配置基线改善。

跨日期的 read-src-010 只能用于功能和阶段方向参考；最终性能结论应在同一部署窗口成对跑旧 commit 与新 commit，避免前导和机器负载漂移被误判为 direct mmap 收益。

提交状态：Dragonfly `5ec242a`（`perf(urma): register mapped read sources directly`），B7 `875c4f8`（`feat(b7): expose direct read source stages`）；两个本地分支各 ahead 1，尚未推送。

---

## 第四十一批：direct mmap source 真机结果（read-src-014，2026-09-23）

`read-src-010` 是优化前未绑核，`read-src-013` 是优化前绑核，`read-src-014` 是 direct mmap 优化后绑核。代码收益应以配置和绑核一致的 **013 → 014** 为准；010 只保留作历史背景。

### 1. 功能门全部通过

- RM/CTP、32MiB Piece、每片 1 个 32MiB READ WR；
- READ 成功 128/128，fallback 0；
- measured source `direct/copied = 96/0`，warmup `32/0`；
- `register_alloc = 0`、`register_copy = 0`；
- destination sample 96/0/0 全部 pool hit，registered peak 512MiB。

这证明当前 provider 接受 file-backed、page-aligned `MappedPiece` 的 direct registration，mapping 持有到 revoke 的实现也通过了完整下载与内容校验。

### 2. 013 → 014 的真实收益

| 指标 | 013：旧代码+绑核 | 014：direct+绑核 | 变化 |
|---|---:|---:|---:|
| aggregate | 3965.9 MiB/s | 5391.5 MiB/s | +35.9% |
| dfget | 249.01ms | 191.59ms | -23.1% |
| READ span | 193.38ms | 109.00ms | -43.6% |
| Piece span | 112.72ms | 81.67ms | -27.5% |
| Piece-stage rate | 9084.1 MiB/s | 12538.6 MiB/s | +38.0% |
| Child Piece E2E p50 | 83.15ms | 42.25ms | -49.2% |
| Parent source E2E p50 | 54.50ms | 6.53ms | -88.0% |
| source register p50 | 24.42ms | 1.29ms | -94.7% |
| SegmentOffer wait p50 | 25.99ms | 2.25ms | -91.3% |

direct source 消除的并非只有 3.84ms memcpy。旧代码的 source registration 在 owner/provider 路径中形成约 24ms 排队，复制和注册到达节奏又把 Parent `wait_read_done` 拉到 23.13ms。014 中 register 降到 1.29ms、wait 降到 5.34ms、revoke 降到 0.65ms，整条 source E2E 从 54.50ms 降到 6.53ms。Child 的 SegmentOffer wait 同步从 25.99ms 降到 2.25ms，两个时钟域给出一致证据。

### 3. 新的瓶颈

- direct register 已接近下限：1.29ms 中 `urma_register_seg`/pin 为 1.02ms，继续做 source copy/pool 的收益上限很低；
- READ CQE p50 为 2.61ms，destination admission 0.48ms，Done send 约 0ms、Done wait 1.46ms，协议数据面已经不是 Piece E2E 主项；
- pwrite p50 从 22.06ms 升到 34.22ms。结合 source 闸门消失和 Piece 更集中完成，最可能的解释是 16 个 Storage consumer 的并发写竞争增强；当前汇总只证明相关性，仍需并发时间线才能确认因果。但 Piece span 仍下降 27.5%、aggregate 提升 35.9%，不能把这项延迟上升误判为整体回退；
- 014 的 pwrite 34.22ms 已占 Piece E2E 42.25ms 的约 81%。下一阶段若严格限定在 URMA transport，已没有与本轮相当的高收益项；若允许优化 URMA destination-to-Storage 集成，重点应是批量/并行 pwrite 的拥塞与 registered buffer 回收节奏。

`toREAD` 从 52.31ms 增至 78.55ms，属于下载开始前的外部波动，且本轮明确不改调度层；它吞掉了一部分数据面收益，但不影响 direct source 的阶段结论。

### 4. 当前决策

- 保留 direct mmap source；真机门已通过；
- 保留 32MiB Piece 对应单个 32MiB WR，不回到 4MiB 多 WR；
- 不继续做 source registered-slot cache：每片最多只能再省约 1ms pin，却会引入长期 pin、token/revoke 与 Storage eviction 耦合；
- 下一步先把 014 作为新的 RM READ 基线。若继续做 URMA 相关优化，优先设计 destination lease 与 pwrite/CRC 的批处理或并发整形，并保持现有 registered-memory budget 和 owner 生命周期。

---

## 第四十二批：正常路径 READ/pwrite batch envelope（2026-09-23）

为回答 read-src-014 与历史 send/recv 的稳态约 10% 差距究竟来自跨机 CTP 传输还是目标端 pwrite 拥塞，本批不恢复 transport-only benchmark，而是在完整 CRC+pwrite 正常路径增加低扰动时间线。

### 1. 事件与开销控制

每个 Piece 只新增两条 debug 事件：

- `urma READ child completed data transfer`：在最后一个 READ CQE retire 后立即记录，携带 `task_id`、Piece、完成字节、WR 数和单调时钟 `read_completion_ns`；
- `finished pwrite for RM-READ lease`：在 blocking worker 的 pwrite 实际返回后立即记录，携带 `pwrite_ns`、启动时观察到的进程级 `pwrite_active_at_start`、结束后的 active 数和成功状态。

开始时间由 B7 使用“完成事件 UTC 时间戳减去同一线程/阶段的单调时钟 duration”还原。这样无需再为 READ start 和 pwrite start 各写一条日志，每片诊断日志从计划的四条降为两条。活跃 pwrite 原子计数只做观测，不参与 admission 或执行顺序；panic/error 由 RAII guard 归还计数。

### 2. B7 输出

每个 measured batch 写入 `urmaReadTimeline`，顶层写入 `result.transfer.urmaReadTimelineSummary`。`read_attribution_summary.py` 新增 `child RM READ batch envelopes` 表，报告：

- `READenv`：首个 Piece 进入 READ driver 到最后一个 READ CQE；
- `CQEspan`：首个到最后一个 READ CQE；
- `pwrEnv`：首个 pwrite 开始到最后一个 pwrite 结束；
- `1CQE->pwr`：首个 READ CQE 到首个 pwrite 开始；
- `lastCQE->end`：最后 READ CQE 到最后 pwrite 结束；
- `overlap`：READ batch envelope 与 pwrite batch envelope 的交叠；
- `peakPwr`：pwrite 启动时观察到的最大并发数；
- `earlyPwr`：最后 READ CQE 前已经启动的 pwrite 数。

事件按 task-scoped batch 日志聚合；多 task batch 也保持一个统一 envelope。旧日志没有新事件时不生成该 summary，不会被判 malformed，也不影响已有阶段表。

### 3. 判读

- 若 `READenv/CQEspan` 已接近 send/recv 的稳态 span，而 `lastCQE->end`、`pwrEnv` 很大且 `peakPwr` 接近 cc，则剩余差距主要是目标端集中 pwrite；
- 若 `READenv` 本身显著偏大且 pwrite 与 READ 已充分 overlap，则优先归因跨机 CTP/READ 数据面；
- 若 `earlyPwr` 很少、overlap 很小，说明整片 lease/Done 边界限制了网络与 Storage 重叠，才值得设计 chunk 级 lease 流水；
- 所有这些值来自正常完整性路径，仍包含 CRC、pwrite、recycle 和 metadata，不需要 benchmark-only 分支。

### 4. 离线验证

- `cargo fmt --all --check`：PASS；
- `cargo check -p dragonfly-client-storage --features urma --tests --offline`：PASS；
- `cargo check -p dragonfly-client --features urma --offline`：PASS（只有既有 dead-code warning）；
- B7 全量单测：118 passed；
- 新增测试精确覆盖 READ envelope、CQE span、pwrite envelope、overlap、峰值并发和提前启动计数；
- 两个仓库 `git diff --check`：PASS。

提交状态：Dragonfly `12edb13`（`perf(urma): trace read and pwrite batch envelopes`），B7 `fea2f00`（`feat(b7): summarize read pwrite overlap`）；均为本地提交，尚未推送。

### 5. 汇总字段传递修正

首次真机 `read-src-016` 的新表只有表头。根因是 `manifest_views()` 已读取 `urmaReadTimelineSummary`，但 `summarize_run()` 构造 record 时遗漏 `read_timeline` 字段；manifest/evidence 无需重跑。B7 `1c49c50`（`fix(b7): retain read timeline in attribution`）补齐字段并新增回归测试；B7 全量单测更新为 119 passed。拉取该提交后重新执行 `read_attribution_summary.py` 即可显示 016 时间线。

---

## 第四十三批：read-src-016 READ/pwrite envelope 归因（2026-09-23）

`read-src-016` 为 direct mmap source、跨机 RM/CTP、32MiB Piece、cc16、单 WR/Piece。128/128 READ 成功、fallback 0；相对 014，aggregate 从 5391.5 增至 5766.8MiB/s，READ/pwrite/Piece E2E 分项基本一致，说明两条每 Piece 完成事件没有造成可见性能回退。

### 1. Batch 时间线

三个 measured batch 的 p50：

| 指标 | 值 |
|---|---:|
| READ batch envelope | 69.33ms |
| 首个到最后一个 READ CQE | 67.78ms |
| pwrite envelope | 86.31ms |
| 首个 CQE到首个 pwrite | 15.35ms |
| 最后 CQE到最后 pwrite结束 | 33.89ms |
| READ/pwrite envelope overlap | 52.43ms |
| pwrite启动时峰值并发 | 15 |
| 最后 CQE前已启动pwrite | 31/32 |

由 envelope 可还原近似关键路径：首个 READ 开始后约 1.55ms出现首个 CQE，约16.90ms开始首个pwrite，最后READ CQE约在69.33ms，最后pwrite约在103.22ms结束。该值与 `firstReadStartToLastPiece = 106.25ms` 只差最后 recycle/metadata/日志调度等收尾，两个统计口径互相闭合。

### 2. 结论

先前“READ整片完成后才pwrite，导致整个batch传输与Storage串行”的假设被否定：虽然单Piece内部仍有整片屏障，但Piece并发使31/32个pwrite在最后READ CQE前启动，READ与pwrite已重叠52.43ms。

当前关键路径是：

```text
首个READ开始
  -> 约16.9ms后首个pwrite开始
  -> READ与pwrite重叠约52.4ms
  -> 最后READ CQE
  -> pwrite继续排空约33.9ms
  -> recycle/metadata/Piece完成
```

`peakPwr = 15` 几乎达到 cc16，同时单Piece pwrite p50 为32.93ms；因此最强证据指向同一task文件上的大量32MiB并发pwrite及其最终排空。READ数据面本身的batch envelope为69.33ms，对应1GiB约14.4GiB/s；pwrite envelope为86.31ms，对应约11.6GiB/s，Storage envelope更慢且决定最后33.9ms尾部。

与历史单机 RTP send/recv CC32 比较时，严格稳态完成斜率约为：send/recv 13497MiB/s（13.18GiB/s），016 READ约12426MiB/s（12.14GiB/s），READ低约7.9%。由于前者是单机RTP、16MiB/CC32并使用window级pwritev，后者是跨机CTP、32MiB/CC16，不能把7.9%直接归因于opcode；016说明至少目标端pwrite drain是明确存在的应用关键路径。

### 3. 下一步门

优先做URMA READ destination-to-Storage的pwrite并发整形A/B，而不是source cache或增加READ WR：保持32MiB单WR、cc16和512MiB registered budget，只将同task并发pwrite上限比较4/8/16。观察 `pwrEnv`、`lastCQE->end`、Piece span和aggregate：

- 若限流8使单次pwrite明显缩短且pwrite envelope/尾部下降，则文件系统并发竞争得到确认，可固化进程级RM-READ写入permit；
- 若限流后pwrite envelope增大，则当前15路并发已接近吞吐最优，不应加锁；下一候选才是chunk级READ完成后提前消费，但它需要新的partial lease/CRC顺序与owner协议，复杂度显著更高。


---

## 第四十四批：CC4/8/16 真机曲线与独立 pwrite admission（2026-09-23）

### 1. 真机并发曲线

保持跨机 RM/CTP、1GiB 文件、32MiB Piece、单个32MiB READ WR、direct mmap source、相同绑核与注册内存预算，只改变 concurrentPieceCount：

| CC | aggregate | READ envelope | pwrite envelope | 最后CQE到最后pwrite | pwrite p50 | peak pwrite |
|---:|---:|---:|---:|---:|---:|---:|
| 4 | 6028.0MiB/s | 87.39ms | 88.90ms | 5.00ms | 3.90ms | 3 |
| 8 | 6231.8MiB/s | 73.46ms | 79.99ms | 14.51ms | 13.17ms | 7 |
| 16 | 5579.0MiB/s | 63.95ms | 80.76ms | 34.50ms | 31.23ms | 14 |

READ 随 CC 增加持续改善：按1GiB/READ envelope计算约为11.44、13.61、15.64GiB/s。但 pwrite envelope 在 CC8 已约80ms，CC16没有增加批次写吞吐，只把单Piece pwrite从13.17ms放大到31.23ms，并把最后CQE后的排空从14.51ms放大到34.50ms。CRC稳定在约3.5ms，不是瓶颈。

完整 aggregate 的最优点为CC8；CC16相对CC8下降约10.5%。同时 CC16 的 SegmentOffer、Done wait、Parent register/wait/revoke也变慢，说明高并发还增加 provider/owner 竞争。当前方向应保留较深 READ 并发，但把实际 pwrite 并发限制在Storage的有效区间，而不是统一降低Piece调度并发。

### 2. 实现

storage.server.urma.read 新增 maxConcurrentStorageWrites，取值1..=1024，默认1024以保持未配置时的旧行为。Content 创建一个进程级、仅供RM-READ lease消费者使用的共享 semaphore：

- CRC blocking worker在等待permit前立即启动，不受pwrite cap限制；
- 只有实际 pwrite worker持有permit，syscall完成即释放；
- READ posting、CQE owner loop、普通TCP/SEND-RECV写路径不使用该semaphore；
- 等待permit的时间记录为 pwrite_admission_ns，实际pwrite日志带 pwrite_limit；
- READ子配置现在参与父级嵌套校验，避免0 permit配置造成永久等待。

B7 支持 urmaRead.maxConcurrentStorageWrites，归因表新增 pwrCap 和 pwrWait，并提供固定CC16、其余参数不变的三组case：

- read-piece32-cc16-pwrite4-post1-pipe2
- read-piece32-cc16-pwrite8-post1-pipe2
- read-piece32-cc16-pwrite16-post1-pipe2

### 3. 离线验证

- cargo fmt --all：PASS；
- cargo check -p dragonfly-client-storage --features urma：PASS（只有既有dead-code warning）；
- URMA配置测试：6 passed；
- pwrite limiter配置接线测试：1 passed；
- B7全量单测：119 passed；
- JSON与两个仓库 git diff --check：PASS。

### 4. 真机判定门

保持CC16，依次执行cap4、cap8、cap16，并建议倒序再跑一轮。主要比较：

1. pwrWait 是否按预期限流且 peakPwr 不超过cap；
2. cap8能否保留CC16约64ms的READ envelope；
3. cap8能否缩短单次pwrite、pwrEnv、lastCQE->end 和完整aggregate；
4. cap4若排队明显增加且总包络变长，则下界过低；
5. cap16应复现未限流基线；若不能复现，先排查运行窗口漂移，不做性能结论。


### 5. 真机结果：限流正确，但没有形成吞吐优化

固定CC16的首轮结果：

| cap | aggregate | READ envelope | pwrite envelope | 最后CQE到最后pwrite | permit wait p50 | pwrite p50 | peak pwrite |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 4 | 5961.1MiB/s | 64.74ms | 81.83ms | 33.45ms | 21.49ms | 9.78ms | 4 |
| 8 | 5554.6MiB/s | 65.92ms | 80.58ms | 33.60ms | 10.24ms | 18.97ms | 8 |
| 16 | 6136.7MiB/s | 68.75ms | 85.48ms | 33.50ms | 0ms | 30.81ms | 15 |

peak pwrite精确受cap约束，说明实现生效。单Piece的 wait+pwrite 分别约31.27、29.21、30.81ms，Piece E2E分别39.55、39.28、41.27ms；降低并发只是把相近的总服务时间从pwrite内部排队移到semaphore admission。三个case的pwrite envelope约81--85ms，最后CQE后的排空都约33.5ms，Storage批次吞吐与关键尾部没有随cap下降。

aggregate的排序不能作为cap结论：三组 toREAD 分别54.60、79.74、57.81ms，外部启动波动已经大于数据面差异。用同一边界计算首个READ开始到最后pwrite结束，三组约为98.19、99.52、102.25ms；历史未限流基线约98.45ms，因此首轮没有证明cap4/8带来真实关键路径收益。

当前决策：

- 不把cap4或cap8固化为吞吐优化默认值，默认1024继续保持旧行为；
- limiter可保留为显式资源整形开关，但当前证据不支持它提高吞吐；
- pwrite在约4路并发已经达到约12GiB/s的批次平台，增加并发只提高单请求延迟；
- 下一优化重点回到READ投喂。CC增加时READ envelope仍持续改善，但当前约65ms对应约15.4GiB/s，仍明显低于双机perftest；
- 现有每Piece READ开始/完成事件足以离线计算read active并发、busy union、空闲间隙和start span。应先补B7 occupancy归因并直接重扫现有日志，再决定是否修改owner loop或控制面调度。


---

## 第四十五批：READ batch occupancy 离线归因（2026-09-23）

B7在现有每Piece READ完成事件基础上重建区间，无需新增Dragonfly日志，新增输出：

- startSp：首个到最后一个READ开始的跨度；
- idle：首个READ开始到最后CQE之间没有任何READ活跃的总时间；
- busy%：该包络中至少一个READ活跃的比例；
- avgRd：按全部READ活跃时长积分计算的平均活跃READ数；
- peakRd：区间扫描得到的峰值活跃READ数。

同时记录 readActiveAreaNs、readBusyUnionNs、readIdleNs、averageReadActiveMilli、readBusyPermille 和 peakReadActive。新版 attribution 在旧manifest没有这些字段时，会直接读取已归档的 measured child range logs 重算，因此 read-pwr4/8/16 和更早结果无需重新跑真机。

离线验证：B7全量单测121 passed，覆盖重叠READ、READ空闲间隙和旧manifest日志重算。提交为 B7 673c165（feat(b7): report read batch occupancy）。


### 真机 occupancy 结果

固定CC16三组cap的READ occupancy高度一致：

| cap | READ envelope | start span | idle | busy | avg active READ | peak active READ |
|---:|---:|---:|---:|---:|---:|---:|
| 4 | 64.74ms | 62.85ms | 6.90ms | 89.3% | 1.80 | 6 |
| 8 | 65.92ms | 62.51ms | 9.69ms | 84.6% | 1.81 | 6 |
| 16 | 68.75ms | 66.50ms | 13.77ms | 80.2% | 1.74 | 6 |

CC16并没有形成16个持续活跃的READ：平均只有约1.8，峰值6；READ start span几乎覆盖整个batch envelope。即使只看busy区间，条件平均活跃数也只有约2.0--2.15。另有约7--14ms完全没有READ活跃，占包络约11%--20%。因此应用READ约15GiB/s与双机perftest约46GiB/s的主要差距可以由投喂不足解释。

pwrite cap不改变上述模式，确认Storage限流不是READ occupancy低的原因。Parent每Piece direct register仍约1.5--2.5ms，其中pin约1ms；32个Piece的source register/offer经单owner逐片推进，与约63--66ms的start span数量级吻合。当前每Piece又只有一个32MiB WR，因此同时活跃Piece少就直接等价于JFS READ深度低。

下一候选保持Piece CC不变，属于URMA内部提交优化：

1. 新增一次owner命令批量post同一Piece的多个READ WR，消除当前每个WR一次command/oneshot往返；
2. 在批量post后比较maxReadSize 32MiB、8MiB、4MiB，使每个活跃Piece分别贡献1、4、8个outstanding WR；
3. 用完成字节、WR计数、CQE路由和owner生命周期的既有校验保持失败路径fail-closed；
4. 若批量多WR仍不能提高READ envelope，再评估task级source registration；后者涉及token轮换、旧descriptor撤销和跨Piece授权边界，风险明显更高，不作为首选。


---

## 第四十六批：owner loop 批量提交多 READ WR（2026-09-23）

第四十五批确认CC16时平均只有约1.8个活跃Piece READ、峰值6，且READ开始时间横跨约63--66ms。原实现的每个READ WR都需要一次async facade command、一次owner loop调度和一次oneshot返回；当把32MiB Piece拆成4MiB或8MiB WR时，这条控制路径会把本应补充JFS深度的多个WR再次串行化。

### 1. Dragonfly实现

Child的`drive_reads()`现在一次计算当前credit允许的全部WR长度，并通过一条`PostReadChildBatch`命令交给owner loop。owner仍对批次内每个WR调用既有`ReadOwners::post()`，因此以下语义保持不变：

- 每个WR仍独立分配READ context并登记CQE route；
- accepted bytes、accepted WR count和outstanding WR count仍逐WR推进；
- 后续WR失败时，先前已经提交的WR继续保留路由，owner由既有`post()`路径进入quarantine并由cleanup排空；
- 空批次在native边界直接作为contract错误拒绝；
- 没有改变READ wire协议、source descriptor、token、lease、CRC或Storage生命周期。

批次返回值只携带成功提交数量，不把context数组搬回async层。Child正常完成日志新增`read_post_batch_count`；结合既有`read_wr_count`可直接验证每条owner命令提交多少WR。

提交：Dragonfly `f38fbdf`（`perf(urma): batch read posts on owner loop`）。

### 2. B7观测与case

B7 timeline从`urma READ child completed data transfer`提取每Piece的`read_wr_count`和`read_post_batch_count`，在batch envelope表新增：

- `WRs`：一个measured batch的READ WR总数；
- `postB`：owner batch-post命令总数；
- `WR/post`：两者之比。

新增`read-piece32-cc16-read8-post1-pipe2`。它与现有32MiB和4MiB case共同组成固定1GiB、Piece32MiB、CC16、默认Storage write cap的单变量矩阵。预期每个measured batch分别为：

| maxReadSize | 每Piece WR | WRs | postB | WR/post |
|---:|---:|---:|---:|---:|
| 32MiB | 1 | 32 | 32 | 1.00 |
| 8MiB | 4 | 128 | 32 | 4.00 |
| 4MiB | 8 | 256 | 32 | 8.00 |

旧日志没有新字段时显示`nan`，READ/pwrite envelope和occupancy仍可正常重算。提交：B7 `51bd567`（`test(b7): measure read post batching`）。

### 3. 离线验证

- `cargo fmt --all`：PASS；
- `cargo test -p dragonfly-client-storage --features urma read_owners --offline`：11 passed；
- `cargo check -p dragonfly-client-storage --features urma --tests --offline`：PASS，只有既有`NativeChild::new` dead-code warning；
- B7全量单测：121 passed；
- 两个仓库`git diff --check`：PASS。

### 4. 真机验证矩阵

每组保持相同绑核和机器状态，并在下一组前cleanup：

```bash
python3 b7.py prepare --profile read --mode dual --run-id read-batch32-001 \
  --case read-piece32-cc16-post1-pipe2 --execute
python3 b7.py run --manifest results/read-batch32-001/manifest.json --execute
python3 b7.py cleanup --manifest results/read-batch32-001/manifest.json --execute

python3 b7.py prepare --profile read --mode dual --run-id read-batch8-001 \
  --case read-piece32-cc16-read8-post1-pipe2 --execute
python3 b7.py run --manifest results/read-batch8-001/manifest.json --execute
python3 b7.py cleanup --manifest results/read-batch8-001/manifest.json --execute

python3 b7.py prepare --profile read --mode dual --run-id read-batch4-001 \
  --case read-piece32-cc16-read4-post1-pipe2 --execute
python3 b7.py run --manifest results/read-batch4-001/manifest.json --execute
python3 b7.py cleanup --manifest results/read-batch4-001/manifest.json --execute

python3 read_attribution_summary.py \
  results/read-batch32-001 \
  results/read-batch8-001 \
  results/read-batch4-001
```

先检查三组均为128/128、fallback 0，并确认`WR/post`为1/4/8。随后比较`READenv`、`CQEspan`、`startSp`、`idle`、`busy%`、aggregate和`lastCQE->end`：

1. 若8MiB或4MiB显著缩短READ envelope且aggregate同步提高，说明单活跃Piece贡献更多outstanding WR能补足JFS深度；再从两者中选择收益与CQE开销更好的值。
2. 若WR/post正确但READ envelope基本不变，瓶颈仍在Parent register/offer的逐Piece启动跨度，多WR不能掩盖source publication节奏；下一步应优化task级source registration，而不继续缩小WR。
3. `avgRd`仍是活跃Piece区间数，不是provider outstanding WR数；可用`avgRd × WR/post`作粗略投喂深度估计，但不能当作精确JFS occupancy。
4. 这轮不要叠加`maxConcurrentStorageWrites=4/8/16`，避免把READ WR粒度与Storage限流混成两个变量。


### 5. 真机结果：批量提交成立，但多WR没有提高READ吞吐

三组均为128/128成功、fallback 0，且观测到的`WRs/postB/WR-per-post`严格符合预期：32MiB为32/32/1，8MiB为128/32/4，4MiB为256/32/8。这证明新实现确实用一条owner命令提交了一个Piece的全部WR，测试没有退回逐WR command/oneshot路径。

| maxReadSize | aggregate | READ envelope | CQE span | start span | idle | busy | avg active Piece | Piece READ p50 |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 32MiB | 5893.9MiB/s | 65.26ms | 62.43ms | 62.58ms | 8.29ms | 87.3% | 1.86 | 3.61ms |
| 8MiB | 5399.4MiB/s | 68.32ms | 64.97ms | 65.06ms | 10.82ms | 84.7% | 1.68 | 3.30ms |
| 4MiB | 6182.1MiB/s | 65.76ms | 63.93ms | 63.89ms | 10.35ms | 84.1% | 1.74 | 3.34ms |

READ envelope三组只相差约3ms，没有随每Piece WR数从1增至4、8而缩短；8MiB反而略慢，4MiB与32MiB基本相同。pwrite envelope约81--82ms、最后CQE到最后pwrite约32--34ms、Piece span约76--78ms，也都没有结构性变化。aggregate排序主要受`toREAD`的61--91ms启动波动影响，不能据此选择4MiB。

`startSp`约63--65ms，几乎等于整个READ envelope；最后一个Piece直到接近batch结束才开始READ。Parent measured source数据同时显示每Pieceregister p50约1.95--2.02ms，32个Piece串行注册的累计量约62--65ms，和start span直接闭合。pin本身约1.03--1.06ms，其余约1ms来自同一注册路径。由此可把瓶颈进一步定位为Parent source publication/register节奏，而不是Child post命令次数、单Piece WR深度或provider READ大小。

当前决策：

- 默认继续使用32MiB maxReadSize；4MiB没有证明收益，却将CQE数量放大8倍；
- 保留批量post实现，避免未来设备cap或更大Piece需要多WR时重新引入逐WR异步往返；
- 不再继续测试更小READ WR；
- 下一优化转向task级source registration/reuse：同一1GiB task应尽量只注册一次page-aligned source mapping，再为各Piece发布不同offset/length的descriptor；
- 实现前必须先明确cache key、文件变更校验、token/generation轮换、并发Piece引用计数、最后一个引用后的revoke/unregister，以及失败时旧descriptor不可继续授权。不能仅按路径缓存native handle。
