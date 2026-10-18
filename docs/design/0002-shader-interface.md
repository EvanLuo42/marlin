# 0002 — Shader 接口契约

状态：草案 · 2026-10-16

这份文档写的是 RHI 对编译好的 shader 有哪些要求。Marlin 用自研 shader 语言（语言设计另定），编译器用 Rust 写，是纯库，直接输出 DXIL 和 SPIR-V。这里的约定就是那个编译器要对准的目标。RHI 不编译源码，只接受字节码（`marlin_rhi::ShaderModule { code, entry }`）。

## 1. 字节码

| Backend | 格式 | 要求 |
|---|---|---|
| D3D12 | DXIL | DXBC 容器，以 `DXBC` 开头，否则 `InvalidInput`；shader model ≥ 6.6；容器带有效的验证哈希（D3D12 拒绝未签名的 DXIL；改用 bypass 哈希之前，要先确认目标 runtime 接受它的条件）；嵌入的 root signature 不会被使用 |
| Vulkan | SPIR-V | Vulkan 1.3 环境（SPIR-V ≤ 1.6），以 magic `0x07230203` 开头，否则 `InvalidInput`；资源表、采样器表、push constants 的绑定见 §6 |
| 主机 | 各平台格式 | 随各平台 backend 定义 |

- `entry` 是入口名。SPIR-V 模块和 DXIL library 可以有多个入口；单入口 DXIL 在 D3D12 下会忽略它。
- 每个 stage 一个 module：vertex、pixel、compute、mesh、amplification。
  - mesh 和 amplification 是**必需的基线**。
  - 光追只用 inline ray query（`RayQuery`），任何 stage 都能用，不引入 RT pipeline stage（没有 raygen/miss/hit shader，也没有 shader table）。需要 `Features::ray_query`。

## 2. 绑定模型：只有 bindless

所有 pipeline 共用一个布局，没有 descriptor set、root table 或 vertex input layout。

### 2.1 Push constants

- 最多 `MAX_PUSH_CONSTANTS` = 32 个 32 位字（128 字节）。Vulkan 保证的最小 push constant 也是这么大。
- D3D12：root constants，位于 `b0, space0`，对所有 stage 可见。HLSL 写法是 `ConstantBuffer<Push> push : register(b0);`。
- Vulkan：push data（`VK_EXT_descriptor_heap`）；SPIR-V 里是 `PushConstant` 存储类，或映射到 push data 的 set 0 binding 2 uniform block（§6）。
- 写入接口是 `CommandList::push_constants(offset_words, &[u32])`，写进最近绑定的那类 pipeline（graphics 或 compute）。每次 `begin` 都会把 push constants 清零。
- 用法：放资源表下标、少量标量（深度、实例基址等）。大块数据放 uniform/structured buffer，用下标去取。

### 2.2 资源表与采样器表

| 表 | 内容 | D3D12 / SM 6.6 | 下标来源 |
|---|---|---|---|
| 资源表（`MAX_BINDLESS_RESOURCES` = 262144 槽） | buffer 与 texture 视图 | `ResourceDescriptorHeap[i]` | `ShaderView::index()` |
| 采样器表（`MAX_BINDLESS_SAMPLERS` = 2048 槽） | sampler | `SamplerDescriptorHeap[i]` | `Sampler::index()` |

加速结构也占资源表的槽：`AccelerationStructure::index()` 处是一个 `RaytracingAccelerationStructure`（top-level 结构给 ray query 用）。

- 视图 drop 时槽位回收，随即可能被复用。shader 只能访问在 GPU 执行期间仍然存活的视图（`Queue::submit` 的 safety 条款）。
- 下标是普通 `u32`，可以放在 push constants 里，也可以放在 buffer 里（材质表、实例数据）。
- 跨 wave 不一致的下标要加 `NonUniformResourceIndex`，由编译器或作者负责。

### 2.3 视图类型对照

| RHI 视图 | 只读 | 可写 |
|---|---|---|
| `BufferViewKind::Uniform`（offset 256 对齐，≤ 64 KiB） | `ConstantBuffer<T>` | — |
| `BufferViewKind::Structured { stride }` | `StructuredBuffer<T>` | `RWStructuredBuffer<T>` |
| `BufferViewKind::Raw` | `ByteAddressBuffer` | `RWByteAddressBuffer` |
| `TextureViewDimension::D2` | `Texture2D<T>` | `RWTexture2D<T>`（单个 mip） |
| `TextureViewDimension::D2Array` | `Texture2DArray<T>` | `RWTexture2DArray<T>` |
| `TextureViewDimension::Cube` | `TextureCube<T>` | — |
| `TextureViewDimension::CubeArray` | `TextureCubeArray<T>` | — |
| 多重采样纹理的 `D2` / `D2Array` | `Texture2DMS<T>` / `Texture2DMSArray<T>` | — |
| `AccelerationStructure`（graph 里的用法见 0007 §9） | `RaytracingAccelerationStructure` | — |
| `Sampler` | `SamplerState`（`reduction` 为 min/max 时也是它）/ `SamplerComparisonState`（`compare` 有值时） | — |

- 深度纹理的只读视图按深度平面读：`D32Float`、`D32FloatS8` 读成 `R32Float`，`D16Unorm` 读成 `R16Unorm`（都是 `float`）。
- `D32FloatS8` 用 `Format::R8Uint` 建的视图读 stencil 平面：`Texture2D<uint>`，值在 **r** 通道（D3D12 的 stencil 本来在 g，backend 用 component mapping 搬到 r，和 Vulkan 的 stencil aspect 一致）。
- `D2` 视图只能看 layer 0；数组的其他单层用一层的 `D2Array`。

### 2.4 输出与光栅状态相关的语义

- pixel shader 输出 `SV_Target0..7`，对应 `color_targets` 的顺序。
- dual-source blending（`BlendFactor::Src1*`）：只有一个 color target，`SV_Target1` 是第二个源。
- alpha to coverage（`MultisampleState::alpha_to_coverage`）取 `SV_Target0` 的 alpha。
- 每图元着色率：mesh/vertex shader 输出 `SV_ShadingRate`（`VariableRateShading::Image` 级别）；它和 draw 的速率、shading-rate image 怎么合并由 `set_shading_rate` 的两个 combiner 决定。
- shading-rate image 是 `R8Uint` 纹理，每个 texel 管 `Features::shading_rate_tile_size`² 个像素，值是 `ShadingRate::encoded()`：`log2(宽) << 2 | log2(高)`，D3D12 和 Vulkan 编码相同。

## 3. 顶点与实例

- 没有 vertex buffer 和 input layout。vertex shader 自己用 `SV_VertexID` 从 structured/raw buffer 取顶点（vertex pulling）。
- `SV_VertexID` 包含 `draw` 的 `first_vertex`，以及 `draw_indexed` 的 `vertex_offset`（index + base vertex）。
- `SV_InstanceID` 一律从 0 开始，不包含 `first_instance`：D3D12 在 SM 6.8 之前看不到它。需要实例基址就通过 push constants 传进去。
- index buffer 走固定功能（`set_index_buffer`，u16/u32）。

## 4. 坐标与光栅约定

- NDC：x 向右，y 向上，裁剪空间 z ∈ [0, 1]。与 D3D/Metal 一致；Vulkan backend 会用负高度 viewport 翻转。
- 深度用 **reverse-Z**：近平面 = 1，远平面 = 0，比较用 `GreaterEqual`（`DepthState::reverse_z`），清除值为 0。marlin-math 的 `perspective_rh` 等函数直接产出这种投影。
- 正面是屏幕上逆时针（`FrontFace::CounterClockwise`），默认剔除背面。
- viewport 和 scissor 以渲染目标左上角为原点，单位是像素。
- 世界坐标系（来自 marlin-math）：右手系，Y 向上，相机看向 −Z，列向量，矩阵列主序存储。Marlin shader 里的 `M * v` 是列向量乘法；buffer 里的 `float4x4` 按列主序排列，跟 marlin-math 的存储一致。

## 5. 测试 shader 与引擎 shader

占位 HLSL 已经全部删掉（第 10e 步，2026-10-18），现在所有 shader 都是 Marlin shader，仓库里没有 HLSL、DXIL，也没有 dxc 编出的 SPIR-V。

- **测试 shader** 在运行时编译，不走 build.rs（2026-10-17 定）。`marlin_gpu_test_support` 在每个测试二进制里建一个编译器 session，第一次用到某个 entry 时才编译，结果一直留到进程结束。
  - 共用的包 `Test` 在 `tools/gpu_test_support/shaders/`，用 `shader("Fill")` 按 entry 名取，stage 由 entry 的属性决定。
  - 只有一个 crate 用的 shader 放在那个 crate 的 `tests/data/shaders/`，用 `crate_shader!("CookTexture.TexelLoad")` 取。这个包依赖 `Test`。
  - 需要设静态参数时用 `Shader::new(entry).with_const(path, value)`。
  - shader 编不过时测试会 panic，panic 信息里带着编译器的诊断。
  - 不用 GPU 的 `unit` 测试会把 `Test` 包里的每个 entry 都编一遍；设了 `MARLIN_SPIRV_VAL` 时，还会再用 spirv-val 检查一遍。
- **引擎 shader** 由 cooker 编译，作为资产交给引擎，引擎自己不带 shader（2026-10-18 定）。
  - `marlin_render::RendererDesc::shaders` 由创建方传入。
  - 源码在 `content/engine/shaders/`（`Marlin.Engine` 包），`cook_shader` 把它编成库（0003 §4.4）。
  - `framework::run` 按包名从挂载的内容里加载这个库，取 `Marlin.Engine.ForwardVs` / `ForwardPs`。
  - 测试经 `marlin-gpu-test-support` 在运行时编译同一份源码（`shader("Marlin.Engine.ForwardVs")`）；端到端测试则用 cook 出来的包。
- 编译器只产出 SPIR-V。后端格式不是 SPIR-V（D3D12）时没有测试 shader 可用；D3D12 只留给 Xbox（0016）。

## 6. SPIR-V 绑定（Vulkan）

已定，见 [0016 §4](0016-rhi-vulkan.md)：bindless 用 `VK_EXT_descriptor_heap`，不用 descriptor set 和 pipeline layout。shader 两种写法都接受：
- 原生 `SPV_EXT_descriptor_heap`（`ResourceHeapEXT` / `SamplerHeapEXT`，push constants 是 `PushConstant` 存储类）：自研编译器直接产出这种（2026-10-17 定）。资源 heap 的槽宽由 shader 自己算：`max(OpConstantSizeOfEXT(image), OpConstantSizeOfEXT(buffer))`（`OpSpecConstantOp` 的 `UGreaterThan` + `Select`，用 `ArrayStrideIdEXT` 标在 heap 的运行时数组上），sampler heap 的步长是 `OpConstantSizeOfEXT(sampler)`；驱动给出这些大小，编译器不用知道。RHI 的槽宽必须正好是这个最大值（0016 §4.1）。
- set 0 映射：binding 0 资源表、binding 1 采样器表、binding 2 push constants 的 uniform block。自研编译器只用 binding 0 取加速结构，因为从 `ResourceHeapEXT` 直接 load 出来的加速结构，在 NVIDIA 驱动上做 ray query 什么也找不到，validation 却不报错（2026-10-18 在 RTX 5060 上实测）。heap 槽号仍然是 `AccelerationStructure::index()`。

## 7. 待定

- 主机平台的对应方式。
- （mesh shader、ray query、wave 大小、16 位与 64 位运算、重心坐标等能力已经在 `Features` 里报告。）
