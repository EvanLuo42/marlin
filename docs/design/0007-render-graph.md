# 0007 — Render graph

状态：草案 · 2026-10-16

前置：
- [0001](0001-repo-layout.md)：crate 分层，`runtime/engine/render_graph`
- [0002](0002-shader-interface.md)：bindless ABI 和 push constants
- [0005](0005-shader-compiler.md) §9：Rust 端手写参数结构，用反射校验布局

本文以已删除的旧版渲染文档中的 render graph 一节为底稿，按这次访谈的结论修订。

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 模型 | 每帧声明式重建；**声明顺序就是逻辑执行顺序**；编译器只做不改变可观察结果的变换：剔除、别名、跨队列调度 | 1 |
| 粒度 | **整帧一个图**：所有 view、所有窗口、UI、present 都在同一个图里 | 1.3 |
| 资源 | 瞬态（图创建，参与别名）、导入（外部 RHI 对象）、**历史**（graph 管理的双缓冲）和持久（graph 管理的单份） | 2 |
| Pass 形式 | **显式声明 + 闭包**：setup 闭包里用 `b.read_*`/`b.write_*` 声明访问，返回执行闭包；执行闭包里用 `ctx.srv(h)`/`ctx.uav(h)` 取 bindless 下标填参数 | 3 |
| 句柄 | `Copy` 的小整数，带版本号，每写一次产生新版本；用旧版本在 Development 构建里 panic；生命周期参数保证句柄不逃出本帧 | 2.4 |
| 队列 | 默认 Graphics；`b.async_compute()` 显式标注；graph 可以把异步 pass **提前**、把等待点**推迟** | 4.3 |
| 第一版范围 | 瞬态别名、异步计算与 Copy 队列、并行录制、编译缓存全部要做 | 4–6 |
| 编译缓存 | 按拓扑哈希缓存 `CompiledPlan`；命中时连同物理资源和 bindless 视图一起复用 | 5 |
| 并行录制 | 按代价切段，每段一个 job、一个 command list，按队列顺序提交 | 6 |
| 历史资源 | `g.history(name, view, desc)` 返回 (上一帧, 本帧)；graph 负责交换、尺寸变化时重建、长期不用时退休 | 8 |
| 加速结构 | 只有 inline ray query，没有 RT 专用的 pass；AS 是第三种资源；BLAS 按组导入，静态的建完就不再写、动态的每帧 refit；TLAS 的实例由 GPU 生成，每帧重建；设计写全，实现排在 RHI 补齐 AS 之后 | 9 |
| 调试 | 每 pass 计时与标记、Development 校验、图导出 JSON、资源捕获，第一版都做 | 10 |
| 验收 | 把现有前向渲染器、`Offscreen` 和 present 迁到 graph 上，`samples/basic` 画面不变；再加一条用到别名和异步计算的后处理链 | 11 |

## 1. 模型

### 1.1 每帧重建，顺序语义

- 每帧从头声明一遍图。图对象、pass 记录、闭包都从帧 arena（`marlin_mem::FrameArenas`）分配，帧结束整体丢弃。
- **声明顺序就是逻辑执行顺序**，和单线程程序一样：一个 pass 读某资源，读到的是声明顺序里在它之前最后一次写入的内容。
- 编译器在此基础上只做三类变换，并且都不改变可观察结果：
  - **剔除**：结果没人用的 pass；
  - **别名**：生命周期不重叠的瞬态资源共用内存；
  - **跨队列调度**：异步 pass 放到 Compute 队列并调整位置（§4.3）。
- 每次写产生资源的一个新版本，句柄带着版本号（§2.4）。顺序语义决定读到什么，版本号用来在 Development 构建里抓"读到了错误时刻的数据"这类 bug。

### 1.2 graph 管什么、不管什么

- **管**：本帧中会改变状态的资源，也就是会被写入、或者在不同访问方式之间切换的资源。
- **不管**：常驻只读资源，例如网格的顶点/index buffer、cook 出来的纹理、采样器。它们上传后一直停在 `SHADER_READ`（或 `INDEX`）状态，shader 直接用 bindless 下标访问。参数结构里直接放它们的下标，graph 不跟踪。
- 构建和 compaction 都完成之后的静态 BLAS 也属于这一类，但它们仍然作为 AS 组导入，好让 ray query 和下一次构建之间的依赖可见（§9.4）。
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

这四类都可以是 texture 或 buffer。加速结构是第三种资源，另外还有"AS 组"，见 §9。

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

- `TextureHandle<'g>`、`BufferHandle<'g>` 是 `Copy` 的小整数：资源槽加版本号。
- **版本**：每次写（`b.write_*`、`b.color`、`b.depth`）返回同一资源的新版本句柄。之后再拿旧版本去读或写，Development 构建里直接 panic，并给出 pass 名和资源名。
- **生命周期**：`'g` 是本帧图的生命周期，句柄不能逃出这一帧。跨帧的数据走历史或持久资源（§8），每帧重新取句柄。
- **子资源范围**：`tex.mip(i)`、`tex.mips(a..b)`、`tex.layer(i)`、`buf.range(offset, size)` 返回带范围的句柄，用于 HZB 逐级生成、立方体贴图逐面这类情况。
- 句柄只在 graph 内部有意义；要给 shader 的下标在执行闭包里用 `ctx.srv`/`ctx.uav`/`ctx.cbv` 取（§3.2）。

## 3. Pass

### 3.1 声明读写，闭包录制

```rust
let half = g.create_texture("BloomHalf", TextureDesc::new_2d(w / 2, h / 2, Format::Rgba16Float));

let half = g.add_compute("BloomDown", |b| {
    let src = b.read_texture(hdr, Access::ComputeRead);
    let dst = b.write_texture(half, Access::ComputeWrite);
    (dst, move |ctx: &mut PassContext| {
        ctx.set_pipeline(&shaders.bloom_down);
        ctx.push(&BlurArgs { src: ctx.srv(src), dst: ctx.uav(dst), linear, radius: 1.5 });
        ctx.dispatch(groups2(w / 2, h / 2, 8, 8));
    })
});
```

- `add_raster`、`add_compute`、`add_copy` 都接一个 setup 闭包。setup 闭包在声明时立即执行：通过 builder `b` 声明读写和 pass 的选项，返回 (输出, 执行闭包)。输出是任意值，一般是新版本的句柄或句柄结构体。
- 访问用 `b.read_texture`/`b.write_texture`/`b.read_buffer`/`b.write_buffer` 声明：
  - 读返回同一个句柄，写返回新版本的句柄（§2.4）；
  - 可以只声明子资源范围，例如 `b.write_texture(hzb.mip(i), Access::ComputeWrite)`。
- `Access` 是一个枚举，每个值对应 RHI 的一个 (Stage, Access, Layout) 三元组：

  | `Access` | 含义 |
  |---|---|
  | `ComputeRead`、`VertexRead`、`PixelRead`、`MeshRead` | 对应 stage 的 shader 只读 |
  | `ComputeWrite`、`PixelWrite` | shader 读写（UAV / 存储纹理） |
  | `Uniform` | uniform 读 |
  | `Indirect`、`Index` | indirect 参数、index buffer |
  | `CopySrc`、`CopyDst` | 拷贝 |
  render target 和深度不在这里，由 `b.color`、`b.depth` 声明（§3.3）。加速结构的访问方式见 §9.2。
- shader 通过 buffer 里存的下标间接访问的 graph 资源（例如材质表引用的反馈 buffer），也照样用 `b.read_*`/`b.write_*` 声明。
- 执行闭包是 `FnOnce(&mut PassContext) + Send`，捕获 setup 里拿到的句柄。

### 3.2 PassContext

- `ctx.srv(h)`、`ctx.uav(h)`、`ctx.cbv(h)`：返回 bindless 下标，视图覆盖声明时的子资源范围。只能解析本 pass 声明过的句柄（§10.2）。
- 参数结构是 Rust 端手写的 `#[repr(C)]` struct，资源字段就是 `u32`（0004 §10、0005 §9）。布局由 0005 §9 的反射比对负责，graph 不做额外检查。
- 闭包不能插 barrier、不能开关 render pass，也看不到 RHI 对象本身，只能通过 `PassContext`：
  - `set_pipeline`、`dispatch`、`dispatch_indirect`、`draw*`、`dispatch_mesh*`、`set_viewport`、`set_scissor`、`set_stencil_reference`；
  - `push(&T)`：写 push constants，T 不超过 128 字节（0002 §2.1）；
  - `upload(&T) -> u32`：写进当帧上传 ring，返回 uniform 视图的下标，用来传更大的数据；
  - `index_buffer(h)`、`indirect_args(h)`：使用本 pass 声明过的 buffer；
  - `parallel(n, |i, sub| ...)`：把一个大 pass 拆成 n 个子 list 并行录制（§6.3）。

### 3.3 Pass 类型和选项

```rust
let (color, depth) = g.add_raster("Forward", |b| {
    let color = b.color(target, LoadOp::Clear(clear), StoreOp::Store);
    let depth = b.depth(depth, LoadOp::Clear(0.0), StoreOp::DontCare);
    let inst = b.read_buffer(instances, Access::VertexRead);
    ((color, depth), move |ctx: &mut PassContext| { ... })
});

let lum = g.add_compute("Luminance", |b| {
    b.async_compute();
    ...
});
```

- **Raster**：带附件。`b.color`、`b.depth` 声明附件和 load/store op，返回新版本句柄；graph 负责 begin/end render pass。
- **Compute**。
- **Copy**：默认在 Graphics 队列；`b.copy_queue()` 放到 Copy 队列。
- builder 上的其他选项：
  - `b.async_compute()`：放到 Compute 队列（§4.3）；
  - `b.side_effect()`：没有输出也不剔除，用于 readback、present、捕获；
  - `b.cost(us)`：录制代价的提示，没有历史计时的时候用（§4.7）。
- 内置的 side-effect pass：`g.present(image)`、`g.readback(buf) -> Readback`、`g.capture(h, "name")`（§10.4）。

### 3.4 子系统的接入约定

- 每个子系统提供 `fn add_passes<'g>(g: &mut RenderGraph<'g>, inputs: &XInputs<'g>) -> XOutputs<'g>`，输入和输出都是句柄结构体。
- 子系统之间只交换句柄，不交换 RHI 对象。
- 渲染器的整帧就是按顺序调用各个子系统的 `add_passes` 组装起来的。

## 4. 编译

### 4.1 剔除

从根 pass 反向遍历，可达的才保留。根包括 side-effect pass，以及写了导入、历史或持久资源的 pass。

### 4.2 依赖

- 按声明顺序扫描每个资源（细到子资源）的访问序列，得到写后读、读后写、写后写三种边。
- 同一资源上连续的读之间没有边，它们的状态合并成一个读状态（例如 `SHADER_READ` 同时覆盖 compute 和 pixel stage）。

### 4.3 队列分配与异步调度

- 默认放在 Graphics 队列；`b.async_compute()` 的放到 Compute 队列，`b.copy_queue()` 的放到 Copy 队列。
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
- **跨帧**：跨帧状态表（§2.3）除了状态，还记录每个导入、历史和持久资源最后一次访问所在的队列和 fence 值。下一帧第一次访问它的 pass 如果在另一个队列上，graph 插入对那个 fence 值的等待。否则下一帧的异步 pass 可能和上一帧另一个队列上还没跑完的 pass 重叠（§9.9 是一个例子）。

### 4.5 生命周期与别名

- 每个瞬态资源的生命周期是一个区间，按"各队列时间线上的全局位置"计算，跨队列的资源用 §4.4 延长后的区间。
- 用区间图上的 first-fit 给每个资源分配 heap 偏移。
- buffer 和 texture 分开放 heap，render target/深度纹理再单独一类（`HeapKind::Buffers`、`Textures`、`Targets`）：既能满足 D3D12 heap tier 1，也方便统计。tier 2 时也不合并，保持统一。加速结构单独一类，暂不参与别名（§9.5）。
- 别名的接手方在第一次使用时 before = `Undefined`，RHI 按现有实现激活别名（enhanced 的 `DISCARD`、legacy 的 aliasing barrier）。

### 4.6 barrier 推导

- 按子资源跟踪状态：默认整个资源，用了 `mip`/`layer` 视图的按范围跟踪。
- 每个 pass 之前需要的 barrier 合成一批，一次 `texture_barriers`/`buffer_barriers` 调用发出。
- 导入资源的最终状态写回 §2.3 的状态表；历史和持久资源同样处理。
- **legacy 路径的真实状态**：legacy D3D12 的 barrier 必须写出资源当前的真实状态，即使内容要丢弃。录制顺序在并行录制时和执行顺序不一致，所以 RHI 不再自己记录，由 graph 按执行顺序提供（`TextureBarrier::undefined_from` 和 `TextureBarrier::discard`，见 §11.3）。

### 4.7 分段

- 每个队列的 pass 序列按估算代价切段，每段的录制量大约 0.1–0.3 ms。
- 代价取上一帧该 pass 的 CPU 录制耗时（按 pass 名记），没有记录时用 `b.cost()` 提示，再没有就按默认值。
- 跨队列的同步点必然是段边界。
- 每段是一个 job、一个 command list。

## 5. 编译缓存

### 5.1 拓扑键

拓扑键是下面这些内容的哈希（`marlin-hash`）：

- 每个 pass 的名字、类型、队列标注；
- 每次访问：资源槽、访问方式、子资源范围；
- 瞬态资源的描述，包括推导出的 usage；
- 导入、历史和持久资源的初始状态。

不包括：闭包和它捕获的值、导入资源具体是哪个 RHI 对象（但包括它的描述）。

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

## 9. 加速结构

光追只用 inline ray query（0002 §1、0004 §12.4），没有 RT pipeline，所以 graph 不需要 RT 专用的 pass 类型：ray query 写在普通的 compute pass（或 raster pass 的 pixel shader）里。graph 要管的是**加速结构（AS）本身**：它的构建、它和构建输入之间的依赖，以及它被 ray query 读取时的同步。

本节先把设计写全。RHI 需要的加速结构功能和全局内存 barrier 都已经补齐（§11.3）；graph 第一版不等它。

### 9.1 AS 作为资源

- AS 是第三种资源，和 texture、buffer 并列：`AccelHandle<'g>`，描述是 `AccelDesc { kind: Tlas | Blas, size }`。
  - `size` 来自 RHI 的 prebuild 查询（D3D12 `GetRaytracingAccelerationStructurePrebuildInfo`、Vulkan `vkGetAccelerationStructureBuildSizesKHR`）：输入几何或实例上限 → AS 大小、构建 scratch 大小、refit scratch 大小。
  - D3D12 和 Vulkan 的 AS 都放在 buffer 里；Metal 是单独的对象。RHI 统一成一个 `Accel` 类型，graph 只看 `Accel`。
- **没有 layout**：AS 一直处于"加速结构"状态（D3D12 legacy 要求它创建时就在 `RAYTRACING_ACCELERATION_STRUCTURE` 状态，并且永远不切换）。所以 AS 上的 barrier 只有执行依赖和内存可见性，没有布局转换。
- **bindless**：ray query 用的 TLAS 占一个资源表槽位，shader 里的 `AccelerationStructure` 就是这个下标（0004 §10）。执行闭包里用 `ctx.accel(h)` 取。BLAS 不需要槽位，它只通过 TLAS 的实例被引用。

### 9.2 访问方式

`Access` 表（§3.1）为 AS 增加以下几项：

| `Access` | 用在 | RHI 映射（D3D12 enhanced / Vulkan） |
|---|---|---|
| `AccelBuildWrite` | 构建或 refit 的目标 AS | sync `BUILD_RAYTRACING_ACCELERATION_STRUCTURE`，access `RAYTRACING_ACCELERATION_STRUCTURE_WRITE` / `ACCELERATION_STRUCTURE_WRITE_KHR` |
| `AccelBuildRead` | refit 的源 AS；TLAS 构建时被引用的 BLAS | 同上 sync，access `..._READ` |
| `AccelBuildInput` | 顶点、index、实例描述 buffer | sync `BUILD_RAYTRACING_ACCELERATION_STRUCTURE`，access `SHADER_RESOURCE`（legacy：`NON_PIXEL_SHADER_RESOURCE`） |
| `AccelScratch` | 构建 scratch | sync 同上，access `UNORDERED_ACCESS` |
| `AccelCopySrc`、`AccelCopyDst` | compaction 的拷贝 | sync `COPY_RAYTRACING_ACCELERATION_STRUCTURE` |
| `AccelPostbuild` | 写入 compaction 后大小的 buffer | sync `EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO` |
| `ComputeRead`、`PixelRead`（作用在 AS 上） | ray query | 对应 stage 的 sync，access `RAYTRACING_ACCELERATION_STRUCTURE_READ` |

- 声明方式和其他资源一样：`b.write_accel(h, Access::AccelBuildWrite)` 返回新版本，`b.read_accel(h, Access::ComputeRead)`。
- legacy D3D12 没有这些 sync 类型，AS 之间的依赖用 AS buffer 上的 UAV barrier 表达；构建输入切到 `NON_PIXEL_SHADER_RESOURCE`。这层映射在 RHI 里，graph 只发 (stage, access) 对。

### 9.3 构建 pass

```rust
let tlas = g.create_accel("SceneTlas", AccelDesc::tlas(capacity));
let tlas = g.add_accel("BuildTlas", |b| {
    let inst = b.read_buffer(instances, Access::AccelBuildInput);
    b.read_accel_group(&static_blas, Access::AccelBuildRead);
    b.read_accel_group(&dynamic_blas, Access::AccelBuildRead);
    let scratch = b.create_buffer("TlasScratch", BufferDesc::scratch(capacity_scratch));
    let scratch = b.write_buffer(scratch, Access::AccelScratch);
    let tlas = b.write_accel(tlas, Access::AccelBuildWrite);
    (tlas, move |ctx: &mut PassContext| {
        ctx.build_tlas(tlas, inst, capacity, scratch);
    })
});
```

- 新的 pass 类型 `add_accel`，**默认就是异步计算**（§4.3），用 `b.graphics_queue()` 改回 Graphics 队列。AS 构建不能放在 Copy 队列上。
- `PassContext` 增加：
  - `build_blas(dst, geometry, scratch)`、`refit_blas(dst, src, geometry, scratch)`（`src` 可以等于 `dst`，原地 refit）；
  - `build_tlas(dst, instances, count, scratch)`；
  - `copy_accel_compact(dst, src)`、`emit_compacted_size(src, out_buffer, offset)`。
- 同一个 pass 里连续的多次构建如果共用一块 scratch，graph 在它们之间自动插 scratch 的 UAV barrier。更好的做法是每次构建用 scratch 的不同区段，这样它们可以并行；`BufferDesc::scratch` 的大小按区段之和给出。

### 9.4 AS 组

问题：每帧要建的 BLAS 数量是变化的（流送进来的新网格、预算内能建几个）。如果每个 BLAS 都是图里单独的资源、单独声明，拓扑每帧都变，编译缓存（§5）就失效了。

做法：

- 把一批 BLAS 当作**一个逻辑资源**导入：`g.import_accel_group("StaticBlas", &pool)`，得到一个 `AccelGroupHandle`。
- 对组的访问在拓扑上就是一次访问，不管这一帧组里有几个 BLAS 被构建。拓扑键只记录"这一帧有没有写这个组"。
- 组上的 barrier 用**全局内存 barrier**（D3D12 enhanced 的 global barrier、Vulkan 的 `VkMemoryBarrier2`、D3D12 legacy 的空资源 UAV barrier），而不是逐个 BLAS 的 barrier（RHI 的 `memory_barrier`，§11.3）。
- 组里具体建哪几个、写到哪里，是执行闭包里的数据（一个列表），不进拓扑。

### 9.5 谁持有 AS

| AS | 持有者 | graph 里的身份 | 生命周期 |
|---|---|---|---|
| 静态网格的 BLAS | 网格驻留系统，和顶点/index buffer 放在一起 | 组 `StaticBlas` | 网格驻留期间一直存在；构建和 compaction 完成后就不再被写 |
| 蒙皮/形变网格的 BLAS | 场景渲染器 | 组 `DynamicBlas`，每帧导入 | 实例存在期间；每帧 refit |
| TLAS | graph | 瞬态 AS，每帧重建 | 本帧 |

- **TLAS 每帧完整重建**，不做 TLAS refit：实例的增删和大幅移动都很常见，完整重建的质量更稳定，代价也小（实例数而不是三角形数）。
- TLAS 是瞬态的，不需要跨帧。需要上一帧 TLAS 的子系统用历史资源（§8）。
- 实例上限 `capacity` 是 TLAS 描述的一部分，也就进了拓扑键。为了不让实例数的小变化导致缓存失效，`capacity` 按 2 的幂向上取整，并且只增不减；连续若干帧低于一半时才缩小。
- AS 的内存：
  - BLAS 数量多、大小不一，不能每个都是一个 committed 资源。它们由 RHI 之上的 GPU 内存子分配层管理（RHI 只提供 heap、placed 资源和 AS 存储 buffer 里按偏移建 AS 的能力，子分配策略放在它上面；这一层和流送的纹理、网格共用），按 AS 要求的 256 字节对齐；
  - 瞬态 TLAS 和 scratch 放在帧槽的瞬态 heap 里。AS 单独一个内存类别，不和其他资源别名；同类之间是否可以别名，等 RHI 实测之后再放开（§13）。

### 9.6 TLAS 的实例由 GPU 生成

```
GPU Scene 实例表 ─┐
网格表（BLAS 引用）─┼─ compute "WriteTlasInstances" ─→ 实例描述 buffer（瞬态）─→ "BuildTlas"
可见性/距离参数 ───┘
```

- CPU 不碰每个实例。CPU 只知道 GPU Scene 的实例数 N，用它定 `capacity`。
- 一个 compute pass 遍历 GPU Scene 的实例表，为每个实例写一条实例描述：变换（3×4）、mask、flags 和 BLAS 引用。第 i 条描述对应 GPU Scene 的第 i 个实例，所以 ray query 命中后的 `q.Committed.InstanceIndex` 就是 GPU Scene 的实例下标；24 位的自定义 `InstanceID` 字段先保留不用。
- 不进 TLAS 的实例（距离剔除、BLAS 还没建好、被禁用）写成**非激活实例**：BLAS 引用为 0。D3D12 和 Vulkan 都会跳过这种实例。这样构建时传的实例数可以直接用 `capacity`，不需要 GPU 端的计数。
- **实例描述的格式是 target 相关的**：D3D12 和 Vulkan 都是 64 字节、布局相同；Metal 是另一种格式。shader 标准库提供一个写实例描述的函数，按 `Target.*` 常量选择布局；RHI 提供实例描述的大小和"BLAS 引用"值（D3D12/Vulkan 是 GPU 地址，Metal 是资源 ID）。网格表里存的就是这个引用值，BLAS 换了（比如 compaction 之后）就通过场景的 scatter 上传更新。

### 9.7 静态 BLAS：预算、异步构建与 compaction

```
帧 N      : BuildBlas（预算内的一批，ALLOW_COMPACTION）→ emit 压缩后大小 → readback
帧 N+k    : readback 就绪 → 按大小分配 → CompactBlas（拷贝压缩）→ 网格表换成新引用
帧 N+k+F  : 旧的未压缩 BLAS 经退休队列释放
```

- **构建预算**：网格驻留系统维护一个待建队列。每帧按三角形数预算（cvar，比如 `r.rt.blas_budget_tris`）从队头取一批来建。预算之外的网格这一帧没有 BLAS，它们的实例在 TLAS 里是非激活的（§9.6）。
- **异步**：`BuildBlas` 和 `CompactBlas` 都是 `add_accel` pass，默认在 Compute 队列上，和光栅重叠。
- **compaction**：
  - 构建时带 `ALLOW_COMPACTION`，构建后用 `emit_compacted_size` 把大小写进一个 buffer，再用 `g.readback` 读回；
  - 读回在若干帧后就绪（按帧槽 fence），这时分配精确大小的内存，在一个 `CompactBlas` pass 里拷贝压缩；
  - 在压缩完成之前，网格一直用未压缩的 BLAS，不影响渲染；
  - 旧 BLAS 交给退休队列，等引用它的所有帧完成后再释放。
- 构建和 compaction 都只写 `StaticBlas` 组；完成之后这个 BLAS 不再被写，就像上传完成的顶点 buffer 一样。

### 9.8 动态 BLAS：refit 与重建

- 蒙皮 compute pass 把形变后的顶点写进一个持久 buffer（每个蒙皮实例一份）。
- `RefitBlas` pass 读这些顶点（`AccelBuildInput`），对 `DynamicBlas` 组里的 BLAS 原地 refit（`ALLOW_UPDATE`）。
- refit 的质量会随形变累积下降，所以下面两种情况改为完整重建：
  - 距上次重建已经过了 N 帧（cvar，默认 30），各实例的重建时间错开，避免集中在同一帧；
  - CPU 端标记了不连续的形变（瞬移、换动画、布料重置）。
- 重建和 refit 在同一个 pass 里，按实例选择，不影响拓扑。

### 9.9 同步

- **帧内**：BLAS 构建 → TLAS 构建 → ray query 的依赖都是普通的依赖边，由 §4.2–4.4 处理。
- **ray query 隐式读 BLAS**：ray query 读 TLAS 时，实际上也在读 TLAS 引用的所有 BLAS。graph 记录"这个 TLAS 由哪些组构建"，ray query pass 对 TLAS 的每次读，自动加上对这些组的读。这样，组的下一次写（例如下一个 refit）一定排在这些 ray query 之后。
- **跨帧**：动态 BLAS 的 refit 跑在 Compute 队列上，可能和上一帧 Graphics 队列上还没跑完的 ray query 重叠。§4.4 的跨帧规则对 AS 组同样适用：组上一帧最后一次被访问的队列和 fence 值记在状态表里，下一帧的 refit 在另一个队列上时先等它。

### 9.10 调试

- 图导出（§10.3）列出 AS、组、每帧构建的数量和三角形数、TLAS 的 capacity 和激活实例数。
- Development 校验：
  - 对 AS 做 `AccelBuildWrite` 之外的写访问；
  - 实例描述 buffer 的大小小于 `capacity × 实例描述大小`；
  - ray query 读了本帧还没构建的瞬态 TLAS。
- 调试视图（实例 ID、BLAS 是否压缩、refit 次数）通过 ray query 的 compute pass 输出到一张纹理，再走资源捕获（§10.4）。

## 10. 调试与工具

### 10.1 计时与标记

- 每个 pass 自动包一对 `begin_event`/`end_event`，名字就是 pass 名。
- 每个 pass 前后各写一个 timestamp；copy 队列上的用 `CopyTimestamp`。
- 结果在帧槽回收时 resolve，通过 `marlin_profile::set_gpu_timings` 注册的 `GpuTimings` 实现交给 profiler，每个队列一条 track。
- 同一份数据也提供给 §4.7 的分段：GPU 时间以后可以用来做调度提示，第一版只用 CPU 录制时间。

### 10.2 Development 校验

只在 debug/development 构建里开，shipping 构建里关掉：

- `PassContext` 只能解析本 pass 声明过的句柄；
- 读或写了旧版本的句柄（§2.4）；
- 写了标成 `.read_only()` 的导入资源；
- 同一个 pass 对同一子资源既声明读又声明写（应该只声明一次写访问，`ComputeWrite` 这类访问本身包含读）；

graph 的误用一律 panic，并给出 pass 名和资源名。RHI 本身返回的错误（设备丢失、内存不够）通过 `marlin-error` 以 `Result` 返回。

### 10.3 图导出

- `g.dump()` 把编译后的图写成 JSON：pass、队列、段、资源、生命周期区间、别名偏移、barrier、同步点、上一帧的计时。
- runtime 是 no_std 的，JSON 由 render_graph 里一个很小的写出器生成。
- 用途：编辑器的图可视化面板，以及测试里的快照比对。

### 10.4 资源捕获

- `g.capture(h, "name")` 在 h 的当前版本之后插一个 side-effect 的 copy pass，把内容拷到一个以 name 为键的持久调试纹理。
- 编辑器的调试视图都走这条路。
- 捕获只在被请求的帧上发生；它改变拓扑键，所以开着捕获时用的是另一份 plan。

## 11. 落地

### 11.1 crate 与依赖

- `marlin-render-graph`（`runtime/engine/render_graph`）依赖 `marlin-rhi`、`marlin-mem`、`marlin-jobs`、`marlin-containers`、`marlin-hash`、`marlin-profile`、`marlin-diag`、`marlin-error`。
- RHI 保持"薄"：状态跟踪、barrier 推导、延迟销毁都在 graph 里，RHI 不变（§11.3 的改动除外）。

### 11.2 迁移现有代码

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
- 后处理链的 shader 用 Marlin shader 编写，编译器已经能产出 SPIR-V（0005 §11 第 10 步），第 2 步不再需要等。graph 本身和第 1 步不依赖新 shader。

### 11.3 RHI 需要的改动（已完成）

- **legacy barrier 的真实 before 状态由调用方提供**：backend 不再按录制顺序记录纹理状态（并行录制下会出错，§4.6）。`TextureBarrier::undefined_from` 给出 `before.layout` 为 `Undefined` 时资源真实所在的 layout，enhanced 路径忽略它。`TextureBarrier::discard(texture, 上一个 TextureState, after)` 是 graph 要用的写法：等上一次访问的 stage 完成，丢弃内容，并把上一 layout 告诉 legacy 路径。
- `CommandList::fill_buffer` 和 `CommandList::clear_storage_texture` 已经补上（D3D12 用 `ClearUnorderedAccessView*`），graph 的清零（计数器、直方图）直接用它们。
- **加速结构**（§9）。RHI 里的 `AccelerationStructure` 就是本文的 `Accel`：
  - AS 住在**存储 buffer** 里：`BufferUsage::ACCELERATION_STRUCTURE_STORAGE` 的 buffer（只能有这一种用途，device-local），committed 或 placed 都行，D3D12 上带加速结构标志。`create_acceleration_structure_in(storage, offset, kind, size)` 在它的 256 字节对齐偏移上建 AS，§9.5 的子分配层就用它把许多小 BLAS 放进一个 buffer（placed 资源本身要 64 KiB 对齐）。`create_acceleration_structure(kind, size)` 是一个 AS 独占一个 committed 存储 buffer 的简写；
  - 同一个存储 buffer 里的 AS 共用这个 buffer 的 barrier（`AccelerationStructure::buffer` 返回它）；
  - prebuild 查询 `acceleration_structure_sizes`：AS 大小、构建 scratch、refit scratch；
  - 命令 `build_acceleration_structure`（BLAS/TLAS 都用它，`update` 字段做 refit，可以原地）、`copy_acceleration_structure`（含 compact）、`write_compacted_sizes`，可以录在 Graphics 和 Compute 队列的 list 上；
  - AS 的 bindless 槽位（`AccelerationStructure::index`），以及 `Stage`/`Access` 里的 AS 构建、拷贝、postbuild 和读取；
  - 实例描述 `AccelerationStructureInstance`（64 字节，对齐 `INSTANCE_ALIGNMENT`）和 BLAS 引用值 `AccelerationStructure::instance_address`。
- **全局内存 barrier** `CommandList::memory_barrier(before, after)`（§9.4）：不指名资源，两边都是 `BufferState`（stage + access）。它不改 layout 或状态，所以 access 只能是 `MEMORY_BARRIER_ACCESS` 里的（UAV、AS 读、AS 写）或 `NONE`，别的冒险仍然用 buffer/texture barrier。D3D12 enhanced 是 global barrier，legacy 是不指名资源的 UAV barrier；Copy 队列不能用。

### 11.4 实现顺序

R1（0008 §8）的前七步就是本文，每步单独提交、带测试：

| 步 | 内容 |
|---|---|
| R1a | 单队列的核心：声明、句柄版本、剔除、依赖边、usage 推导、barrier 推导、瞬态别名、拓扑键和编译缓存；用录制后端做 CPU 测试 |
| R1b | `FrameContext`（§7）：执行 plan、物理资源、上传 ring、退休队列；导入 swapchain、`present`、`readback`；跨帧状态表（§2.3） |
| R1c | 历史资源和持久资源（§8） |
| R1d | 异步计算和 Copy 队列（§4.3、§4.4） |
| R1e | 并行录制（§6）、计时和标记、图导出、资源捕获（§10） |
| R1f | §11.2 的第 1 条验收：迁移前向渲染器和 framework |
| R1g | §11.2 的第 2 条验收：后处理链 |

R1a 做完时（2026-10-18）定下的细节：
- **后端是泛型参数**：`RenderGraph<'g, B: Backend = Rhi>`。`Backend` 只给出导入对象的类型、它们的描述，以及 placed 资源的大小和对齐。CPU 测试用 `recording::Recording`：导入对象只是描述，内存大小按固定公式算。执行闭包的类型是 `FnOnce(&mut PassContext<'_, B>)`；`PassContext` 在 R1b 补上内容。
- 图的所有分配都在帧 arena（`&Arena` 是 `Allocator`）里，包括执行闭包的 `Box`。
- **写返回整个资源的新版本句柄**：`hzb = b.write_texture(hzb.mip(i), ..)` 之后，`hzb` 仍然覆盖所有 mip。版本按资源计，写任何一个子资源都会让旧句柄失效。读返回传进来的句柄。
- **剔除**比"写导入资源的 pass 都是根"更精确：导入资源（非只读）的内容在帧末算作被读取，再从后往前按子资源传播"之后还有人读"。所以被后面的整体覆盖（clear、DontCare）盖掉的写会被剔除，这同样不改变可观察结果。
- **访问的三种效果**：只读；读改写（storage 写、`CopyDst`、`Clear`、load 为 `Load` 的附件）；整体覆盖（load 为 `Clear` 或 `DontCare` 的附件）。整体覆盖和瞬态资源的第一次使用丢弃旧内容，before 是 `Undefined`，stage 取上一次使用的 stage。
- **连续读**：layout 相同的连续读合成一个状态；读的 stage 已经被覆盖时（例如 `ALL_SHADING` 覆盖 compute）不加 barrier。合并时加宽这一组开头的 barrier 的 after。
- **瞬态资源第一次使用**：before 的 stage 是这块内存之前的占用者最后一次使用的 stage（别名的执行依赖）；没有占用者的 buffer 不加 barrier。`undefined_from` 取这个资源在 plan 末尾的 layout，因为同一份 plan 再执行时，这个 placed 纹理就停在那里；新建的 placed 纹理在 `Common`，由执行方替换（R1b）。
- buffer 的 barrier 总是整个 buffer（RHI 的 buffer barrier 没有范围）；`buf.range` 只决定视图。
- Development 校验另外加了：读一个还没被写过的瞬态资源；导入资源的访问超出它创建时的 usage；pass 类型不允许的访问（compute pass 里的 `PixelRead` 等）；同一 pass 里两次读需要不同的 layout。

## 12. 测试

测试放在各 crate 的 `tests/` 里。

- **CPU 测试**（平台无关，不需要 GPU）：
  - 构造图，断言剔除结果、依赖边；
  - 别名：区间不重叠、偏移满足对齐、heap 大小等于峰值；
  - barrier 序列：与手写的期望逐条比较，包括子资源范围和读状态合并；
  - 异步调度：提前和推迟的位置、同步点合并、资源生命周期延长；
  - 分段：同步点必然是段边界；
  - 缓存：拓扑相同命中，改了任一访问或描述就失效；
  - 句柄版本：读写旧版本时 panic，消息里有 pass 名和资源名；
  - 历史资源：交换、`prev` 为 `None` 的三种情况、退休。
  - 跨帧的跨队列等待：上一帧最后一次访问在另一个队列上时插入等待，在同一队列上时不插；
  - 加速结构：AS 组的访问不随组内 BLAS 数量改变拓扑键；ray query 读 TLAS 时隐式加上对组的读；TLAS 的 `capacity` 取整与缩小规则。

  这些测试用一个不发 GPU 命令的录制后端，或者直接断言 `CompiledPlan`。
- **GPU 测试**（平台相关，在真实硬件上跑，不用 WARP 之类的软件设备）：
  - 别名正确性：两个别名资源先后写入不同图案，读回后各自正确；
  - 异步计算的结果和强制同步执行逐字节一致；
  - legacy barrier 路径和 enhanced barrier 路径在同一台机器上结果一致；
  - 并行录制和单段录制结果一致；
  - §11.2 的两条验收。
  - 加速结构（RT 实现时）：
    - 用 GPU 写的实例描述建 TLAS，ray query 的命中结果（实例下标、T）和 CPU 端用 marlin-math 的 `Ray` 求交的结果一致（射线避开三角形边缘）；
    - compaction 前后，同一组射线的命中结果逐个相同；
    - refit 之后和完整重建的命中结果相同。

## 13. 待定

- 瞬态 heap 收缩的水位和帧数阈值。
- 提交流水化（§6.1）的收益要实测之后再定。
- Metal 后端落地时，TBDR 上的 raster pass 合并和 memoryless 附件。
- split barrier：enhanced barrier 没有，legacy 有；目前不用。
- 跨帧的异步重叠（本帧的异步计算和下一帧的图形重叠）。
- 以 GPU 计时为依据的异步调度提示。
- 加速结构：瞬态 TLAS 之间能否别名；BLAS 构建预算和动态 BLAS 重建间隔的默认值；opacity micromap 以后是否加入 BLAS 构建；Metal 的实例描述格式和 BLAS 引用方式。
- 写实例描述的 shader 标准库函数（§9.6）的具体形状，属于标准库清单（0004 §17）。
