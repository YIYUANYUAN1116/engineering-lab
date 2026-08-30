没必要把 URMA slot pool 直接改造成 RDMA best-fit buffer pool。应该对齐的是生产能力和资源语义，不是内部数据结构。
当前更合理的方向是保留 URMA 预注册 Segment，同时补齐 RDMA 已具备的预算、公平性和可观测能力。

截至 2026-08-30，B6 已完成最小固定分区方案：`maxRegisteredBytes` 控制 process 预注册总量，
`txRegisteredBytes` 提供 TX 保底，RX 使用余量；默认 40 MiB/8 MiB，仍对应 TX 128/RX 512 个
64 KiB slots。第二窗口使用 non-blocking admission，失败安全退化为单窗口，并记录 registered bytes 与
required/optional budget pressure。TX/RX shared overflow、动态 arena 和严格跨 peer slot fairness 尚未实现。
必须对齐的能力包括：
- 注册内存全局有界。
- 资源不足时能等待、拒绝或安全退化，不能死锁。
- 已持有一个 window 时，申请第二个必须支持 non-blocking。
- 多 peer 之间有 admission/fairness。
- active lease 阻止内存注销或复用。
- 能观测 active/idle bytes、等待时间、分配失败和 pipeline 降级。
- TX/RX 配额可以配置，不能永久依赖硬编码数量。
这些可以继续基于 fixed Segment/slot 实现，不要求引入 RDMA 的 PooledBuf。
什么时候需要进一步对齐
1. TX/RX 固定分区造成明显资源浪费
例如：
TX slots：90% 空闲
RX slots：耗尽，频繁 BufferUnavailable
或者反过来。
如果真机数据出现这种单边闲置，说明固定分区（默认 128 TX + 512 RX）不适合实际流量，需要：
- TX/RX 可借用共享区；
- reserved minimum + shared overflow；
- 或按当前连接方向动态划分 slot。
这种情况下需要对齐 RDMA 的“共享注册内存预算”能力，但仍可保留 slot allocator。
2. slot 内部碎片显著
例如大量传输的 negotiated chunk 是 8 KiB，但 slot 固定为 64 KiB：
有效数据：8 KiB
占用注册内存：64 KiB
利用率：12.5%
结果可能是物理注册了 40 MiB，但实际只能支撑约 5 MiB 的有效 inflight 数据。
如果 workload 的 chunk size 分布变化很大，就需要：
- 多种 slot size class，例如 8/16/64/256 KiB；
- 或从 Segment 中按页/extent 分配不同尺寸；
- 或允许一个大 slot 承载多个独立小 WR。
这才是向 RDMA best-fit pool 靠近的典型场景。
3. multi-span 数量成为 CPU/syscall 瓶颈
当前 4 MiB window、64 KiB slot 会形成约 64 个 span，可能带来：
- 64 个 WR/CQE；
- CRC32 遍历 64 个 slice；
- 当前 B3 最多约 64 次 positional write；
- 更多 slot state transition 和 generation 校验；
- 更高 owner recycle command 处理成本。
如果真机数据显示：
- NIC 没跑满；
- CPU 较高；
- pwrite/CQ/owner queue 占比高；
- 增大 window 但吞吐不升；
那么需要减少逻辑 window 的 slot/span 数。选择包括：
1. 先做 pwritev、post-list、CQ batch；
2. 增大 slot size；
3. 增加多 size-class；
4. 最后才考虑 RDMA 式按 window 分配连续 buffer。
通常先做批处理更便宜，不必立即重写 allocator。
4. 双窗口经常退化成单窗口
需要观测：
pipeline_depth_2_success
pipeline_depth_1_fallback
BufferUnavailable
RX slot wait time
Storage consumer hold time
如果双窗口大量退化，但 registered bytes 仍有明显空闲，说明不是总内存不足，而是：
- TX/RX 分区不合理；
- slot size 不合理；
- contiguous TX run 分配产生碎片；
- 公平策略让少数 transfer 占满 slot。
这种情况需要改 allocator/预算模型。
如果所有 registered bytes 都确实被 active leases 占用，则问题可能只是预算太小或 Storage 太慢，不一定需要改成 RDMA pool。
5. Piece/window 尺寸高度不稳定
RDMA best-fit pool比较适合：
- 多种 Piece size；
- 不同 peer 协商出不同 message size；
- window 从几十 KiB 到数 MiB 动态变化；
- 多种 provider/device capability 并存。
如果生产 workload 基本固定为：
chunk = 64 KiB
window = 固定 chunk 数
Piece = 4–64 MiB
fixed slot 往往更简单、更稳定。
如果协商结果分布很散，单一 slot size 的效率会快速下降，此时多 size-class 或 extent allocator 更有价值。
6. 启动时预注册内存太大或太刚性
URMA 当前是启动时注册完整 Segment。可能出现：
- 低流量节点也永久 pin 40 MiB；
- 多 device/runtime 实例使 pin memory 成倍增长；
- provider 对大 Segment 注册或注销很慢；
- 容器 locked-memory limit 较低；
- 扩容时无法在线增加 slot。
这时可以采用 RDMA 风格的“按需增长、空闲回收”，但不必做到每个 window 单独注册。更适合 URMA 的方式是：
初始小 Segment
-> 预算压力下增加一个 Segment arena
-> 每个 arena 内继续使用 slot
-> 长时间空闲后整 arena 注销
这是动态 arena pool，比直接照搬 PooledBuf 更符合 URMA owner-thread 模型。
什么时候不需要对齐
如果真机结果满足以下条件，就不应该为了结构一致而改：
- registration 只发生一次且启动时间可接受；
- pinned memory 占用可接受；
- RX 双窗口基本不降级；
- TX/RX slot 没有明显单边闲置；
- slot 有效载荷利用率高；
- owner queue/recycle 不是瓶颈；
- WR/CQE 和 write syscall CPU 占比可接受；
- 吞吐瓶颈在 Storage、source-fill、page fault 或链路本身。
这时 fixed slot pool通常比动态 pool更好：更少分配、没有 MR cache miss、状态确定、故障和 shutdown 更容易审计。
推荐路线
当前已按 fixed Segment/slot 完成 B4 direct-fill/ring、B5 post/CQ batching 和 B6 byte budget/固定方向
分区/non-blocking 退化/指标。后续顺序是：
1. 在真实 provider 上统一验证 linked post、partial post/error、pipeline depth 和 budget pressure。
2. 用多 peer 压力数据确认是否存在碎片、固定分区失衡、饥饿或频繁 pipeline 降级。
3. 只有证据成立时再选择：
   - 调整 slot size；
   - 多 size-class；
   - shared TX/RX slots；
   - 动态 multi-arena；
   - 最后才是完整 best-fit buffer pool。
一句话总结：URMA 不需要“结构对齐 RDMA”，但必须“能力对齐 RDMA”。只有固定 slot 已经造成可量化的内存浪费、并发退化或 CPU/syscall 瓶颈时，才值得向动态 pool 演进；优先考虑 shared/multi-size arena，而不是直接照搬 RDMA PooledBuf。
