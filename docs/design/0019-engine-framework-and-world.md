# 0019 — 引擎框架与 world：模块、World、实体数据、视图与输入

状态：草案 · 2026-10-19

前置：
- [0001](0001-repo-layout.md)：分层规则（§1.1），L3 crate 的原定分工（§5）
- [0003](0003-asset-pipeline.md)：`.masset`、cook、framework 的原定职责（§9）
- [0008](0008-rendering-overview.md)：帧流水（§3）、视图与 viewport（§5）
- [0009](0009-scene-and-geometry.md)：`RenderScene` 与更新流（§1）
- [0018](0018-vm-language.md)：Marlin script；本文定下它留下的对象模型（§12.3、§17）

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 玩法代码 | **只用 Marlin script**。原生 Rust 只写引擎模块，所有游戏共用一个 `marlin-game` 可执行文件 | 1 |
| 模块 | 引擎由一组**模块**组成，模块集合**按构建目标固定**（game / editor / server），在 umbrella crate 里静态列出，按依赖顺序初始化。项目**不能**在 `.mproject` 里开关模块，只能通过设置和 cvar 改变行为 | 3 |
| 作用域 | 模块状态分**引擎级**（每个进程一份）和**世界级**（每个 World 一份） | 3.3 |
| World | `marlin-world` 持有 ECS 世界、tick 调度、时间、关卡和 Transform 层级；一个进程里可以同时有多个 World（game、editor、PIE、预览） | 4 |
| Tick | 可变 dt 的 tick group，加上固定步长的 group（physics 等）：一帧内可以补跑多步，渲染端对结果插值 | 4.3 |
| 实体模型 | **ECS 平铺 + Prefab**：entity 就是一组组件，层级用 `Parent`/`Children` 表示；Prefab 可以嵌套和覆盖属性；脚本和 editor 看到的是对象化的 `Entity` 句柄 API | 5 |
| 组件数据 | 组件是带反射的 Rust 类型（或脚本里定义的 struct），关卡里存的是"类型 + 反射序列化的字段"。**不再有封闭的组件 enum** | 5.1 |
| 关卡 | **持久关卡 + 子关卡**，子关卡按需加载和卸载；流式 cell 只定接口，以后再做网格流送 | 5.4 |
| 坐标 | **世界坐标用 f64**（大世界坐标）；旋转和缩放用 f32；render 和 physics 内部用 f32，在边界处转成相对坐标 | 6 |
| 渲染归属 | **render 模块自己**持有渲染相关的组件和 extract system，产出 `RenderDelta`；world 不依赖 render。取代 0009 §1.3 | 7 |
| 视图 | **多视图**：`Camera` 组件加上 `ViewTarget`（窗口、分屏区域、离屏纹理）；每帧的 delta 带一个视图列表 | 8 |
| 输入 | **Input Action + Mapping Context**，都是 cooked 资产；上面再加一层**输入路由**，解决 editor、viewport、UI 和游戏之间的输入争夺 | 9 |
| 脚本对象模型 | **脚本组件 + 脚本 system**：`class X : Behaviour` 挂在 entity 上（托管对象，适合稀疏逻辑）；unmanaged struct 组件配合脚本 system（适合大批量的同类 entity） | 10 |
| 网络 | **现在预留，以后实现**：稳定的网络 id、复制标记、tick 编号、为预测和回滚留出位置、server 构建目标 | 11 |
| framework | 只剩 **Host**：平台、窗口、device、内容挂载、模块启动、主循环、输入路由。framework 里不定义任何组件 | 12 |

## 1. 为什么要重构

R0–R4 期间，`marlin-framework` 同时承担了 world、玩法和渲染接线的职责（R4f1，`327bd2d`）：

| 写死的东西 | 位置 | 问题 |
|---|---|---|
| 封闭的 `level::Component` enum（StaticMesh、Camera、Spin、FlyCamera、PointLight、SpotLight） | `framework/src/level.rs` | 关卡格式只认这 6 种组件；每加一种都要改 framework 和 cook 格式 |
| `Engine::spawn` 用一个大 match 把关卡数据转成运行时组件，"0 表示默认值"之类的约定也写在这里 | `framework/src/engine.rs` | 默认值没有和组件类型放在一起 |
| `Spin`、`FlyCamera` 以及它们的按键和灵敏度 | 同上 | 玩法逻辑写进了引擎 |
| system 是手动调用的方法（`self.spin(); self.fly()`） | 同上 | 没用上 `marlin-ecs` 的 `Schedule`、并行和访问检查 |
| extract（`StaticMesh`/`Light` → `SceneUpdates`）写在 Engine 里 | 同上 | 模拟侧需要知道渲染侧的数据格式 |
| `RenderSide` 负责把整个渲染管线接起来（VSM、ReSTIR、TSR……） | `framework/src/delta.rs` | 渲染器的内部组合放在了渲染器外面 |
| `run.rs` 管窗口、device、swapchain，按名字加载 resolve/bake/pairing 资产，热键写死，只支持 Win32 | `framework/src/run.rs` | 主循环知道得太多 |
| editor 直接用 framework 的内部接口（`engine_loaders`、`load_*`） | `apps/editor/src/main.rs` | editor 和 game 走两条路径 |
| 只渲染第一个相机；所有物体用同一个默认材质；`StaticMesh` 带 `color` | | 都是临时做法 |

`marlin-world` 只有空壳，VM 运行时也还没有。如果直接把 VM 接上去，脚本 API 会绑在这些临时结构上。所以要先搭好扩展点（模块、World、反射驱动的数据、render extract），再接 VM。

## 2. 分层与 crate

```
apps/game  apps/editor  apps/server(以后)
     │          │            │
     ▼          ▼            ▼
runtime/marlin（umbrella：每个构建目标的模块列表）
     │
     ▼
marlin-framework（Host：平台、device、主循环、输入路由、boot）
     │
     ├── marlin-render ─┐   marlin-ui-render   marlin-audio   marlin-physics   marlin-script（0018）
     │                  │         …                 │              │                │
     │                  ▼                           ▼              ▼                ▼
     │            marlin-world  ◄───────────────────────────────────────────────────┘
     │                  │
     ├── marlin-input-action ──► marlin-world
     ▼                  ▼
marlin-asset、marlin-material …        L2：marlin-ecs、marlin-reflect、marlin-serial、marlin-math …
```

- **`marlin-world`** 不依赖任何具体的子系统（render、physics、audio、script）。子系统依赖它：定义自己的组件，把 system 挂到它的 tick group 上。
- **`marlin-framework`** 是唯一知道"所有模块"的 L3 crate，但它只调用模块接口，不认识任何具体的组件类型。
- **umbrella `marlin`** 为每个构建目标列出模块（§3.2）。
- 新增的 L3 同层依赖（0001 §1.1 规则 3）在这里登记：
  - `marlin-world → marlin-asset`（关卡和 Prefab 是资产）；
  - `marlin-render`、`marlin-ui-render`、`marlin-audio`、`marlin-physics`、`marlin-anim`、`marlin-nav`、`marlin-vfx`、`marlin-script`、`marlin-input-action` → `marlin-world`（组件和 tick group）；
  - `marlin-framework` → 所有 L3 模块 crate。
  - 禁止反向依赖：`marlin-world` 不能依赖它们中的任何一个；子系统之间需要通信时，通过 world 里的组件和事件，或者 world 定义的 trait。
  - （W3）`marlin-renderer`（render 模块：组件、extract、`WorldRenderer`）→ `marlin-world`、`marlin-render`、`marlin-material`；`marlin-gameplay` → `marlin-world`、`marlin-input`；umbrella `marlin` 列出各构建目标的模块（`game_modules()`）。
- 新 crate `marlin-input-action`（`runtime/engine/input_action`）：action、mapping context 资产和求值（§9）。原来的 `marlin-input`（L1）只负责设备原始状态，不变。
- 这些边在 `cargo xtask check-layers` 里用白名单检查。

## 3. 模块

### 3.1 接口

```rust
pub trait Module: Send + Sync + 'static {
    /// 唯一名字，用于依赖声明、日志和 profile。
    const NAME: &'static str;
    /// 必须先初始化的模块。
    fn dependencies() -> &'static [&'static str] { &[] }

    /// 注册阶段：在任何 World 存在之前调用，只做登记，不碰 GPU 和 IO。
    fn register(r: &mut Registry);

    /// 引擎级启动：device 和内容已经就绪。返回这个模块的引擎级状态。
    fn startup(cx: &mut StartupContext) -> Result<Self, ModuleError> where Self: Sized;

    /// 每创建一个 World 调用一次：插入世界级资源（resource）。
    fn init_world(&self, world: &mut World) {}

    fn shutdown(&mut self, cx: &mut ShutdownContext) {}
}
```

`Registry` 能登记的东西：

| 登记项 | 例子 |
|---|---|
| 组件类型 | 带反射的组件，标明是否可以写进关卡、是否复制、存储方式（table / sparse set） |
| 资源类型和默认值 | 世界级的 `RenderWorld`、`PhysicsScene` |
| system | 挂到某个 tick group，带访问声明、顺序约束、运行条件（§4.3） |
| 资产 loader | 每个 cooked 类型由拥有它的模块注册（取代 `framework::loaders` 的统一列表） |
| cvar | 模块自己的 cvar；scalability group（`sg.*`）仍然是引擎级的，它设置的 cvar 分别归各个模块 |
| 输入 | 模块提供的默认 mapping context（例如 dev 工具的热键） |
| render feature | render 模块对外的注入点（0008 §5.5） |
| 脚本绑定 | 带 `#[script]` 的类型和函数（0018 §12.1） |

- 注册是**纯数据**：注册完成后得到一张不可变的 `ModuleTable`，World 用它构建调度表。所以同一个进程可以用同一张表创建任意多个 World。
- 模块之间不直接互相调用。需要合作时，一个模块定义 trait 或资源，另一个模块读取。例如 physics 写 `GlobalTransform`，render 读它。

### 3.2 每个构建目标一个固定的模块集合

```rust
// runtime/marlin/src/lib.rs
pub const GAME_MODULES: ModuleList = modules![
    AssetModule, WorldModule, InputActionModule, RenderModule, UiModule,
    AudioModule, PhysicsModule, AnimModule, ScriptModule, GameplayModule,
    #[cfg(not(marlin_build = "shipping"))] DevToolsModule,
];
// apps/editor 在此基础上加 editor 模块（editor/* 的 crate）。
// 以后的 server 目标去掉 Render、Ui、Audio、InputAction 和 DevTools。
```

- 依赖顺序在启动时做拓扑排序；有环或者缺少依赖就直接报错退出。
- **项目不能开关模块**，原因：
  1. 所有游戏共用一个 exe，模块总是编译进去的，关掉也不会让 exe 变小；
  2. 关卡和 Prefab 引用的是组件类型。如果组件集合随项目变化，cooked 数据就依赖项目配置，资产也没法在项目之间共享；
  3. AAA 引擎（UE）的核心模块也不能关，可以开关的只有插件。
- 项目通过**设置**影响行为：cvar、scalability、项目设置资产（例如不开音频设备、不开光追档位）。不会出现某个模块不存在的情况。
- 可选插件（第三方平台服务之类）以后单独设计，不和核心模块混在一起。

### 3.3 引擎级和世界级

| 作用域 | 生命周期 | 例子 |
|---|---|---|
| 引擎级 | 进程 | 资产服务器、render device 和 shader 库、音频设备、VM 的代码镜像、输入设备 |
| 世界级 | 一个 World | `RenderScene`（0008 §5.1）、physics scene、导航网格、脚本的托管对象、输入玩家 |

- 世界级状态是 World 里的 resource，由 `init_world` 插入。system 只能通过 resource 访问模块的状态，不持有对模块对象的引用，所以访问检查（`marlin-ecs` 的 `Access`）覆盖了所有数据。
- 引擎级状态由 Host 持有；system 需要读取时（例如 extract 时读 render 的 shader 库），Host 在每帧开始时把一个只读引用作为 resource 放进 World。

## 4. World

### 4.1 对象

```rust
pub struct World {
    ecs: marlin_ecs::World,
    kind: WorldKind,          // Game、Editor、Pie、Preview
    role: NetRole,            // Standalone、Server、Client（§11）
    schedule: WorldSchedule,  // 由 ModuleTable 构建，按 tick group 组织
    levels: Levels,           // 持久关卡和已加载的子关卡（§5.4）
    // Time、FixedTime 等是 ECS resource
}
```

- 一个进程里可以有多个 World：独立游戏有一个 `Game`；editor 有一个 `Editor` World，再加上每个 PIE 会话一个 `Pie` World，以及资产编辑器的 `Preview` World。
- 每个 World 有自己的调度表和自己的世界级资源。它们共享引擎级状态（资产、device）。
- `WorldKind` 决定哪些 tick group 运行：`Editor` World 不跑玩法 group（不跑脚本 Tick，也不跑物理），但会跑 Transform 传播和 extract，所以 editor 里移动物体能实时看到。

### 4.2 帧

Host 每一帧对每个需要 tick 的 World 依次运行：

```
FrameStart        时间、输入求值（§9）、资产加载回调、子关卡加载和卸载
PrePhysics        大多数玩法逻辑：脚本 Tick 默认在这里
┌ 固定步长循环（0..N 次）──────────────────────────┐
│ FixedPrePhysics → Physics → FixedPostPhysics      │
└───────────────────────────────────────────────────┘
PostPhysics       读物理结果的玩法逻辑、动画（以后可能拆出 Animation group）
LateUpdate        相机、跟随
TransformPropagate 计算 GlobalTransform（§6）
Extract           render、audio 等把本帧的变化写成快照（§7）
FrameEnd          清理 change tracker、应用剩余的 Commands
```

- tick group 是 `marlin-ecs` 里的 system set。group 内部按访问声明并行；group 之间严格按顺序执行；结构性修改（`Commands`）在每个 wave 结束时应用（`marlin-ecs` 的现有规则）。
- 模块可以在某个 group 内部用 `before`/`after` 指定相对顺序，但不能新增 group。group 列表属于 world，变动时要修改本文档。
- 三级流水不变（0008 §3.1）：Extract 结束后，World 就可以开始下一帧，Render 在另一组 job 上消费快照。

### 4.3 时间与固定步长

- `Time` resource：`dt`（钳到 `t.max_dt`，默认 0.1 s）、不受缩放影响的 dt、`time_scale`、暂停标志、帧号。
- `FixedTime`：步长 `t.fixed_hz`（默认 60）、累加器、每帧最多 `t.max_substeps` 步（默认 4，超过的时间直接丢掉，防止螺旋式掉帧）、当前的**固定 tick 编号**（网络要用，§11），以及插值系数 `alpha = 累加器 / 步长`。
- 由固定步长 group 驱动的 entity 带 `Interpolated` 标记：保存上一步和这一步的 `GlobalTransform`，Extract 时按 `alpha` 插值。所以 60 Hz 的物理在 144 Hz 的屏幕上也是平滑的。
- 暂停的 World 不再推进 `Time`，但还会跑 `TransformPropagate` 和 `Extract`（暂停菜单下画面要能正常显示，editor 的调试相机也要能动）。

## 5. 实体、组件与关卡数据

### 5.1 组件就是数据

- 组件是 `#[derive(Component, Reflect)]` 的 Rust 类型，或者脚本里定义的 unmanaged struct（§10）。关卡和 Prefab 里，组件按"注册名 + 反射字段"存储，和 `.masset` 的其他对象一样（0003 §1.2：缺失的字段取默认值，未知字段保留，支持版本升级）。
- **同一个类型既是编辑数据，也是运行时数据。** 不再有"关卡组件 enum → 运行时组件"这一层转换：
  - 默认值写在类型的 `Default` 里，不再用"0 表示默认"；
  - 单位换算（流明 → 坎德拉、角度 → 余弦）由使用方完成，例如 render 的 extract；
  - 派生或缓存的数据（`Spin` 的初始旋转、渲染行号）放进另一个**瞬态组件**，由组件 hook 或 system 生成。瞬态组件不注册为可序列化。
- 组件注册时带上标志：`Saved`（可以写进关卡和 Prefab）、`Transient`（运行时专用）、`Replicated`（§11），以及 editor 提示（反射的 `FieldAttrs`）。

### 5.2 实体标识

| 标识 | 类型 | 稳定性 | 用途 |
|---|---|---|---|
| `Entity` | 下标 + 世代号 | 只在一个 World 的生命周期内有效 | 运行时引用、脚本句柄 |
| `EntityGuid` | 128 位 | 跨保存、跨 cook 稳定 | 关卡里的实体；Prefab 实例里的实体 = hash(实例 guid, Prefab 内的 guid) |
| `NetId` | 32 位 | 每个会话由 server 分配 | 网络复制（§11） |

- 数据里引用别的实体用 `EntityRef`，存的是 `EntityGuid`。spawn 时通过 World 的 guid 表解析成 `Entity`。目标所在的子关卡还没加载时，解析结果为空；目标加载后再解析。

### 5.3 Prefab

- Prefab 是一个资产：一棵 entity 子树（每个 entity 带 guid、名字、父子关系、组件）。
- 关卡和 Prefab 里都可以放 **Prefab 实例**：`PrefabInstance { Prefab = <link>, Transform, Overrides = [...] }`。
- 覆盖（override）的种类：
  - 设置组件的字段：`Set { Entity = <Prefab 里的 guid>, Component = Light { Intensity = 1200.0 }, Fields = ["intensity"] }`（具体规则见 §17 W2）；
  - 增加或删除组件；
  - 增加子 entity；
  - 删除 Prefab 里的某个 entity。
- 嵌套 Prefab 的覆盖从内往外依次叠加。editor 能显示哪些字段被覆盖了，也能把覆盖还原或者应用回 Prefab（和 UE 的 Blueprint/Prefab 工作流一样）。
- **cook 时展开**：cooker 把 Prefab 实例展开成平铺的 entity 列表，所以运行时 spawn 一个关卡不需要处理覆盖。Prefab 自己也会 cook 成同样的平铺格式，供运行时动态 spawn（`world.spawn_prefab(link, transform)`）。
- editor 加载的是源数据，不是 cooked 数据，这样才能保留 Prefab 链接和覆盖关系。

### 5.4 关卡和子关卡

- `Level` 资产 = 一组 entity（以及 Prefab 实例）+ 关卡设置（环境等也是组件，挂在一个 `LevelSettings` entity 上，不再是 `Level` 的特殊字段）。
- World 有一个**持久关卡**，可以按需加载和卸载**子关卡**：`world.load_sublevel(link, transform)` 返回 `SublevelId`；子关卡的 entity 带 `InLevel(SublevelId)`，卸载时一起 despawn。
- 加载是异步的：资产加载完成后，在下一帧的 `FrameStart` spawn。大关卡分多帧 spawn，每帧有时间预算（`w.spawn_budget_ms`）。
- **cooked 格式**：entity 按 archetype 分组，每组的组件按列存储（反射序列化的 blob；热点类型可以用 0003 的 Pod cooked 布局）。spawn 时每个 archetype 一次批量插入，不需要逐个 entity 改变 archetype。
- **流式 cell 只定接口**：
  - `StreamingSource` 组件：玩家、相机等位置会触发加载的东西；
  - `StreamingPolicy` trait：根据各个 source 决定哪些子关卡应该加载；
  - 现在只有一个手动策略（脚本或关卡显式加载）。网格式的自动流送（World Partition）以后在 `tools/cook_level` 里加 cell 切分，再加一个网格策略。

## 6. 坐标与 Transform

```rust
/// 相对父节点（没有父节点时相对世界）。
pub struct Transform { pub translation: DVec3, pub rotation: Quat, pub scale: Vec3 }
/// 世界空间，由 TransformPropagate 计算。
pub struct GlobalTransform { pub translation: DVec3, pub rotation: Quat, pub scale: Vec3 }
```

- 位置用 f64，旋转和缩放仍然用 f32，和 UE5 的大世界坐标一样。`marlin-math` 增加 `DVec3`，以及必要的 f64 运算（加减、与 f32 向量混合运算、转换成相对坐标）。
- 传播在 `TransformPropagate` group 里：从根节点深度优先遍历，只重写确实移动了的 `GlobalTransform`（自己的 `Transform` 变了、父节点变了、祖先移动了），所以 extract 用 `Changed<GlobalTransform>` 拿到的正好是移动过的实体。W1 的实现是单线程的独占 system，每帧会访问所有带 Transform 的实体；等 profile 显示它是瓶颈时再按根节点并行。
- 和 f32 系统的边界：
  - **render**：`RenderScene` 里实例的位置存成 **tile（`IVec3`）+ tile 内的 f32 偏移**；视图也有自己的 tile。shader 先用整数算出两个 tile 之差，再加上 f32 偏移，得到相对相机的 f32 坐标。这样不需要原点重定位，也不会因为相机移动而重写所有实例。具体格式在 0009 里改。迁移期间先用 tile = 0，结果和现在一样。
  - **physics**：physics scene 用 f32，相对一个场景原点；原点怎么选（以及要不要分区）由以后的 physics 文档决定。接口只传 f64 的世界坐标。
  - **audio、nav**：同样在各自的边界转换。

## 7. render 模块持有 extract

- 渲染相关的组件都定义在 `marlin-render`（或它的子模块）里：

| 组件 | 取代 | 内容 |
|---|---|---|
| `MeshRenderer` | `StaticMesh` | 网格链接、每个 section 的材质实例链接、阴影和可见性标志、自定义数据（0011 §7.3，原来的 `color` 改用它） |
| `PointLight`、`SpotLight`、`DirectionalLight` | `Light` 和关卡环境里的太阳 | 物理单位（流明、勒克斯、度），extract 时再换算 |
| `Camera` | 同名的 render 类型 | 投影、曝光、优先级、`ViewTarget`（§8） |
| `SkyAtmosphere`、`Fog`、`PostProcessVolume` …… | `LevelEnvironment` | 0013 的环境参数 |

- render 的 extract system 运行在 `Extract` group 里：用 change detection 生成 `SceneUpdates`，处理网格上传，收集视图列表，产出 `RenderDelta`。entity 和渲染行的对应表放在世界级 resource `RenderWorld` 里（原来 `Engine` 里的那几个 `HashMap`）。
- **`RenderSide` 搬进新的 `marlin-renderer`**（render 模块的 crate），成为 `WorldRenderer`：它持有 `RenderScene` 和各个渲染特性的持久状态，每帧接收 delta、建图。这就是 0008 §5.3 的 `SceneRenderer` 的落地。没有放进 `marlin-render`，是因为 `marlin-material` 依赖 `marlin-render`，而 `WorldRenderer` 两个都要用；`marlin-render` 仍然只管场景和各个 pass（W3 的实现，见 §17）。
- render 需要的引擎 shader 和资产（库、resolve、bake、pairing pattern）由 `RenderModule::startup` 按包名加载（0003 §4.4），主循环不再知道它们。
- `RenderScene` 仍然不依赖 ECS（0009 §1.3 的前半句保留）：editor 的材质球和缩略图可以直接构造它，不需要 World。0009 §1.3 的后半句（"ECS 组件和 extract 放在 framework"）由本节取代。
- 其他子系统（audio、vfx、以后的 physics 调试绘制）用同样的模式：组件和 extract 都在自己的模块里。

## 8. 视图

- `Camera` 组件带一个 `ViewTarget`：

```rust
pub enum ViewTarget {
    /// 玩家的视口：由 Host 根据本地玩家和分屏布局决定是哪个窗口的哪一块。
    LocalPlayer(u8),
    /// 某个窗口的一个矩形区域（归一化坐标）。
    Window { window: WindowId, rect: Rect },
    /// 离屏渲染目标：小地图、监视器画面、截图。
    Texture(Handle<RenderTarget>),
}
```

- 每个本地玩家有一个"当前视图相机"（`view_camera: Entity`），由脚本切换；以后可以加相机混合。
- Extract 生成**视图列表**：每个视图有稳定的 `ViewId`（相机的 `EntityGuid`，或者 editor 视口自己的 id）、相机参数（相对视图 tile 的坐标，§6）、目标和区域、调试模式。render 根据 `ViewId` 找到对应的 `ViewState`（0008 §5.1）。
- editor 的视口相机**不是** entity：它属于视口（0008 §5.4），由 Host 把它作为额外的视图加进那个 World 的视图列表。所以编辑器相机不会被保存进关卡，也不会被脚本看到。
- "只渲染第一个相机"的规则删除。

## 9. 输入

### 9.1 Action 与 Mapping Context

- **Input Action** 资产：名字、值类型（bool、轴 1D/2D/3D）。
- **Mapping Context** 资产：一组"按键或摇杆 → action"的映射，每个映射可以带：
  - modifier：死区、取反、交换轴、缩放、平滑；
  - trigger：按下、松开、按住（时长）、点按、组合键（chord）、连按。
- 每个本地玩家有一个 context 栈，带优先级。高优先级的 context 用掉的按键，低优先级的 context 就看不到（例如驾驶 context 盖住步行 context）。
- 求值发生在 `FrameStart` 的 `InputActionModule` system 里，结果是每个玩家一个 `ActionState`（当前值、本帧开始/结束的事件）。脚本只能读 action，看不到具体按键；按键重绑定只需要修改玩家的 context 覆盖。
- 晚更新相机（0008 §3.4）：Extract 时再读一次最新的 look action 更新相机，只影响视图，不影响游戏状态。

### 9.2 输入路由

原始事件经过路由才到达 action 求值。路由分层，从上往下：某一层用掉（consume）的事件，下面的层就看不到。

| 层 | 内容 | 什么时候用掉事件 |
|---|---|---|
| 1. 全局命令 | editor 的少数全局键：结束 PIE、弹出（eject）、释放捕获 | 任何时候都优先，绑定的键很少 |
| 2. UI 焦点 | editor 控件和游戏内 UI | 文本框有焦点时用掉所有键盘输入；鼠标按命中测试交给最上面的控件 |
| 3. Viewport | 每个 viewport 一个状态机（§9.3） | 取决于状态 |
| 4. World 玩家 | 本地玩家的 context 栈 | action 求值 |

- editor 自己的快捷键也是 action 和 mapping context，只是放在第 1、3 层的 editor context 里，不另外实现一套快捷键系统。
- 独立运行的游戏没有第 1 层；第 2 层是游戏内 UI；第 3 层只有一个窗口，状态永远是"游戏捕获"。game 和 editor 走同一条路径。

### 9.3 Viewport 状态

| 状态 | 怎么进入 | 鼠标 | 键盘 |
|---|---|---|---|
| 未聚焦 | 默认 | 只有悬停（高亮、提示） | 不接收 |
| 聚焦 | 点击 viewport | editor 的视口操作：选择、gizmo | editor 的视口快捷键（W/E/R、F 聚焦……） |
| 临时捕获 | 编辑模式下按住右键（飞行导航）；preview 里拖拽旋转 | 锁定并隐藏，松开后还回去 | 导航期间 WASD/QE 归相机 |
| 游戏捕获 | 点击 PIE viewport；独立游戏中始终如此 | 由游戏的 input mode 决定 | 全部交给游戏，第 1 层的全局键除外 |

- **input mode**（脚本可以设置）：`GameOnly`、`UiOnly`、`GameAndUi`，以及鼠标是否锁定、是否显示。例如打开游戏内菜单时切到 `UiOnly`，视角不会再跟着鼠标动。
- **释放捕获**：PIE 中按 Shift+F1 把鼠标还给 editor，游戏继续运行；Esc 留给游戏自己用；结束 PIE 用第 1 层的另一个全局键。
- **设备分配**：键鼠和第一个手柄给玩家 0；其他手柄按"按键加入"或脚本的分配规则给后面的玩家。每个 PIE World 有自己的玩家，多个 PIE 窗口互不干扰。
- 窗口失去焦点时，所有捕获都释放，按住的键按松开处理（不会出现按键卡住）。

## 10. 脚本对象模型

0018 §12.3 和 §17 留下的问题在这里定下。

### 10.1 两种写法

| 写法 | 定义 | 存储 | 适合 |
|---|---|---|---|
| **Behaviour** | `class Door : Behaviour` | 托管堆上的对象；ECS 里存的是指向它的压缩引用 | 门、开关、任务逻辑：数量少、逻辑多、经常引用其他对象 |
| **struct 组件 + system** | `struct Velocity : IComponent`（unmanaged），`[System(Group = PrePhysics)] static void Move(Query<ref Transform, in Velocity> q)` | 直接存在 ECS 的表里，和 Rust 组件一样 | 子弹、群体、大量同类 entity；可以写成受检 job（0018 §7） |

- 两种组件都按 §5.1 注册：带反射、可以写进关卡和 Prefab、editor 可以编辑。脚本类型在加载代码镜像时动态注册（`marlin-ecs` 的 `register_reflected`）。
- Rust system 和 editor 通过 `DynamicQuery` 和反射看到脚本组件；脚本通过生成的绑定（0018 §12.1）看到 Rust 组件。

### 10.2 Behaviour 的生命周期

| 回调 | 时机 |
|---|---|
| `OnCreate` | entity spawn 完成后（同一批 spawn 的所有组件和子节点都已存在） |
| `OnEnable` / `OnDisable` | 启用状态变化时 |
| `Tick(dt)` | 每帧，在 `[TickGroup(...)]` 指定的 group 里（默认 `PrePhysics`） |
| `FixedTick(dt)` | 每个固定步长，在 `FixedPrePhysics` 里 |
| `OnDestroy` | despawn 之前；这时挂在这个对象上的 `async` 函数会被取消（0018 §17 的另一个待定项） |

- 每个 group 有一个独占的脚本 system，在 gameplay 线程上按 entity 的 spawn 顺序调用这些回调，所以结果是确定的。托管堆只在这个线程上修改（0018 §7.1）。
- 结构性修改（spawn、despawn、增删组件）在脚本里是立即可见的句柄操作，底层走 `Commands`，在当前 group 结束时应用；脚本里 spawn 的 entity 立即可以拿到句柄，并设置组件。
- 脚本看到的 `Entity` 是值类型的句柄：`GetComponent<T>()`、`AddComponent<T>()`、`Destroy()`、`IsAlive`。访问已销毁的 entity 抛 `ObjectDestroyedException`（0018 §12.3）。

## 11. 网络（预留）

现在只预留，不实现。实现另写一篇文档。

- **模型**：server 权威的复制，客户端对自己控制的对象做预测。
- **现在要留好的东西**：
  - `NetId` 组件（§5.2），由 server 分配；
  - 组件和字段的复制标记（`Replicated`、字段级的反射属性），复制条件以后再定；
  - World 的 `NetRole`（Standalone、Server、Client），运行条件可以按角色开关 system；
  - 固定步长的 tick 编号（§4.3）：预测和回滚以它为准；
  - 预测组件的状态可以通过反射做快照和恢复（回滚需要）；脚本的跨平台位级确定性（0018 §15）在这里起作用；
  - server 构建目标（§3.2）：没有 render、UI、audio 和输入路由。
- 玩法代码现在就按"状态放在组件里、通过 action 读输入"来写。这样以后加网络时，基本不需要改玩法代码。

## 12. framework 与 Host

`marlin-framework` 只剩下 Host：

1. **boot**：游戏从 stage 出来的目录按约定挂载 `content/paks/` 里的包，应用包里 cook 好的 config，加载 `game.default_level`；editor 读工程的 `config/`，挂载 cookd（0003 §4.6，2026-10-19 修订，原来是 `boot.toml`）。
2. **平台**：`App`、窗口、surface。`Surface → SurfaceHandle` 的转换移到 L1（`marlin-app` 和 `marlin-rhi` 之间），Host 里不再出现 `unreachable!("only Win32")`。
3. **device**：RHI instance、device、每个窗口的 swapchain、`FrameContext`。
4. **模块**：注册 → 启动 → 每个 World 的 `init_world`。
5. **World**：game 目标创建一个 Game World 并加载入口关卡；editor 目标交给 editor 模块。
6. **主循环**：
   - 读取事件 → 输入路由；
   - 对每个 World：在 job 上跑调度表，直到 Extract；
   - 渲染上一帧所有 World 的快照和 UI，然后 present；
   - 继续保留 Game N+1 ∥ Render N 的流水（`run.rs` 现在的做法）。
7. **dev 工具**：stats overlay、F3、标题栏帧率、vsync 热键都移进 `DevToolsModule`，热键改成 action，shipping 构建里没有这个模块（现在 `dev.rs` 的 stub 做法改成"不在模块列表里"）。
8. **退出**：Esc 退出不再写死。游戏是否、何时退出由脚本决定；dev 构建的默认 context 里有一个退出 action。

`Spin` 和 `FlyCamera` 不是 demo 专用，引擎也需要这类基础组件（UE 有 `RotatingMovementComponent` 和 `SpectatorPawn`）。它们移到 `GameplayModule`（`runtime/engine/gameplay`，新 crate），改名为 `RotatingMovement` 和 `SpectatorMovement`，输入改用 action。

## 13. Editor

- editor 也是一个 Host，模块列表 = game 模块 + editor 模块。它不再直接调用 framework 的内部函数。
- **Editor World**：加载**源**关卡（保留 Prefab 链接），`WorldKind::Editor`，不跑玩法 group。
- **PIE**：把 Editor World 的 entity 按 cooked 关卡的格式做一个快照（展开 Prefab，和 cook 走同一段代码），再 spawn 到一个新的 `Pie` World。这样 PIE 的加载语义和正式游戏相同，PIE 里的修改也不会影响编辑数据。
- **Preview World**：资产编辑器需要完整场景时（Prefab 编辑器、动画预览）用一个小的 `Preview` World；只要画一个网格或材质时直接用 `RenderScene`（0009 §1.3）。
- 每个 editor 视口持有：自己的 `ViewState`、编辑器相机、所属 World、§9.3 的输入状态。

### 13.1 Host 的统一形态（W5 定下）

game 和 editor 共用**一个** Host 循环，不各写一份。差别只在 `HostApp` 的三个部分里：

```rust
pub trait HostApp {
    type Sim: Send;     // 游戏侧：World、editor 的文档和模型、UI 录制；每帧在 job 上运行
    type Frame: Send;   // 一帧的快照：各 World 的 RenderDelta、视图、UiDrawList、光标请求
    type Render;        // 主线程：各 World 的 WorldRenderer、窗口的 UiRenderer
    fn simulate(sim: &mut Self::Sim, cx: &mut SimContext) -> Self::Frame;
    fn prepare(render: &mut Self::Render, cx: &mut RenderContext, frame: &Self::Frame) -> Result<..>;
    fn render<'g>(render: &'g mut Self::Render, frame: &'g Self::Frame, g: &mut RenderGraph<'g>,
                  windows: &mut [WindowTarget<'g>]);
    fn shutdown(render: &mut Self::Render, frames: &mut FrameContext) {}
}
```

- Host 负责：平台、窗口和 swapchain（resize、最小化、遮挡、vsync）、device、`FrameContext`、`Engine`、host 命令（§9.2）、dev overlay、帧节奏，以及 Game N+1 ∥ Render N 的流水。
- **Host 管理多个窗口**（0008 §6.4）：
  - 每个窗口有自己的 swapchain；一帧里所有未最小化的窗口都在同一张 graph 里画、一起 present。`render` 拿到的是这些窗口的 `WindowTarget`（id、大小、是不是主窗口、swapchain 图像），往里面画什么由 app 决定。
  - `SimContext.windows` 是每个窗口这一帧的状态（大小、缩放、焦点、最小化），app 在里面写回该窗口的光标模式。被用户关掉的窗口出现在 `closed` 里。
  - app 通过 `SimContext.requests` 打开或关闭窗口（`WindowRequest`）；host 在这一帧的事件处理完之后再执行。
  - 第一个窗口是主窗口：host 命令只看它，dev overlay 和截图只画在它上面，关掉它就退出。
  - game 只用主窗口；editor 以后的浮动面板和多个场景窗口都走这条路径。
- **UI 在游戏侧录制**（0008 §3.2），渲染侧只拿 `UiDrawList`。为此字形 id 表 `GlyphIds` 是共享的（`Arc`），录制和 `UiRenderer::prepare` 可以在不同线程上进行。
- **渲染侧随时可以为新的 World 建 `WorldRenderer`**（PIE 开始时）：render 模块启动后给出 `RenderShaders`（引擎 shader、默认材质和 pairing pattern 的共享副本），创建 renderer 不需要借用正在被模拟占用的 `Engine`。UI 模块同样给出 `UiAssets`（UI 字体）。
- game 的实现是 `GameApp`：一个 Game World、一个 `WorldRenderer`、窗口就是视口。editor 的实现是 `EditorApp`（`apps/editor`），由下面这些 editor crate 组成。

### 13.2 editor crate 的分工（W5 定下）

- `editor/core`：
  - `SourceContent`：项目的源内容索引加上 cook 的文本 schema，`update()` 重新扫描改动过的文件。
  - `LevelDocument`：从源数据读入的关卡和它用到的所有 Prefab（递归）。读源对象用的是 cook 的同一个函数（`marlin_cook_core::read_source_object`）；展开 Prefab 用的也是 cook 的同一段代码（`marlin_world::cook_level`）。以后的 undo/redo、选择集、命令系统都建立在文档上。
- `editor/viewport`：
  - `ViewportCamera`：编辑器相机，属于视口、不是 entity。它用引擎的 spectator 绑定（`Marlin.Engine.Input.Spectator`）求值自己的 `InputUser`，飞行的计算和 `SpectatorMovement` 共用 `marlin_gameplay::fly`。
  - `SceneViewport`：视口的 id、`ViewportInput`（§9.3）、相机，以及编辑或运行状态。它把自己的视图加进所属 World 的视图列表，目标是 `ViewTarget::Offscreen(id)`。
  - `PieSession`：Pie World。由 `LevelDocument::cook()` 得到的 `CookedLevel` spawn 出来，所以和正式游戏加载的数据完全相同；结束时直接丢弃。
- editor 的命令也是 action（`content/engine/input/Editor.masset`）：Alt+P 开始或结束 PIE，Shift+F1 在 PIE 中释放鼠标（游戏继续运行）。
- editor 的模块列表现在就是 game 的模块列表：还没有只属于 editor 的组件或 system。等有了（例如选择标记、gizmo），再以 editor 模块的形式加入，不提前建空模块。
- Preview World：`WorldKind::Preview` 和 `Engine::create_world` 已经够用；第一个资产编辑器出现时再接入，现在不建 UI。

## 14. 落地顺序

每一步结束时，`samples/basic` 和 `samples/lights` 的画面和现在一致（现有的 end-to-end 测试和截图比对作为验收）。

| 步骤 | 内容 | 验收 |
|---|---|---|
| **W1** World 核心 | `marlin-math` 的 `DVec3`；`marlin-world`：`World`、`WorldKind`、tick group 和 `WorldSchedule`、`Time`/`FixedTime` 和插值、`Transform`/`GlobalTransform` 传播、组件注册表（Saved / Transient / Replicated 标志）、`EntityGuid` 和 `EntityRef` | world 的单元测试：传播、固定步长、调度顺序 |
| **W2** 数据 | 新的关卡格式（反射组件列表、`LevelSettings`）；Prefab 资产、实例和覆盖；`tools/cook_level` 改成展开 Prefab 并按 archetype 输出；子关卡加载和卸载；迁移两个 sample 的 `.masset` | cook 测试；子关卡加载和卸载测试；sample 画面不变 |
| **W3** 模块与 render | `Module`、`Registry`、`ModuleTable`；Host；loader 分到各个模块；render 模块的组件和 extract；`RenderSide` → `WorldRenderer`；视图列表；`GameplayModule`；`DevToolsModule`；framework 里删除所有组件类型 | framework 不再定义组件；`check-layers` 加上 §2 的白名单 |
| **W4** 输入 | `marlin-input-action`（资产、cook、求值）；Host 的输入路由；viewport 状态；input mode | 路由测试：文本框焦点、捕获、释放、窗口失焦 |
| **W5** editor | editor 改用 Host；Editor/PIE/Preview World；视口相机；PIE 快照 | editor 能打开 sample、开始和结束 PIE |
| **W6** 脚本 | `marlin-script` 运行时（0018）；`ScriptModule`；Behaviour 和脚本 system；绑定生成 | sample 里的 `RotatingMovement` 用脚本重写一遍作为对照 |

W6 之后再回到 render（R4f 剩下的部分、R5）。physics、audio、anim 各自有自己的设计文档，按本文的模块和 world 接口接入。

## 15. 对其他文档的修改

- 0001 §5：`world/` 和 `framework/` 的说明改为本文的分工；新增 `input_action/`、`gameplay/`；§10 第 1 项（VM 执行策略）0018 已经定了。
- 0003 §9：framework 的职责以本文 §12 为准。
- 0008 §5.4：视口和视图列表以本文 §8 为准。
- 0009 §1.3：后半句由本文 §7 取代；实例位置的 tile 格式（§6）在 0009 里补上。
- 0018 §12.3、§17：对象模型和 `async` 取消由本文 §10 决定。

## 16. 待定

- 网络复制的具体设计（复制条件、相关性、带宽、预测和回滚的实现）：另写文档。
- 网格流送（World Partition）的 cell 切分和策略。
- 存档：哪些组件要存、格式（大概率是 `Saved` 组件的反射序列化加上 `EntityGuid`），等脚本可用后再定。
- 相机混合和镜头系统。
- 可选插件的机制（§3.2）。

## 17. 实施状态

### W1（2026-10-19）：World 核心

- `marlin-math::DVec3`：f64 位置，`relative_to(origin)` 先在 f64 里求差再舍入成 f32；`Quat * DVec3` 在 f64 里旋转。反射名 `marlin_math::DVec3`。
- `marlin-world`：
  - `World`：`WorldKind`（Game / Editor / Pie / Preview）、`NetRole`（只实现了 Standalone）、`WorldSchedule`（每个 `TickGroup` 一个 `marlin-ecs` 的 `Schedule`）。`World::frame` 按 §4.2 的顺序运行；Editor World 和暂停的 World 只跑 `FrameStart`、`TransformPropagate`、`Extract`、`FrameEnd`；最后调用 `clear_trackers`，所以世界级查询里的 "changed" 指的是"本帧"。
  - `Time`、`FixedTime`、`TimeSettings`（来自 cvar `t.max_dt`、`t.fixed_hz`、`t.max_substeps`；host 可以按 World 覆盖）。超出 `max_substeps` 的时间直接丢掉。
  - `Interpolated`：World 在每个固定步长前后记录 `Transform`；新加的实体从静止状态开始；`at(alpha)` 给出绘制用的变换。
  - `Transform` / `GlobalTransform`（f64 平移）和 `propagate_transforms`。父节点被删除或 despawn 后，子节点回到世界空间；没有 `Transform` 的祖先会切断传递链。
  - `ComponentRegistry`：`add`（带反射，`SAVED` / `REPLICATED` / `TRANSIENT`）、`add_with_storage`、`add_runtime`（不带反射，只用于运行时）；按反射名和类型查找；类型或名字重复时 panic。World 创建时把所有注册项安装进 ECS（存储方式加反射信息）。
  - `EntityGuid`（128 位；`derive(instance, inner)` 属于 cooked 数据格式，测试里固定了一个参考值）、`EntityRef`、`GuidIndex`：由 `EntityGuid` 的 `on_insert` / `on_replace` hook 维护，每次结构操作返回后就是最新的；重复的 guid 保留第一个并记录警告。
- 测试：`cargo test -p marlin-world --test unit`（传播、精度、只写移动过的实体、换父节点、tick 顺序、暂停和 editor、固定步长、插值、guid 索引、注册表）；`marlin-math` 的 `dvec` 测试。
- 还没做：模块接口（W3）会把 `ComponentRegistry` 和 system 的登记接到 `Module::register` 上；现在由调用方直接调用 `World::add_system`。

### W2（2026-10-19）：关卡、Prefab 和子关卡

- **通用的组件值**：
  - `marlin-reflect` 新增 `ReflectDynamic` 类型数据（`DynamicHolder`）：一个 opaque 类型可以装一个运行时才确定类型的值。
  - `marlin-serial` 把它写成 CUSTOM：holder 的类型 id + 长度，里面是被装值的类型 id、签名和值；类型 id 为 0 表示空。读的时候遇到没注册或 holder 不接受的类型，就保持原值不变，后面的数据照常读。
  - `.masset` 把它写成 `Light { Intensity = 800.0 }`（unit struct 写成 `Tag`，空的写成 `None`），类型名必须在 schema 里。`.masset` 也支持 `ReflectProxy`。
  - `marlin-world::ComponentData` 就是这样一个容器，只接受 `ComponentRegistry` 登记过（带 `IsComponent` 类型数据）的类型，所以里面的值一定是 `Send + Sync`。
- **数学类型的文本形式**：`Vec2/3/4`、`IVec*`、`UVec2`、`DVec3`、`Quat` 带 `meta(text = "tuple")`，`.masset` 写成 `(x, y, z)`。`Transform` 的旋转在文本里是四元数 `(x, y, z, w)`；编辑器负责显示欧拉角。
- **`EntityGuid` 包一个 `marlin_asset::Uuid`**，文本里是 UUID 字符串。
- **源数据**（`marlin-world::level`）：
  - `Level` / `Prefab` = `Entities`（`EntityDesc`：`Guid`、`Name`、`Parent`、`Components`）加 `Instances`（`PrefabInstance`：`Guid`、`Name`、`Parent`、`Prefab`、`Transform`、`Overrides`）。
  - 覆盖有三种：
    - `Set { Entity, Component, Fields }`：`Fields` 是反射路径（`"intensity"`、`"translation.x"`）；`Fields` 为空时整个组件替换；目标实体没有这个组件时整个加上。
    - `RemoveComponent { Entity, Component }`：按反射名匹配，完整名或短名都可以。
    - `RemoveEntity { Entity }`：连同子孙一起删除。
  - 覆盖里的 `Entity` 是 Prefab 自己看到的 guid（嵌套实例里的实体用派生 guid）。
  - Prefab 必须正好有一个根。实例的根实体使用实例的 guid、parent、transform（以及非空的 name），其余实体使用 `EntityGuid::derive(实例, 内部 guid)`。
  - 给实例"增加子实体"不需要单独的覆盖：在关卡里放一个 parent 指向派生 guid 的实体就行。
- **`flatten`** 先展开嵌套实例（内层的覆盖先应用），再应用外层的覆盖。会报的错误：Prefab 缺失、循环引用、根不止一个、guid 重复、同一实体有两个同类型组件、覆盖的目标或路径不存在。cooker 和以后的 PIE 共用这段代码。
- **`CookedLevel`**：按组件类型集合分成 archetype，每个 archetype 按列存 `ComponentData`；Prefab 的 cooked 结果带上根实体的 guid。
  - 运行时每个实体用 `marlin-ecs` 新增的 `insert_cloned` 插入：克隆所有组件后一次 archetype 移动。
  - 列里的每个元素都带自己的类型 id。等需要时再换成按列的 Pod 布局。
- **运行时**（`marlin-world::levels`）：
  - `World::spawn_level`（立即）、`load_sublevel` → host 调用 `provide_level` → `FrameStart` 里按 `w.spawn_budget_ms` 分帧 spawn、`unload_sublevel`、`spawn_prefab`。
  - 实体带上 `EntityGuid`、`InLevel`，以及非空的 `EntityName`。
  - 子实体比父实体先 spawn 时，等整个关卡 spawn 完再挂到父实体上；父实体不在这个关卡里时记录警告。
  - 子关卡的放置变换只作用在根实体上。
- **Cook**（`marlin-cook-level`）：
  - `register(cookers, registry)` 登记 `Level`、`Prefab` 两个 cooker，并把 SAVED 组件类型加进文本 schema。
  - 所有 SAVED 组件的反射结构指纹放进 cook key：组件结构一变，关卡就会重新 cook。
  - Prefab 是 build dependency，cook 时递归读取。Prefab 改了，放置它的关卡（包括间接放置的）都会重新 cook。
  - parent 指向关卡外的实体时，cook 记录警告。
- **Framework 过渡**（W3 会把这些移进各自的模块）：
  - `Engine` 改用 `marlin_world::World`，组件改为带反射、在 `Default` 里给默认值的类型：
    - `StaticMesh` → `MeshRenderer`；
    - `Spin` → `RotatingMovement`（加上运行时的 `RotatingBase`）；
    - `FlyCamera` → `SpectatorMovement`（加上运行时的 `SpectatorLook`）；
    - `Camera` 改用度数；
    - `PointLight` / `SpotLight` 用流明和度数，不再用"0 表示默认值"；
    - 关卡的 `Environment` → 一个带 `LevelSettings` 组件的实体。
  - rotate 和 spectate 挂在 `PrePhysics`，render extract 挂在 `Extract`，extract 读的是 `GlobalTransform`。
  - 网格资源一旦被用过就一直保留，关卡重载时不会重新上传。
  - `engine_components()` 同时给 World 和 cooker 用。
- **样例迁移**：`samples/basic`、`samples/lights` 的关卡用一次性脚本转换成新格式：
  - guid 是确定的：`<样例前缀>-8001-<序号>`；
  - 原来的欧拉角换算成四元数；
  - 环境参数移到 `LevelSettings` 实体上。
- **验证**：
  - 单元测试：`marlin-world`（展开、覆盖、错误、archetype、spawn、流式加载、卸载、Prefab）、`marlin-cook-level`（完整 cook 流程、嵌套覆盖、改 Prefab 后重新 cook、错误）、`marlin-masset`（动态值、元组、proxy）、`marlin-serial`、`marlin-ecs` 都通过。
  - GPU 测试：framework 的 end-to-end 测试全部通过，包括 golden image（basic 样例的画面和迁移前一致）和改文本后画面实时更新；editor 的 GPU 测试通过。

### W3（2026-10-19）：模块、Host 和 render 模块

- **模块系统**（`marlin-world::module`）：
  - `Module` trait：必须实现的只有 `name`、`register`；可选的有 `dependencies`、`startup`、`init_world`、`pre_world_frame`、`post_world_frame`、`world_ready`、`shutdown`。
  - 和 §3.1 草稿相比，多了两个"世界帧前后"的钩子：需要引擎级状态的工作（通过资产服务器加载关卡和网格）在这里做；还多了 `world_ready`，host 用它判断"关卡已加载"。
  - `Registry`：组件（`ComponentRegistry`）、按 tick group 登记的 system 工厂（每个 World 一份 system）、loader 工厂（可以拿到引擎资源，例如 blob streamer）、cvar。
  - `Modules::new`：按依赖排序（不受依赖约束时保持给定顺序）；重名、缺少依赖、循环依赖都会报错；所有登记冻结成 `ModuleTable`，再登记 cvar。
  - `EngineContext`（资产服务器、vfs、job、`Resources`）和 `Resources`（按类型存放的引擎级资源）。
  - **注册必须在 boot 之前**：boot 应用 config 时，未知的 cvar 会被报出来（2026-10-19 起是警告，0003 §4.6），而且模块的 cvar 要先存在才收得到值，所以 host 先调用 `Modules::new`，再 boot，最后 `Engine::new(vfs, modules)` 启动各模块。
- **`WorldModule`**（world）：
  - 登记 `CookedLevel` 的 loader 和 `t.*`、`w.*` cvar。
  - 在 `pre_world_frame` 里替子关卡的请求加载资产并交给 World。
  - 资产重新加载（文本被编辑）时，在原来的位置重新 spawn。加载失败的子关卡会被卸载。
- **`marlin-renderer`（新 crate，`RenderModule`）**：
  - 组件 `MeshRenderer`、`Camera`（新增 `target: ViewTarget`、`priority`）、`PointLight`、`SpotLight`、`LevelSettings`。文本名不变，反射全名改为 `marlin_renderer::*`，cooked 数据会因为 key 变化自动重新 cook。
  - extract system 挂在 `Extract` 组。
  - loader：网格（流式）、`Texture` / `PairingPattern`（`InPlace`）、`ShaderLibrary`。
  - `startup` 按包名加载 `RenderAssets`（引擎 shader、默认材质的 resolve 和 bake、pairing pattern），放进引擎资源。
  - `post_world_frame` 加载 extract 新遇到的网格，生成 `RenderDelta` 留在 World 里，由 host 用 `take_delta` 取走。
  - 画质 cvar（`sg.*`、`r.*`、dev 的 `r.sharc.view`）由这个模块登记，`settings::RenderSettings` 每帧读入 `WorldRenderer`。
- **`WorldRenderer`**（原来的 `RenderSide`）：
  - `new(device, assets, &RenderAssets, format, scene_ids(world))`；`with_shaders(...)` 是原来那个显式传 shader 的版本。
  - `apply` 记下 delta 里的视图，以及每个视图上一帧的相机。
  - `add_view_passes(g, view, target)`：exposure、TSR、去噪和 ReSTIR 的历史都按视图 id 区分，specular 信号用 id 加上最高位。`add_passes` 只画 `ViewTarget::Main`。
  - `Offscreen` 也移到这个 crate。
- **视图列表**：
  - 每个 `Camera` 都会成为 `RenderDelta::views` 里的一个视图，按 priority 降序排列（相同时按 id 排）。
  - `ViewId` 来自相机实体的 `EntityGuid`（`Uuid::hash64`），没有 guid 时用实体本身。
  - `delta.view(target)` 和 `WorldRenderer::view_for(target)` 选出该 target 优先级最高的视图。
  - 现在 host 只画 `Main`；`LocalPlayer`、`Offscreen` 等 W4/W5 有了分屏和 editor 视口再接入。
- **`marlin-gameplay`（新 crate，`GameplayModule`）**：`RotatingMovement`、`SpectatorMovement` 和它们的 system（`PrePhysics`），输入读取 World 里的 `InputState` resource（由 host 每帧设置，W4 换成 action）。
- **`UiModule`**（ui_render：font loader、`load_ui_font`）和 **`AnimModule`**（anim：skeleton 和 clip 的 loader）。
- **`marlin_asset::InPlace<T>` + `CookedAsset`**：各资产类型在自己的 crate 里提供视图并做校验；`Loaders::register_in_place::<T>()`。原来 framework 里那张统一的 loader 列表取消了。
- **framework 只剩 Host**：
  - `Engine` 持有模块、资产服务器、blob streamer 和引擎资源；World 归调用方。`create_world`、`frame(world, dt, input, jobs)`、`is_ready(world)`、`loaders()`（给另外的资产服务器用）。
  - `run(boot, title, modules)`：建一个 Game World，加载入口关卡；世界帧在 job 上跑，`WorldRenderer` 同时画上一帧。
  - `DevToolsModule` 登记 `r.stats`。build 里有这个模块时，host 才画 stats overlay；shipping 的模块列表里没有它。
  - framework 不再定义任何组件。`boot` 只登记 game、window 相关的 cvar（2026-10-19 起没有 content 的了，0003 §4.6）。
- **umbrella `marlin::game_modules()`**：World、Anim、Ui、Render、Gameplay，以及（非 shipping 时的）DevTools。
  - `apps/game`、editor、cooker（`game_components()`，cook 时和游戏用同一个组件表）都从这里取模块。
  - editor 改用 `Engine` + World + `WorldRenderer`；UI 的 shader 和字体用它自己的资产服务器加载。
- **验证**：
  - 单元测试：world（模块排序、错误、钩子顺序、system 和组件来自 table、`WorldModule` 从资产加载子关卡、加载失败时卸载）、renderer（extract、视图列表、资产按名字加载）、gameplay、framework（boot、UI 字体、quality preset）都通过。
  - GPU 测试：framework 的 end-to-end（golden image 与之前一致、cookd 和打包的结果逐像素相同、改文本后画面实时更新、exposure 稳定、resize 时 validation 安静）和 hud 测试通过；editor 的 GPU 测试通过。
  - 构建：workspace check、clippy、check-layers 都干净；shipping 构建能编译。
  - 手动冒烟测试：cook 并打包 `samples/basic` 后运行 `marlin-game` 15 秒，日志显示 "level loaded: 13 entities, 3 meshes"，运行期间没有错误。`marlin-game` 本身仍然没有自动化测试。
  - 冒烟测试中发现的问题：只重新打包、没有重新 cook 时，包里还是旧组件类型 id 的关卡，这些组件读回来是空的，实体会悄悄缺少组件。现在 `CookedLevel::unknown_components` 会统计这类组件，spawn 关卡、子关卡或 Prefab 时记录警告，提示重新 cook。

### W4（2026-10-19）：输入 action 和路由

- **`marlin-input-action`（新 crate）**：
  - **资产**：
    - `InputAction { Value = Bool | Axis1D | Axis2D | Axis3D }`。
    - `MappingContext { Mappings = [{ Action, Source, Modifiers, Triggers }] }`。
    - 数据源（`InputSource`）：`Key("Left Shift")`，名字不对时在读文本时就报错（`KeyCode` 通过 proxy 写成字符串）；`Mouse(Right)`、`MouseMotion`、`MouseWheel`、`Pad(Start)`、`LeftStick` / `RightStick`、`LeftTrigger` / `RightTrigger`。
    - modifier：`Negate`、`Swizzle(Yxz…)`、`Scale { Factor }`、`ScaleByDeltaTime`、`DeadZone { Lower, Upper }`。
    - trigger：`Down`（不写时的默认）、`Pressed`、`Released`、`Hold { Seconds }`、`Tap { Seconds }`、`Chord(<action>)`。
    - 两种资产都用 `register_named_generic` cook（按原样序列化），引擎按 `!Name` 找自己的那些。
  - **`InputFrame`**：一个用户一帧的原始输入，由 `InputState` 按设备生成（`Devices`：键鼠、第几个手柄）。在一帧内按下又松开的键也算按下，所以不会丢掉短按。路由的每一层可以 `consume` 掉自己用过的输入源。
  - **`InputUser`**：按优先级排列的 `CompiledContext` 栈。
    - 每个 mapping 依次读输入源、应用 modifier、判断 trigger：显式 trigger 只要有一个触发即可；chord 必须全部满足。
    - action 只要有一个 mapping 触发就算触发，值是这些 mapping 的值之和（按钮型 action 为 1）。
    - 触发了的 mapping 会消耗它的输入源，优先级更低的 context 和下一层路由都看不到。
    - chord 在所有 context 都读完后才判断，所以带 chord 的 mapping 只对下一层消耗输入，不会对同一用户里优先级更低的 context 消耗（Alt+Enter 中的 Enter）。
    - `ActionState` 有 `value`、`triggered`、`started`、`completed` 四项。
  - **`InputModule`**：
    - World 资源 `Players`：默认 1 个玩家；host 用 `set_input` 把路由后的输入交给玩家，`FrameStart` 用不受时间缩放影响的 dt 求值，所以暂停时菜单仍然能用。
    - 玩家通过 `add_context(asset, priority)` 请求 context，模块在 `pre_world_frame` 里加载、编译后加入栈。`world_ready` 会等这些 context 加载完。
    - World 资源 `InputSettings { mode: GameOnly | UiOnly | GameAndUi, cursor: Visible | Hidden | Locked }`，由玩法设置，host 照着执行。
    - `load_context`：host 启动时按名字阻塞加载 context。
- **引擎内容 `content/engine/input`**：
  - `Spectator.masset`：Move、Look、LookEnable、Rise、Boost，以及 context `Marlin.Engine.Input.Spectator`。手感和原来写死的按键一致：鼠标每像素 0.003 rad，只有按住右键时生效；摇杆 2.5 rad/s；Shift 加速 ×3。
  - `Host.masset`：
    - `Marlin.Engine.Input.Host`（所有构建都有）：F11、Alt+Enter 切换全屏。
    - `Marlin.Engine.Input.Dev`（只在开发构建中加载）：Esc 或手柄 Start 退出、V 切换 vsync、F3 显示统计。
    - 按 §12 的约定，shipping 游戏的退出由游戏自己负责。
- **玩法**：`GameplayModule` 启动时按名字找到 spectator 的 action，在每个 World 里给玩家 0 加上 spectator context；spectate system 读取玩家 0 的 action。玩法代码不再出现任何按键。
- **Host 路由**（`marlin_framework::input`）：
  - `HostInput` 先求值 host 的 context，返回命令（quit、fullscreen、vsync、stats）和消耗之后剩下的输入。
  - `ViewportInput` 实现 §9.3 的状态机：
    - game 视口一开始就处于 GameCapture，按 input mode 把输入交给 World；收到 release 命令时回到 Focused。
    - editor 视口点击后进入 Focused；鼠标悬停时按住右键进入 Captured（光标锁定，World 收到输入），松开回到 Focused；点击视口外面回到 Unfocused。
    - 有文本焦点时去掉键盘输入。
  - `apply_cursor` 把光标模式应用到窗口。
  - `run` 和 editor 都走这条路径：`Engine::frame` 不再接收原始输入，输入经路由交给玩家（`give_player_input`）。
  - editor 的场景视口改成"点击聚焦、按住右键飞行并锁定光标"。
  - 还没做：Shift+F1 释放捕获（PIE，W5）；UI 层的悬停和文本焦点（等 editor UI 框架）。
- **验证**：
  - 单元测试：input_action（轴累加、chord、按下沿、Hold/Tap、优先级与消耗、dead zone 与 dt、序列化往返）、gameplay（spectator 按 action 移动）、framework 路由（editor 的捕获和释放、game 视口的 input mode、文本焦点、release、host 消耗输入）都通过。
  - GPU 测试：framework 原有的全部通过；新增一个测试用 cook 出来的真实引擎绑定验证：Esc 退出并被消耗，W 让 spectator 沿视线前进 8 m，右键加鼠标移动 100 px 转 0.3 rad，只移动鼠标不转。editor 的 GPU 测试也通过。
  - 构建：workspace check、clippy、check-layers 干净；shipping 构建能编译。
  - 冒烟测试：运行 `marlin-game` 12 秒，正常加载关卡，没有关于 context 缺失的警告。没有在窗口里手动按键试过；按键到 action 的整条链路由上面的 end-to-end 测试覆盖。

### W5（2026-10-19）：editor 走 Host

- **Host 统一**（§13.1）：
  - `run.rs` 删除，game 和 editor 共用 `host.rs` 的 `run_app::<A: HostApp>`。game 是 `GameApp`，editor 是 `EditorApp`（`apps/editor/src/app.rs`）。
  - Host 管理多个窗口：每个窗口一个 swapchain，同一张 graph 画完后一起 present；app 通过 `WindowRequest` 打开或关闭窗口，并为每个窗口设置光标模式。窗口失焦时，捕获自动退回 Focused（§9.3）。
  - `RenderShaders`（render 模块）和 `UiAssets`（UI 模块）是 engine 资源，所以随时可以创建 `WorldRenderer`（PIE 开始时）和 UI renderer。dev overlay 也改为使用这两样，不再有自己的 asset server；shipping 构建里连查找都没有。
  - `GlyphIds` 改为共享（`Arc`），UI 在 sim 侧录制。
- **源数据到 cooked 只有一条路**：
  - `marlin_cook_core::read_source_object` 和 `marlin_world::{read_prefabs, cook_level, dangling_parents}` 从 cook_level 中提出来，cooker 和 editor 都调用它们。
  - `editor/core`：`SourceContent`（源索引 + cook 的 schema）、`LevelDocument`（关卡和它的所有 Prefab，`cook()`，`is_stale()`）。
- **Editor World**：`WorldKind::Editor`，由 `LevelDocument::cook()` spawn 出来，不跑玩法 group。每 0.5 s 检查一次源文件，文档过期就重新读入并替换子关卡。
- **视口**（`editor/viewport`）：
  - `ViewportCamera` 不是 entity，用引擎的 spectator 绑定和 `marlin_gameplay::fly` 飞行，视图以最高优先级加进 Editor World 的视图列表，画到 `Offscreen(index)`。
  - `SceneViewport` 管理编辑或运行状态和 §9.3 的输入状态。
- **PIE**：`PieSession` 把文档 cook 出来 spawn 到 `Pie` World。渲染侧在会话开始时为它建一个 `WorldRenderer`，结束时等 GPU 空闲后丢掉。Editor World 的 delta 每帧都要取并应用，PIE 期间也一样。
- **命令**：`content/engine/input/Editor.masset`：
  - Alt+P 开始或结束 PIE，Shift+F1 在 PIE 中释放鼠标。
  - 工具栏的 ▶ / ■ 按钮走同一个命令：shell 的 layout 给出按钮的矩形，editor 判断点击。等 editor UI 框架有了按钮控件，再换成控件。
- **`InputAction.ConsumeInput`**（Enhanced Input 的做法）：chord 用的辅助 action（Host 的 Alt、spectator 的 LookEnable）不消耗按键，所以下一层仍能用 Alt 组合键。§9 和 W4 中"触发了的 mapping 会消耗输入源"的说法相应修正为"`ConsumeInput` 为真的 action 才消耗"。
- **顺带修复**：
  - Vulkan 的 placed buffer（render graph 的临时 buffer）的地址对齐改为与 committed buffer 相同（256）。之前 AS build 的 scratch 可能只有 16 字节对齐，editor 中出现了验证错误。
  - cooker CLI 测试的引擎单元数改为 13（W4 加了两个输入文件，W5 加了一个）。
- **还没做 / 已知限制**：
  - editor 不再有"画面没变就不重画"的空闲优化，因为 Host 循环每帧都跑。等 Host 有了按需重画的节奏控制后再加回来。
  - Preview World 只有 API，没有 UI。
  - 视口的悬停和文本焦点仍由 editor 自己判断（shell 的固定矩形），等 editor UI 框架。
- **验证**：
  - 单元测试：editor-core（文档 cook 结果与 cook 相同；改 Prefab 后文档过期，重新打开后读到新值）、editor-viewport（编辑时飞相机、World 收不到输入；运行时 World 收到输入，Shift+F1 释放；视图 id 和目标）、input_action（chord 辅助键不消耗）、framework 路由（失焦释放捕获）。quick tier 全部通过。
  - GPU 测试：editor 新增 `alt_p_plays_the_level_in_editor_and_stops_it`，用 cook 出来的真实绑定验证：Alt+P 开始 PIE，PIE 中旋转物体在转，Alt+P 结束；工具栏 ▶ 开始、■ 结束。framework、ui-render、rhi、render-graph、render、material 的 GPU 测试都通过。
  - 构建：workspace check、clippy、check-layers 干净；shipping 构建没有警告。
  - 冒烟测试：editor 打开 `samples/basic` 运行 400 帧并截图，没有验证错误；`marlin-game` 运行 12 秒，正常加载关卡。没有在窗口里手动按 Alt+P 或点按钮试过。
