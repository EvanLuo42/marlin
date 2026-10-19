# 0017 — RHI 显存分配

状态：草案 · 2026-10-18

前置：
- [0016](0016-rhi-vulkan.md)：Vulkan backend（本文改它的 `memory.rs` / `resource.rs`）
- [0007](0007-render-graph.md)：render graph（瞬态资源、上传与回读）
- [0014](0014-testing.md)：测试体系

## 0. 决定摘要

| 问题 | 决定 | § |
|---|---|---|
| committed 资源的内存 | **在 RHI 里子分配**（VMA / D3D12MA 的做法）：每个 memory type × 资源组一个池，池里是大块（block），块内用 TLSF 分配；不再是一个资源一次 `vkAllocateMemory` | 2, 3 |
| 共享与 backend 的分界 | 算法、块的簿记、专用/池化策略、统计在 `src/memory/`（与 backend 无关）；backend 实现 `MemoryBackend`（分配/释放一个原始块、映射），并负责选 memory type、创建资源、绑定、专用分配 | 2 |
| 块内算法 | **TLSF**（两级分离适配）：O(1) 分配释放、簿记在块外、立即合并；`Tlsf` 公开，上层的 mega-buffer 等也用它 | 5 |
| 块大小 | device-local 长到 256 MiB，host 内存（上传/回读）长到 64 MiB；≤ 1 GiB 的堆用堆大小的 1/8；新块从 1/8 起翻倍 | 3 |
| 专用分配 | 驱动 requires/prefers（`VkMemoryDedicatedRequirements`）、导出/导入的内存、超过半个块的资源；index 与 indirect 参数 buffer 暂时也专用（§14） | 4 |
| `bufferImageGranularity` | 不在块内处理：granularity > 1 时 buffer 与 image 分池（D3D12 resource heap tier 1 同理分三组） | 3.2 |
| 每帧上传/回读 | RHI 里不做环形分配器（RHI 没有帧的概念）；回读 buffer 走池化（不再有驱动分配），上传沿用 render graph 的线性块 | 6 |
| 映射 | host-visible 块创建时整体持久映射；只用 coherent 类型，不需要 flush | 7 |
| 预算与统计 | `memory_budget`（`VK_EXT_memory_budget`）不变；新增 `Device::memory_stats`：块、子分配、专用分配，以及累计驱动分配次数 | 8 |
| 碎片整理 | **不做**；池里只有小而长寿的资源，流送与瞬态资源用 `Heap` 自己管理 | 9 |
| 线程安全 | 每个池一把锁；专用分配的统计是原子计数 | 10 |
| 驻留与共享 | 驻留优先级只作用于有自己内存的资源（专用、heap）；共享/导入的资源永远专用 | 11 |
| D3D12 | 同一套共享代码：原始块 = `ID3D12Heap`，池化 = placed resource，专用 = committed resource；Xbox 时再接 | 12 |

## 1. 问题

0016 的 Vulkan backend 给每个 committed 资源一次 `vkAllocateMemory`（并且总是 dedicated）。代价：

- **CPU**：在 RTX 5060 上一次分配约 70 µs，释放也不便宜。render graph 每帧给回读建 3 个小 buffer（`frame.rs` 的 “Readback destinations”），光这一项每帧约 0.21 ms CPU，对 165 fps（6 ms）以至上千 fps 的目标太贵。
- **数量上限**：`maxMemoryAllocationCount` 在 Windows 上通常是 4096；每个资源一次分配，资源一多就撞上限。
- **浪费**：每次分配至少按页（常为 64 KiB）对齐，256 字节的 buffer 也占一页。

AAA 引擎的标准答案是 VMA / D3D12MA 式的子分配：少量大块，资源放在块里。

## 2. 分层

```
runtime/platform/rhi/src/
  memory/
    mod.rs     Allocator<R>、MemoryBackend、ResourceClass、MemoryType、专用策略、统计
    tlsf.rs    Tlsf（公开）
  backend/vulkan/memory.rs   memory type 选择、MemoryBackend for Shared（VkDeviceMemory）、
                             Bound（专用 or 池化）、Memory（专用分配与 heap）
  backend/vulkan/resource.rs 创建 buffer/texture → 取需求 → Bound::allocate → 绑定
```

**共享部分**（`src/memory/`，no_std，与 backend 无关）：
- `Allocator<R>`：按 memory type × 池组的池；块的创建、增长、回收；`allocate` / `free`；`stats`。
- `Tlsf`：块内的偏移分配。
- `wants_dedicated`：专用还是池化（§4）。
- 统计：块、子分配、专用分配（含 heap）、累计驱动分配次数。

**backend 实现的 trait**：

```rust
pub(crate) trait MemoryBackend {
    type Raw: Copy; // VkDeviceMemory / ID3D12Heap
    fn allocate_block(&self, memory_type: u32, size: u64) -> Result<(Self::Raw, Option<NonNull<u8>>)>;
    unsafe fn free_block(&self, raw: Self::Raw);
}
```

backend 在创建设备时给出每个 memory type 的描述（是否 device-local、块大小）和资源类到池组的映射（§3.2），并把 `Allocator` 放在设备的共享状态里；资源持有自己的 `Allocation`，销毁资源后归还。专用分配需要资源句柄（`VkMemoryDedicatedAllocateInfo`、`CreateCommittedResource`），所以由 backend 自己做，再用 `dedicated_allocated` / `dedicated_freed` 记进统计。

加一个 backend 只需：实现 `MemoryBackend`、描述 memory type、在创建资源处调用 `Allocator::allocate`。

## 3. 池与块

### 3.1 块大小与增长

- 每个池是一组块，每块一个 `Tlsf`。分配时依次试池里的块，都放不下才建新块。
- 块大小（backend 给出，Vulkan 的取值）：device-local 类型 256 MiB；host 内存类型 64 MiB（上传、回读，多是小 buffer；大的走专用）；所在堆 ≤ 1 GiB 时（256 MiB 的 BAR 窗口、集显的小堆）取堆大小的 1/8。
- **增长**（同 VMA）：新块取 1/2、1/4、1/8 块大小中最小的那个——只要它比池里现有的块都大、且至少装得下两倍的请求。所以第一块是 1/8（device-local 32 MiB，host 8 MiB），之后翻倍。驱动报内存不足时块大小减半重试，直到放不下请求为止。
- **回收**：块空了就释放，但每个池保留一个空块，避免“每帧建一个资源再丢掉”反复分配和释放整块。设备销毁时释放所有块；仍有子分配的块记一条警告（资源泄漏）。

### 3.2 资源组与 `bufferImageGranularity`

Vulkan 要求相邻的线性资源（buffer）和最优平铺资源（optimal image）在 `bufferImageGranularity` 粒度上不共用一页。VMA 在块内追踪相邻资源的类型；我们选更简单也更常见的做法：**不同类分池**。

- `ResourceClass` 有 `Buffer`、`Texture`、`Target`（render target 和 depth-stencil）三类；backend 把每类映射到一个池组。
- Vulkan：granularity 为 1 时三类同组；否则 buffer 一组、image 一组（所有 image 都是 optimal tiling）。
- D3D12：resource heap tier 1 要三组（buffer、RT/DS 纹理、其余纹理各自的 heap）；tier 2 一组。

代价是多几个块，换来块内不需要关心邻居。

### 3.3 对齐

`Tlsf::allocate(size, align)` 直接按资源要求的对齐（2 的幂）分配：先看请求自己那一档的前几个空闲区间，常常本来就对齐或正好合适；否则按 `size + align - 1` 找一个从哪开始都放得下的区间。对齐产生的前置空隙作为空闲区间留着，可以再分配。

buffer 的起始地址至少按 256 字节对齐（`UNIFORM_ALIGNMENT`、`STORAGE_ALIGNMENT`、`ACCELERATION_STRUCTURE_ALIGNMENT` 中最大的）：调用者在 buffer 内的偏移按这些常量对齐即可，与专用分配时一样（实测：不这样做时，加速结构的 scratch 地址违反 `minAccelerationStructureScratchOffsetAlignment`）。

## 4. 专用分配

以下情况资源拿自己的驱动分配（Vulkan 用 `VkMemoryDedicatedAllocateInfo`，D3D12 用 committed resource）：

1. 驱动 **requires 或 prefers**（`VkMemoryDedicatedRequirements`）：一些厂商对 render target 这样报告，专用分配能启用压缩或更好的放置。VMA 默认也照办。
2. **导出或导入**的内存（`BufferUsage::SHARED`、`TextureUsage::SHARED`、`import_*`）：OS 句柄对应整个分配，不能和别人共享。
3. **大于半个块**：放进块里会浪费或逼出新块。
4. **index 与 indirect 参数 buffer**：暂时如此，见 §14。

其余都池化。`Heap`（placed / sparse 用）本来就是一整块驱动内存，统计里也算作专用分配。

## 5. TLSF

两级分离适配（Masmano et al. 2004），偏移版本（簿记在块外，因为块是 GPU 内存）：

- 空闲区间按大小分档：一级是 2 的幂，二级把每个 2 的幂再分 32 档；小于 32 字节的每字节一档。
- 两个位图（一级 `u64`、每个一级下二级 `u32`）找到第一个非空且够大的档：O(1)。
- 分配时从区间里切出请求，前后剩余（对齐空隙、尾部）立即成为空闲区间。
- 释放时与物理相邻的空闲区间立即合并：O(1)，没有延迟合并或整理。
- 节点存在 `Vec` 里，用过的槽位回收；空闲链表和物理邻接都是下标。
- 最坏的内部浪费是查找时向上取整到档的下界（≤ 1/32），但分配只占请求的大小。

为什么不用 buddy：buddy 把每个请求取整到 2 的幂，平均浪费约 25%，且对齐和大小绑在一起。TLSF 是 VMA（默认算法）和 D3D12MA 的选择。线性/栈式分配器适合每帧整体重置的场景，见 §6。

`Tlsf` 公开（`marlin_rhi::Tlsf`），上层管理大 buffer 里的区间（几何 mega-buffer、实例数据池）也用它。

## 6. 每帧的上传与回读

RHI 没有帧的概念，环形/线性分配器放在知道帧的地方：

- **上传**：render graph 的 `FrameContext` 每个帧槽有一串上传块（`upload_chunk`，默认 4 MiB），帧内线性分配、帧槽复用时整体重置。它们是持久 buffer，本文不改。
- **回读**：每帧新建的回读 buffer（`COPY_DST`、`Readback`）现在池化：创建只剩 `vkCreateBuffer`、取需求、TLSF 分配、绑定，不再有驱动分配；释放只是归还区间（池里保留空块，见 §3.1）。测试（§13）检查连续 200 帧各建 3 个回读 buffer 不增加驱动分配。若 `vkCreateBuffer` 本身的开销以后在剖析里显眼，再在 `FrameContext` 里按帧槽回收回读 buffer。

## 7. 映射

- host-visible 的块在创建时整体 `vkMapMemory` 一次，块存在期间一直映射；子分配的地址是块地址加偏移。专用分配同样。
- `MemoryLocation` 选 memory type 时 `Upload`、`Readback`、`GpuUpload` 都要求 `HOST_COHERENT`，所以不需要 `vkFlushMappedMemoryRanges` / `vkInvalidateMappedMemoryRanges`，也不用按 `nonCoherentAtomSize` 对齐。以后若引入非 coherent 类型，flush 的范围要对齐到 `nonCoherentAtomSize`，这是 backend 的事。

## 8. 预算与统计

- `Device::memory_budget`（`VK_EXT_memory_budget`；D3D12 用 `QueryVideoMemoryInfo`）不变：OS 给的预算与进程的用量。
- 新增 `Device::memory_stats() -> MemoryStats`：按 local / non-local（与 `MemoryBudget` 同样的划分）给出块数与字节、子分配数与字节、专用分配数与字节；外加 `driver_allocations`：设备创建以来的驱动分配次数。稳定运行的帧不应让它增长；引擎的性能统计（`r.stats`）可以显示它的每帧增量。
- 分配器**不因超预算拒绝分配**：超预算时 OS 换页，帧会卡，但不会失败；保持在预算内是流送系统的事（看 `MemoryUsage::headroom` 卸载流送内存）。

## 9. 碎片整理

不做。理由：
- 池里只有 committed 资源：小 buffer、常驻纹理、render target，寿命长、数量不大。TLSF 的立即合并让碎片保持在低水平。
- 寿命短或成批变化的内存——render graph 的瞬态资源、流送纹理、sparse 页——都在 `Heap` 里由上层自己摆放和别名。
- 搬动资源需要重建视图、更新 bindless 槽位、在 GPU 上复制并与在途帧同步，这只有渲染器能做。若以后需要，在 RHI 里加“列出某块里的资源”的查询，由上层驱动搬移（VMA 的 defragmentation 也是这种分工）。

## 10. 线程安全

- 每个池一把 `marlin_sync::Mutex`；分配和释放只锁所在的池。建新块和释放空块时持有锁（同一池并发的分配会等这次驱动调用，而不是各建一块）。
- 专用分配不经池锁；统计用原子计数。
- `memory_stats` 依次锁每个池读计数；结果是近似的快照。

## 11. 与驻留、共享的关系

- Vulkan 的驻留优先级（`VK_EXT_pageable_device_local_memory`）作用于整个 `VkDeviceMemory`。池化资源和别人共用一块，所以 `make_resident` / `evict` / `set_residency_priority` 对它们不起作用（和 placed resource 一样；D3D12 上同样只能对 committed resource 和 heap 做）。需要单独驻留控制的大资源本来就是专用分配（§4 第 3 条）或在自己的 heap 里。
- 共享（导出）与导入的资源总是专用分配，`export` 只认有自己内存的资源。

## 12. D3D12（Xbox）

共享代码不变，D3D12 backend 将来：
- `MemoryBackend::Raw = ID3D12Heap`：`allocate_block` 用 `CreateHeap`（`D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS` 等按池组设定），映射用一个覆盖整个 heap 的 placed buffer 的 `Map`。
- memory type = heap 类型（DEFAULT、UPLOAD、READBACK、GPU_UPLOAD）；local 与否同 `MemoryBudget` 的分组。
- 池化资源 = `CreatePlacedResource`（对齐：buffer 64 KiB，小纹理 4 KiB，MSAA 4 MiB——由 `GetResourceAllocationInfo` 给出）；专用 = `CreateCommittedResource`。
- 池组：resource heap tier 1 三组，tier 2 一组（§3.2）。

在接好之前 D3D12 的 `memory_stats` 返回全 0。

## 13. 测试

- 平台无关（`cargo test -p marlin-rhi --test unit`，`tests/unit/tlsf.rs`）：分配不重叠、释放后合并回整块、对齐、对齐空隙可复用、耗尽与恢复、重复释放会 panic；以及 2 万步的随机分配/释放，对照已发出的区间检查不重叠、对齐、用量。
- GPU，共享设备（`tests/gpu/allocator.rs`）：64 个奇数大小的相邻子分配 buffer 经 GPU 拷贝往返，内容互不干扰。
- GPU，独立二进制（`tests/isolated_gpu_memory.rs`，自己的设备、测试串行，计数不受别的测试影响）：512 个小 buffer 最多 4 次驱动分配；每帧 3 个回读 buffer × 200 帧不增加驱动分配；反复建删小纹理不增加驱动分配。
- 其余 RHI、render graph、render、framework 的 GPU 测试照常跑，覆盖池化后的所有资源路径。

## 14. 待定

- **index 与 indirect 参数 buffer 暂时专用分配**（§4 第 4 条）。现象：池化后，放在别的 buffer 用过又释放的内存里时，`indirect::draws_indexed_indirect`（DGC 的 indexed indirect draw）在 NVIDIA（RTX 5060）上让 GPU 挂起（device lost，TDR）；只在之前的测试先用过同一段内存时出现，内存不复用（全新、驱动清零的内存）时不出现；把这两类 buffer 改为专用后 RHI 全部 GPU 测试反复通过。根因未查明（候选：某处依赖内存初始为零，或 DGC 的 index buffer 处理）。查明之前保持专用；查的时候每次复现都会让整台机器卡顿数秒，需与用户约好时间。
- D3D12 接入（§12），有 Xbox 时做。
- 在 `FrameContext` 里按帧槽回收回读 buffer（§6），视剖析结果。
- 内存优先级随块设置（`VkMemoryPriorityAllocateInfoEXT`）：比如 render target 的池给高优先级。
- 若 granularity 分池的块浪费可观，改成块内追踪相邻资源类型（VMA 的做法）。
