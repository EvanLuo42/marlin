# 0016 — RHI Vulkan backend

状态：草案 · 2026-10-17

前置：
- [0002](0002-shader-interface.md)：shader 接口契约（本文 §4 定下它 §6 的 SPIR-V 部分）
- [0014](0014-testing.md)：测试体系

## 0. 决定摘要

| 问题 | 决定 | § |
|---|---|---|
| 哪些平台用 Vulkan | Windows 和 Linux **立刻**切到 Vulkan；D3D12 只留给 Xbox（代码暂留，之后删掉，有 Xbox 时再加回） | 1 |
| API 绑定 | 手写 FFI（同 d3d12/ffi.rs），只绑用到的部分；运行时用 `marlin_pal::ext::Dylib` 加载 `vulkan-1.dll` / `libvulkan.so.1`，缺了就是 `Unsupported`；no_std，无第三方依赖 | 2 |
| 基线 | Vulkan **1.4** instance/device ＋ 必需扩展 `VK_EXT_descriptor_heap`、`VK_KHR_shader_untyped_pointers`（shader 通过无类型指针访问 heap）、`VK_EXT_mesh_shader`、`VK_EXT_device_generated_commands`；其余能力按 `Features` 可选 | 3 |
| bindless | **descriptor heap，不用 descriptor set 和 pipeline layout**：一个资源 heap、一个 sampler heap，每个 command list 开头绑定一次；push constants 走 push data | 4 |
| shader 侧绑定 | 原生 `SPV_EXT_descriptor_heap`（自研编译器产出这种），以及 set 0 的固定映射（binding 0 资源、1 sampler、2 push data）；自研编译器只经 binding 0 取加速结构 | 4.2 |
| 屏障 | synchronization2，`Stage`/`Access`/`Layout` 一一映射；`Features::enhanced_barriers = true` | 5 |
| 交换链依赖 | 不改 API：acquire 的信号量挂到 swapchain 所在队列，下一次 `submit` 自动等待；present 前空提交一次发信号 | 7 |
| 测试 | 同一套 RHI 测试；只对某个图形 API 有意义的 case 保留，按 `marlin_rhi::GRAPHICS_API` 跳过；golden 目录按 API 名 | 9 |

## 1. 平台与 backend

- `backend/mod.rs` 是唯一按平台选 backend 的地方：`windows`、`linux` → `vulkan`；`xbox` → `d3d12`（+ `platforms/xbox/rhi`）；`macos` 仍是 stub（以后 Metal）。
- 平台名不决定任何 backend 事实。需要知道 backend 的代码用 `marlin_rhi::GRAPHICS_API`（`GraphicsApi::Vulkan` 等）和 `marlin_rhi::SHADER_FORMAT`。
- D3D12 代码在桌面上不再编译。不再为它投入；删除时一并删掉只为它存在的公共 API（见 §10）。

## 2. 代码结构

`runtime/platform/rhi/src/backend/vulkan/`：

| 文件 | 内容 | 负责 |
|---|---|---|
| `mod.rs` | `Backend`、`Types`、contract 的各个 `impl`（多数只转发到下面的文件）、错误映射 | 核心 |
| `ffi/` | `vk.spec` 列出要用的结构体、函数类型、常量；`gen.awk` 从 SDK 头文件生成 `generated.rs`（不手改，rustfmt 跳过）；`mod.rs` 是手写部分（回调、函数表 `EntryFns` / `InstanceFns` / `DeviceFns`） | 各区在 `vk.spec` 和函数表里自己的区块追加；`generated.rs` 冲突时合并 `vk.spec` 后重新生成 |
| `instance.rs` | 加载 loader 库、instance、validation layer 与 GPU-AV、adapter 枚举 | 核心 |
| `device.rs` | 选 adapter、基线检查、扩展与特性、队列族、`Features`、`Shared` | 核心 |
| `memory.rs` | memory type 选择、`Memory` 分配、`Heap` | 核心 |
| `descriptor.rs` | 资源/sampler heap、槽位分配、写描述符、set 0 映射 | 核心 |
| `queue.rs` | 队列、timeline 信号量 fence、`submit`、`wait_idle`、待等待信号量 | 核心 |
| `command.rs` | pool、list、begin/end（绑定 heap、清零 push data）、屏障与布局映射、push constants、dispatch | 核心 |
| `resource.rs` | buffer 与 buffer 视图（核心）；texture、texture 视图、RTV/DSV、sampler、placed texture、sparse、`format_support` | 核心 + A 资源 |
| `copy.rs` | buffer 拷贝与 fill（核心）；texture 拷贝、resolve、clear | 核心 + A |
| `pipeline.rs` | shader stage 与映射、compute pipeline、pipeline cache（核心）；graphics、mesh pipeline | 核心 + B 管线与绘制 |
| `draw.rs` | render pass（dynamic rendering）、动态状态、draw、mesh dispatch、indirect（DGC）；`DrawState` | B |
| `query.rs` | query pool、timestamp、occlusion、pipeline statistics、predication、时钟校准、marker；`QueryState` | C 查询、光追与诊断 |
| `raytracing.rs` | 加速结构 | C |
| `residency.rs` | 内存预算、驻留优先级 | C |
| `diag.rs` | validation 计数、device lost、调试名、native 句柄（核心）；调试事件/标记、crash 报告 | 核心 + C |
| `swapchain.rs` | surface、交换链、present、HDR、输出、`tearing` | D 交换链与互操作 |
| `share.rs` | 外部内存/信号量导出导入、native 包装 | D |

骨架里每个转发函数都已存在，未实现的函数体是 `todo!("vulkan: ...")`，负责人只改自己的文件。各区的录制状态放在自己文件的 `DrawState` / `QueryState` 里，`CommandList` 每次 `begin` 重置它们。

## 3. 基线与特性

**必需**（缺了 `create_device` 返回 `Unsupported`）：
- `apiVersion >= 1.4`。1.4 核心里用到：timeline semaphore、synchronization2、dynamic rendering、buffer device address、maintenance4–6、`shaderDrawParameters`、`hostQueryReset`、`scalarBlockLayout`（buffer 布局由 shader 决定，§4.2）、`descriptorIndexing` 系列（运行时数组、non-uniform 下标）。
- `VK_EXT_descriptor_heap`（§4）。
- `VK_EXT_mesh_shader`：`taskShader` + `meshShader`。
- `VK_EXT_device_generated_commands`：indirect 命令写 `INDIRECT_CONSTANT_WORD`（§6）。
- 有 surface 时 `VK_KHR_swapchain`。

**可选**（查到就开，写进 `Features`）：

| `Features` | Vulkan |
|---|---|
| `ray_query` | `VK_KHR_acceleration_structure` + `VK_KHR_ray_query` |
| `variable_rate_shading` / `shading_rate_tile_size` | `VK_KHR_fragment_shading_rate`（pipeline/primitive/attachment 三级） |
| `conservative_rasterization` | `VK_EXT_conservative_rasterization` |
| `depth_bounds` | `depthBounds` |
| `barycentrics` | `VK_KHR_fragment_shader_barycentric` |
| `sampler_min_max` | 1.2 `samplerFilterMinmax` |
| `shader_float16_int16` / `shader_int64` | 核心特性位 `shaderFloat16` + `shaderInt16` / `shaderInt64` |
| `atomic_int64` | 1.2 `shaderBufferInt64Atomics` + `shaderSharedInt64Atomics` |
| `texture_atomic_int64` | `VK_EXT_shader_image_atomic_int64` 的 `shaderImageInt64Atomics` |
| `sparse_resources` / `sparse_volumes` | `sparseBinding` + `sparseResidencyBuffer` + `sparseResidencyImage2D` / `Image3D` |
| `buffer_markers` | `VK_AMD_buffer_marker`（NVIDIA 也有） |
| `pipeline_statistics` | `pipelineStatisticsQuery` |
| `gpu_upload_heap` | 有 `DEVICE_LOCAL | HOST_VISIBLE` 且覆盖大部分显存的 memory type（resizable BAR） |
| `copy_queue_timestamps` | 传输队列 `timestampValidBits > 0` |
| `mixed_heaps` | 恒为 true（Vulkan 内存不分 buffer/texture heap 类型） |
| `wave_size_range` | subgroup size control 的 min/max |
| `tearing` | surface 支持 `IMMEDIATE` |
| 其他扩展 | `VK_EXT_memory_budget`、`VK_EXT_pageable_device_local_memory`、`VK_EXT_calibrated_timestamps`/`KHR`、`VK_EXT_device_fault`、`VK_NV_device_diagnostic_checkpoints`、`VK_EXT_conditional_rendering`、`VK_EXT_hdr_metadata`、`VK_EXT_swapchain_colorspace`、外部内存/信号量（`_win32` / `_fd`） |

64 位整数（2026-10-18，visbuffer 渲染器要用，0009 §4.3）：
- `shader_int64`、`atomic_int64`、`texture_atomic_int64` 分别对应 shader 的 capability `Int64`、`Int64Atomics`、`Int64TextureAtomics`（0004 §3.1、§12.3）。`vulkan-1.4` target 把三个都当作可选 capability，设备没有就跳过，和 `RayQuery` 一样。
- 特性链里查到就启用；`VK_EXT_shader_image_atomic_int64` 也是查到就开。
- `Format::R64Uint` 是 `VK_FORMAT_R64_UINT`，用作 visbuffer：只做存储纹理、拷贝和清除。有 `shaderImageInt64Atomics` 时，Vulkan 保证这个格式能做存储图像和原子操作。shader 给 64 位存储图像写明格式（`R64ui`），所以 `format_support` 不看它的无格式读写位。
- D3D12 没有 64 位格式，`R64Uint` 映射到 `R32G32_UINT`，这是 64 位 typed 原子操作要求的视图格式。

adapter：`AdapterPreference::HighPerformance` 选独显优先、然后显存最大；`Software` 选 `CPU` 类型（lavapipe），测试不用它。adapter 不满足基线就跳过（例如本机 AMD 核显只有 1.3）。

## 4. Bindless：descriptor heap

### 4.1 heap

- **资源 heap**：一个 device-local、host 可写的 buffer（`DESCRIPTOR_HEAP` usage + device address），`MAX_BINDLESS_RESOURCES` 个槽。槽宽 `stride = max(imageDescriptorSize, bufferDescriptorSize)` 向上对齐到两者对齐的较大值，之后是驱动要的 reserved range（`minResourceHeapReservedRange`）。
- **sampler heap**：同样，`MAX_BINDLESS_SAMPLERS` 个槽，槽宽 `samplerDescriptorSize`（对齐后），加 `minSamplerHeapReservedRange`。
- 视图、sampler、加速结构创建时从空闲表拿槽，用 `vkWriteResourceDescriptorsEXT` / `vkWriteSamplerDescriptorsEXT` 直接写进映射内存；drop 时归还。`ShaderView::index()` 就是槽号。
- 每个 command list `begin` 时 `vkCmdBindResourceHeapEXT` + `vkCmdBindSamplerHeapEXT`，之后不再变。
- Vulkan 的 image 描述符带 layout：可写视图用 `GENERAL`，只读视图用 `SHADER_READ_ONLY_OPTIMAL`（深度只读用 `DEPTH_STENCIL_READ_ONLY_OPTIMAL`）；`Layout` 到 Vulkan 的映射（§5）要保证 shader 访问时布局和描述符一致。

### 4.2 shader 侧

两种 shader 都接受，backend 每次建 pipeline 都附上同一份映射（`VkShaderDescriptorSetAndBindingMappingInfoEXT`），原生 heap 的 shader 不受它影响：

1. **原生 `SPV_EXT_descriptor_heap`**：shader 直接用 `ResourceHeapEXT` / `SamplerHeapEXT`，下标即槽号（字节偏移 = 槽号 × 槽宽）。槽宽由 shader 用 `OpConstantSizeOfEXT` 算出：`max(image, buffer)` 的描述符大小，sampler heap 是 sampler 的大小（2026-10-17 定，0002 §6）。所以 backend 的槽宽正好是 `max(imageDescriptorSize, bufferDescriptorSize)`，sampler 的槽宽正好是 `samplerDescriptorSize`。这两个值要是不是各自对齐的整数倍，就不支持这个 device（2026-10-18 起，`descriptor.rs`）。push constants 用 `PushConstant` 存储类，读的是 push data。shader 用到无类型指针，所以基线要求 `VK_KHR_shader_untyped_pointers` 和 `shaderUntypedPointers`。
2. **set 0 映射**（dxc 的"emulated heap"形式）：自研编译器只用 binding 0 取加速结构。从 `ResourceHeapEXT` 直接 load 出来的加速结构，在 NVIDIA 驱动上做 ray query 什么也找不到，validation 也不报错（2026-10-18，RTX 5060）；而经 binding 0 读同一个槽是对的。

| SPIR-V | 映射到 | `VkDescriptorMappingSourceEXT` |
|---|---|---|
| set 0 binding 0（所有资源类型的运行时数组，互相 alias） | 资源 heap，偏移 0，数组步长 = 槽宽 | `HEAP_WITH_CONSTANT_OFFSET` |
| set 0 binding 1（sampler 数组） | sampler heap，偏移 0，步长 = sampler 槽宽 | `HEAP_WITH_CONSTANT_OFFSET` |
| set 0 binding 2（uniform block） | push data 偏移 0 | `PUSH_DATA` |

buffer 成员的 offset 由 shader 自己声明（SPIR-V 的 `Offset` 装饰），RHI 不规定布局，只打开 `scalarBlockLayout`，让任意自然对齐的 offset 都合法。布局规则归自研 shader 编译器定（它产出的 offset 要和手写的 Rust 结构体一致，用反射检查），见 §11。Marlin shader 是标量布局：紧密排列、按标量自然对齐，和 `#[repr(C)]` 的结构体一致。

**已知的驱动问题**（2026-10-18，RTX 5060，经 descriptor heap 的 buffer 指针，storage buffer 和 uniform 都有）：布局类型里**类型是 struct 或 struct 数组的成员**，它的 `Offset` 装饰被忽略，读出来都在偏移 0；向量、标量、元素不是 struct 的数组，以及内层 struct 自己的成员 offset、数组步长都正确。SPIR-V 是合法的（验证层带 spirv-val 也不报错），把多段 `OpAccessChain` 合成一段也没有用。

编译器绕开了它（2026-10-18，0005 §4.4 阶段 6）：SPIR-V 后端在布局类型里把嵌套 struct 展开成外层的成员，struct 数组成员写成 32 位字数组、按字下标读写，所以布局类型里不再有这两种成员。shader 里照常写嵌套 struct（`InstanceTransforms` 恢复成两个 `Rows3x4` 字段）。回归测试是 marlin-rhi 的 GPU 测试 `shader_layout`（`Test.NestedLayout`）：从 buffer 和 uniform 逐叶子、逐成员、整体读嵌套 struct 和 struct 数组（含动态下标），再整体、逐成员写回。驱动修好之后可以去掉这层处理，那时先让这个测试在去掉之后通过。

### 4.3 push constants

- `MAX_PUSH_CONSTANTS` = 32 字 = 128 字节（`maxPushDataSize` 至少 128）。`push_constants` → `vkCmdPushDataEXT`；graphics 和 compute 共用一份 push data，所以"写进最近绑定的那类 pipeline"在 Vulkan 上自然成立。`begin` 时清零。
- pipeline 不建 `VkPipelineLayout`，创建时带 `VK_PIPELINE_CREATE_2_DESCRIPTOR_HEAP_BIT_EXT`。

## 5. 屏障与布局

- `Stage` → `VkPipelineStageFlags2`、`Access` → `VkAccessFlags2`、`Layout` → `VkImageLayout`，查表一一对应；`vkCmdPipelineBarrier2`。
- `Layout::Common` → `GENERAL`；`Present` → `PRESENT_SRC_KHR`；`Undefined` → `UNDEFINED`。`TextureBarrier::undefined_from` 在 Vulkan 上就是 `oldLayout = UNDEFINED`（不需要真实布局）。
- buffer 屏障直接映射；`memory_barrier` 是全局 `VkMemoryBarrier2`。
- 队列族所有权：资源一律 `CONCURRENT` 共享模式（graphics/compute/transfer 三个族），不做 ownership transfer。

## 6. 命令

- render pass → dynamic rendering（`vkCmdBeginRendering`），load/store op 直接映射；viewport 用负高度翻转（0002 §4）。
- 管线状态大部分静态，viewport/scissor/stencil ref/blend constants/depth bounds/shading rate 为动态状态。
- `draw*_indirect` / `dispatch*_indirect`：indirect 参数结构的第一个字是 `constant`。用 DGC（`VK_EXT_device_generated_commands`）：indirect commands layout = push data token（写 `INDIRECT_CONSTANT_WORD`）+ draw/dispatch token；有 count buffer 的用 `maxSequenceCount` + count。preprocess buffer 由 backend 按需分配并缓存。
  - `IndirectArgs::constant` 为 false 时（render graph 的 indirect 命令都是）不送 constant，改用原生命令：`vkCmdDispatchIndirect`（每条记录一次）、`vkCmdDraw[Indexed]Indirect[Count]`、`vkCmdDrawMeshTasksIndirect[Count]EXT`，从记录的第二个字读、步长不变；只有带 count 的 dispatch 没有原生命令，仍走 DGC。DGC 每条命令在 GPU 上要 10–20 µs（preprocess 加执行），CPU 上每次还要查一遍 preprocess 内存需求；`samples/basic` 每帧约 180 条 indirect 命令，换成原生以后 GPU 一帧从 2.0 ms 降到 1.0 ms（2026-10-18）。为此 baseline 加上 `multiDrawIndirect`、`drawIndirectFirstInstance`、`drawIndirectCount`。
- 查询：timestamp / occlusion / pipeline statistics 映射到 `VkQueryPool`；pipeline statistics 结果按 `PipelineStatistics` 字段顺序写出（Vulkan 位序与它几乎一致，mesh 图元数要额外的 `MESH_PRIMITIVES_GENERATED` 查询）。
- predication → `VK_EXT_conditional_rendering`（可选）。
- 调试事件/标记 → `VK_EXT_debug_utils` 的 label；`write_marker` → `vkCmdWriteBufferMarker2AMD`，没有就 fill。

## 7. 队列、fence、交换链

- fence = timeline semaphore（可导出）。`wait_for_fences` = `vkWaitSemaphores`（ANY / ALL）。
- 队列：graphics 族；有"只 compute"族就是 compute 队列，有"只 transfer"族就是 copy 队列；没有就是 `None`。
- `submit` = `vkQueueSubmit2`；队列对象上有一个"待等待的二进制信号量"列表，`submit` 取走并等待（交换链 acquire 用，§7.1）。
- `wait_idle` = `vkQueueWaitIdle`。
- 新纹理要从 `UNDEFINED` 转到 `GENERAL`。backend 把这些转换攒成一批，在下一次 `submit` 之前提交，随后任何队列上的每次提交都要等这批完成。
- 这批转换跑在 backend 自己的队列上：在用到的队列族里，取第一个至少有两个队列的族（先 graphics，再 compute，再 copy），用它的第二个队列。资源是 `CONCURRENT` 的，所以换队列不需要所有权转移。
- 要是转换跟在应用队列上，就会排在那个队列已有的工作后面，而所有队列又都等它，结果互相等待。例子：graphics 提交等 compute 的 fence，这时新建一张纹理，graphics 再提交一次（把转换带上），之后 compute 的提交也得等这次转换。这会死锁（2026-10-18 发现，回归测试是 `queues::new_textures_do_not_tie_queues_together`）。
- 没有空闲队列的设备还按原来的做法，把转换放在提交的那个队列上。
- sparse bind 仍然在调用方指定的队列上执行，因为它和那个队列上的工作有先后约定。

### 7.1 交换链

- surface：`SurfaceHandle::Win32` → `VK_KHR_win32_surface`。Wayland/X11 变体随 L1 的 Linux 窗口加入。
- `acquire`：`vkAcquireNextImageKHR` 带一个二进制信号量，挂到交换链队列的待等待列表；帧延迟用 `VK_KHR_present_wait2`（没有就等上一轮该图像的 present fence），超时 `TimedOut`。
- `present`：先对该队列空提交一次，发一个该图像专属的二进制信号量（发信号的第一同步域包含之前提交的所有命令），再 `vkQueuePresentKHR` 等它。
- `resize` 等 GPU 空闲后重建。`OUT_OF_DATE` / `SUBOPTIMAL` 映射到 `PresentStatus`。

## 8. 诊断

- `InstanceDesc::validation`：启用 `VK_LAYER_KHRONOS_validation` + debug utils messenger，消息进 `marlin_diag`，错误计数给 `Device::validation_errors`。没装 layer 就跳过并 warn。
- 开启验证时也打开验证层的 debug printf（`printf_enable`，`printf_to_stdout` 关闭）。着色器用 `Debug.Print`、`Debug.Assert` 写的内容（`NonSemantic.DebugPrintf`）以 info 级、`shader: ...` 的形式进入 `marlin_diag`，不算验证错误；其余 info 级消息只记为 debug（2026-10-18）。
- `gpu_validation`：用 `VK_EXT_layer_settings` 打开 GPU-AV。
- `crash_diagnostics`：`VK_EXT_device_fault`（故障地址、厂商信息）+ checkpoint / buffer marker 面包屑；第一次报告 `DeviceLost` 时写日志。
- `set_name` → `vkSetDebugUtilsObjectNameEXT`。

## 9. 测试

- 所有 RHI 测试与图形 API 无关，同一套在 Vulkan 上跑。只对某个 API 有意义的 case（例如"D3D12 一定有专用 compute/copy 队列"）**保留**，用 `#[gpu_test(api = d3d12)]` 代替 `#[test]`：其他 API 上按 `marlin_rhi::GRAPHICS_API` 在运行期 `skip!`。
- `marlin_gpu_test_support::devices()` 返回每条屏障路径一个设备：Vulkan 只有一条，所以一个；D3D12 是 enhanced + legacy 两个。测试遍历切片，不写死下标。
- 测试 shader 是 Marlin shader，在运行时编译成 SPIR-V（[0002 §5](0002-shader-interface.md)）。
- golden 目录 `tests/data/golden/<api>/`，`<api>` 是 `GraphicsApi::name()`（`vulkan`、`d3d12`）。Vulkan 的参考图第一次用 `MARLIN_BLESS=1` 生成，人工看过再提交。
- validation 开着跑；测试结束检查 `validation_errors() == 0` 的地方照旧。

## 10. 并行实施计划

| 阶段 | 内容 | 谁 |
|---|---|---|
| 0 | 本文；`GraphicsApi`；切换 backend；Vulkan 骨架（全部转发函数，返回 `Unsupported`） | 主 session |
| 1a | 测试与 API 去 D3D12 化（§9 全部）＋ 所有占位 shader 的 `.spv` | 并行 session T |
| 1b | Vulkan 核心：loader、instance、validation、adapter、device、队列、fence、内存、buffer、buffer 视图、descriptor heap、command list、屏障、submit、buffer 拷贝/fill、compute pipeline、dispatch。完成标志：`fill` compute 写 buffer 回读通过 | 主 session |
| 2 | 四个并行 session，各自只改 §2 表里自己的文件（`ffi.rs` 追加自己的区块）：A 资源、B 管线与绘制、C 查询/光追/诊断、D 交换链与互操作 | 并行 session A–D |
| 3 | 按顺序 rebase 合并到本分支；全量 GPU 测试（`cargo xtask test --tier gpu`、`desktop`）在真机上跑通；修 validation 报错 | 主 session |
| 4 | （之后）删 D3D12 backend 及只为它存在的公共 API（`DeviceDesc::disable_enhanced_barriers` 等） | 另行 |

## 11. 待定

- buffer 布局规则（shader 编译器决定；它产出的 `Offset` 和手写 Rust 结构体靠反射对齐）。
- Linux 的 Wayland/X11 surface（等 L1 Linux 窗口）。
- 内存子分配（现在 committed = 每资源一次 `vkAllocateMemory`，测试规模够用；Windows 上 `maxMemoryAllocationCount` 通常 4096）。
