# 0002 — Shader 接口契约

状态：草案 · 2026-10-16

这份文档写的是 RHI 对编译好的 shader 有哪些要求。Marlin 用自研 shader 语言（语言设计另定），编译器用 Rust 写，是纯库，直接输出 DXIL 和 SPIR-V。这里的约定就是那个编译器要对准的目标。RHI 不编译源码，只接受字节码（`marlin_rhi::ShaderModule { code, entry }`）。

## 1. 字节码

| Backend | 格式 | 要求 |
|---|---|---|
| D3D12 | DXIL | DXBC 容器，以 `DXBC` 开头，否则 `InvalidInput`；shader model ≥ 6.6；容器带有效的验证哈希（D3D12 拒绝未签名的 DXIL；改用 bypass 哈希之前，要先确认目标 runtime 接受它的条件）；嵌入的 root signature 不会被使用 |
| Vulkan | SPIR-V | backend 尚未实现，具体要求见 §6 |
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
- 世界坐标系（来自 marlin-math）：右手系，Y 向上，相机看向 −Z，列向量，矩阵列主序存储。shader 里的 `mul(M, v)` 对应列向量约定：HLSL 的 `float4x4` 从 buffer 读入时默认按 column-major 打包，跟 marlin-math 的存储一致。

## 5. 当前的占位实现

自研编译器还没有，现在的 shader 是**占位品**：用 HLSL 写，拿 Windows SDK 的 dxc 手工编译一次，只提交 `.dxil`。dxc 和 HLSL 都不进任何构建或管线。

| 位置 | 用途 |
|---|---|
| `runtime/platform/rhi/tests/shaders/` | RHI 测试：compute 写 bindless UAV；绘制测试覆盖 vertex pulling、uniform、纹理、采样器和 reverse-Z；`solid` 覆盖多重采样、混合、保守光栅、深度范围、VRS 和查询；`sample` 覆盖 min/max 采样、stencil 视图、多重采样读取和 cube array；`rayquery` 覆盖加速结构和 inline ray query |
| `runtime/engine/render/shaders/` | marlin-render 前向 pass |

测试通过 `tests/support/mod.rs` 的 `shader(name, stage)` 查找 shader。等编译器就绪，计划换成一个测试支持 crate：
- 它的 `build.rs` 把编译器库作为 `[build-dependencies]` 调用，编译 `tests/shaders/**` 下的全部 Marlin shader，把结果写进 `OUT_DIR`，再生成同样形状的 `include_bytes!` 查找表。
- shader 编译错误会让构建失败。
- 增量编译依靠 `rerun-if-changed`。
- workspace 设置 `[profile.dev.build-override] opt-level = 3`，让编译器在构建期以优化速度运行。

到时候 cooker 也调用同一个库。

## 6. 待定（Vulkan / 主机 backend 落地时确定）

- SPIR-V 绑定映射。预期方案：
  - 资源表映射为 set 0 binding 0，用 `VK_EXT_mutable_descriptor_type` 实现的统一资源数组。
  - 采样器表映射为 set 0 binding 1 的 sampler 数组。
  - push constants 映射为 `push_constant` block。
  - 另一个候选是 `VK_EXT_descriptor_buffer`。
- 主机平台的对应方式。
- （mesh shader、ray query、wave 大小、16 位与 64 位运算、重心坐标等能力已经在 `Features` 里报告。）
