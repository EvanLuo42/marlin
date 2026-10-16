# 0005 — RHI 补齐与 Render Graph

状态：草案 · 2026-10-16

前置：[0004](0004-rendering-overview.md)。本文分两部分：
- §1：为了支撑 0006–0009，`marlin-rhi` 要补哪些能力。
- §2：`marlin-render-graph` 的设计。

RHI 的定位不变，仍然是"薄、显式、没有状态跟踪"（`rhi/src/lib.rs` 的模块文档）。状态跟踪、barrier 推导和延迟销毁都放在 render graph 里。

## 1. RHI 补齐

### 1.1 基线提升

| 项 | 现在 | 改为 |
|---|---|---|
| D3D12 | FL 12_0、SM 6.6、binding tier 3 | 另外要求 mesh shader tier 1、SM 6.6 的 wave 指令；启动时检查，不满足就拒绝该 adapter |
| Vulkan | — | 1.3 + `VK_EXT_mesh_shader` + `VK_EXT_mutable_descriptor_type`（或 descriptor buffer）+ timeline semaphore + synchronization2 + dynamic rendering |
| Metal | — | Metal 3（Apple Silicon）：mesh shader，以及 argument buffer tier 2 |

`Features` 增加以下字段：
- `mesh_shader`，必需。
- `ray_query`，ext。
- `atomic_int64`，ext，供软件光栅使用。
- `wave_size_range`
- `opacity_micromap`，ext。
- `work_graphs`，ext，预留。
- `hdr_output`

### 1.2 队列与同步

```rust
impl Device {
    pub fn queue(&self, ty: QueueType) -> Option<&Queue>;   // Graphics 总有；Compute/Copy 可能为 None（退化到 Graphics）
}
impl Queue {
    pub unsafe fn submit(&self, desc: &SubmitDesc);
}
pub struct SubmitDesc<'a> {
    pub waits:   &'a [(&'a Fence, u64)],   // GPU 端等待（D3D12 Queue::Wait / VkSemaphoreSubmitInfo）
    pub lists:   &'a [&'a CommandList],
    pub signals: &'a [(&'a Fence, u64)],
}
```

- `CommandPool` 和 `CommandList` 带队列类型。Compute 和 Copy 队列上的 list 只能录对应的命令，api 层会做校验。
- 跨队列的资源访问方面：D3D12 enhanced barriers 没有所有权的概念，只要求布局属于目标队列所支持的集合，例如 `COMPUTE_QUEUE_*`、`COMMON`。Vulkan 默认用 `VK_SHARING_MODE_CONCURRENT`，避免显式的所有权转移；如果实测性能有问题，再改成 exclusive 加 release/acquire。
- 保留现有的 `FramesInFlight`，供 demo 使用。正式渲染器的帧同步由 render graph 的 `FrameContext` 负责（§2.6）。

### 1.3 新命令

| 命令 | 说明 |
|---|---|
| `draw_indirect`、`draw_indexed_indirect`、`dispatch_indirect` | 参数来自 buffer |
| `*_indirect_count` | 带 count buffer 的 multi-draw / multi-dispatch（D3D12 ExecuteIndirect，Vulkan `*IndirectCount`） |
| `dispatch_mesh`、`dispatch_mesh_indirect(_count)` | mesh shader 管线 |
| `write_timestamp(&QueryPool, index)`、`resolve_queries` | 统计每个 pass 的 GPU 耗时 |
| `begin_event(name, color)`、`end_event`、`set_marker` | PIX、RenderDoc、Vulkan debug utils |
| `write_breadcrumb(value)` | GPU 崩溃定位（DRED / Aftermath / 主机等价物，ext） |
| `copy_texture`、`copy_texture_region`、`fill_buffer`、`clear_texture_uav` | — |
| `build_acceleration_structures(&[BuildDesc])`、`copy_acceleration_structure`（compact/clone） | ext `ray_query` |

### 1.4 管线

- `MeshPipelineDesc { amplification: Option, mesh, pixel: Option, raster, depth, color_targets }`。
- `ComputePipelineDesc` 可以选择指定 `wave_size`（SM 6.6 `[WaveSize]`）。
- ray query 只在 compute（以及 pixel）shader 里使用，**不提供** RT pipeline 和 SBT。理由见 0006 §6.2：命中点着色走 wavefront，所以只需要加速结构加 inline 查询。
- 创建 pipeline 必须线程安全，可以在 job 里并行完成。加载时一次性批量创建，并接入 D3D12 pipeline library / `VkPipelineCache`。主机上的 pipeline 直接就是 cook 产物。
- shader 绑定模型维持 0002 的约定：全局一个布局，32 字的 push constants，bindless。

### 1.5 资源与内存

- **Heap 与 placed 资源**：
  - `Device::create_heap(&HeapDesc { size, location, kind })`；
  - `create_placed_buffer/texture(&heap, offset, &desc)`；
  - `memory_requirements(&desc) -> (size, align)`。
  - 用途：render graph 的瞬态别名，以及流送纹理池的子分配。
- **别名**：同一块内存的新资源首次使用时，以 `Layout::Undefined` 作为 before（discard）。D3D12 enhanced barriers 和 Vulkan 都直接支持；legacy 路径下发 aliasing barrier。
- **纹理**：
  - 新增 `D3`（3D 纹理）。froxel、云噪声、LUT、探针体积都要用。
  - 新增 per-mip 和 per-slice 的 UAV 视图。
- **格式**：新增
  - R8Uint、R16Uint、R16Unorm、RG16Uint、RG32Uint（visbuffer）、RGBA16Unorm、RGBA16Uint、RGBA32Uint
  - R11G11B10F 作为 UAV
  - RGB10A2Unorm（HDR10 swapchain）
  - D32FloatS8
  - Bc6hSfloat（cook_texture 已经能编码这个格式）
- **swapchain**：增加色彩空间 `{ SrgbNonlinear, Hdr10St2084, ScRgbLinear }`，以及 `display_hdr_info()`（峰值亮度、最小亮度、色域）和 `set_hdr_metadata`。
- **视图槽位原地更新**：`unsafe fn rebind_view(&ShaderView, &Texture, &ViewDesc)` 把同一个 bindless 下标改指到新的纹理上。流送升级 mip 时下标保持不变，材质参数块不需要改（0006 §8.1）。调用方必须保证旧资源不再被 GPU 使用，这一点由 render graph 的退休队列保证。
- **瞬态视图区段**：bindless 资源表分成两段：
  - 持久段：沿用现在的 free list。
  - 瞬态段：每帧一段 ring，供 render graph 给瞬态资源建视图，不需要加锁，帧退休时整段回收。
  
  对应的 API 是 `ViewRange` 和 `create_view_in(range, index, …)`。

### 1.6 Ray query ext（`marlin_ext` 运行期能力 `RayQuery`）

- `AccelerationStructure`：分 BLAS 和 TLAS，两者都放在 buffer 里，与 placed 资源配合使用。
- `BlasBuildDesc`：
  - 几何是 `triangles { positions: buffer + stride + format, indices, opaque }`；
  - `flags` 包括 `PREFER_FAST_TRACE`、`ALLOW_UPDATE`、`ALLOW_COMPACTION`；
  - `mode` 为 build 或 refit。
- TLAS 的实例 buffer 由 GPU 写入（GPU Scene 生成），按平台布局写，用 `instance_desc_layout()` 查询这个布局。
- 不透明度：
  - 有 opacity micromap ext 时，附加 OMM；
  - 没有时，在 ray query 的候选循环里用通用 alpha 测试（0009 §5.3）。
- shader 侧：TLAS 作为一个 bindless 视图（`RaytracingAccelerationStructure`），下标放在 per-frame 常量里。

### 1.7 Vulkan 与 Metal 的影响

做 D3D12 扩展时，同时对照 Vulkan 和 Metal 来设计抽象：

| 抽象 | Vulkan | Metal | 注意 |
|---|---|---|---|
| bindless 资源表 | mutable descriptor 数组（set 0 b0） | argument buffer | Metal 的资源驻留需要 `useResource`/`useHeap`；用 heap 统一 `useHeap` |
| timeline fence | timeline semaphore | `MTLSharedEvent` | — |
| GPU 端 wait | semaphore wait | encodeWait | — |
| enhanced barrier 三元组 | synchronization2 | 大多数情况自动跟踪；untracked 资源用 fence/barrier | Metal backend 对 untracked heap 资源手动插 `memoryBarrier` |
| indirect count | `vkCmdDrawIndirectCount` | ICB 或 compute 写 indirect 参数 | Metal 没有 count 版本，需要 GPU 端模拟 |
| ray query | `VK_KHR_ray_query` | intersection query | — |

### 1.8 R0 实现情况（2026-10-16）

已实现（D3D12；stub backend 在 Linux/macOS 上编译通过）：

- **基线**：在原来的基础上，再要求 mesh shader tier 1 和 wave 指令，不满足的 adapter 直接拒绝。
- **Features**：新增 `mesh_shader`、`ray_query`（只检测 DXR 1.1，功能本身未实现）、`atomic_int64`、`copy_queue_timestamps`、`mixed_heaps`（resource heap tier 2）、`wave_size_range`。
- **队列**：D3D12 总是创建专用的 Compute 和 Copy 队列。`Device::dedicated_queue(ty)` 在没有专用队列时返回 `None`，`queue_for(ty)` 这时退化到 Graphics 队列。
  - `CommandPool` 和 `CommandList` 带队列类型；不属于该类型的命令会 panic，提交到别的类型的队列会返回 `InvalidInput`。
  - `Queue::submit_with(&SubmitDesc { waits, lists, signals })` 在 GPU 端等待。
- **Indirect**：每条记录的第一个字是 `constant`，写进 push constant 的第 `INDIRECT_CONSTANT_WORD`（= 31）个字。D3D12 用 command signature 实现（CONSTANT + DRAW/DRAW_INDEXED/DISPATCH/DISPATCH_MESH）。
  - stride 由命令类型决定：`DrawIndirectArgs` 20 字节，`DrawIndexedIndirectArgs` 24 字节，`DispatchIndirectArgs` 16 字节。
  - count buffer 是 `IndirectArgs::count`，没有单独的 `*_indirect_count` 方法。
  - ExecuteIndirect 之后，第 31 个字的值是未定义的。
- **Mesh shader**：`MeshPipelineDesc` 通过 `ID3D12Device2::CreatePipelineState` 的 state stream 创建；命令是 `dispatch_mesh` 和 `dispatch_mesh_indirect`。
- **查询**：`QueryKind::Timestamp` 用在 graphics/compute list 上，`QueryKind::CopyTimestamp` 用在 copy list 上（D3D12 的 copy 队列要求单独类型的 query heap）。`resolve_queries` 把结果写成 u64；`Queue::timestamp_frequency` 返回频率。
- **标记**：`begin_event(name)`、`end_event`、`set_marker(name)` 使用 PIX 的无格式字符串编码（metadata 0，UTF-16）。暂不支持颜色参数，需要 PIX 的二进制事件编码。
- **Heap 与 placed 资源**：
  - `create_heap(&HeapDesc { size, memory, kind })`，kind 为 `Any`、`Buffers`、`Textures`、`Targets`；heap tier 1 不允许 `Any`。
  - `buffer_requirements` / `texture_requirements` 返回 `MemoryRequirements`。小的采样纹理会尝试 4 KiB 对齐（先按 D3D12MA 的 tile 数估算，避免对齐请求被拒时报 validation 错误）。
  - `create_placed_buffer/texture`：placed 资源持有 heap 的引用；heap 创建时带 `CREATE_NOT_ZEROED`。
- **别名**：before 为 `Undefined` 时激活别名。
  - Enhanced barrier：placed 的 RT/DS 纹理加 `DISCARD` 标志。
  - Legacy：先发 aliasing barrier，再做 transition，RT/DS 随后 `DiscardResource`。legacy 状态要求 before 必须写出真实的当前状态，所以 backend 记录每个纹理最后一次 barrier 的状态，用来替换 `Undefined`。这个状态只按录制顺序跟踪，乱序录制时可能过时。
  - placed buffer 从 `BufferState::NONE` 开始使用时发 aliasing barrier。
- **纹理**：`TextureDimension::D3`、`TextureDesc::new_3d`、`TextureViewDimension::D3`（SRV，以及覆盖整个 mip 的 UAV）。buffer↔texture 拷贝支持 `z`/`depth`，各 slice 在 buffer 中依次排列。`copy_texture` 拷贝任意 box，要求格式相同（或互为 sRGB/linear）；depth-stencil 会同时拷两个 plane。
- **格式**：R8Uint、R16Uint、R16Unorm、Rg16Uint、Rg32Uint、Rgba16Unorm、Rgba16Uint、Rgba32Uint、Bc6hSfloat、D32FloatS8。
  - D32FloatS8 配套完整的 stencil：`DepthState::stencil`（`StencilState`/`StencilFace`/`StencilOp`）、`DepthAttachment::stencil_load/stencil_store`、`CommandList::set_stencil_reference`。
  - 采样视图读的是 depth plane；buffer 拷贝也只拷 depth plane（`Format::copy_block_size`）。
- **CPU 侧**：
  - `marlin_mem::{Arena, FrameArenas}`。
  - `marlin_ecs` 的 change detection：`Mut`/`Ref`、`Changed`/`Added`、`World::removed`、`clear_trackers`。
  - `marlin_jobs::{JobHandle, Batch}`：`spawn`/`spawn_after`/`batch`，dependency 等待期间不占用 fiber。

推迟到后续阶段：
- `fill_buffer`、`clear_texture_uav`、`rebind_view`/`ViewRange`。
- per-slice 的 3D UAV。
- ray query 的实现（加速结构）、`opacity_micromap`/`work_graphs`/`hdr_output` 这几个 feature、HDR swapchain。
- `write_breadcrumb`、pipeline library/cache。
- stencil plane 的采样和拷贝。
- BC 与无压缩格式之间的重解释拷贝（GPU BC 编码要用）。

另外，**GPU 内存的子分配策略**（D3D12MA/VMA 式的块池和 TLSF）不放在 RHI 里，而是在 heap/placed 原语之上单独做一层。

## 2. Render Graph

### 2.1 模型

- 每帧声明式地重建（Frostbite FrameGraph / UE RDG 的方式）。图对象、pass 闭包和资源描述都从帧 arena 分配，帧结束时整体丢弃。
- **顺序语义**：pass 的声明顺序就是逻辑执行顺序，和单线程程序一样直观。编译器只在不改变可观察结果的前提下做三件事：剔除、别名、跨队列调度。
- 资源分两类：
  - **瞬态资源**：由图创建，生命周期只在本帧内，参与别名。
  - **外部资源**：import 进来，可能来自持久池、GPU Scene 或 swapchain。可以 export 到下一帧，例如 history。

### 2.2 API

```rust
let mut g = RenderGraph::new(&mut frame);            // FrameContext：arena、上传 ring、档位、计时

let depth = g.create_texture("SceneDepth", TextureDesc::d2(w, h, Format::D32Float)
    .usage(DEPTH_STENCIL | SAMPLED));
let vis = g.create_texture("VisBuffer", TextureDesc::d2(w, h, Format::Rg32Uint));
let hzb_prev = g.import_texture("HzbPrev", &persist.hzb, persist.hzb_state);

let cull = g.add_pass("CullInstances", Queue::Graphics, |b| {
    let scene = b.read_buffer(scene.instances, Access::ComputeRead);
    let hzb   = b.read_texture(hzb_prev, Access::ComputeRead);
    let out   = b.create_buffer("VisibleClusters", BufferDesc::structured::<u32>(MAX));
    let out   = b.write_buffer(out, Access::ComputeWrite);
    (out, move |ctx: &mut PassContext| {
        ctx.set_compute_pipeline(&shaders.cull_instances);
        ctx.push(&CullParams { scene: ctx.srv(scene), hzb: ctx.srv(hzb), out: ctx.uav(out), .. });
        ctx.dispatch(groups(n, 64), 1, 1);
    })
});
```

- **句柄**：
  - `TextureHandle` 和 `BufferHandle` 是 `Copy` 的小整数，带版本号。每写一次就产生一个新版本。
  - 拿旧版本去读，在 debug 构建里直接 panic，这样能捕获"读到了错误时刻的数据"这类 bug。
  - 句柄只在本帧的图里有效，生命周期参数保证它不能逃出这一帧。
- **访问声明**：`Access` 是一个枚举，比如 `ComputeRead`、`ComputeWrite`、`PixelRead`、`RenderTarget(LoadOp, StoreOp)`、`DepthWrite`、`DepthRead`、`Indirect`、`CopySrc`、`CopyDst`、`RayQueryAS`、`Present`。每个值对应一个 RHI 的 (Stage, Access, Layout) 三元组。可以只声明子资源范围（mip、slice），例如 HZB 逐级生成。
- **Pass 类型**：
  - `Raster`：带附件。graph 负责 begin/end render pass。
  - `Compute`
  - `Copy`
  - `AsyncCompute`（提示）
  - 另有标记 `side_effect`：没有输出也不剔除，用于 readback、present、调试捕获。
- **执行闭包** `FnOnce(&mut PassContext) + Send`。`PassContext` 提供：
  - command list 的受限视图：不能自己插 barrier，不能开 render pass；
  - 句柄到 RHI 对象的解析，以及 bindless 下标（`srv`、`uav`）；
  - 当帧上传分配，`ctx.upload(&T) -> u32` 返回 uniform 视图的下标；
  - push constants 写入。
- **shader 参数结构**：Rust 侧的 `#[repr(C)]` 结构体由 shader 编译器根据 shader 源生成（构建期通过 build.rs 完成），布局与 shader 端逐字节一致，不手写。

### 2.3 编译

编译分以下几步：

1. **剔除**：从输出（export、present、side_effect）反向可达的 pass 才保留。
2. **依赖 DAG**：边就是资源版本的读写关系，加上同一资源上的写后读和读后写。
3. **队列分配**：
   - 带 `AsyncCompute` 提示的 pass 放到 Compute 队列，Copy pass 放到 Copy 队列，其余都在 Graphics。
   - 如果平台没有独立的 Compute 或 Copy 队列，就退化到 Graphics。
4. **跨队列同步**：在跨队列的依赖边上插入 fence 的 signal/wait，同一对队列之间的同步点合并成一个。异步 pass 的资源生命周期会延长到对应的同步点之后，避免被别名覆盖。
5. **生命周期与别名**：
   - 按"队列时间线上的位置"计算每个瞬态资源的首次和末次使用区间。
   - 用区间图上的 first-fit 把资源放进每帧的瞬态 heap（每个内存类别一个 heap）。
   - buffer 和 texture 分开放 heap，避免 D3D12 heap tier 1 的限制，也方便统计。
6. **barrier 推导**：
   - 按子资源跟踪状态（默认整个资源，声明了范围的按范围跟踪）。
   - 每个 pass 前把需要的 barrier 合成一批。
   - 同一资源的连续读不插 barrier，读状态会合并（比如 `ComputeRead | PixelRead`）。
   - 别名资源首次使用时 before = Undefined。
   - 外部资源的最终状态写回它的 import 记录。
7. **分段**：
   - 在每个队列的 pass 序列里按估算代价切段。代价取上一帧该 pass 的 CPU 录制耗时；没有记录时用 pass 的 `cost` 提示。
   - 每段大致相当于 0.1–0.3 ms 的录制量，跨队列同步点必然是段边界。
   - 每段是一个 job、一个 command list。

### 2.4 编译缓存

- 编译结果是一份 `CompiledPlan`，包括保留的 pass、队列分配、别名偏移、barrier 列表、同步点和分段。
- **拓扑键**是以下内容的哈希：
  - pass 名称、类型和队列提示；
  - 每次访问（资源槽、版本、Access、范围）；
  - 瞬态资源的描述；
  - 外部资源的初始状态。
  
  键不变，就直接复用 plan，只需要重新绑定这一帧的物理对象。
- 稳态下（相机在动，但图的结构不变）每帧只需要算哈希加查表，目标 < 20 μs。
- 缓存按 LRU 保留最近 8 份 plan，用来应对编辑器开关视口、调试视图切换这类情况。

### 2.5 执行与并行录制

```
compile ─┬─ job: seg G0 → list G0 ─┐
         ├─ job: seg G1 → list G1 ─┤
         ├─ job: seg C0 → list C0 ─┼─ submit job（按队列、按顺序，带 waits/signals）→ present
         └─ job: seg G2 → list G2 ─┘
```

- 每个段的 job 从**线程无关**的 pool 里取 command pool：key 是 (帧槽, 队列, 段序号)，不跟线程绑定。fiber 可能在不同线程上恢复，而 D3D12/Vulkan 的 command list 并不要求在同一个线程上录制，所以这样是安全的。规则是**同一个 list 不能并发录制**。
- 提交 job 等待所有段完成，然后按队列依次 `submit`。也可以流水化：前面的段一完成就先提交，进一步缩短 GPU 的空闲。
- 大 pass 可以在内部继续拆分。例如 VSM 的多个光源视图，可以用 `ctx.parallel(n, |i, sub_ctx| …)` 并行录制。拆出来的子 list 在段内按顺序拼接。

### 2.6 FrameContext、资源池与退休

- `FrameContext` 持有：
  - 帧 arena；
  - 上传 ring（Upload 内存，线性分配，按帧回绕）；
  - 瞬态 heap 组（每个帧槽一组，按本帧的峰值需求增长，按水位收缩）；
  - 瞬态视图区段；
  - timestamp query pool；
  - 退休队列。
- **持久资源池**：export 出来的纹理和 buffer 按描述放进池里复用，history 资源由各子系统以句柄形式持有。
- **退休**：本帧 drop 的 RHI 对象、`rebind_view` 换下来的旧纹理、释放的持久 bindless 槽位，都挂到当前帧槽的退休列表上，等该帧槽的 fence 完成后再真正释放。

### 2.7 调试与工具

- `g.dump()` 输出 JSON：包括 pass、资源、生命周期、别名、barrier 和队列，供编辑器的 graph 可视化面板使用。
- **资源捕获**：`g.capture(handle, "name")` 会把一个瞬态资源 copy 到一个持久的调试纹理，供编辑器查看。调试视图（visbuffer 的三角形 ID、材质 bin、VSM 页、tile 热度）都走这条路。
- 每个 pass 自动加 `begin_event`/`end_event` 和 timestamp，结果交给 `marlin-profile` 的 `set_gpu_timings`。
- 校验（Development 构建）：
  - 检测未声明的访问：`PassContext` 只允许解析本 pass 声明过的句柄；
  - 检测重复写同一版本；
  - 检测异步 pass 读写了 graphics 队列正在写的资源。

### 2.8 子系统的接入约定

- 每个子系统暴露 `fn add_passes(g: &mut RenderGraph, inputs: XInputs) -> XOutputs`，输入输出都是句柄结构体。0006 §10 的帧图就是由这些函数组装起来的。
- 子系统之间只通过句柄交换数据，不共享 RHI 对象。

## 3. 测试

- 编译器全部是 CPU 测试：
  - 构造图，断言剔除结果、别名区间（不重叠且 offset 合法）、barrier 序列（与手写期望逐条比较）、跨队列同步点、分段；
  - 缓存命中和失效。
- GPU 测试：
  - 别名正确性：两个别名资源先后写入不同的图案，读回后各自正确；
  - 异步计算的结果和同步执行一致（结果逐字节相同）；
  - legacy barrier 路径和 enhanced barrier 路径在同一台机器上的结果一致。
