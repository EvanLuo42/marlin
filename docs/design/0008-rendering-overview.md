# 0008 — 渲染总览：平台、档位、帧流水与视图

状态：草案 · 2026-10-16

前置：
- [0002](0002-shader-interface.md)：shader ABI、bindless、坐标约定
- [0004](0004-shader-language.md)–[0006](0006-shader-ir.md)：Marlin shader 语言、编译器、IR
- [0007](0007-render-graph.md)：render graph

同一批文档：
- [0009](0009-scene-and-geometry.md)：场景与几何
- [0010](0010-lighting.md)：光照、降噪、超分、后处理
- [0011](0011-material-system.md)：材质系统
- [0012](0012-ui-renderer.md)：UIRenderer
- [0013](0013-environment.md)：环境与效果

这一组文档来自 2026-10-16 的渲染设计访谈，取代已删除的旧版渲染文档。

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 平台 | PS5、PS5 Pro、Xbox Series X\|S、Switch 2、PC | 1 |
| 帧率基线 | PS5/XSX/XSS/PC 60fps，主机可以有 30fps 画质模式；NS2 30fps，轻量游戏可以 60 | 1.2 |
| 硬件基线 | **全平台必需硬件光追（inline ray query）**、mesh shader、SM 6.6 bindless、64 位 typed 原子、wave 指令。没有软件追踪回退 | 1.3 |
| PC 最低配 | RTX 2060 / RX 6600 / Arc A580 级，6–8 GB 显存 | 1.3 |
| 档位 | 每个特性一个 0–N 档的 cvar，组合成命名预设 Low/Medium/High/Ultra/Cinematic；每个平台 cook 一份默认预设和覆盖值；主机的画质/性能模式就是两份预设 | 2 |
| 管线 | 全平台一条管线：cluster DAG → visbuffer → 材质缓冲 → ReSTIR 统一光照 → 降噪 → TSR/ML 超分 → 后处理 → 显示变换，档位只裁剪参数 | 2.3 |
| 帧流水 | Game N+1 ∥ Render N ∥ GPU N−1。帧末有一个很短的 extract 阶段；没有专门的渲染线程，全是 job | 3 |
| 场景 | 独立的 `RenderScene`，持久的 GPU Scene，ECS 的变化通过 extract 写入 | 3.2、0009 |
| SceneRenderer | 每个 `RenderScene` 每帧一个 `SceneRenderer`，接收一组 `SceneView`；场景级的工作在视图之间共享，视图级的工作各做各的 | 5 |
| Viewport | editor/game 的概念，只持有 `ViewState`（历史、曝光、蓄水池、VSM 缓存） | 5.3 |
| 合成 | 视口的显示空间结果作为 graph 句柄交给 UIRenderer，UI 把它当成一个 image 图元画出来；整帧一个图 | 6 |
| 扩展点 | host 侧的 `RenderFeature` 在固定注入点加 pass；device 侧的扩展走材质系统（0011） | 5.5 |
| 文档 | 0008 总览、0009 场景与几何、0010 光照、0011 材质、0012 UI、0013 环境 | — |

## 1. 平台与目标

### 1.1 平台

| 平台 | GPU | 带宽 | 关键能力 | 超分 |
|---|---|---|---|---|
| PS5 | RDNA2 36CU，10.3 TF | 448 GB/s | RT 较弱（遍历跑在 shader 里），primitive shader，Kraken 硬件解压 | TSR |
| PS5 Pro | 60CU，RT 快 2–3 倍，300 TOPS ML | 576 GB/s | 同 PS5，另有 ML 单元 | PSSR 2 |
| Xbox Series X | RDNA2 52CU，12.1 TF | 560 GB/s（10 GB） | DX12 Ultimate 全套，BCPack/DirectStorage 硬件解压 | TSR |
| Xbox Series S | RDNA2 20CU，4 TF | 224 GB/s（8 GB） | 同 XSX；**主机里的下限** | TSR |
| Switch 2 | Ampere 12 SM，3.1 TF（底座）/ 1.7 TF（掌机） | 102 / 68 GB/s | RT core、tensor core、mesh shader，带宽是最大瓶颈 | DLSS |
| PC 最低配 | RTX 2060 / RX 6600 / Arc A580 | — | DX12U + RT | TSR / DLSS / FSR / XeSS |
| PC 高端 | RTX 40/50、RX 9070 | — | 加 cooperative vectors（SM 6.9）、DLSS 4、FSR 4 | 同上，加帧生成 |

### 1.2 帧率与预算

- PS5、XSX、XSS、PC：60fps 基线，预算 16.6 ms。主机可以提供 30fps 的画质模式，作为另一份预设（§2）。
- NS2：30fps 基线，预算 33.3 ms；底座和掌机是两份预设。轻量游戏可以用 60fps 的预设。
- 帧时间的主控制手段是**动态分辨率**：按 GPU 计时调整内部分辨率，再由 TSR 或 ML 超分输出到显示分辨率（0010 §7）。

### 1.3 硬件基线

所有平台都必须有：

- inline ray query（0002 §1），没有 RT pipeline；
- mesh/amplification shader；
- SM 6.6 级的 bindless（0002 §2）；
- 64 位整数原子操作，要能用在 typed UAV 上（visbuffer 和 VSM 要用，0009 §4.3）；
- wave 指令。

不做 SDF 或体素的软件追踪回退，所以 GI、阴影、反射只有一条基于光线的实现，各档位只调预算（光线数、分辨率、更新频率，0010 §3）。

PC 最低配是 RTX 2060 / RX 6600 / Arc A580 这一档，这和现有 RHI 基线（mesh shader tier 1、SM 6.6）一致。PC Low 预设的画质大约相当于 XSS。

### 1.4 为下一代留的口子

PS6 和 Xbox "Magnus" 传闻在 2027 年发布，都是 RDNA5。现在先在设计上留出这些位置：

- **Cinematic 档的 ReSTIR PT**（0010 §3）：下一代主机上它可能就是默认档。
- **神经组件**：NRC（0010 §4.2）走 cooperative vectors，用 target 描述的 capability 做开关。
- **DXR 2.0 的 cluster AS**（CLAS、partitioned TLAS、间接构建）：cluster DAG 的 cluster 上限 128 三角形，本来就符合 CLAS 的约束（0009 §3）。

## 2. 画质档位

### 2.1 特性 cvar

- 每个特性一个整数 cvar，0 是最低档，例如 `r.lighting.tier`、`r.geo.error_px`、`r.sun.mode`、`r.restir.res`、`r.cache.kind`、`r.upscaler`、`r.clouds.quality`。
- cvar 用 marlin-config 定义，可以在运行时改。改动会让 graph 的拓扑变化（0007 §5），所以会用另一份编译好的 plan。
- 每个子系统在自己的 `add_passes` 里读它的 cvar，不在别处做平台分支。

### 2.2 预设与平台

- 命名预设（Low、Medium、High、Ultra、Cinematic）就是一组 cvar 值，写在 `.masset` 里。
- 每个平台 cook 一份默认预设和覆盖值。主机的画质模式和性能模式就是两份预设；NS2 的底座和掌机也是两份。
- PC 的设置菜单直接改单个 cvar，"自定义"预设就是被改过的那组值。

### 2.3 默认配置

下表是各平台默认预设里的关键档位，数值是初始目标，实测后再调（§9）。

| | NS2 掌机 | NS2 底座 | XSS | PS5 / XSX | PS5 Pro | PC Low | PC Medium | PC High/Ultra | PC Cinematic |
|---|---|---|---|---|---|---|---|---|---|
| 目标帧率 | 30 | 30 | 60 | 60 | 60 | 60 | 60 | 60+ | 30–60 |
| 内部分辨率（输出） | 360–540p（720p） | 540–720p（1080p） | 720–900p（1440p） | 1080–1440p（4K） | 1440p（4K） | 自适应 | 自适应 | 自适应 | 自适应 |
| 几何误差（像素） | 2 | 1.5 | 1.5 | 1 | 1 | 1.5 | 1 | 1 | 0.5 |
| 光照档（0010 §3） | L0 | L1 | L1 | L2 | L3 | L1 | L2 | L3 | L4 |
| 太阳阴影 | VSM | VSM | VSM | VSM | ReSTIR + RT | VSM | VSM | ReSTIR + RT | PT |
| 世界 cache | SHaRC | SHaRC | SHaRC | SHaRC | SHaRC | SHaRC | SHaRC | SHaRC / NRC | NRC |
| 超分 | DLSS | DLSS | TSR | TSR | PSSR 2 | 按厂商 | 按厂商 | 按厂商 | 按厂商（推荐 DLSS-RR） |
| 毛发 | card | card | card | strand | strand | card | strand | strand | strand |

## 3. 帧流水与多线程

### 3.1 三级流水

```
时间 →
Game  : [ G N ][E N][ G N+1        ][E N+1][ G N+2 ...
Render:            [ R N: 建图·编译·并行录制·提交 ]  [ R N+1 ...
GPU   :                      [ GPU N ................ ]
```

- **Game N+1 ∥ Render N ∥ GPU N−1**。三者各自内部都是并行的 job。
- **extract（E）**：游戏帧末尾的一个短阶段。并行 job 用 ECS 的变化检测，把变了的组件写成 `RenderScene` 的更新（§3.2），同时打包本帧的视图描述和 UI 绘制列表，形成一份不可变的**帧快照**。
- extract 一结束，游戏就开始下一帧。Render N 只读 `RenderScene` 和帧快照，不碰 ECS。
- extract N+1 要等 Render N 的 CPU 部分结束，因为它要写 `RenderScene`。Render 的 CPU 部分通常比游戏帧短，这个等待一般不会发生；发生时说明渲染的 CPU 部分是瓶颈，profiler 会把它标出来。
- GPU 侧最多 2 帧在飞（0007 §7）。

### 3.2 extract

- 每个渲染相关的组件有一个 extract 系统，运行在 ECS 的并行调度里（marlin-ecs 的 schedule），只处理这一帧变了的实体（marlin-ecs 的 change detection）。
- 输出是按表分开的更新流：实例的增删改、光源、贴花、体积、天空和环境参数等（0009 §1）。每个 extract job 写自己的那一段，最后按表合并。
- 相机、视口尺寸、调试开关这类视图参数在 extract 时拷进快照。
- editor 的 UI 是立即模式录制，录制发生在 editor 帧里（属于 Game 这一级），extract 时只把录好的 `UiDrawList` 交出去（0012 §2）。

### 3.3 Render 阶段

1. **应用场景更新**：多个 job 并行，按表应用 extract 的输出，记下哪些 GPU 行需要上传（0009 §2）。
2. **流送与驻留**：处理上一帧的 GPU 反馈（cluster 页、纹理 mip、BLAS 构建队列），发起 IO（0009 §6）。
3. **建图**：按顺序调用各个子系统的 `add_passes`，整帧一个图（0007 §1.3）。
   - 声明顺序就是语义顺序，所以声明本身在一个 job 里按顺序执行。
   - 声明很便宜：渲染是 GPU-driven 的，CPU 端没有逐物体的循环。
   - 需要 CPU 预处理的子系统（例如 UI 的命令打包），预处理放在建图之前的并行 job 里，建图时只引用结果。
4. **编译或命中缓存**（0007 §5）。
5. **并行录制**：按段分给 job（0007 §6）。
6. **提交并 present**：按窗口依次 present。

### 3.4 延迟

- 流水比"同帧串行"多一帧 CPU 延迟。下面这些手段用来补回来：
  - 帧节奏：用可等待的 swapchain（或平台的等价物）在游戏帧开头等待，把输入采样尽量推迟到 GPU 快要空闲的时候（Reflex 和 anti-lag 的思路）；
  - 相机的**晚更新**：extract 时再读一次最新的输入，更新第一人称相机。只更新相机，不改游戏状态。
- PC 的帧生成（0010 §7.3）会再加延迟，它和上面这些手段一起配置。

### 3.5 线程

- 没有渲染线程和 RHI 线程，所有工作都是 marlin-jobs 的 fiber job。
- 提交是一个 job，依赖所有录制段。present 在提交之后。
- 唯一会阻塞的是 GPU 帧槽的 fence 等待（帧槽回收时），它在 job 系统里以等待计数器的方式实现，不占线程。

## 4. crate 布局

| crate | 位置 | 内容 |
|---|---|---|
| `marlin-render-graph` | `runtime/engine/render_graph` | 0007 |
| `marlin-render` | `runtime/engine/render` | `RenderScene`、GPU Scene、`SceneRenderer`、几何、光照、后处理、环境；按模块划分 |
| `marlin-material` | `runtime/engine/material` | 材质类型、实例、参数表、closure 注册表的运行时部分（0011）；0001 的布局里已经有这个 crate，Cargo 里还没建 |
| `marlin-ui-render` | `runtime/engine/ui_render` | UIRenderer（0012） |
| `marlin-font` | `runtime/engine/font` | shaping、字形栅格化、字形缓存（0012 §6） |
| `marlin-shader` | `runtime/engine/shader` | 运行时 shader 容器、permutation 查找、pipeline 缓存 |
| `marlin-vfx` | `runtime/engine/vfx` | 粒子模拟；通过 0013 §8 的接口交给渲染器 |
| 厂商超分 | `platforms/<p>/` 或 `runtime/engine/render_ext/*` | DLSS、FSR、XeSS、PSSR 的 `Upscaler` 实现；PC 上的是 cargo feature，主机上的在私有仓库 |
| `marlin-cook-material` | `tools/cook_material` | 材质 permutation、closure 注册表、平台简化（0011 §8） |
| `marlin-cook-mesh` | `tools/cook_mesh`（已有） | 加上 cluster DAG 的构建（0009 §3） |
| `marlin-cook-terrain` | `tools/cook_terrain` | 地形（0013 §4） |

`marlin-render` 内部的模块：

```
render/src/
├── scene/        RenderScene、更新流、GPU Scene 上传
├── view/         SceneView、ViewState、相机、jitter
├── geometry/     cluster 遍历、剔除、HW/SW 光栅、visbuffer、蒙皮
├── material/     分箱、ResolvePixel 调度、材质缓冲、tile 分类
├── rt/           BLAS/TLAS（0007 §9）、简化材质图集、hit buffer
├── lighting/     ReSTIR、cache、VSM、降噪、半透明光照
├── post/         曝光、bloom、DOF、运动模糊、显示变换
├── upscale/      TSR、Upscaler trait、动态分辨率
├── env/          天空、云、雾、水面、毛发、粒子的渲染端
├── streaming/    cluster 页、纹理 mip、RVT
├── debug/        调试视图、统计、拾取
└── feature.rs    RenderFeature 注入点
```

模块之间按 0007 §3.4 的约定交换句柄结构体。哪个模块以后变得太大，再拆成独立 crate。

## 5. SceneRenderer 与视图

### 5.1 对象

| 对象 | 生命周期 | 持有者 | 内容 |
|---|---|---|---|
| `RenderScene` | 和世界一样长 | 世界（game）或预览面板（editor） | 实例、光源、材质实例、天空等的 SoA 表；GPU Scene 的 buffer（0009 §1–2） |
| `ViewState` | 和视口一样长 | 视口 | 稳定的 `ViewId`（graph 历史资源的键）、曝光、TSR/降噪历史、ReSTIR 蓄水池、VSM 页缓存、上一帧相机 |
| `SceneView` | 一帧 | 帧快照 | 相机、尺寸、预设、调试模式、输出要求、`&mut ViewState` |
| `SceneRenderer` | 一帧 | Render 阶段 | 一个 `RenderScene` 加上它本帧的所有视图 |

### 5.2 哪些工作共享

| 场景级：每帧一次，所有视图共享 | 视图级：每个视图各做一次 |
|---|---|
| GPU Scene scatter 上传 | 实例和 cluster 剔除、LOD 选择 |
| 蒙皮、形变 | HW/SW 光栅、visbuffer、HZB |
| BLAS refit/构建、TLAS 构建（0007 §9） | 材质分箱、解算、材质缓冲 |
| 世界 radiance cache 的更新（0010 §4） | ReSTIR、降噪、TSR |
| light tile 预采样 | VSM 的页标记与渲染（静态页缓存跨视图共享） |
| 天空 LUT、云的天气图 | 体积雾 froxel、云的 raymarch |
| 流送反馈的合并 | 后处理、显示变换 |

- 视图之间没有依赖，graph 会根据资源依赖把它们交错调度。
- 低频视图（反射捕获、小地图、editor 里没有焦点的视口）可以按 `SceneView::update_rate` 隔帧渲染；跳过的那一帧不声明它的 pass，UI 继续用它的持久输出（§5.4）。

### 5.3 API 草稿

```rust
pub struct SceneView<'a> {
    pub state: &'a mut ViewState,       // 持久：历史、蓄水池、曝光……
    pub camera: CameraParams,           // 视图矩阵、投影、上一帧矩阵由 ViewState 提供
    pub size: UVec2,                    // 显示分辨率；内部分辨率由动态分辨率决定
    pub preset: &'a QualityPreset,
    pub debug: DebugView,
    pub output: ViewOutputKind,         // Display（显示变换之后）/ SceneLinear / Capture
}

pub struct SceneRenderer<'s> { scene: &'s RenderScene, /* 本帧的场景级句柄 */ }

impl<'s> SceneRenderer<'s> {
    pub fn new(scene: &'s RenderScene) -> Self;
    /// 声明场景级 pass 和每个视图的 pass；返回每个视图的输出句柄。
    pub fn add_passes<'g>(&mut self, g: &mut RenderGraph<'g>, views: &mut [SceneView<'_>])
        -> SmallVec<[ViewOutput<'g>; 4]>;
}

pub struct ViewOutput<'g> {
    pub color: TextureHandle<'g>,       // 按 output 的要求：显示空间或场景线性
    pub depth: TextureHandle<'g>,       // editor 叠加层和拾取要用
    pub vis: TextureHandle<'g>,         // visbuffer，拾取和选中描边要用
}
```

- 同一帧里可以有多个 `SceneRenderer`，每个对应一个 `RenderScene`，比如主世界、材质球预览、资产缩略图。它们在同一个图里。
- `SceneView::output` 默认是 `Display`：输出已经过显示变换（0010 §8），编码成窗口的输出空间。离线捕获和截图用 `SceneLinear`。

### 5.4 Viewport

- **viewport 是 editor 和 game 的概念**，渲染器不知道它。viewport 持有一个 `ViewState`、相机和视口设置，每帧把它们写进快照里的一个 `SceneView`。
- editor 的视口：透视/正交、调试视图（缓冲可视化、光照档覆盖）、PIE，都是不同的 `SceneView` 设置。
- game：主视图就是一个视口；分屏是多个视口。
- 视口不可见（被折叠或在后台标签页里）时，不生成 `SceneView`。`ViewState` 的历史资源按 0007 §8 的规则在若干帧后退休。
- 隔帧渲染的视口：输出拷进一个持久纹理（`g.persistent`，键是 `ViewId`），UI 在没渲染的帧继续采样它。

### 5.5 RenderFeature

host 侧的扩展点。项目或插件可以在固定的注入点加 pass，不需要改渲染器：

```rust
pub trait RenderFeature: Send + Sync {
    fn points(&self) -> InjectionPoints;     // 关心哪些点
    fn add_passes<'g>(&self, g: &mut RenderGraph<'g>, at: InjectionPoint, io: &mut FeatureIo<'g>);
}
```

| 注入点 | 位置 | 可用的句柄 |
|---|---|---|
| `AfterVisibility` | visbuffer 和 HZB 之后 | visbuffer、深度、速度 |
| `AfterMaterial` | 材质缓冲写完、光照之前 | 材质缓冲（只读）、深度、法线 |
| `AfterLighting` | 不透明光照合成之后 | 场景颜色（可写）、上面所有的 |
| `AfterTranslucency` | 半透明之后、超分之前 | 同上 |
| `PostUpscale` | 超分之后、后处理材质之前 | 显示分辨率的场景颜色 |
| `Overlay` | 显示变换之后 | 显示空间颜色、深度：editor 的 gizmo、网格线、选中描边都在这里 |

- Toon 的描边可以是一个 `AfterLighting` 的 feature，也可以是一个后处理材质（0011 §10.2）。
- 光照、材质这类 device 侧的扩展走材质系统（0011 §5），不走这里。

## 6. 合成：视口、UI 与窗口

### 6.1 一个窗口一帧的图

```
SceneRenderer(world).add_passes ──┐ ViewOutput.color（显示空间）
SceneRenderer(preview).add_passes ┤
                                   ▼
UiRenderer.add_passes(window, draw_list, external_images) ──→ window 的 UI 目标 ──→ 编码 ──→ g.present
```

- `UiDrawList` 里的视口图像是一个 image 命令，引用的是 `ExternalImageId`（0012 §7）。录制 UI 时 graph 还不存在，所以只能先写 id。
- 建图时，UIRenderer 收到一张 `ExternalImageId → TextureHandle` 的表，在 UI 的 pass 里用 `b.read_texture` 声明读取，依赖由 graph 处理。
- 视口图像在 UI 里就是一个普通图元：圆角、裁剪、层叠、半透明覆盖都自然支持。视口上方的弹窗、视口里的工具栏也不需要特殊处理。

### 6.2 尺寸

- 视口的尺寸由 UI 布局决定，下一帧生效：布局在 editor 帧里算出视口矩形，extract 时写进 `SceneView::size`。
- resize 期间图像尺寸和矩形差一帧时，image 命令按矩形缩放采样，不会出现黑边。
- 连续拖拽时，历史资源的重建会被推迟到尺寸稳定之后（`r.view.resize_settle_frames`，默认 4），中间几帧用旧尺寸渲染再缩放。

### 6.3 色彩

- 场景视图的输出已经过显示变换（0010 §8），是窗口输出空间里的显示参考值。
- UI 在 sRGB 编码空间混合（0012 §8）。
- 窗口是 HDR 时（scRGB FP16 或 PQ swapchain）：
  - 场景视图的显示变换按窗口的峰值亮度输出；
  - UI 按 paper white（`r.ui.paper_white_nits`）缩放；
  - 最后的编码 pass 把 UI 目标写进 swapchain。

### 6.4 多窗口

- editor 的每个窗口都有自己的 swapchain，都在同一个图里导入和 present（0007 §1.3）。
- 窗口之间可以共享视口的输出，比如一个视口同时出现在主窗口和一个浮动预览窗口里，两边各自引用同一个句柄。

### 6.5 game

```
SceneRenderer.add_passes(main view) → ViewOutput.color
UiRenderer.add_passes(HUD, external = { main view })   // HUD 以主视图为底图
→ 编码 → present
```

- HUD 把主视图当作全屏的 image 底图，菜单和 HUD 画在上面。这和 editor 是同一条路径。
- 世界内的 UI 渲染到纹理，再作为材质参数使用（0012 §10）。

## 7. 调试与工具

- **计时**：每个 pass 的 GPU 计时（0007 §10.1）。每个子系统另外汇总出一张预算表，和 §2.3 的预算对比。
- **调试视图**（`SceneView::debug`）：
  - visbuffer 的实例、cluster、三角形、LOD 级别；
  - 过度绘制，以及软光栅和硬件光栅的比例；
  - 材质分箱和 permutation；每像素的 closure 数和字节数热图；
  - ReSTIR 蓄水池的 M、重用来源、duplication map；
  - SHaRC 体素和命中率；VSM 页；
  - 简化材质（RT 命中点看到的样子）；
  - 每个光照分量单独显示：直接光、间接光、镜面、自发光。
- **拾取**：读回鼠标位置的 visbuffer 像素，得到实例 id，不需要单独的拾取 pass。
- **选中描边**：用 visbuffer 的实例 id 生成选中遮罩，在 `Overlay` 注入点画描边。
- **shader 热重载**：cook 服务重编以后，pipeline 缓存原地替换（0011 §8.4）。
- **捕获**：0007 §10.4 的资源捕获，加上 PIX/RenderDoc/Razor/NSight 的帧捕获标记。

## 8. 落地顺序

| 阶段 | 内容 | 验收 |
|---|---|---|
| R1 | graph（0007）；GPU Scene；meshlet（离散 LOD）+ visbuffer；只有默认 Slab 的材质解算；一盏方向光加环境光的直接光照 | 取代现有的前向渲染器，`samples/basic` 画面等价 |
| R2 | cluster DAG 与流送；两遍 HZB；软光栅；VSM | 大场景，几何误差 ≤ 1 px |
| R3 | BLAS/TLAS；ReSTIR 框架的 DI 部分；降噪器；TSR | 千盏光源的场景，帧时间和光源数无关 |
| R4 | 统一 DI+GI 蓄水池；SHaRC；简化材质图集；镜面追踪；半透明 | L0–L3 全部可用；PS5 上 60fps |
| R5 | 完整 Substrate；closure 注册表；Toon 示例；贴花；后处理；显示变换 | Toon 示例不改引擎代码就能接进来 |
| R6 | 天空、云、雾；地形；毛发；水面；粒子渲染端 | 0013 各节的验收 |
| R7 | Cinematic：ReSTIR PT Enhanced、NRC、波前式完整材质；厂商超分 | 和 editor 参考路径追踪器对比 |
| U1–U3 | UIRenderer（0012 §13），和 R 系列并行 | editor 的窗口、面板和视口都用它画 |

- 主机后端在私有仓库里跟进，每个阶段的验收在已有的平台上都要过。
- 光照、几何的 shader 都用 Marlin shader 编写，桌面上产出 SPIR-V（0016），编译器已经具备（0005 §11 第 10 步）；DXIL 只在 Xbox 需要时再做。

## 9. 待定

- §2.3 每个平台的分辨率和档位要在硬件上实测后再定。
- 建图本身的并行化：现在是一个 job 按顺序声明。以后如果声明成为瓶颈，可以让各个视图声明到独立的片段里，再按顺序拼接；这需要 0007 支持图片段。
- 帧生成和延迟手段在各平台上怎么组合。
- 多 GPU、VR、立体渲染：不在这一版的范围内。
- 下一代主机的档位（§1.4）。
