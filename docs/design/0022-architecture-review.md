# 0022 — 架构审查：一致性、共享抽象与收敛计划

状态：草案 · 2026-10-20

前置：
- [0001](0001-repo-layout.md)：分层、crate 约定、错误约定（§11）
- [0003](0003-asset-pipeline.md)、[0007](0007-render-graph.md)、[0008](0008-rendering-overview.md)、[0015](0015-language-frontend.md)、[0016](0016-rhi-vulkan.md)、[0019](0019-engine-framework-and-world.md)、[0021](0021-tool-ui-architecture.md)
- [0023](0023-errors-and-logging.md)：错误与日志规范（本文 §4 的展开，优先级最高）

本文记录 2026-10-19 对整个仓库（约 35 万行 Rust）做的一次**设计层面**的审查：不看功能缺口和 WIP，只看不一致、缺少共享抽象（“每处自己写一个”）、分层泄漏，以及 3A 引擎会怎样组织。每条问题给出证据、为什么要紧和改法；最后是收敛顺序和需要拍板的问题。

## 0. 结论

**还称不上整体优雅。** 各子系统的**内部**大多是 3A 水准，问题集中在**接缝**上：凡是需要“一个横切服务、所有人共用”的地方，大多还没建，于是每个 feature、tool、app 各写一份。第二个系统性原因是文档里的规则没有被机器检查，依赖图和约定在逐步漂移。

这些都能靠**加一层共享服务 + 把规则写进 `xtask check`** 收敛，不需要推倒重来。

| # | 领域 | 核心问题 | 优先级 | § |
|---|---|---|---|---|
| E | 错误与日志 | 35 万行里约 70 条日志；没有任何 binary 装 sink；插件边界的错误是 `String` | **P0** | 4、0023 |
| R1 | 渲染 | 没有统一的 GPU 延迟释放；导入资源的状态表不清理（潜在错误 barrier） | **P0** | 5.1 |
| L1 | 分层 | 0001/0019 的同层白名单、L2/L3 禁平台代码没有检查；已有未登记的边 | **P0** | 3.1 |
| R2–R4 | 渲染 | 没有 `ViewInfo`/`SceneTextures`、PSO 注册表、统一上传队列 | P1 | 5.2–5.4 |
| A1–A2 | 资产 | source/cooked/loaded 类型不关联；cook key 不含输出布局 | P1 | 6.1–6.2 |
| F1–F3 | 框架 | 渲染侧没有模块模型；Engine 不持有 World；两套启动路径 | P1 | 7.1–7.3 |
| L2–L3 | 分层 | Module 内核在 `marlin-world`；工具为读格式链接整个 GPU runtime | P1 | 3.2–3.3 |
| D | 重复 | handle ×6、UUID ×2、Rect ×4、手写线程池 ×7、LE 读写器 ×5、cook_mesh 自带数学 | P2 | 8 |
| C | 编译器 | Target 不驱动管线；`compile.rs` 是 god function；诊断/反射/capability 多处拼装 | P2 | 9 |

## 1. 范围与方法

六个方向并行审查，再人工抽查关键证据：crate 依赖图与分层；横切基础（错误、日志、handle、哈希、线程、数学）；渲染栈（rhi、render_graph、render、renderer、material、ui_render、shader）；资产与 cook 管线；语言工具链；framework / ECS / 平台 / editor。行号以 2026-10-19 的 `main`（`afeff7b`）为准。

## 2. 做得好的（保持）

改动时不要破坏这些：

- **RHI 边界**：唯一的 cfg 分派点（`backend/mod.rs`），关联类型契约、无动态分派；只有 timeline fence；sync/access/layout 三元 barrier；bindless + 单一 pipeline layout；native 逃生口有文档。
- **Render graph**：GPU-less 的 `Recording` 后端可测；按拓扑缓存 plan、aliasing、async compute 和 copy 队列、并行录制、逐 pass GPU 计时、JSON dump；`History`/`Persistent` 保留资源；所有场景和 UI pass 都走 graph，没有 feature 手写 barrier。
- **Extract 边界**：ECS 产出 `RenderDelta`，render 自有的 `RenderScene` 镜像它，GPU scatter 只更新变化的行；帧 N 渲染与帧 N+1 模拟流水。
- **Cook 侧**：一个 `Cooker` trait、一个注册点；`read_source` 只允许读对象链接的文件并校验哈希，依赖被**强制**追踪而不是靠约定；分层、内容寻址的 DDC。
- **Reflect / serial 分工**：一个 `Reflect` derive 同时驱动文本 schema、可演化二进制和指纹；`serial_derive` 只补 `Pod`。
- **运行时 std 纪律**：L2/L3 不碰 `std::thread/sync/fs`，哈希集中在 `marlin-hash`/`marlin-containers`，`Name` 是唯一的运行时 interner。
- **语言前端**：`common/*` 不依赖编译器，`front` 不提 shader；方言校验真正集中（`front/syntax/src/validate.rs` 由 `Dialect` 数据驱动）；query 引擎一个 query 一个类型；CTFE 复用 IR 解释器。
- **ECS**：调度跑在 `marlin-jobs` 上；只有一个 `Entity`，`EntityGuid` 只用于持久化；模块拓扑排序、冻结的 `ModuleTable`。
- **平台 cfg 收得很紧**：基本只出现在 pal、app、input、net、rhi。
- **`marlin-diag` 本身设计得好**：不分配、编译期按级别剔除、前缀规则、`chain`/`fatal!`/`OrFatal`。问题在于**没人用**，见 §4。

## 3. 分层与 crate 图

### 3.1 L1：分层规则没有被检查（P0）

- `xtask/src/check.rs:108-127` 只检查层级顺序和 `marlin-pal` 的使用者。0001 §1.1 的同层边登记（rule 3）、L2/L3 禁平台代码、`deny.toml` 的 no_std 白名单都没有检查；0019 §2 说“按白名单检查”，但白名单不存在。
- 已经出现未登记的边：`render → anim`（`render/src/asset/mesh.rs:17`）、`material → render`（与 `render/src/lib.rs:1` “render 拥有材质”方向相反）、`ui_render → world`、`gameplay → input_action`。
- `Layer::of`（`check.rs:52-67`）没有 0021 的顶层 `ui/`；编译器层排在 umbrella 之上，编译器 crate 可以依赖整个 runtime。
- **改法**：白名单作为数据放进 `check.rs`；新增 `ui` 层；grep L2/L3 源码里的 `cfg(windows|unix|target_os)`；同时落实 0023 §6 的错误/日志检查。

### 3.2 L2：Module 内核在 `marlin-world`（P1）

- `Module`、`Registry`、`EngineContext`、`Resources` 定义在 `world/src/module.rs`，于是 `ui_render/src/module.rs:6` 把 World 拉进了 UI painter；0021 要求 launcher 不链接 World，但通过 ui_render 它会链接。
- 渲染是“库 + 模块”拆开的（`render` + `renderer`），`ui_render`、`anim`、`input_action` 则把模块写在库里面，两种形状并存。
- **改法**：模块内核下沉到 world 之下的小 crate（暂名 `marlin-module`）；所有子系统统一一种形状：要么都拆“库 + 模块”，要么模块 impl 放在 feature 后面。（待定，§11）

### 3.3 L3：工具为读数据格式链接整个 GPU runtime（P1）

- `cook_mesh`、`cook_texture`、`cook_render`、`test_scenes` 依赖 `marlin-render`，连带 rhi、render_graph、anim、world，实际只用 `marlin_render::asset::*`；`cook_texture` 依赖 `marlin-rhi` 只为了两个对齐常量。
- `tools/cooker/src/lib.rs:71-74` 为拿 `ComponentRegistry` 构建了整套 `marlin::game_modules()`，于是 cooker 链接了 app、input、rhi、net、framework。
- `marlin-shader` 已经用可选 `rhi` feature 把数据和 GPU 绑定分开，render 应该照做。
- **改法**：cooked 格式类型放进无 rhi 的 crate/feature（如 `marlin-render-asset`）；组件注册做成可以不构建运行时模块就收集的纯数据。

### 3.4 L4：平台类型重复、平台代码漏到 L3（P1）

- `framework/src/host.rs:238` 和 `apps/demo_clear/src/main.rs:70` 手写 `let Surface::Win32 {..} = … else { unreachable!() }`：L3 里出现了平台代码，Linux/Wayland 一到就要改 framework。
- `app::Surface`（`app/src/window.rs:78`）与 `rhi::SurfaceHandle`（`rhi/src/types.rs:1820`）是两个平行 enum；HDR/显示信息在 `app::Display` 和 `rhi::OutputInfo` 各有一份，rhi 还自己去查 GDI/DWM。
- 根因之一：`marlin-math` 没有任何依赖却放在 L2，L1 用不了，于是 `Rect` 定义了 4 次（app、rhi、ui_render、font）。
- **改法**：rhi 直接接收 `app::Surface`（或在 L0 定义唯一的 surface 类型）；显示/HDR 查询只留在 app；几何基础类型（至少 `Rect`、`Extent`）下沉到 L0。（待定，§11）

### 3.5 L5：约定不统一（P2）

- `marlin-std` facade：L2 全部遵守，L3 的 asset、font、framework、gameplay、input_action、renderer、world 直接依赖 base crate。规则要么改成“都可以直接依赖”，要么在 check 里强制。
- 文档里的 cargo `editor` feature 不存在；cookd 客户端（`asset/src/dev.rs`）无条件编进游戏，导致 `asset → marlin-net`，与 0003 §4.6 “游戏不连 cookd”矛盾。应该放在 `editor`/`not(shipping)` 后面。
- 命名：`compiler/common` → `marlin-lang-*`，`compiler/front` → `marlin-front-*`，`compiler/shader` → `marlin-shaderc-*`；`marlin-shaderc-core` 实际是 shader 标准库（建议 `-stdlib`）；`marlin-diag`（日志）和 `marlin-lang-diag`（编译诊断）容易混；`marlin-error` 是 L0 的 proc-macro，其他地方都叫 `*-derive`。
- `marlin-cookd` 不在 `[workspace.dependencies]`，两处手写路径；runtime crate 还 dev-depend 一个工具 binary crate。
- 13 个占位 crate（audio、nav、physics、vfx、audio_device、services、editor 的若干、cook_audio）被 umbrella 和 cooker 引用；`marlin-rhi` 约 4 万行，生成的 FFI、TLSF、两个后端都在一个 crate 里，桌面上也编 D3D12。建议拆出 `rhi-vulkan-sys` 和 GPU 内存 crate；占位 crate 等有内容再加。
- 工具使用 std/第三方没有政策：`asset_db` 用 `memmap2` 和裸 `windows-sys`（而 `marlin_io::mmap` 已存在），`cook_core` 用 `std::fs`。建议规则：“工具用 std + 我们的 runtime crate；第三方只用于解析器”。

## 4. 错误与日志（P0，详见 0023）

这是审查中**最普遍**的问题。规范在 [0023](0023-errors-and-logging.md)，这里只列现状证据。

### 4.1 日志几乎不存在

- 非测试代码里的日志调用（`error!/warn!/info!/debug!/trace!`）：rhi 33 条，editor 8 条，demo_clear 7 条，world 6 条，framework 5 条，game 5 条，其他零星几条，**合计约 70 条**。
- **以下 crate 一条日志都没有**：asset、render、render_graph、material、ui_render、shader、font、jobs、config、ecs、io、mem、net、app、input、全部 cook_* 和 asset_db、全部 editor/*、全部编译器 crate。
  - 资产加载失败（`asset/src/server.rs` 的 `fail_unit`）只变成状态，**没有调用方在栈上，也没人记日志**：贴图坏了就静默地不显示。
  - 渲染从不记录 pipeline 编译、swapchain 重建、显存预算、graph plan 重建。
  - job system、IO 队列、网络不记录任何东西。
- **没有任何 binary 注册 sink**。`rt::start` 只调用 `marlin_diag::init()`；默认 sink 是 `DebugOutput`，只有挂调试器时才看得见。游戏、editor 都没有日志文件；cooker、cookd、store 甚至不调 `hosted::init`，运行时 crate 在 cook 时的 `warn!` 完全不可见，也没有崩溃报告。
- 工具退回 `eprintln!`：cooker 47 处（包括 `warning:` 行）、cookd 7、store 6、bench 9；cooker 还重写了一份 `diag::chain`（`tools/cooker/src/main.rs:166` 的 `marlin_error_chain`）。
- 错误用 `{e:?}` 打印（`framework/src/input.rs:48`、`input_action/src/module.rs:163,255`、`world_module.rs` 等），丢掉了 `chain` 的可读来源链。
- 0020 §10.5 的 Output Log 要求“资产路径、entity、源文件行号可点击”，现在的 `Record` 只有 `fmt::Arguments`，没有结构化主体。

### 4.2 错误约定只执行了一半

- 0001 §11 的错误约定（`marlin-error` derive、`{kind, raw}`、记录一次、`fatal!`）设计正确；但 73 个 `*Error` 类型里只有 18 个 crate 用 derive，16 个运行时类型手写 `impl Error`，即使所在 crate 已经依赖 `marlin-error`（diag `level.rs:181`、`sink.rs:172`；asset `package.rs:240`、`server.rs:68`、`vfs.rs:48`、`wire.rs:59`；renderer `assets.rs:44`；以及 io、hash、jobs、serial、reflect、shader、net）。
- **插件边界是字符串**：`Cooker::cook -> Result<Cooked, String>`（`tools/cook_core/src/cooker.rs:279`）、资产 loader `LoadFn -> Result<_, String>`（`asset/src/server.rs:143,160`）、警告是 `Vec<String>`。`Result<_, String>` 共约 70 处：cook_font 15、编译器 IR 10、cook_shader 7、cook_mesh 7、editor/core 5……另有 46 处 `format!("{e:?}")` / `map_err(|e| format!(..))` 把类型化错误压扁成字符串（例如 `AssetError::Corrupt(format!("{e:?}"))`）。后果：来源链丢失、无法按种类分支（重试 / 缺依赖 / 数据损坏）、editor 无法把问题挂到字段或源码位置上。
- **内容问题和程序错误没有区分**：cook 和 config 产生的是“这份数据第几行哪个字段不对”的诊断（可能多条、有严重度），它们和“IO 失败”被塞进同一个 `String`。编译器有完整的 `Diagnostic`，到 cook_shader 却变成渲染好的字符串（`cook_shader/src/lib.rs:186,194`）。
- **Drop 里的失败被静默吞掉**：`render_graph/src/frame.rs:1079` `let _ = self.fence.wait(..)`，`rhi/src/frames.rs:121`、`vulkan/swapchain.rs:1304` `let _ = self.wait_idle()`。Drop 无法传播错误，至少要记日志。
- 编译器的 8 个错误类型手写 `std::error::Error`；各阶段错误形状不一（§9.2）。

## 5. 渲染栈

graph 本身很好；问题在 graph **之上**：没人拥有 GPU 生命周期、View 状态、PSO 和一次性 GPU 工作，于是每个 feature 各写一份。UE 的对应物是 `FRDGBuilder` + `FViewInfo` + `FSceneTextures` + 全局 PSO cache，Marlin 缺的就是这一层。

### 5.1 R1：没有统一的延迟释放（P0，含潜在错误）

- RHI 文档说延迟销毁“属于上面的 render graph”（`rhi/src/lib.rs:5`），但 graph 只回收 transient 和 import view。持久对象各自实现“第 N 帧退役、N+FRAMES_IN_FLIGHT 帧释放”：mesh（`renderer/src/world_renderer.rs:219-230`）、TLAS（`render/src/raytracing.rs:813-827`）、UI 逐槽 readback（`ui_render/src/renderer.rs:431,1030`）。
- 至少 4 个独立帧计数器：`Meshes.frame`、`Tlas.frame`、`RenderScene.frame`、`WorldRenderer.frame`，再加上 `FrameContext` 自己的。
- `StateTable` 以原生 handle 为 key（`render_graph/src/backend.rs:55-61`），需要调用方手动 `forget_buffer/forget_texture`；全引擎只有 `raytracing.rs:824` 调过一次。`WorldRenderer` 导入的 PagePool、RtGeometry、BlasTable、BlasPool（`meshes.rs:973-999`）从不 forget。editor 在 `wait_idle` 之后丢弃 PIE renderer（`apps/editor/src/app.rs:452`），驱动复用 handle 后，旧状态会产生**错误的 barrier**。
- **改法**：`FrameContext` 拥有唯一的 `GpuLifetime`：一个帧序号、`retire(obj)`、释放时自动 forget；持久 graph 资源改成 RAII handle，drop 时注销。

### 5.2 R2：没有 View 和 SceneTextures 抽象（P1）

- `WorldRenderer::add_view_passes`（`world_renderer.rs:277-497`）手工串起所有 feature：`vsm.add_passes` 有 14 个位置参数（这些 crate 里共有 12 处 `too_many_arguments`），还有匿名元组 `(material, mask, depth, velocity, camera)`。
- 约 9 个 feature 各自重算 `view_projection`、各自上传 camera 常量：`denoise.rs:221`、`restir.rs:355`、`shadows.rs:462`、`sharc.rs:243`、`visibility.rs:540,738`、`lighting.rs:101`、`material/passes.rs:376`、`translucency.rs:259`。
- 降噪历史用 `main_view | SPECULAR_BIT` 做 key（`world_renderer.rs:39,299`），占用了 view id 的命名空间；jitter 存在 `marlin_render::Camera`（相机描述）里，而它是逐帧 view 状态。
- **改法**：`ViewInfo`（矩阵、上一帧矩阵、jitter、渲染/显示尺寸、帧种子），每个 view 上传一次 uniform；`SceneTextures`（depth、material、velocity、HDR）；所有 feature 统一签名 `add_passes(g, &view, &scene_tex, inputs)`；保留资源的 key 为 `(view, feature, name)`。

### 5.3 R3：PSO 和 sampler 各写各的（P1）

- `create_*_pipeline` 直接调用 22 处，`cache: None` 共 30 处；RHI 的 `PipelineCache` 在 engine 层**零使用**。没有磁盘 PSO 缓存，没有异步编译/预编译，没有 shader 热重载挂点。
- shader 绑定三种形状：`XShaders` + `ENTRIES` + `bind`（Post、Visibility、Material）；`const ENTRY` + `bind_one`（Lighting、LightTiles、Scatter）；`UiShaders::from_modules`。
- 同一个 linear-clamp sampler 创建了 4 次：`post.rs:221`、`upscale.rs:183`、`material/passes.rs:256`、`ui_render/renderer.rs:632`。
- **改法**：一个 `PipelineRegistry`，以 `(EntryDesc, state)` 为 key，持有 `PipelineCache`，返回 handle，作为热重载和预编译的唯一入口；统一一种绑定写法；全局静态 sampler 表（索引固定在 0002）。

### 5.4 R4：上传 / 回读 / 一次性提交有多条路（P1）

- graph 提供 `ctx.upload` 和 `g.readback`；同时 `copy_to_new_buffer`（`meshes.rs:1146`）和 `zeroed_buffer`（`raytracing.rs:527`）各自建 pool/list/fence、提交并 `fence.wait` 阻塞；`WorldRenderer::apply` 注明“阻塞直到复制完成”（`world_renderer.rs:195`），即每个新 mesh 都让渲染线程停一下。UI 有自己的逐槽 readback 和 `unsafe` 读取。
- **改法**：一个 `UploadQueue`（staging ring，作为 graph 的 copy 队列 pass 录制，按 fence 追踪），立即上传和流式上传都走它；`g.readback` 是唯一的回读路径。

### 5.5 R5：crate 职责与 API 一致性（P2）

- `marlin-material` 拥有 render pass（classify、resolve、半透明前向、scene copy）；`marlin-render` 拥有 lighting 和 post，库文档却仍写着“materials”（`render/src/lib.rs:1`）。material 只应保留材质定义、参数和排列；pass 移到 render。
- 两个 `Camera`：组件（`renderer/components.rs:70`）和渲染侧（`render/meshes.rs:56`），且都被相邻 crate 重导出。渲染侧的改名为 `ViewCamera`/`CameraProjection`。
- D3D 词汇漏到 Vulkan 优先的 bindless 设计里：`PassContext::srv/uav/cbv`（`render_graph/src/context.rs:267-278`，324 处调用）、`RenderTargetView`/`DepthStencilView`、共享 desc 里的 D3D12 专用 `disable_enhanced_barriers`。建议 `read_index/write_index/uniform_index`，后端专属选项放进扩展 desc。
- 构造函数各异（`new`、`with_shaders`、`new_texture`、`from_modules`、`bind`、`load`）；错误类型各异（手写 Display 的 `ShaderError`、derive 的 `WorldRendererError`、直接返回 `AssetError`、`marlin_rhi::Result`）；`WorldRenderer` 把每个 feature 作为 `pub` 字段让设置代码直接改（`world_renderer.rs:47-77`），应该通过一个设置结构。

## 6. 资产与 cook 管线

### 6.1 A1：source、cooked、loaded 类型之间没有类型关联（P1）

- `Link<Mesh>`（`renderer/src/components.rs:37`）加载出来是 `LoadedMesh`（`renderer/src/module.rs:55`），`Texture` 是 `InPlace<Texture>`，`Shader`/`ShaderPackage` 是 `ShaderLibrary`。`AssetServer::load<T>` 的 `T` 由调用方猜，猜错了到运行时才是 `AssetError::WrongType`（`asset/src/server.rs:384`）。
- `Cooked.type_id` 由各 cooker 自选（`cook_core/src/cooker.rs:48`），loader 以裸 `u64` 为 key（`server.rs:148`），cooker 用的是 `TypeId` newtype。
- `StringTable` 的 source 类型在 tools crate 里，runtime 叫不出它的名字，只好钉住一个 reflect 名字伪造 type id（`tools/cook_text/src/lib.rs:28-33`），而且 runtime 没有注册它的 loader。
- **改法**：runtime 里一个 `trait Asset { type Loaded; const COOKED_TYPE: TypeId; }`；`Link<A>` → `Handle<A::Loaded>`；`Cooker` 声明它的输出资产类型，注册时两边互相校验。

### 6.2 A2：cook key 不含输出布局（P1）

- key 只含 source 类型指纹和手动递增的 `Cooker::version()`（`cooker.rs:424`）。`Pod` 已经有 `layout_hash`（`serial/src/cooked.rs:153`），但没有工具把它放进 key。改了 `CookedTexture` 而忘记手动 bump，DDC 会一直返回旧 blob：加载时被头部检查拒绝，却永远不会重新 cook。
- `LevelCooker` 的 key 包含组件指纹，但不包含 `CookedLevel` 本身（`cook_level/src/lib.rs:35-49`）。
- `CookContext::profile()` 不追踪，文档要求每个 cooker 自己把它加进 key（`cooker.rs:176`）——这是陷阱。
- **改法**：自动把输出类型的 `layout_hash`/指纹折进 key；调用 `profile()` 时自动追踪，与 `read_source`/`dependency` 一致。

### 6.3 A3：二进制容器格式各写各的（P2）

- `"MCU1"` + u16（`asset/src/cooked.rs:26`）、`"MPK1"` + u16 v4（`package.rs:34`）、`"MMAN"` + u32 v3，而且用的是 **TCP 分帧**的 `wire::Writer`（`cook_core/src/manifest.rs:15-19`）、`"MARLINDB"` + u32（`asset_db/src/snapshot.rs:27`）、`"DDC1"`/`"REF1"` 没有版本字段（`cook_core/src/ddc.rs:23`、`store/src/store.rs:15`）。版本有的在 magic 里，有的在字段里，有的没有。
- **改法**：`marlin-serial` 里一个小 container 模块（magic、版本、校验和、节表、带边界检查的 LE 游标）；工具侧元数据（manifest、snapshot）直接用 `marlin_serial` 结构。字节读写器的重复见 §8。

### 6.4 A4：两套 schema 迁移，都没接上（P2）

- `marlin_serial::migrate` 作用于反射值，按 `#[reflect(version)]`；`masset::Schema` 升级作用于 AST，版本显式给出（`tools/masset/src/schema.rs:16-106`）。
- `Cookers::add` 把每个资产类型以文本版本 **0** 注册，忽略它的 `#[reflect(version)]`（`cooker.rs:422`）；`cook_level` 对组件却传 `info.attrs.version`（`cook_level/src/lib.rs:43`）。生产代码没有注册任何升级或迁移。
- **改法**：一个版本来源（reflect 属性），一个迁移注册表，文本绑定和二进制读取都查它。

### 6.5 A5：关卡绕过 handle 模型、加载在主线程（P2）

- `CookedLevel` 是 `Encoding::Serial`，在主线程反射反序列化，然后 `Arc::new(level.clone())` 再复制一遍（`world/src/world_module.rs:80,98`）；子关卡用单独的请求/`provide_level` 轮询协议，而不是持有 `Handle`。
- `AssetServer::update` 在 job 上解压，但每个 loader 都在调用方线程运行（`server.rs:16`），大的 serial 解码（关卡、shader 库）落在主线程。
- “`vfs.named → load → update_blocking → match state`”复制了 6 次：`framework/src/scalability.rs:136-157`（还建了一个临时 `AssetServer`）、`renderer/src/assets.rs:76-110`（×3）、`ui_render/src/module.rs:20`、`input_action/src/module.rs:188`。
- **改法**：loader 拆成 worker 上的 `decode` 和可选的主线程 `finalize`；`AssetServer::load_named_blocking<T>`；`Levels` 持有 `Handle<CookedLevel>`。长期：关卡按 archetype 分块 cook 成就地 `Pod`。

### 6.6 A6：其他（P2）

- Scalability/DeviceProfile `.masset` 用 `{Name, Value}` 字符串设置 cvar（`framework/src/scalability.rs:50-55`），cook 时不校验，坏值到启动时才暴露。应该在 cook 时按 config 注册表校验。
- 平台名总在 key 里（`cook.rs:238`），平台无关的资产（关卡、字符串表、输入）按平台重复 cook。
- 一个 `.masset` 文件是一个 cook 单元，改一个 entity 整个文件重 cook。
- 身份宽度混用：对象 128 位 UUID、单元 `xxh3_64(资产路径)`（文件移动会改变单元身份和 DDC key）、名字 64 位、类型 64 位。可以接受，但要写明是有意为之；`marlin_hash::Guid` 与 `marlin_asset::Uuid` 重复见 §8。

## 7. 引擎框架、ECS 与编辑器

### 7.1 F1：渲染侧没有模块模型（P1）

- `Module`（`world/src/module.rs:98-133`）只有模拟钩子。extract 之后的 `take_delta → collect_feedback → apply → RenderSettings::from_cvars().apply` 在 `GameApp`（`framework/src/game.rs:54-80`）和 `EditorApp`（`apps/editor/src/app.rs:368-381`，还自己往 delta 里塞 view）里各写一遍；每个 app 自己建 `WorldRenderer`/`UiRenderer`。
- Dev HUD 通过 cfg 的桩类型硬接在 host 上（`framework/src/dev.rs:1-16`；`host.rs:337,474`），而不是一个模块。
- 与 0019 “render 拥有 extract、多视图”矛盾：每加一个 app、viewport 或工具窗口，都要重写一遍 world→renderer 的账目。
- **改法**：framework 里的渲染侧注册表（`SceneRenderers`/`ViewManager`，按 world 和 view 列表）；可选给 `Module` 加渲染侧钩子；app 只声明 view 和目标；DevTools 变成普通模块。

### 7.2 F2：Engine 不持有 World（P1）

- `engine.rs:1-4` 写明“World 归调用方”。editor 在 `simulate` 里先 tick 编辑器 World 再 tick PIE World（`app.rs:368,377`），再分别取 delta；`PieSession::start` 自己建 World（`editor/viewport/src/pie.rs:19-30`）。
- 没有统一的 World 列表供工具、调试器、网络枚举，无法并行 tick，顺序由调用方隐式决定。UE 有 WorldContext 列表。
- `Engine` API 还带着测试便利：每个方法都收 `Option<&JobSystem>` 并懒建单线程后备（`engine.rs:35-41,57,96-99,113-117`），而所有调用方都传 `Some`；`Engine<'v>` 借用 `Vfs`，这个生命周期贯穿 `HostSetup<'a,'v>`/`SimContext`，会妨碍热重挂载（cookd 重载、DLC、editor 切换项目）；`streamer` 同时是字段和 `Resources` 条目（`engine.rs:27,55`）。
- **改法**：`Engine` 持有 `Worlds`（创建、销毁、遍历、tick 全部），每个 World 声明种类和 view；`Arc<JobSystem>`、`Arc<Vfs>`；测试后备移到 test-support。

### 7.3 F3：两套启动路径、三套 cvar 注册（P1）

- 游戏用 `marlin_rt::game_entry!` + `marlin_framework::boot()`（`apps/game/src/main.rs:18-47`）；editor 手写 std `main`、自己装 panic hook 和全局分配器、直接调 `Modules::new`、`register_cvars()`、`marlin_config::register`、`apply_config_files`、`mount`（`apps/editor/src/main.rs:62-73,146-171`）。“先注册模块再应用配置”的顺序约束只靠两处注释。
- cvar 注册：链接段 `declare!`（`config/src/declared.rs`，config 之外零使用）、模块的 `Registry::add_cvars`、framework 的 `call_once register_all`（`boot.rs:55-57`）、editor 的直接 `register`。
- **改法**：一个 `framework::launch(LaunchDesc { target, modules, content: Staged | Cookd(addr), config_scope })`，游戏、editor、launcher、工具都走它；它也是 0023 §3 安装日志 sink 的唯一位置。cvar 只留一种机制（待定，§11）。

### 7.4 F4：事件、输入路由、Transform（P2）

- `marlin_ecs::Events` 需要每个类型手动加 `update_events::<E>` 系统（`ecs/src/event.rs:6,27`），`Registry` 没有 `add_event`，`World::frame` 不交换事件缓冲；跨固定步长读取 0 次还是 N 次没有定义。另外还有 config `on_change` 回调、组件 hook、轮询式平台事件、0021 规划的 signal。要写明每种机制用于什么，并加 `Registry::add_event::<E>(TickGroup)`。
- editor 自己求值 `InputUser`，手工 hit-test 外壳矩形做 Play/Stop（`app.rs:320-337`），绕过了 `framework/src/input.rs:1-3` 规定的“host 命令 → UI → viewport → 玩家”路由；`Option<Uuid>` 逐命令的写法与 `HostInput` 重复。建议在 input_action 加 `CommandSet`，UI 作为 framework 路由的一层。
- editor 运行 `game_modules()`（`main.rs:152`），没有 `editor_modules()`；editor-only 注册没有归宿。
- 两个 `Transform`：`marlin_math::Transform`（f32，文档写着“场景层级存储的形式”，已过时）和 `marlin_world::Transform`（f64 平移，真正的层级组件）。数学那个改名（`Trs`/`Affine3`），`GlobalTransform::relative_to` 是唯一桥梁。
- 占位 crate 形状各异（有的只有 `extern crate alloc`，有的有 `AnimModule` 单元结构，有的是 `::new()`）；模块依赖按字符串名、查找按类型。建议一个单文件模块模板。

## 8. 重复实现（P2）

| 概念 | 实现数 | 证据 | 收敛到 |
|---|---|---|---|
| 代际 handle | ~6 | `containers::SlotMap` 只有 2 个用户；`ecs::Entity`（`entity.rs:13`）、`render::scene::Id<T>`（`scene/id.rs:18`，自带 free list）、render_graph handle（`handle.rs:66,133`）、io `RequestId`（`io/src/queue.rs:17`）与 net `RequestId`/`SocketId`（`net/src/queue.rs:27,35`，连 `tag/from_tag` 都一样）、font cache、shadows | `containers` 的 handle 分配器（含原子预留变体）+ 一个原始 `GenIndex`；L0 的 io/net 共用一份低层副本 |
| UUID | 2 | `marlin_hash::Guid`（hash crate 外零使用）、`marlin_asset::Uuid`（自带 parse/format，`id.rs:68-115`） | 删 `Guid`，或让它成为 `Uuid`/`EntityGuid` 的底 |
| splitmix64 | 4 | mem `segment.rs:139`、math `random.rs:20`、asset `id.rs:58`、world `guid.rs:48` | `marlin-hash` |
| 字符串 interner | 3 | `Name`、`arena::Interner`（编译器）、render_graph 线性扫描的 `Mutex<Vec<&'static str>>`（`frame.rs:1790`） | runtime 用 `Name` |
| 并行 map | 7+ | `available_parallelism` + `thread::scope`：`cook_core/src/cook.rs:522`、`package.rs:147`、`asset_db/src/lib.rs:258,322,693`、`scan.rs:139,256`、`cookd/src/lib.rs:160`、`cook_shader/src/lib.rs:426`、`cook_texture/src/par.rs:15`；嵌套时超额订阅，无取消、无 profile；`Session::compile_batch` 是串行的 | 工具跑在 `JobSystem` 上，至少 `cook_core` 一个 `par_map` |
| 数学 | 多处 | `cook_mesh` 不依赖 `marlin-math`，自带 `Mat4 = [f32;16]` 和四元数（`src/math.rs`），`cross/dot/normalize` 在 `import.rs:572`、`meshlet.rs:36-52`、`simplify.rs:84-92`（f64）、`tangent.rs:21-33` 各一份；`render/src/asset/mesh.rs:266,275` 也有；sRGB 两份（`ui_render/src/shape.rs:440`、`cook_texture/src/color.rs:8`）；f16 两份（`math/scalar.rs:398`、`shader/ir/src/num.rs:5`） | `marlin-math`；cook 和运行时必须用同一套量化/解码 |
| `Rect` / `Bounds` | 4 / 3 | font `pack.rs:12`、ui_render `style.rs:53`、app `display.rs:20`、rhi `types.rs:1398` | L0 几何类型（§3.4） |
| LE 字节读写 | 5+ | serial `wire.rs`、asset `wire.rs`、asset `le.rs`、`cook_font/src/ttf.rs:35`、`asset_db/src/snapshot.rs:171`、`cook_core/src/ddc.rs:103`、render `cluster.rs`/`meshes.rs` 里 17 处裸 `from_le_bytes` | 一个带边界检查和类型化错误的 `ByteReader/ByteWriter` |
| 路径 | 4 | 工具里内容路径都是 `String`；`join` 在 asset_db 两份（`lib.rs:169`、`scan.rs:44`）、`cook_mesh/src/import.rs:127` 一份；编译器 `SourcePath`（`span/src/lib.rs:138`） | `ContentPath`（包 + 相对路径），放在 io 或 asset |
| 阻塞加载 | 6 | §6.5 | `load_named_blocking<T>` |

另外：编译器和工具大量使用 std `HashMap`（43 和 25 个文件），即 SipHash 和随机迭代顺序；cook 输出只要迭代 map 就有**确定性**风险，cooker 应该用 `marlin_containers::HashMap` 或 `BTreeMap`。`MemTag`/`Tagged`/`track_alloc` 在 `marlin-mem` 之外零使用，按子系统的内存预算什么也报告不了。

## 9. 语言工具链

### 9.1 C1：Target 不驱动管线（P2）

- `shader/target/src/lib.rs:29-34` 声明了 `calls`、`vectors`、`wave_sizes`、`register_budget`，`backend_spirv` 之外没人读。`opt::pipeline(&Options)` 不接收 `Target`，总是全部内联。
- SPIR-V target 声明 `calls: true`（`backend_spirv/src/lib.rs:88`），`write_module` 却拒绝多于一个函数的模块（`:99-104`）：描述与后端互相矛盾。
- `opt/src/lib.rs:31` 说 1.4/1.5 两个 pass“随 DXIL 后端一起来”，但没有挂点；加 DXIL 时要么在 opt 里 `match target`，要么改 shaderc，违反“后端便宜”的规则。
- **改法**：`pipeline()` 接收 `&Target`，按字段决定 pass；`Backend::legalize_passes()`（或写出前的钩子）让后端专属降级留在后端 crate；没人读的字段先删。

### 9.2 C2：一个 `Diagnostic`，但没有一条诊断路径（P2）

- `DefDiagnostic`、`TyDiagnostic` 没问题；`InferDiagnostic`（`hir_ty/src/infer/mod.rs:93`）和 `LowerError`（`lower/src/error.rs:11`）用 `&'static str` 而不是 `diag::Code`；`opt::Problem` 用 `u32` 索引 `module.locs`；`ir::SourceLoc`（`ir/src/module.rs:119`）重新实现了 `Span`；`BackendError`、`Backend::validate -> Vec<String>`、`reflect -> Result<_, String>`、`SessionError`/`SourceError` 都是字符串。`shaderc/src/compile.rs` 在局部闭包里把这些重新拼成 span 和诊断。
- **改法**：每个阶段边界直接返回 `Diagnostic`（或实现 `ToDiagnostic`）；`Code` 全局一张表；IR 位置表携带 `Span`/`FileId`。与 0023 §4 的 `Issue` 衔接。

### 9.3 C3：capability、反射、Stage 多处拼装（P2）

- capability 在前端推断一次（`hir_ty/src/caps.rs`），facade 遍历 IR 又推断一次（`compile.rs` 的 `lowered_capabilities`/`scalar_capabilities`），名字是字符串字面量（`"Int64Atomics"`），target 里又列一遍字符串。应该是 `ir`/`opt` 里的一个分析（与 `uniformity` 并列）和一个 capability 枚举。
- 反射在三处构建：`reflect` crate 只覆盖 IR 部分，`compile.rs` 拼 push 字段类型字符串、从源码切属性文本、通过 `reflect::Front` 塞进 static 参数、导出和 capability；`target_constants` 总是空。`Stage` 有三个：`ir/src/module.rs:80`、`hir_ty/src/attrs.rs:16`、`runtime/engine/shader/src/reflect.rs:53`。`reflect` 应该是唯一的生产者，Stage 只留一个。

### 9.4 C4：`compile.rs` 是 god function；消费方抵消了 session 设计（P2）

- `shaderc/src/compile.rs` 同时做名字查找（`find_item` 在 query 系统之外扫描每个包的每个 item tree，应该是 `hir_def` 里一个有记忆的 `ItemByPath` query）、管线编排与错误转换、反射组装。`run()` 应该只调用返回统一结果的阶段函数。
- `cook_shader` 每个包/入口新建 `Session`（`tools/cook_shader/src/lib.rs:166,252,394`），“cook 守护进程持有一个长生命周期增量 session”的设计从未被使用；为取 `fingerprint` 建一次性 `Session`（`:50-61`），指纹应该是 `Target` 的自由函数；`compile_all` 自己实现并行（`:~414`）；CRLF 归一化重复（`Session::new` 和 `cook_shader:~383`）；诊断以渲染好的字符串到达 cook（`:186,194`），而 `common/diag/src/lib.rs:5` 说 cooker 会把值转发给 marlin-diag。

### 9.5 C5：语言描述的接缝（P2）

- 0015 把 `Language` 放在 `shader/syntax`，代码里它在 `hir_ty/src/lib.rs:69`（因为 `Language` 依赖 `hir_def`）；名字存了两份（`Dialect.name`、`Language.name`）；共享 `Language` 里写了语言特例 `type_and_function_names: &["DiffPair"]`。把 `Language` 拆成叶子 crate（或移进 `front/syntax`），让每种语言的 `syntax` crate 拥有完整描述。
- 0015 §1 列出的 `mir`、`mono` crate 不存在，`lower` 直接 HIR → IR（`lower/src/expr.rs` 约 2k 行）。更新文档，或写明单态化在 lowering 里。
- `Name`/`NamespacePath` 是不带 interner 的 `Arc<str>`，IR 用 `arena::Interner`；编译器用 std `HashMap` 和自己的 `arena::Idx`。在 VM 语言（0018）共享 `compiler/front` 之前定下 no_std 容器的方向。

## 10. 收敛顺序

按“放着不管会恶化多快”排序。每一步都应该同时把对应规则加进 `xtask check`，否则会再次漂移。

1. **错误与日志（0023）**：每个 binary 统一安装 sink；必须记录的日志点补齐；插件边界改成类型化错误和 `Issue`；Drop 失败要记录。这一项**最先做**：后面每一步的调试都依赖它。
2. **`GpuLifetime`（R1）**：带着真实的错误 barrier 隐患。
3. **分层检查 + Module 内核下沉（L1、L2）**：不强制，依赖图每次提交都会继续烂。
4. **渲染层的共享服务（R2–R4）**：`ViewInfo` + `SceneTextures` + `PipelineRegistry` + `UploadQueue`。每多一个渲染 feature，重复就多一份，越早越便宜。
5. **资产类型关联与 cook key（A1、A2）**。
6. **framework：`launch()`、World 列表、渲染侧模块（F1–F3）**：在 launcher 和更多 editor 面板落地之前做。
7. **去重收尾（§8）**：handle、UUID、数学下沉、`ContentPath`、字节读写器、工具改用 `JobSystem`。
8. **编译器（§9）**：在 DXIL 后端或 VM 语言开工之前做。

## 11. 待定问题（需要拍板，按 multiple-choice 问答定）

1. Module 内核放哪里：新的 `marlin-module` crate（L2 或 L3 最底）？子系统统一“库 + 模块”两个 crate，还是模块 impl 放 feature 后面？
2. cvar 注册保留哪种：链接段 `declare!`（若保留则下沉到 L0 `marlin-rt::registry`，reflect 和组件也可用），还是模块作用域的 `add_cvars`（更符合“每个构建目标固定模块集合”，server 构建不会列出渲染 cvar）？
3. 几何基础类型下沉：把整个 `marlin-math` 移到 L0，还是只拆一个 L0 的几何小 crate？
4. render 的 cooked 格式：`marlin-render-asset` 独立 crate，还是 `render` 加 `gpu` feature（仿 `marlin-shader`）？
5. `marlin-std` facade：L3 也强制，还是放宽为“L3 可以直接依赖 base crate”？
6. 编译器 crate 前缀统一为什么（`marlin-lang-*` 全家，或按目录名）？
