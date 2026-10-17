# 0007 — Render graph

状态：草案 · 2026-10-16

前置：
- [0001](0001-repo-layout.md)：crate 分层，`runtime/engine/render_graph`
- [0002](0002-shader-interface.md)：bindless ABI 和 push constants
- [0004](0004-shader-language.md) §10、§11.3：资源类型和 `[Push]` 参数
- [0005](0005-shader-compiler.md) §6、§9：反射；Rust 端手写参数结构，用反射校验布局

本文以已删除的旧版渲染文档中的 render graph 一节为底稿，按这次访谈的结论修订。

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 模型 | 每帧声明式重建；**声明顺序就是逻辑执行顺序**；编译器只做不改变可观察结果的变换：剔除、别名、跨队列调度 | 1 |
| 粒度 | **整帧一个图**：所有 view、所有窗口、UI、present 都在同一个图里 | 1.3 |
| 资源 | 瞬态（图创建，参与别名）、导入（外部 RHI 对象）、**历史**（graph 管理的双缓冲）和持久（graph 管理的单份） | 2 |
| Pass 形式 | **手写参数结构推导访问**：`#[derive(PassParams)]` 的 `#[repr(C)]` struct，资源字段是 `Srv`/`Uav`/`Cbv` 句柄，GPU 上就是一个 u32；graph 从字段推导读写；闭包只录制命令 | 3 |
| 其余访问 | render target、深度、indirect 参数、index buffer、copy、通过 buffer 间接引用的资源，在 pass builder 上显式声明 | 3.4 |
| 校验 | Development 构建用反射同时校验参数结构的布局和每个资源字段的读写类型 | 3.3、9.2 |
| 队列 | 默认 Graphics；`.async_compute()` 显式标注；graph 可以把异步 pass **提前**、把等待点**推迟** | 4.3 |
| 第一版范围 | 瞬态别名、异步计算与 Copy 队列、并行录制、编译缓存全部要做 | 4–6 |
| 编译缓存 | 按拓扑哈希缓存 `CompiledPlan`；命中时连同物理资源和 bindless 视图一起复用 | 5 |
| 并行录制 | 按代价切段，每段一个 job、一个 command list，按队列顺序提交 | 6 |
| 历史资源 | `g.history(name, view, desc)` 返回 (上一帧, 本帧)；graph 负责交换、尺寸变化时重建、长期不用时退休 | 8 |
| 调试 | 每 pass 计时与标记、Development 校验、图导出 JSON、资源捕获，第一版都做 | 9 |
| 验收 | 把现有前向渲染器、`Offscreen` 和 present 迁到 graph 上，`samples/basic` 画面不变；再加一条用到别名和异步计算的后处理链 | 10 |

## 1. 模型

### 1.1 每帧重建，顺序语义

- 每帧从头声明一遍图。图对象、pass 记录、闭包和参数结构都从帧 arena（`marlin_mem::FrameArenas`）分配，帧结束整体丢弃。
- **声明顺序就是逻辑执行顺序**，和单线程程序一样：一个 pass 读某资源，读到的是声明顺序里在它之前最后一次写入的内容。
- 编译器在此基础上只做三类变换，并且都不改变可观察结果：
  - **剔除**：结果没人用的 pass；
  - **别名**：生命周期不重叠的瞬态资源共用内存；
  - **跨队列调度**：异步 pass 放到 Compute 队列并调整位置（§4.3）。
- 句柄里不出现版本号。版本只在编译器内部存在，用来推导依赖。

### 1.2 graph 管什么、不管什么

- **管**：本帧中会改变状态的资源，也就是会被写入、或者在不同访问方式之间切换的资源。
- **不管**：常驻只读资源，例如网格的顶点/index buffer、cook 出来的纹理、采样器。它们上传后一直停在 `SHADER_READ`（或 `INDEX`）状态，shader 直接用 bindless 下标访问。参数结构里这类字段写成普通 `u32`（采样器写成 `SamplerIndex`），graph 不跟踪。
- 上传完成之前的那次状态切换（`COPY_DST → SHADER_READ`）由上传方负责。以后流送上传改成 graph 里的 Copy pass 时，这条规则不变：资源交给 shader 之前，状态已经切好。

### 1.3 整帧一个图

- 主相机、编辑器的各个视口、反射捕获、阴影的光源视图、UI、每个窗口的 present，都在同一个图里声明。
- 好处：
  - 只需每帧做一次的 pass（场景更新、蒙皮）只声明一次，各个 view 共享它们的输出；
  - 别名和异步调度能看到全帧，不会因为图的边界浪费重叠机会；
  - 只有一次编译、一组段、一轮提交。
- 多窗口：每个 swapchain 在建图前 acquire，然后用 `g.import_swapchain(&image)` 导入，最后由 `g.present(...)` 这个 side-effect pass 送回 `PRESENT` 状态。真正的 `Swapchain::present` 在提交之后按窗口依次调用。

## 2. 资源

### 2.1 四类资源

| 类别 | 创建 | 生命周期 | 内存 | 跨帧 |
|---|---|---|---|---|
| 瞬态 | `g.create_texture` / `g.create_buffer` | 本帧内，从首次使用到末次使用 | 帧槽的瞬态 heap，参与别名 | 否 |
| 导入 | `g.import_texture` / `g.import_buffer` / `g.import_swapchain` | 调用方持有 | 调用方的 | 状态由 graph 的状态表跟踪（§2.3） |
| 历史 | `g.history` | graph 持有，双缓冲 | committed，不参与别名 | 是，见 §8 |
| 持久 | `g.persistent` | graph 持有，单份 | committed，不参与别名 | 是，见 §8 |

### 2.2 瞬态资源的描述

- 描述里**不写 usage**。graph 汇总这个资源在本帧的所有访问，推导出 `TextureUsage`/`BufferUsage`。例如某个纹理既被写成 render target 又被采样，usage 就是 `RENDER_TARGET | SAMPLED`。
- 这样作者不会漏写 usage，usage 也不会多给（多给会影响压缩和布局）。
- 推导出来的 usage 是拓扑键的一部分（§5.1）。

### 2.3 导入资源的状态

- graph 有一张**跨帧的状态表**，按 RHI 对象记录"上一帧结束时它处于什么状态"。导入时不需要调用方报告当前状态。
- 第一次导入某个对象时，状态取 `TextureState::UNDEFINED`/`BufferState::NONE`；调用方也可以用 `.initial_state(s)` 显式给出。
- 对象 drop 时调用方要通知 graph（`g.forget(&texture)`），或者让 graph 在该对象长期未导入时自动清理状态表项。
- 导入的对象可以标成只读（`.read_only()`）：本帧任何写访问都会在 Development 构建里报错。

### 2.4 句柄

- `Tex`、`Buf` 是 `Copy` 的 4 字节句柄，指向本帧图里的资源表。
- 视图句柄在资源句柄上派生：
  - `tex.srv()`、`tex.uav()`：整个资源；
  - `tex.mip(i).srv()`、`tex.mips(a..b).uav()`、`tex.layer(i)`：子资源范围（HZB 逐级生成、立方体贴图逐面）；
  - `buf.srv()`、`buf.uav()`、`buf.cbv()`、`buf.range(offset, size)`。
- 视图句柄（`Srv<T>`、`Uav<T>`、`Cbv`）也是 4 字节，`#[repr(transparent)]` 包着一个 u32。这个 u32 在建图时是"本帧视图表的下标"，执行时被替换成 bindless 下标。
- **不用生命周期参数**把句柄绑在图上，否则所有参数结构都要带生命周期参数。作为代替，视图表下标里带一段帧序号；Development 构建里，用上一帧的句柄会直接 panic。

## 3. Pass

### 3.1 参数结构

```rust
#[derive(Reflect, PassParams)]
#[repr(C)]
struct BlurArgs {
    src: Srv<Texture>,        // Texture2D<float4>
    dst: Uav<Texture>,        // StorageTexture2D<float4>
    linear: SamplerIndex,     // Sampler，不跟踪
    radius: f32,
}

let half = g.create_texture("BloomHalf", TextureDesc::new_2d(w / 2, h / 2, Format::Rgba16Float));
g.compute("BloomDown", BlurArgs { src: hdr.srv(), dst: half.uav(), linear, radius: 1.5 },
    |ctx, a| {
        ctx.set_pipeline(&shaders.bloom_down);
        ctx.dispatch(groups2(w / 2, h / 2, 8, 8));
    });
```

- 参数结构是 Rust 端手写的 `#[repr(C)]` struct，和 shader 的 `[Push]` 参数类型一一对应（0005 §9：不生成 Rust 代码）。
- 资源字段写成句柄类型，和 shader 语言的资源类型对应：

  | Rust 字段 | shader 类型（0004 §10） | graph 推导的访问 |
  |---|---|---|
  | `Srv<Buffer>` | `ReadOnlyBuffer<T>`、`ReadOnlyByteBuffer` | 本 pass 所在 stage 的 shader 读 |
  | `Uav<Buffer>` | `Buffer<T>`、`ByteBuffer` | shader 读写 |
  | `Cbv` | `Uniform<T>` | uniform 读 |
  | `Srv<Texture>` | `Texture2D<T>`、`Texture2DArray<T>`、`TextureCube<T>` | shader 读 |
  | `Uav<Texture>` | `StorageTexture2D<T>`、`StorageTexture2DArray<T>` | shader 读写（单个 mip） |
  | `u32` | 常驻资源的下标 | 不跟踪 |
  | `SamplerIndex` | `Sampler`、`ComparisonSampler` | 不跟踪 |

- `#[derive(PassParams)]` 生成两个方法：
  - `visit_accesses`：列出每个句柄字段和它的访问方式，graph 在声明时调用，推导依赖和 barrier；
  - `resolve`：执行时把每个视图表下标原地换成 bindless 下标。

  字段类型和大小都不变，所以 `resolve` 之后的结构就是 GPU 要的那份字节。
- 支持嵌套 struct 和定长数组，例如 `[Srv<Texture>; 4]`。
- 参数结构整体写进 push constants，大小不能超过 128 字节（0002 §2.1），否则 `derive` 报编译错误。更大的数据由 pass 自己 `ctx.upload(&T)` 写进当帧上传 ring，再把得到的 `Cbv` 放进参数结构。
- stage 由 pass 类型决定：compute pass 是 compute stage；raster pass 默认"vertex + pixel 都可能读"，可以用 `#[stage(pixel)]` 字段属性收窄，以减少 barrier 的等待范围。

### 3.2 闭包

- 执行闭包是 `FnOnce(&mut PassContext, &Args) + Send`，拿到的 `&Args` 已经 resolve 过。
- graph 在调用闭包之前就把参数写进 push constants，闭包只负责绑定 pipeline 和发命令。需要不同参数的多次 dispatch 用 `ctx.push(&other)` 覆盖。
- 闭包不能插 barrier、不能开关 render pass，也看不到 RHI 对象本身，只能通过 `PassContext`：
  - `set_pipeline`、`dispatch`、`dispatch_indirect`、`draw*`、`dispatch_mesh*`、`set_viewport`、`set_scissor`、`set_stencil_reference`；
  - `push(&T)`、`upload(&T) -> Cbv`；
  - `index_buffer(h)`、`indirect_args(h)`：解析本 pass 已声明的 buffer；
  - `parallel(n, |i, sub| ...)`：把一个大 pass 拆成 n 个子 list 并行录制（§6.3）。

### 3.3 和反射的校验

Development 构建第一次用某个 pipeline 执行某个参数类型时，做一次比对（之后按 (pipeline, 类型) 缓存结果）：

- **布局**：沿用 0005 §9 的规则，比较字段名、偏移和大小。
- **读写类型**：
  - 反射里是只读资源类型的字段，Rust 端必须是 `Srv` 或 `u32`；
  - 是可写类型的，Rust 端必须是 `Uav`；
  - 是 `Uniform<T>` 的，必须是 `Cbv` 或 `u32`。
- 不一致就报错，指出 pass 名、字段名，以及双方的类型。

这条校验保证 graph 推导出的访问和 shader 实际的访问一致。shader 通过 buffer 里存的下标间接访问的资源（材质表、实例数据里引用的纹理）看不到，这部分靠 §3.4 的显式声明。

### 3.4 Pass 类型和显式声明

```rust
g.raster("Forward", ForwardArgs { frame: frame_cb, instances: inst.srv() }, |ctx, a| { ... })
    .color(target, LoadOp::Clear(clear), StoreOp::Store)
    .depth(depth, LoadOp::Clear(0.0), StoreOp::DontCare)
    .index_buffer(mesh_indices)          // 只在 index buffer 由 graph 管理时才需要
    .indirect_args(draw_args);

g.compute("Lum", args, |ctx, a| { ... }).async_compute();
g.copy("Readback", |ctx| { ... }).copy_src(hdr).copy_dst(staging);
g.compute("Resolve", args, ...).reads(material_table).writes(feedback);
```

- **Raster**：带附件，graph 负责 begin/end render pass。附件的 load/store 写在 builder 上，不写在参数结构里。
- **Compute**。
- **Copy**：没有参数结构，访问全部显式声明（`.copy_src`、`.copy_dst`）。默认在 Graphics 队列；`.copy_queue()` 放到 Copy 队列。
- builder 上的其他声明：
  - `.reads(h)` / `.writes(h)`：通过 buffer 里存的下标间接访问的 graph 资源；
  - `.side_effect()`：没有输出也不剔除，用于 readback、present、捕获；
  - `.cost(us)`：录制代价的提示，没有历史计时的时候用（§4.7）。
- 内置的 side-effect pass：`g.present(image)`、`g.readback(buf) -> Readback`、`g.capture(h, "name")`（§9.4）。

### 3.5 子系统的接入约定

- 每个子系统提供 `fn add_passes(g: &mut RenderGraph, inputs: &XInputs) -> XOutputs`，输入和输出都是句柄结构体。
- 子系统之间只交换句柄，不交换 RHI 对象。
- 渲染器的整帧就是按顺序调用各个子系统的 `add_passes` 组装起来的。

## 4. 编译

### 4.1 剔除

从根 pass 反向遍历，可达的才保留。根包括 side-effect pass，以及写了导入、历史或持久资源的 pass。

### 4.2 依赖

- 按声明顺序扫描每个资源（细到子资源）的访问序列，得到写后读、读后写、写后写三种边。
- 同一资源上连续的读之间没有边，它们的状态合并成一个读状态（例如 `SHADER_READ` 同时覆盖 compute 和 pixel stage）。

### 4.3 队列分配与异步调度

- 默认放在 Graphics 队列；`.async_compute()` 的放到 Compute 队列，`.copy_queue()` 的放到 Copy 队列。
- 平台没有专用队列时（`Device::dedicated_queue` 返回 `None`），退化到 Graphics 队列。这时异步 pass 按声明位置执行，结果不变。
- **提前与推迟**：
  - 每个异步 pass 在它的 Compute 队列上放到依赖允许的最早位置：只要它的所有前驱都已经在它之前完成。
  - Graphics 队列上，等待异步 pass 的点放到第一个真正依赖它的 pass 之前，而不是放在它的声明位置。
  - 这两步只调整跨队列的位置，同一队列内部的相对顺序不变。
- 作者负责挑选哪些 pass 适合异步（例如和光栅重叠的光照、自动曝光）；graph 不会自作主张把没有标注的 pass 挪到异步队列。

### 4.4 跨队列同步

- 每个队列一个 timeline fence。跨队列的依赖边变成"signal 某值 / wait 该值"，并且只在段边界上发生（§4.7）。
- 同一对队列之间相邻的多个同步点合并成一个。
- 异步 pass 用到的瞬态资源，生命周期延长到对应的等待点之后，避免和 Graphics 队列上的资源别名冲突。
- 跨队列的资源状态交接：生产方队列上先把资源切换到消费方需要的状态（D3D12 enhanced barrier 和 Vulkan 的队列族所有权转移都按这个顺序做）。

### 4.5 生命周期与别名

- 每个瞬态资源的生命周期是一个区间，按"各队列时间线上的全局位置"计算，跨队列的资源用 §4.4 延长后的区间。
- 用区间图上的 first-fit 给每个资源分配 heap 偏移。
- buffer 和 texture 分开放 heap，render target/深度纹理再单独一类（`HeapKind::Buffers`、`Textures`、`Targets`）：既能满足 D3D12 heap tier 1，也方便统计。tier 2 时也不合并，保持统一。
- 别名的接手方在第一次使用时 before = `Undefined`，RHI 按现有实现激活别名（enhanced 的 `DISCARD`、legacy 的 aliasing barrier）。

### 4.6 barrier 推导

- 按子资源跟踪状态：默认整个资源，用了 `mip`/`layer` 视图的按范围跟踪。
- 每个 pass 之前需要的 barrier 合成一批，一次 `texture_barriers`/`buffer_barriers` 调用发出。
- 导入资源的最终状态写回 §2.3 的状态表；历史和持久资源同样处理。
- **legacy 路径的真实状态**：RHI 的 legacy 实现目前按录制顺序记录每个纹理上一次 barrier 的状态，用来替换 `Undefined`。并行录制时各段的录制顺序和执行顺序不一致，这个记录会出错。graph 知道每个物理资源在执行顺序上的真实状态，所以改为由 graph 提供（RHI 的改动见 §10.3）。

### 4.7 分段

- 每个队列的 pass 序列按估算代价切段，每段的录制量大约 0.1–0.3 ms。
- 代价取上一帧该 pass 的 CPU 录制耗时（按 pass 名记），没有记录时用 `.cost()` 提示，再没有就按默认值。
- 跨队列的同步点必然是段边界。
- 每段是一个 job、一个 command list。

## 5. 编译缓存

### 5.1 拓扑键

拓扑键是下面这些内容的哈希（`marlin-hash`）：

- 每个 pass 的名字、类型、队列标注；
- 每次访问：资源槽、访问方式、子资源范围；
- 瞬态资源的描述，包括推导出的 usage；
- 导入、历史和持久资源的初始状态。

不包括：参数结构里的标量、闭包、导入资源具体是哪个 RHI 对象（但包括它的描述）。

### 5.2 CompiledPlan

- 内容：保留的 pass、队列分配、同步点、别名偏移、barrier 列表、分段。
- 稳态下相机在动、但图的结构不变时，每帧只算哈希、查表、重新绑定导入资源，目标 < 20 μs。
- 未命中时完整编译。目标：300 个 pass 的图 < 1 ms。
- LRU 保留最近 8 份 plan，用来应对编辑器开关视口、切换调试视图。

### 5.3 物理资源和视图跟着 plan 走

- 每份 plan 在每个帧槽上有一组**物理资源**：瞬态 heap 里按别名偏移 placed 的纹理和 buffer，以及它们的 bindless 视图。
- plan 命中时整组复用，稳态下不创建也不销毁任何 RHI 对象，bindless 槽位也不变。
- plan 被 LRU 淘汰时，它的物理资源挂到退休队列，等对应帧槽的 fence 完成再释放。

## 6. 执行与并行录制

### 6.1 流程

```
声明 ─ 编译/查缓存 ─┬─ job: 段 G0 → list G0 ─┐
                   ├─ job: 段 G1 → list G1 ─┤
                   ├─ job: 段 C0 → list C0 ─┼─ 提交 job（按队列、按顺序，带 waits/signals）→ present
                   └─ job: 段 G2 → list G2 ─┘
```

- 录制 job 用 `marlin_jobs` 的 `batch` 展开。
- 提交 job 依赖所有段完成，然后按队列依次 `submit_with`。
- 以后可以流水化：前面的段一完成就先提交，进一步缩短 GPU 空闲。第一版不做。

### 6.2 command pool

- 每段从**线程无关**的池里取 command pool 和 list，key 是 (帧槽, 队列, 段序号)。
- fiber 可能在别的线程上恢复，D3D12 和 Vulkan 都不要求 command list 在同一个线程上录制，所以这样是安全的。
- 规则：同一个 list 不能并发录制。

### 6.3 pass 内部的并行

- `ctx.parallel(n, |i, sub| ...)` 把一个 pass 拆成 n 个子 list 并行录制，例如阴影的多个光源视图。
- 子 list 在段内按 i 的顺序拼接。raster pass 的每个子 list 各自开关同一个 render pass，load op 只在第一个子 list 生效，store op 只在最后一个生效。

## 7. 帧上下文与内存

`FrameContext` 替代 framework 里直接使用的 `FramesInFlight`，持有：

- 帧 arena（`FrameArenas`）；
- 上传 ring：Upload 内存上的线性环，按帧回绕，`ctx.upload` 从这里分配；
- 每个帧槽一组瞬态 heap：按本帧峰值增长，连续若干帧低于水位再收缩（收缩会让 plan 的物理资源失效，需要重建）；
- 每个队列的 timeline fence 和帧槽的完成值；
- timestamp query pool（graphics/compute 一个，copy 队列单独一个 `CopyTimestamp`）；
- 退休队列：本帧释放的 RHI 对象和 bindless 槽位，等该帧槽的 fence 完成后才真正 drop。

帧数仍然是 2 帧在飞（`frames_in_flight = 2`）。

## 8. 历史资源与持久资源

```rust
let taa = g.history("TaaHistory", view.id, TextureDesc::new_2d(w, h, Format::Rgba16Float));
// taa.prev: Option<Tex>，上一帧写的内容；taa.cur: Tex，本帧要写的
let hzb = g.history("Hzb", view.id, hzb_desc);
let probes = g.persistent("ProbeCache", PERSISTENT_SCOPE, BufferDesc::structured::<Probe>(N));
```

- **键**：(名字, view)。view 是调用方给的稳定 id，例如编辑器视口的 id；整帧共享的资源用一个固定的作用域值。
- **双缓冲**：graph 为每个键持有两份 committed 资源，每帧交换。本帧的 `cur` 就是下一帧的 `prev`。
- **`prev` 为 `None`** 的情况：第一次出现、描述变了（例如窗口 resize）、调用方 `g.invalidate_history(view)`（镜头切换）。描述变了时 graph 重建这两份资源，旧的进退休队列。
- **退休**：某个键连续若干帧（默认 `2 × frames_in_flight + 2`）没有被请求，就释放。
- **持久资源**（`g.persistent`）：单份，内容一直保留，例如探针缓存、GPU Scene 之外的累积缓冲。描述变了时重建，内容清空，并通过返回值的 `fresh` 标志告诉调用方。
- 历史和持久资源都不参与别名，不进瞬态 heap，状态由 graph 跨帧跟踪。
- 它们的 usage 按历次访问的并集推导；出现新的访问方式时，资源重建（只在开发期偶尔发生）。

## 9. 调试与工具

### 9.1 计时与标记

- 每个 pass 自动包一对 `begin_event`/`end_event`，名字就是 pass 名。
- 每个 pass 前后各写一个 timestamp；copy 队列上的用 `CopyTimestamp`。
- 结果在帧槽回收时 resolve，通过 `marlin_profile::set_gpu_timings` 注册的 `GpuTimings` 实现交给 profiler，每个队列一条 track。
- 同一份数据也提供给 §4.7 的分段：GPU 时间以后可以用来做调度提示，第一版只用 CPU 录制时间。

### 9.2 Development 校验

只在 debug/development 构建里开，shipping 构建里关掉：

- `PassContext` 只能解析本 pass 声明过的句柄；
- 用上一帧的句柄；
- 参数结构和反射不一致（§3.3）；
- 写了标成 `.read_only()` 的导入资源；
- 同一个 pass 对同一子资源既声明读又声明写（应该用 `Uav`）；

graph 的误用一律 panic，并给出 pass 名和资源名。RHI 本身返回的错误（设备丢失、内存不够）通过 `marlin-error` 以 `Result` 返回。

### 9.3 图导出

- `g.dump()` 把编译后的图写成 JSON：pass、队列、段、资源、生命周期区间、别名偏移、barrier、同步点、上一帧的计时。
- runtime 是 no_std 的，JSON 由 render_graph 里一个很小的写出器生成。
- 用途：编辑器的图可视化面板，以及测试里的快照比对。

### 9.4 资源捕获

- `g.capture(h, "name")` 在 h 的当前版本之后插一个 side-effect 的 copy pass，把内容拷到一个以 name 为键的持久调试纹理。
- 编辑器的调试视图都走这条路。
- 捕获只在被请求的帧上发生；它改变拓扑键，所以开着捕获时用的是另一份 plan。

## 10. 落地

### 10.1 crate 与依赖

- `marlin-render-graph`（`runtime/engine/render_graph`）依赖 `marlin-rhi`、`marlin-mem`、`marlin-jobs`、`marlin-containers`、`marlin-hash`、`marlin-profile`、`marlin-reflect`、`marlin-diag`、`marlin-error`。
- 新增 proc-macro crate `marlin-render-graph-derive`（`runtime/engine/render_graph_derive`），提供 `#[derive(PassParams)]`，由 `marlin-render-graph` 重新导出。
- 反射比对要用 shader 产物的反射类型：放在 shader 编译器的 `reflect` crate 里（0005 §6.1），render_graph 依赖它。
- RHI 保持"薄"：状态跟踪、barrier 推导、延迟销毁都在 graph 里，RHI 不变（§10.3 的改动除外）。

### 10.2 迁移现有代码

- **`marlin-render`**：前向渲染器改成 `add_passes`：
  - 深度缓冲变成瞬态资源，`DepthTarget` 和 `resize` 里重建深度的逻辑删掉；
  - 每帧的实例数据从 `ctx.upload` 分配，不再自己维护每帧的 buffer；
  - 一个 raster pass，target 是调用方给的颜色句柄。
- **`marlin-framework`**：
  - `run.rs` 用 `FrameContext` 替代 `FramesInFlight`，swapchain image 通过 `import_swapchain` 导入，用 `g.present` 交还；
  - `Offscreen` 改成一个瞬态颜色目标加 `g.readback`。
- **验收**：
  1. `samples/basic` 的画面和迁移前逐字节一致（用 `Offscreen` 读回比对）；
  2. 加一条后处理链：HDR 场景目标 → bloom 降采样/升采样链（多个瞬态纹理，别名可以发生）→ 异步计算的亮度直方图与自动曝光 → tonemap 到 swapchain。
- 后处理链的 shader 用 Marlin shader 编写，所以第 2 步在编译器能产出 DXIL 之后做。graph 本身和第 1 步不依赖新 shader。

### 10.3 RHI 需要的改动

- **legacy barrier 的真实 before 状态由调用方提供**：去掉 backend 按录制顺序记录纹理状态的做法（并行录制下会出错，§4.6）。具体做法是在 `TextureBarrier` 上加一个"别名激活时资源的真实上一状态"字段，enhanced 路径忽略它。
- 建议同时补上 R0 推迟的 `fill_buffer` 和 `clear_texture_uav`：graph 的清零（计数器、直方图）要用到。没有它们时用一个 compute pass 代替。

## 11. 测试

测试放在各 crate 的 `tests/` 里。

- **CPU 测试**（平台无关，不需要 GPU）：
  - 构造图，断言剔除结果、依赖边；
  - 别名：区间不重叠、偏移满足对齐、heap 大小等于峰值；
  - barrier 序列：与手写的期望逐条比较，包括子资源范围和读状态合并；
  - 异步调度：提前和推迟的位置、同步点合并、资源生命周期延长；
  - 分段：同步点必然是段边界；
  - 缓存：拓扑相同命中，改了任一访问或描述就失效；
  - `PassParams` derive：访问列表、resolve 前后字节、嵌套和数组、超过 128 字节时编译失败；
  - 历史资源：交换、`prev` 为 `None` 的三种情况、退休。

  这些测试用一个不发 GPU 命令的录制后端，或者直接断言 `CompiledPlan`。
- **GPU 测试**（平台相关，在真实硬件上跑，不用 WARP 之类的软件设备）：
  - 别名正确性：两个别名资源先后写入不同图案，读回后各自正确；
  - 异步计算的结果和强制同步执行逐字节一致；
  - legacy barrier 路径和 enhanced barrier 路径在同一台机器上结果一致；
  - 并行录制和单段录制结果一致；
  - §10.2 的两条验收。

## 12. 待定

- 瞬态 heap 收缩的水位和帧数阈值。
- 提交流水化（§6.1）的收益要实测之后再定。
- Metal 后端落地时，TBDR 上的 raster pass 合并和 memoryless 附件。
- split barrier：enhanced barrier 没有，legacy 有；目前不用。
- 跨帧的异步重叠（本帧的异步计算和下一帧的图形重叠）。
- 以 GPU 计时为依据的异步调度提示。
