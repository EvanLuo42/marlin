# 0004 — 渲染总览：目标、档位、帧流水线与 crate 布局

状态：草案 · 2026-10-16

这是渲染系列文档的入口，场景渲染和 UI 渲染都在范围内。各部分细节分在下面几篇：

| 文档 | 内容 |
|---|---|
| 0004（本文） | 目标、档位、帧流水线、线程模型、crate 布局、实施阶段、测试 |
| [0005](0005-rhi-and-render-graph.md) | RHI 需要补的能力、render graph |
| [0006](0006-scene-renderer.md) | GPU Scene、几何与可见性、阴影、光照各档、半透明、流式加载 |
| [0007](0007-material-system.md) | 材质系统：对齐 UE（Substrate），用文本 shader 代替节点图 |
| [0008](0008-ui-rendering.md) | UI 渲染（参考 Decima UIPainter 的 tile 方案，binning 放到 GPU）、字体与文字 |
| [0009](0009-environment-and-effects.md) | 大气、体积雾、体积云、地形、植被、后处理链、GPU 粒子 |

参考资料：
- Wessel Mast，*UIPainter: Tile-Based Rendering in One Draw Call*（GDC 2024，Guerrilla）
- UE5 的 RDG、Nanite、Substrate、Path Tracer、Mesh Terrain
- Frostbite FrameGraph
- Bitterli 等人的 ReSTIR DI，Ouyang 等人的 ReSTIR GI，Lin 等人的 ReSTIR PT
- NVIDIA SHaRC
- Majercik 等人的 DDGI
- Hillaire 的大气渲染
- Schneider 的 Nubis（Decima 体积云）
- Falcor 的材质与 BSDF 接口

## 0. 决定摘要（来自需求访谈）

| 维度 | 决定 |
|---|---|
| 画质与伸缩 | 一套管线、多个档位。Switch 2 跑低档，PS5/XSX 跑中档，高端 PC 跑高档；另有参考档（编辑器 / 照片模式） |
| 几何 | GPU-driven **Visibility Buffer**：实例剔除 → meshlet 剔除（两阶段 HZB）→ mesh shader 光栅 → 材质分桶 |
| 硬件基线 | **mesh shader 必需**：SM 6.6 + mesh/amplification（DX12 Ultimate 级，Metal 3）。光追作为 ext 提供 |
| 光追 | 只用 inline ray query（不用 RT pipeline，也没有 SBT）。高档和参考档开启 |
| 直接光 | 低/中档：clustered 光源 + VSM；高档：ReSTIR DI；参考档：ReSTIR PT |
| GI | 低档：烘焙探针 + 稀疏 RT 更新；中档：RT 探针 + SHaRC；高档：ReSTIR GI + SHaRC；参考档：ReSTIR PT |
| 阴影 | 虚拟阴影贴图（VSM）；高档的直接光可见性改用光线 |
| 降噪与超分 | 自研 TSR 和 ReSTIR 降噪器。DLSS/FSR/XeSS/PSSR 作为平台 ext 接入，输入契约统一 |
| 材质 | **对齐 UE 的 Substrate**，节点图换成文本 shader（Marlin shader 语言）。MPC、Material Layers、动态实例、非 Surface 域第一版都做 |
| 材质执行 | visbuffer → 按材质分桶 → 每个材质一个 compute resolve → Substrate material buffer → 按复杂度分 tile 计算光照 |
| 命中点着色 | Wavefront：先写命中记录，再按材质分桶，复用同一个材质 resolve kernel |
| 不可 lower 的材质 | 例如 Toon BSDF：消费引擎给的光照样本，同时**必须**提供一个 fallback slab，供光线命中时使用 |
| Permutation | 只编译实际用到的组合；运行时从不编译；开发期由 cookd 按需编译并热重载 |
| 半透明 | 按物体排序前向着色；粒子、头发等密集层可选 MBOIT |
| 材质几何阶段 | 支持 alpha test 和 WPO，位移以后再加 |
| 场景表示 | 渲染侧持久 GPU Scene，模拟侧每帧给出**不可变 delta**（修订 0003 §9 的"全量快照"） |
| 纹理流送 | 普通纹理靠 GPU 反馈驱动 mip 流送；地形和超大纹理用软件虚拟纹理 |
| 网格 LOD | 离散 LOD，每级切 meshlet；格式里预留 cluster DAG 字段 |
| Render graph | 每帧声明式重建，编译结果按拓扑哈希缓存 |
| 帧流水线 | Game N+1 ∥ Render N ∥ GPU N-1，并用 late latch 补偿延迟 |
| 异步计算 | pass 上标注 `async_compute`，跨队列同步由 graph 自动处理；Copy 队列专门用于流送 |
| 并行录制 | 编译后的 pass 序列按代价切段，每段一个 job、一个 command list，按顺序提交 |
| UI | Decima 式 8×8 tile、寄存器内混合，但 **binning 在 GPU 上做**。支持贝塞尔描边、任意填充路径、渐变和图案填充 |
| 文字 | 按精确尺寸光栅化字形进缓存（skyline 打包），世界空间文字用 MSDF；完整 OpenType shaping，自研 |
| UI 缓存 | 控件缓存命令段，GPU 按 tile 哈希跳过没有变化的 tile |
| 游戏 UI | 复用 `ui_core`（core + alloc），逻辑经 VM 绑定 |
| 色彩 | UI 在 sRGB 空间混合；场景在线性 Rec.2020 中计算，用自研 tonemapper，HDR 与 SDR 共用同一条曲线 |
| 环境 | Hillaire 大气、froxel 体积雾、Nubis 式体积云 |
| 地形 | 参考 UE Mesh Terrain：真 3D 网格，由非破坏式修改器栈生成，进入同一条 visbuffer 管线 |
| VFX | 粒子模块写成文本 shader，emitter 是 .masset，编译期把模块融合成 kernel |
| 下一个 backend | Vulkan，之后 Metal |
| 测试 | 确定性的 pass 精确断言；光照用 FLIP 感知误差和参考 PT 对比。全部在真实硬件上跑 |

## 1. 目标与非目标

**目标**

1. **CPU 侧可伸缩。** draw call 的数量与场景复杂度无关。CPU 的渲染成本主要来自"有多少东西变了"，而不是"有多少东西"。
2. **GPU 侧可伸缩。** 同一套管线，从 Switch 2 一直到高端 PC，换档位只换 pass 的实现，或者调参数。图的结构和数据格式不变。
3. **一份材质代码到处运行。** 光栅、光线命中、前向半透明、阴影深度，都从同一份材质源生成入口。只要材质能 lower 成 Substrate slab，就能自动参与所有光照路径，ReSTIR 和 PT 也包括在内。
4. **UI 高性能。** 编辑器在 4K 下画几万个图元时，CPU 录制 < 1 ms，GPU < 1 ms；界面静止时，GPU 开销接近零。
5. **运行时不编译 shader**，主机上也一样。所有 pipeline 都在 cook 期产出。

**非目标（本系列不展开）**

- shader 编译器本身：另有设计文档。这里只列出渲染需要它提供的语言特性（0007 §2）。
- 移动端 tile-based GPU 的专门优化。
- 在 VR / 多视图立体渲染上做专门优化。接口里留着多 view，但不针对它做优化。

## 2. 档位（Tier）

档位是**运行时**选择，不需要换一套构建。每个 pass 根据当前档位选择实现，档位维度也会参与 permutation（0007 §7）。

| 特性 | 低（Switch 2） | 中（PS5/XSX、中端 PC） | 高（高端 PC、PS5 Pro） | 参考 |
|---|---|---|---|---|
| 几何 | visbuffer | visbuffer | visbuffer | visbuffer 主可见性 + PT |
| 材质 buffer | 单 slab（参数混合） | Substrate，预算 M | Substrate，预算 H | 完整 Substrate |
| 直接光 | clustered + VSM（降分辨率） | clustered + VSM | ReSTIR DI（RT 可见性） | ReSTIR PT |
| GI | 烘焙探针 + 稀疏 RT 更新 | RT 探针 + SHaRC | ReSTIR GI + SHaRC | ReSTIR PT |
| 镜面反射 | 探针 + SSR | SSR + 低粗糙度 RT 反射 | ReSTIR GI + 镜面 RT | PT |
| AO | GTAO | GTAO | 由 RT GI 隐含 | — |
| 体积雾 | 低分辨率 froxel | froxel | froxel + 更多光源 | PT 介质 |
| 体积云 | 1/4 分辨率，降频更新 | 标准 | 标准 + 高质量阴影 | PT 介质 |
| 超分 | TSR | TSR / 厂商 ext | TSR / 厂商 ext（含 Ray Reconstruction） | 累积 |

各档共用的部分：GPU Scene、visbuffer、材质 resolve、VSM、探针格式、SHaRC、TSR 和后处理链。真正分叉的只有"直接光"和"间接光"这两组 pass。

## 3. 帧流水线与线程模型

### 3.1 三级流水

```
            帧 N-1             帧 N               帧 N+1
Game     │ sim(N)       │ sim(N+1)      │ sim(N+2)      │
Render   │ render(N-1)  │ render(N)     │ render(N+1)   │
GPU      │ gpu(N-2)     │ gpu(N-1)      │ gpu(N)        │
```

- **Game 阶段**：ECS schedule、VM、物理、动画。结束时运行 *extract* system，产出 `RenderDelta(N)`，这是一份不可变的数据（见 0006 §1）。
- **Render 阶段**：
  1. 应用 `RenderDelta(N)`，更新渲染侧 CPU 表，并安排 GPU scatter 上传。
  2. 构建 render graph，包括场景的各个 view、UI 和合成。
  3. 编译 graph，把各段的录制并行展开在 job 上。
  4. 按顺序提交，然后 present。
- **GPU 阶段**：每个队列有自己的 timeline fence。`frames_in_flight` 是 2，这样资源 ring 的深度就是 3：CPU 正在写一份，GPU 正在用一份，还有一份等待退休。

两个 CPU 阶段都由 fiber job 图组成，**不设固定的渲染线程**（0003 §9）。框架每帧用两个 counter 串起来：`render(N)` 依赖 `extract(N)` 完成；`sim(N+2)` 依赖 `render(N)` 已经把 `RenderDelta(N)` 消费完，这样 delta 的内存才能回收。

**延迟补偿**：
- **Late latch**：在提交前的最后一刻，用最新的输入重新算一次相机矩阵，写进 per-view 常量。剔除 pass 用的是 sim 阶段的相机，所以要扩大 guard band，并在 HZB 测试里留出余量。
- **帧节拍**：swapchain 的 frame latency waitable 决定 Render 阶段什么时候开始；Game 阶段的开始时间再由 Render 阶段的开始时间反推。PC 上可以接入 Reflex / Anti-Lag ext。

### 3.2 内存

- **帧 arena**：`RenderDelta`、render graph 节点、pass 闭包、上传暂存的 CPU 侧数据，都从当帧 arena 分配，3 个 arena 轮转使用。`marlin-mem` 需要先实现 `arena/`，这是 0001 §2.3 里尚未完成的一项。
- **GPU 上传 ring**：每帧一段位于 Upload 内存的线性环，常量和 scatter 数据都从这里分配。
- **延迟销毁**：GPU 资源和 bindless 槽位都挂在退休队列上，等对应帧的 fence 完成后才释放（0005 §2.6）。

### 3.3 多视图与多窗口

- 一个 frame 里有多个 `RenderView`：主相机、编辑器的若干视口、反射捕获、VSM 的各个光源视图。所有 view 共用 GPU Scene，以及只需每帧做一次的 pass（场景更新、蒙皮、BLAS 构建、探针更新）。
- 编辑器里每个 OS 窗口有一个 swapchain 和一个 UI 根。场景视口先渲染到纹理，再作为 UI 的图像图元显示（0008 §6）。

## 4. Crate 布局

```
runtime/platform/rhi       L1  +多队列、indirect、mesh shader、ray query、查询、标记、placed 资源（0005 §1）
runtime/core/mem           L2  +arena（帧分配器）
runtime/core/ecs           L2  +change detection（extract 依赖它）
runtime/core/jobs          L2  +可跨帧持有的 counter / 依赖句柄（帧流水线需要）
runtime/engine/render_graph L3 render graph（0005 §2）
runtime/engine/shader      L3  运行时 shader 库：cooked pipeline 容器、permutation 查找、pipeline 缓存与热重载
runtime/engine/material    L3  新增：材质类型 / 实例 / 动态实例 / MPC / 参数表（0007）
runtime/engine/render      L3  场景渲染器：GPU Scene、view、各 pass、光照档位、流送（0006、0009）
runtime/engine/terrain     L3  新增：地形 cell 运行时数据、RVT 合成、碰撞查询（0009 §4）
runtime/engine/vfx         L3  GPU 粒子运行时（0009 §7）
runtime/engine/font        L3  OpenType 解析、shaping、字形光栅、字形缓存（0008 §2）
runtime/engine/ui_render   L3  立即模式绘制 API 与 GPU tile 渲染（0008）
runtime/engine/world       L3  +transform 层级（extract 读取 world transform）
editor/ui_core             core+alloc 保留模式 UI（编辑器和游戏共用）
editor/viewport            视口、拾取、gizmo
editor/world_tools         +地形修改器栈编辑
tools/cook_shader          材质与引擎 shader 的 cook：调用编译器库、收集 permutation、写 pipeline 包
tools/cook_mesh            LOD、meshlet、顶点流量化、RT 代理
tools/cook_level           +地形 cell 生成、植被摆放
tools/cook_font            新增：字体子集化、MSDF 预生成（可选）
```

同层依赖（需要按 0001 §1.1 规则 3 登记）：
- `render → render_graph, shader, material, font?`：font 用于场景内的 MSDF 文字。
- `ui_render → render_graph, shader, font`
- `material → shader`
- `terrain → render`
- `vfx → render, material`

`render` 是一个大 crate，内部按 0006 的章节分模块。如果以后编译时间成了问题，再按"光照档位"拆出子 crate。

## 5. 实施阶段

| 阶段 | 内容 | 前置 | 可并行 |
|---|---|---|---|
| R0 ✅ | RHI 补齐：多队列与 GPU 端 wait、indirect（含 count）、mesh shader、timestamp、debug 标记、placed 资源与 heap、3D 纹理、新格式；`mem::arena`；ECS change detection；jobs 跨帧 counter（2026-10-16 完成，范围与偏差见 0005 §1.8） | — | 各项互相独立 |
| R1 | `render_graph`：声明、编译、别名、barrier、异步计算、并行录制、缓存，以及调试可视化 | R0 | 与 U1 并行 |
| U1 | `font`（解析、光栅、字形缓存）+ `ui_render`（GPU binning 与 tile 着色，基础图元和文字） | R0 | 与 R1 并行 |
| R2 | GPU Scene + delta 同步；`cook_mesh` 产出 meshlet 和 LOD；剔除 + visbuffer；**临时**固定材质 resolve（DefaultLit 单 slab）；clustered 直接光；TSR 和基础后处理。删除旧 `marlin-render` 的前向路径 | R1 | |
| U2 | shaping（完整 OpenType）、路径填充、渐变、命令段缓存、脏 tile；接入 `ui_core` | U1 | 与 R2、R3 并行 |
| R3 | 材质系统完整版（需要 shader 编译器就绪）：Substrate、分桶、permutation、实例链、MPC、动态实例、非 Surface 域 | R2 + 编译器 | |
| R4 | VSM；探针 GI（低档、中档）+ SHaRC；GTAO、SSR；半透明前向 + MBOIT | R3 | 光照和半透明可以并行 |
| R5 | 流送：纹理 mip 反馈、网格 LOD 页、虚拟纹理；Copy 队列上传 | R2 | 与 R4 并行 |
| R6 | RT ext：BLAS/TLAS、wavefront 命中着色、ReSTIR DI、ReSTIR GI、降噪器（高档） | R4 | |
| R7 | 环境：大气、体积雾、体积云 | R4 | 与 R6 并行 |
| R8 | 地形（修改器栈 → cell 网格 → RVT）、植被 | R5 | 与 R6、R7 并行 |
| R9 | GPU 粒子 | R3 | 与 R6–R8 并行 |
| R10 | ReSTIR PT 参考档 | R6 | |
| R11 | Vulkan backend；之后做 Metal | R1 | 从 R1 之后可以随时开始 |

shader 编译器是 R3 的硬性前置。在它就绪之前，R1、R2、U1 用到的引擎 shader 仍然按 0002 §5 的占位方式处理：手工编译成 `.dxil`，只提交字节码。

## 6. 测试策略

延续"真实硬件 + 读回断言"的约定，按确定性分层：

| 层 | 对象 | 断言方式 |
|---|---|---|
| 纯 CPU | render graph 编译（剔除、别名区间、barrier 序列、队列调度、分段）、meshlet 构建、LOD 误差、Substrate 展平与预算、shaping、字形光栅、UI 命令编码 | 精确断言，或与黄金数据逐字节比较 |
| GPU 确定性 | 剔除结果（可见 cluster 集合）、visbuffer 的三角形 ID、材质分桶计数、VSM 页标记、UI binning 列表与 tile 覆盖、纯 SDF 图元像素 | 像素或 buffer 精确比较 |
| GPU 光照 | BSDF white furnace（能量守恒）、各档光照、TSR、体积 | FLIP 误差 ≤ 阈值，与黄金图比较；ReSTIR/探针与参考 PT 做收敛性对比（误差随样本数下降） |
| 端到端 | samples 工程 cook → 加载 → 渲染固定相机路径 | FLIP + 性能预算回归（GPU timestamp） |

黄金图按平台和厂商分别存放（PC NVIDIA、PC AMD、各主机），更新时需要人工确认。随机数一律用确定性种子（帧号加像素哈希），保证同一硬件上可以复现。

## 7. 对既有文档的修订

- **0001 §5**：新增 `material`、`terrain` 两个 crate，以及 `tools/cook_font`。
- **0002**：stage 增加 mesh/amplification；增加 inline ray query（0005 §1.4）；新增 3D 纹理和格式；基线提升到 mesh shader 必需。
- **0003 §9**：渲染快照改成"持久 GPU Scene + 每帧不可变 delta"（0006 §1）。

## 8. 待定问题

1. 主机上 ReSTIR GI 能否作为中档的可选项（PS5 Pro 已归入高档）。需要实测。
2. 是否引入 D3D12 work graphs ext，替代材质分桶的"每个材质一个 indirect dispatch"（0007 §6.3）。
3. 软件光栅微小三角形（需要 64 位原子）放在哪个阶段做，以及是否做。
4. 低档上 VSM 的物理页池大小和更新预算，需要等 Switch 2 实测。
