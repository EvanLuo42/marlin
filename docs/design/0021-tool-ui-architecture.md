# 0021 — 工具 UI 架构：editor、launcher 和 UI 框架

状态：草案 · 2026-10-19

前置：
- [0001](0001-repo-layout.md) §6：editor/tools/apps 的原定分工（本文修改 §6.1）
- [0003](0003-asset-pipeline.md) §4.5–4.6：项目、挂载、config；§13 待做里的“发行版只读 DDC 层”和“用户目录的引擎缓存”
- [0012](0012-ui-renderer.md)：立即模式 UI renderer（`Painter`、chunk、裁剪、外部图像、tile 损伤）
- [0019](0019-engine-framework-and-world.md) §9（输入路由）、§13（Host、Editor/PIE/Preview World、editor crate 分工）
- [0020](0020-editor-ux.md)：editor 的功能和交互，是本文的需求；本文修改它的 §10.7 和 §11.3

本文定 **editor 和 launcher 的 UI 怎么搭**：crate 放在哪里、UI 框架的范式、各级 context 怎么管理、文档和撤销、线程、窗口和 docking、主题和资源、本地化，以及给游戏调试用的立即模式便利 API。所有决定来自 2026-10-19 的多选问答。

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 程序 | editor 只从 `.mproject` 启动；另有独立的 **`marlin-launcher`**：项目浏览器、模板新建、引擎版本管理、`.mproject` 文件关联 | 1、13 |
| crate 位置 | 新的顶层 **`ui/`**：`ui/core`、`ui/widgets`、`ui/theme`（std，桌面工具专用）；editor 和 launcher 共用。`editor/` 只放编辑器业务 | 1 |
| 游戏 UI | 正式游戏 UI（HUD、菜单）**以后单独定**，`ui/` 不为它让步；游戏调试 UI 用 `ui_render::imm` | 1.3 |
| 范式 | **保留的控件树 + 细粒度 signal**（Signal / Memo / Effect）；视图函数只跑一次，数据变了只更新受影响的节点 | 3 |
| 编写 | **Rust builder**：`column().gap(4).child(...)`，不用宏 DSL，也不用标记文件 | 3.4 |
| 布局 | **自研**：flex 行列 + overlay、grid、splitter、scroll、虚拟化列表、dock 等专用容器；测量 → 排布两遍。不用 taffy | 4 |
| 线程 | **UI 和文档在同一个 editor 逻辑线程**，同步读写；PIE/预览 World 在 job 上按自己的节奏 tick，UI 只读它最近一次完成的快照；重操作是后台任务 | 5 |
| context | **按树作用域、按类型提供**：App → 窗口 → 顶层 tab（文档）→ 面板；`provide` / `use_context::<T>()` | 6 |
| 撤销 | **反射属性差分事务**：`doc.transact(...)` 记录字段旧值/新值和结构变化；同一份差分也是变更通知 | 7 |
| 数据真相 | **文档是真相**；Editor World 是文档的投影，按差分增量更新 | 7.3 |
| 面板归属 | 面板属于创建它的顶层 tab，可以浮动成 OS 窗口，但只能停靠回同一个顶层 tab | 8.3 |
| 窗口 | **自绘标题栏**（菜单和顶层 tab 放进标题栏），保留系统贴靠、阴影和缩放；**所有窗口是一棵 UI 树的多个根** | 8 |
| 拖 tab | 拖出 tab 栏**立即变成跟随光标的真实 OS 窗口**，经过的任何窗口显示十字靶标 | 8.4 |
| 弹出层 | 默认画在窗口内的 overlay 层，放不下时开**不抢焦点的 popup OS 窗口** | 8.5 |
| 样式 | **语义 token + 每种控件一个类型化的样式结构**（有变体，没有选择器和级联）；主题写在 **Rust 代码**里 | 10 |
| 资源 | editor/launcher 的图标（**SVG 子集**）、字体、翻译表是**引擎目录下的裸文件**，启动时读，不经过 cook（UE 式） | 11 |
| 字体 | 自带 Inter Regular/Medium/Bold 和 JetBrains Mono；其他字形（CJK 等）**从系统字体回退** | 11.3 |
| shader | 引擎内容（包括 UI 和引擎 shader）随引擎分发**源码**，首次启动和改动后编译，存进**按用户、跨项目共享的引擎 DDC**；安装版附带预填的只读 DDC 包 | 11.4 |
| 本地化 | 新的 runtime crate **`marlin-i18n`**（no_std，所有层可用）；`loc!(key, "English")` + 提取工具；**.po 源 + 二进制表** | 12 |
| 输入 | UI 是 0019 §9.2 路由栈的第 2 层；**完整 IME**；键盘导航现在做，读屏只预留（控件声明角色和名字） | 9 |
| 文本编辑 | editor 里**不做代码编辑**：材质和脚本都在外部 IDE 写；Output Log 是只读的虚拟化列表 | 9.4 |
| 大列表 | 列表/树/表格/网格全部**虚拟化**，只认数据源接口 | 4.3 |
| 属性面板 | 反射元数据 + 按类型注册的字段编辑器和组件布局定制 | 14.1 |
| 扩展 | editor 模块在构建时固定，**静态 Rust 注册**面板、资产编辑器、菜单、命令、属性定制 | 14.2 |
| 动画 | 框架内置的属性过渡（时长和缓动来自 token），只在动画进行时请求重绘；偏好里可关 | 3.5 |
| imm | `marlin_ui_render::imm`：**调试工具级**的 imgui 式 API（窗口、自动布局、常用控件、图表），no_std，Shipping 可裁掉 | 15 |
| 测试 | **无头逻辑测试** + **绘制列表快照**；不另做 GPU 参考图（ui_render 自己已有） | 16 |

## 1. 程序和 crate

### 1.1 三个程序

| 程序 | 作用 | UI |
|---|---|---|
| `marlin-game` | 游戏（no_std），只从 cook 并 stage 过的目录运行（0003 §4.6） | 游戏 UI（以后定）+ `ui_render::imm` 调试 UI |
| `marlin-editor` | 编辑器，**只从 `.mproject` 启动**（命令行参数或文件关联转过来） | `ui/*` |
| `marlin-launcher` | 项目浏览器、新建项目、引擎版本管理、`.mproject` 关联（§13） | `ui/*` |

editor 和 launcher 的样式、控件完全相同。launcher 不链接 World、渲染器和 cookd，只链接 UI 需要的部分（RHI、ui_render、shader 编译器、引擎 DDC）。

### 1.2 目录

```
ui/                    std，桌面工具专用；不依赖 editor/
├── core/              marlin-ui-core：signal 运行时、控件树、布局、输入/焦点/IME、窗口根、
│                      动画、拖放、context 作用域、无头测试驱动
├── widgets/           marlin-ui-widgets：基础控件、虚拟化列表/树/表格/网格、docking、菜单、
│                      弹出层、对话框、toast、属性编辑器框架（反射驱动）
└── theme/             marlin-ui-theme：token、控件样式结构、深色/浅色主题、图标表（§10、§11）
editor/
├── core/              文档、事务和撤销、选择、命令注册、editor 服务（§6、§7）
├── viewport/          场景视口、gizmo、相机、PIE
├── shell/             新建：Level Editor 外壳、顶层 tab、各面板的注册（替代 apps/editor/src/shell.rs）
├── world_tools/
└── asset_tools/
apps/
├── game/  editor/  launcher/（新建）
```

- 现在的 `editor/ui_core`、`editor/ui_theme`、`editor/ui_widgets` 是空 crate，直接搬到 `ui/`，包名改为 `marlin-ui-*`。
- 0001 原来建议 `ui_core` 只用 `core + alloc`，以便游戏复用。§1.3 定了游戏 UI 以后再说，所以 `ui/` 是 **std** 的，可以用 `std` 的集合和线程。
- 依赖方向：`ui/*` → runtime（ui_render、font、math、reflect、i18n、input、app）；`editor/*` → `ui/*`；`ui/*` 不知道 editor 的任何概念（文档、World）。

### 1.3 游戏 UI

- 正式游戏 UI（HUD、主菜单、设置界面、世界内 UI 的控件）等 VM（0018）落地后单独写文档，可能是 VM 驱动的保留模式 + UI 资产。`ui/` 不为它做 no_std 或 VM 绑定的妥协。
- 游戏里的调试界面（dev overlay、控制台、cvar 面板、统计图）用 `ui_render::imm`（§15），editor 不用它。

## 2. 分层总览

```
 editor/shell、asset_tools…        launcher
          │ 面板、资产编辑器、命令          │
 editor/core（文档、事务、选择）           │
          │                            │
 ui/widgets（控件、docking、属性编辑器）◀──┘
 ui/theme（token、样式、图标）
 ui/core（signal、控件树、布局、输入、窗口根）
          │ 每个窗口一个 UiDrawList（chunk 按节点缓存）
 runtime: ui_render（Painter、chunk、裁剪、外部图像） · font · i18n · input · app（窗口）
```

## 3. 响应式核心

### 3.1 Signal、Memo、Effect

- `Signal<T>`：可读写的值；`Memo<T>`：由其他信号算出来的值，惰性、带缓存，值没变就不通知下游；`Effect`：读信号、产生副作用（改控件属性、请求重绘）。
- 句柄是 `Copy` 的小值（arena 下标 + 代数），由 UI 运行时持有存储；不用 `Rc<RefCell>`。访问已释放的句柄是 panic，并指出它属于哪个作用域。
- 依赖自动追踪：在 Memo/Effect 里读到的信号就是它的依赖，每次重算重新收集。
- **批处理**：一帧里的所有写先标记，帧末（布局之前）按拓扑顺序统一重算 Memo、运行 Effect；同一个 Effect 一帧最多跑一次。
- **作用域**：每个控件节点有一个响应式作用域，节点删除时，它创建的信号、Memo、Effect 一起释放。

### 3.2 控件属性的绑定

- 控件的每个属性要么是常量，要么绑定一个信号或闭包（闭包会被包成 Memo）：`label(name)` 里的 `name` 可以是 `&str`、`Signal<String>` 或 `move || doc.name(e)`。
- 绑定的属性变化只把那个节点标记为需要重新布局（尺寸可能变）或只需要重画（颜色变），不重建子树。
- 结构变化（列表增删行、条件显示）用专门的结构控件：`show(cond, ...)`、`for_each(source, key, row)`、`switch(...)`；它们按 key 复用子节点。

### 3.3 和文档的连接

- 文档的数据不放进信号；文档提供**按路径的失效通知**：事务提交后的差分（§7）列出被改的 (entity, 组件, 字段路径)，UI 运行时把它们映射到订阅了这些路径的 Memo。
- 视图里读文档用 `doc.read(path)` 之类的访问函数，在 Memo 里调用时自动登记“依赖这个路径”。这样 Details 里一个字段只在它自己的字段变化时重算，Outliner 的一行只在那个 entity 的名字或层级变化时重算。

### 3.4 编写方式

```rust
fn outliner_row(cx: &Cx, e: EntityGuid) -> impl View {
    let doc = cx.use_context::<LevelDoc>();
    row().height(token::ROW_HEIGHT).gap(4)
        .child(icon(move || doc.primary_icon(e)))
        .child(label(move || doc.name(e)).color(move || doc.name_color(e)))
        .on_double_click(move |cx| doc.focus(cx, e))
}
```

- 纯 Rust builder，不用宏；控件构造函数返回 builder，`child`/`children` 接受任何 `View`。
- 视图函数只在节点创建时运行一次；之后的变化全靠绑定。

### 3.5 动画

- 可动画的属性（颜色、不透明度、尺寸、位置偏移）可以声明过渡：`.transition(Prop::Background, theme.motion.fast)`；时长和缓动是主题 token。
- 有动画在进行时，运行时请求下一帧；都结束后不再重绘，配合 0012 的 tile 损伤，空闲时不画也不 present。
- 偏好里的“减少动态”把所有过渡时长设为 0。不做时间轴和关键帧。

## 4. 控件树和布局

### 4.1 节点

- 控件树放在 arena 里（`NodeId`），每个节点：控件对象、样式、布局缓存、响应式作用域、可访问性信息（角色、名字，§9.3）、绘制 chunk 缓存。
- `Widget` trait：`measure`（给约束，返回期望尺寸）、`arrange`（给最终矩形，排布子节点）、`paint`（往 `Painter` 写命令）、`event`（输入事件），以及可选的 `hit_test`。
- **绘制缓存**：每个节点的绘制结果是一个 `ui_render` chunk（0012 §2.3），只有被标记为需要重画的节点才重录；没变的 chunk 跨帧复用。渲染器的 tile 损伤再挡掉没变的像素。

### 4.2 布局

- 两遍：自顶向下传约束（最小/最大宽高），自底向上返回期望尺寸；然后自顶向下排布。布局结果缓存，约束和子节点都没变就跳过。
- 容器：
  - `row` / `column`：flex，`grow`、`shrink`、`basis`、`align`、`justify`、`gap`、`wrap`；
  - `stack` / `overlay`：叠放，按对齐摆放；
  - `grid`：固定/比例/自动的行列轨道；
  - `splitter`：可拖动的分隔条，记住比例；
  - `scroll`：滚动区域，平滑滚动，滚动条按主题；
  - `virtual_list` 等（§4.3）；`dock_area`（§8）。
- 所有尺寸是逻辑像素；窗口缩放 = 显示器 DPI × 用户比例（0020 §1.2）。窗口换显示器时按新 DPI 重新布局，文字按新的像素字号重新栅格化。
- 文本测量由 `marlin-font` 的 shaping 和换行完成，结果按（文本、字体、字号、宽度）缓存。

### 4.3 虚拟化

- `virtual_list`、`virtual_tree`、`table`、`tile_grid` 只认**数据源**：行数、第 i 行的稳定 key、行内容的视图函数、（树）展开状态。
- 只为可见行（加上下各一屏的余量）创建节点，滚出视野的行节点按 key 回收复用。
- 树先按展开状态拍平成行。行高固定，或可变（先按估计值排，测量后修正，保持锚点行不跳动）。
- 选择、键盘导航、拖放、框选都在这一层实现，Outliner、Content Browser、Output Log、资产选择器共用。

## 5. 线程

### 5.1 editor 逻辑线程

- UI 树、文档、选择、撤销栈、editor 服务都在 Host 的 sim 侧（0019 §13.1 的 `HostApp::Sim`），同一个线程**同步**读写。属性面板直接读反射字段，命令直接改文档。
- 每帧：处理输入 → 运行命令和事务 → 应用后台任务的结果 → 刷新响应式（§3.1）→ 布局 → 录制脏节点 → 每个窗口产出一个 `UiDrawList`，交给渲染侧。

### 5.2 World 和 UI 解耦

- **Editor World** 不跑玩法，只在文档的差分到来时增量更新（§7.3），开销小，留在逻辑线程。
- **PIE 和预览 World** 在 job 上按自己的节奏 tick，**可以跨多个 UI 帧**：游戏只跑 15 fps 时，UI 仍按显示器刷新率运行。
  - 每个这样的 World 对外发布“最近一次完成的帧”：渲染用的 `RenderDelta` 和 UI 要看的快照（Outliner 的 entity 列表、Details 选中 entity 的字段值）。
  - UI 对 PIE World 的修改（Details 改值、F8 后拖 gizmo）排进命令队列，在下一个 tick 开始时应用。
  - 视口显示该 World 最近完成的图像；Host 需要支持“某个 World 的模拟不和 Host 帧绑定”，这是 Host 的一项改动（§17）。
- 重操作（导入、保存大关卡、缩略图、搜索索引、批量编辑的计算部分）是**后台任务**：在 job 上运行，状态栏显示进度、可以取消（0020 §10.1），结果在逻辑线程上提交成一个事务。逻辑线程上不做阻塞 IO。

## 6. Context

### 6.1 作用域

| 作用域 | 提供者 | 内容 |
|---|---|---|
| App | 程序启动 | 项目和挂载、`SourceContent`、资产数据库、cookd 客户端、偏好和项目设置、命令注册表、主题、本地化、后台任务、通知（toast）、剪贴板 |
| 窗口 | 每个 OS 窗口根 | 窗口 id、DPI、焦点状态、overlay 层 |
| 顶层 tab | 每个文档 | 文档（`LevelDoc`、`PrefabDoc`…）、撤销栈、选择、它的 World 句柄、PIE 会话 |
| 面板 | 每个面板实例 | 面板自己的状态（滚动、过滤、展开）、面板所在的 mapping context |

- 父节点 `cx.provide(value)`，子孙节点 `cx.use_context::<T>()` 沿树往上找最近的一个；找不到时 panic，并打印查找路径。
- 面板挂在哪个顶层 tab 下，就自动拿到哪个文档；面板代码不需要知道“当前文档”是什么。
- 浮动窗口里的面板，逻辑上仍是它所属顶层 tab 的子孙（§8.3），所以 context 链不变。

### 6.2 “当前”

- “活动文档” = 键盘焦点所在的顶层 tab。全局命令（Ctrl+Z、Ctrl+S）作用于活动文档；0020 §7.1 的规则由此实现。
- 没有全局单例；测试可以在一个进程里建多个 App 作用域。

## 7. 文档、事务和撤销

### 7.1 事务

```rust
doc.transact(loc!("Undo.Move", "Move"), |tx| {
    for e in selection { tx.set(e, Transform::translation, new_pos(e)); }
});
```

- 事务里的每次写都经过 `tx`，按反射路径记录**旧值和新值**；结构变化（增删 entity、组件、改父级、Prefab 覆盖的增删）也记录成差分项。
- 撤销 = 反向应用差分；重做 = 正向应用。插件和面板不写撤销代码。
- 连续交互合并：gizmo 拖动、数值字段拖动期间只预览（§7.3），松手时提交一个事务（0020 §12.4）。选择变化也是事务项，连续选择合并（0020 §7.2），但不标记文档为已修改。

### 7.2 差分的其他用途

同一份差分广播给：UI（§3.3 的按路径失效）、Editor World（§7.3）、自动备份（只写有变化的文档）、git 状态和“未保存”标记、以后的多人编辑（预留）。

### 7.3 文档是真相

- gizmo、Details、Outliner 的所有编辑都写文档；Editor World 按差分增量更新对应的 entity 和组件，Prefab 链接和覆盖信息只在文档里。
- 拖动中的预览：直接改 Editor World 里的值，不进文档；松手时提交事务，World 收到差分后的结果与预览一致。取消（Esc）时从文档恢复。
- PIE 的“保留此改动”（0020 §8.3）= 在编辑文档上提交一个事务。

## 8. 窗口和 docking

### 8.1 一棵树，多个根

- 所有 OS 窗口属于同一个 UI 运行时：同一套信号、焦点、拖放状态。每个窗口是树的一个根节点，各自布局，各自产出一个 `UiDrawList`。
- 面板在窗口之间移动只是换父节点，滚动位置、展开状态和绑定都不丢。
- 窗口通过 Host 的 `WindowRequest` 打开和关闭（0019 §13.1）；关闭主窗口退出程序。

### 8.2 自绘标题栏

- 所有窗口不用系统标题栏，客户区延伸到整个窗口：
  - 主窗口：标题栏里是应用图标、菜单栏、顶层 tab、窗口按钮；
  - 浮动窗口：标题栏就是它的 tab 栏。
- 保留系统行为：Windows 11 的贴靠布局（悬停最大化按钮）、窗口阴影、拖边缩放、双击标题栏最大化、Win+方向键。平台层（`marlin-app`）提供非客户区命中测试回调：UI 报告哪块是标题、哪块是按钮、哪块是缩放边。

### 8.3 dock 数据模型

- 每个顶层 tab 有一个 `DockArea`：
  - 主树：`Split { axis, children, ratios }` 或 `TabGroup { panels, active }`；
  - 浮动窗口列表：每个浮动窗口有自己的一棵同样的树；
  - 关闭面板的“上次位置”记录（0020 §2.4）。
- 面板实例 = 面板类型（稳定的字符串 id，如 `Marlin.LevelEditor.Outliner`，由模块注册，§14.2）+ 实例号（Viewport 1–4）。
- 面板只能停靠在它所属顶层 tab 的 `DockArea` 里（主树或浮动窗口）；拖到别的顶层 tab 上不显示靶标。文档关闭时，它的浮动窗口一起关。
- 顶层 tab 本身也可以拖出成独立的 OS 窗口（0020 §2.1）；这时那个窗口的根就是这个顶层 tab 的 `DockArea`。
- 布局按用户保存在用户目录（0020 §2.4），每种顶层 tab 类型一个文件，内容是树结构 + 面板 id + 比例 + 浮动窗口的屏幕矩形（按显示器记，显示器不在了就移回主显示器）。遇到不认识的面板 id 就跳过。

### 8.4 拖放 tab

- tab 拖出 tab 栏（超过阈值）后，立即变成一个**跟随光标的半透明 OS 窗口**，显示面板的真实内容。
- 光标经过的窗口（包括其他浮动窗口和主窗口）显示十字靶标和外围落点（0020 §2.2）。悬停落点时用蓝色矩形预览；在靶标外松开，窗口留在原地成为浮动窗口。
- 平台层需要：拖动期间跨窗口的全局鼠标捕获、按光标移动窗口、查询光标下的窗口（忽略被拖的那个）、窗口不透明度。

### 8.5 弹出层

- 菜单、下拉、tooltip、拾色器、自动补全默认画在窗口的 overlay 层（同一个根下的最上层）。
- 放不下时（小浮动窗口里的长菜单、靠近屏幕边缘的 tooltip）开一个 **popup OS 窗口**：无边框、不出现在任务栏、不抢焦点、跟随所属窗口。弹出层的逻辑节点仍在原窗口的树下，只是绘制到另一个根。
- 弹出层的关闭规则：点击外部、Esc、所属窗口失焦（菜单）。

### 8.6 模态

- 模态对话框（0020 §12.2 列出的几种）是一个居中的 OS 窗口，所属窗口变暗且不接受输入；不阻塞逻辑线程（不跑嵌套消息循环），用回调或 future 返回结果。

## 9. 输入、焦点和文本

### 9.1 和输入路由的对接

UI 是 0019 §9.2 的第 2 层：

1. 第 1 层 editor 全局命令（结束 PIE、Eject、释放捕获）先看。
2. UI：
   - 鼠标按命中测试交给最上面的控件，拖动时捕获；
   - 键盘先给焦点控件（原始按键和文本输入），没用掉就冒泡到父控件，最后到**焦点面板的 mapping context**（面板命令是 action，0020 §9.1）；
   - 文本控件有焦点时，普通字符键不触发快捷键（W 不切 gizmo），但带 Ctrl/Alt 的组合仍然可以冒泡。
3. 视口控件没用掉的输入交给第 3 层（视口状态机，0019 §9.3）。
4. 第 4 层是 PIE 的玩家。

### 9.2 焦点和键盘导航

- 焦点控件的面板顶边显示蓝线（0020 §9.3）。
- Tab / Shift+Tab 按布局顺序在可聚焦控件间移动；方向键在列表、树、菜单、tab 栏里移动；Alt 激活菜单栏；Esc 关闭弹出层并把焦点还给打开它的控件。
- 焦点在窗口之间移动时（点了浮动窗口），记住每个窗口最后的焦点控件。

### 9.3 可访问性预留

- 每个控件声明**角色**（button、checkbox、tree item…）、**名字**（可本地化文本）和状态（选中、展开、禁用）。
- 这份数据现在用于无头测试的控件查找（§16）；以后对接 Windows UI Automation 时直接作为读屏数据。

### 9.4 文本输入

- **完整 IME**：Windows 上用 TSF（Text Services Framework），组字串在文本框里内联显示（带下划线），候选窗跟随文字光标。`marlin-app` 增加文本输入接口：开始/结束组字、组字串和光标、提交的文本、候选窗位置。Linux 走 IBus/Fcitx（文本输入协议），以后做。
- 文本控件：单行和多行文本框（名字、搜索、数值表达式、批量重命名）；选择、剪贴板、撤销（控件自己的小栈，提交时变成一个文档事务）、按词移动。
- **不做代码编辑器**：材质和 `.mscript` 都在外部 IDE 写，editor 不内嵌代码视图（修改 0020 §11.3）。Output Log 是虚拟化的只读行列表，可以选中和复制。

### 9.5 拖放

- 框架内的拖放带**类型化负载**（资产引用、entity 列表、面板、文件路径），目标按类型决定是否接受，并显示落点反馈。
- 从系统拖入的文件（0020 §6.5 的导入）转换成“文件路径”负载。

## 10. 主题和样式

### 10.1 token

- 语义 token：颜色（背景各层级、文字各层级、强调色、选中、焦点、语义色：Prefab 蓝、覆盖、新增绿、错误红、警告黄、PIE 绿/黄…，0020 §1.2）、间距、圆角、边框宽度、字体和字号、行高、阴影、动画时长和缓动。
- 深色主题是默认值，浅色主题是同一组 token 的另一套值；现有 shell 的颜色是深色主题的起点。

### 10.2 控件样式

- 每种控件一个类型化的样式结构，例如 `ButtonStyle { normal, hovered, pressed, disabled, focused: ButtonState }`，字段引用 token 或直接给值。
- 控件可以选**变体**：`button(...).variant(Variant::Primary)`（primary、secondary、ghost、danger）。
- 没有选择器和级联：一个控件长什么样只看它的样式结构和变体，方便查。

### 10.3 存放

- 主题写在 **Rust 代码**里（`ui/theme`），编译进程序；切换主题 = 换一套 token，全部控件即时更新（token 也是信号）。
- 用户可调的只有偏好里的选项：深色/浅色、UI 缩放、减少动态。

## 11. 工具资源和引擎 DDC

参照 UE：Slate 的图标、字体、翻译表都是引擎目录下的裸文件，启动时从磁盘读；引擎 shader 随引擎分发源码，首次启动编译进 DDC，安装版附带预填的 DDC 包。

### 11.1 目录

```
<engine>/
├── marlin-editor.exe  marlin-launcher.exe  …        一堆 exe
├── resources/                                     工具 UI 的裸资源，不 cook
│   ├── icons/      *.svg（按类别分子目录）
│   ├── fonts/      Inter-{Regular,Medium,Bold}.ttf、JetBrainsMono-Regular.ttf（SIL OFL）
│   └── i18n/       <语言>/editor.po（源）
├── content/engine/                                引擎内容（shader 等），按 0003 cook
├── templates/                                     项目模板（§13）
└── ddc/                                           只在安装版：预填的只读 DDC 包
```

### 11.2 图标

- 源文件是 **SVG 子集**：path、基本形状、纯色、线性/径向渐变、描边、变换；不支持滤镜、文字、嵌入位图。解析器自己写，放在 `ui/theme`。
- 运行时解析成 ui_render 的路径命令（0012 §4.2）直接画矢量，任意 DPI 都清晰；按图标缓存解析结果。
- 单色图标用 `currentColor`，按 token 染色；多色图标（资产类型、光源）保留自己的颜色。
- 全局图标库：`ui/theme` 的 build.rs 扫描 `resources/icons/`，生成常量 `icons::SAVE`、`icons::LIGHT_POINT`…，名字拼错就编译报错。editor、launcher 和所有 editor 模块都从这里取。
- 改了 SVG 不需要重新编译（只有增删文件才需要）；开发期监视目录，热重载。

### 11.3 字体

- 自带 Inter Regular / Medium / Bold（正文、标题、覆盖字段的加粗）和 JetBrains Mono（日志、数值对齐、路径）。
- 启动时读 ttf，用 `marlin-cook-font` 的解析和烘焙函数生成运行时格式，结果进引擎 DDC（§11.4），之后启动直接命中。
- **其他字形从系统字体回退**：Inter 里没有的字形（CJK、阿拉伯文、emoji…）按回退链查系统字体。Windows 用 DirectWrite 的系统字体回退找到字体文件，再用同样的解析器加载；Linux 用 fontconfig。
  - 这要求 `marlin-cook-font` 支持 TrueType Collection（`.ttc`，例如 Microsoft YaHei）。CFF 轮廓（`.otf`）暂不支持，回退链跳过只有 CFF 的字体。
  - 这和 UE 不同（UE 自带 CJK 字体，不读系统字体），换来的是安装包小很多。

### 11.4 引擎 DDC

- **引擎内容**（UI 的 compute shader、渲染器和视口的引擎 shader、引擎资产，以及上面的字体和翻译表）随引擎分发源码，**首次启动时和源文件改动后**编译，结果存进**引擎 DDC**：
  - 源码版引擎：`<engine>/.marlin/ddc/`（所有项目共用）；
  - 安装版引擎：引擎目录只读，引擎 DDC 在用户目录，跨项目共享；另外先查随安装附带的**只读预填 DDC 包**（`<engine>/ddc/`，相当于 UE 的 `Compressed.ddp`），通常第一次启动不需要编译。
- 键是内容哈希，所以不同引擎版本共用同一个用户 DDC 也不会冲突。
- **项目内容**照旧 cook 到项目的 DDC（`<项目>/.marlin/ddc/`）。cookd 只负责项目内容和 PIE/预览需要的游戏数据；引擎内容的 cook 结果改为放在引擎 DDC 里（落实 0003 §13 待做里的两项）。
- launcher 链接 shader 编译器和引擎 DDC 的代码，可以独立编译它需要的 UI shader，不需要 cookd 和项目。
- 游戏不变：只读 cook 并打包好的 `.mpak`。

## 12. 本地化：`marlin-i18n`

- 新的 runtime crate `runtime/core/i18n`（`marlin-i18n`，no_std），所有层（runtime、ui、editor、tools）都可以用。
- 代码里写 `loc!("Outliner.Rename", "Rename")`：key + 英文原文（相当于 UE 的 LOCTEXT）。宏在编译期生成一个 `Text`（key 的哈希 + 原文），没有表时显示原文。
- 带参数的文本用 ICU MessageFormat 的子集：命名参数、复数（`{count, plural, one {# item} other {# items}}`）、数字和日期格式化。
- 提取工具（`marlin-i18n-gather`，tools/）扫描源码里的 `loc!` 生成 `.pot`；翻译人员用标准 .po 工具；`.po` 编译成**二进制表**：
  - 游戏和引擎运行时的文本表 cook 进 `.mpak`；
  - editor/launcher 的表从 `resources/i18n/*.po` 编译，结果进引擎 DDC（§11.4）。
- `marlin-i18n` 只提供“给我一张表”的接口，不关心表从哪里来；当前语言、回退链（`zh-Hans` → `en`）在它里面。
- 在 UI 里，当前语言是信号：切换语言时界面上的所有文本即时更新，不用重启。
- 英文为主，中文第二个做（0020 §12.1）；布局给变长的文本留余量。

## 13. Launcher

- `apps/launcher`（`marlin-launcher`），用 `ui/*`，样式和 editor 相同。第一版：
  - **项目浏览器**：最近项目卡片（缩略图、名字、路径、引擎版本），打开 `.mproject`，从列表移除，“在资源管理器中显示”。
  - **新建项目**：选模板（`<engine>/templates/`：空项目，以及从 samples 整理出的几个），填名字和位置，可选 `git init`；生成 `.mproject`、`content/`、`config/`。
  - **引擎版本管理**：用户目录下的已安装引擎表（源码版和安装版，路径 + 版本 + 标识），扫描和手动添加；项目按 `.mproject` 里的引擎绑定（落实 0003 §13 待做的 EngineAssociation）启动对应引擎的 `marlin-editor <项目>`；可以切换项目绑定的引擎。
  - **`.mproject` 文件关联**：双击 `.mproject` 由 launcher 处理：选出绑定的引擎，再启动它的 editor（UE 的 UnrealVersionSelector）。关联在首次运行时征得用户同意后注册，并可在设置里取消。
- editor 不再有项目浏览器模式：没有 `.mproject` 参数时报错，提示用 launcher（修改 0020 §10.7）。启动进度仍显示在 editor 的 splash 上。

## 14. editor 的属性编辑和扩展

### 14.1 属性编辑器

- Details 由 `marlin-reflect` 自动生成。大多数外观靠反射元数据：分类、显示名、tooltip（文档注释）、单位、范围、只读、条件显示、高级（默认折叠）。脚本组件的字段也用同样的元数据。
- 两类代码定制，按类型注册：
  - **字段编辑器**：某个类型的字段怎么画和编辑（`Color`、`AssetRef<T>`、`EntityRef`、旋转、数组…，0020 §5.5）；
  - **组件布局**：整个组件的分区怎么排（隐藏某些字段、加按钮、自定义行）。
- 属性编辑器只认“反射对象 + 路径 + 事务接口”，所以 Details、World Settings、Project Settings、Editor Preferences、通用资产编辑器共用同一个（0020 §10.3、§10.6、§11.1）。
- 多选、默认值的 ↶、Prefab 覆盖的显示（0020 §5.2、§5.4）都在这个框架里，不由各个字段编辑器各自实现。

### 14.2 扩展

- 和 0019 的模块一致：editor 模块在构建时固定，各自在初始化时**静态注册**：
  - 面板类型（稳定 id、标题、图标、所属顶层 tab 类型、默认位置）；
  - 资产编辑器（按资产类型，没有的用通用的反射 Details 编辑器）；
  - 菜单和工具栏的扩展点（按扩展点 id 插入条目）；
  - 命令（input action + 执行函数 + 是否可用的条件，0020 §9）；
  - 属性定制（§14.1）、Content Browser 的资产类型信息（颜色、图标、缩略图生成器）。
- 不做动态加载的 editor 插件。用脚本（`.mscript`）扩展 editor 以后在 0018 里定。

## 15. `ui_render::imm`：调试用的立即模式 API

- `marlin-ui-render` 里新增 `imm` 模块，no_std；ui_render 的核心（`Painter`、绘制列表）不变，`imm` 只是建在 `Painter` 上的一层。
- 用法（imgui 式）：

```rust
imm.window("Stats", |ui| {
    ui.label(format_args!("{:.1} ms", frame_ms));
    if ui.button("Capture") { capture(); }
    ui.slider("Exposure", &mut exposure, -4.0..=4.0);
    ui.plot_lines("GPU", &gpu_history);
});
```

- 范围（调试工具级）：
  - 可拖动、可折叠、可调大小的窗口，位置记在内存里（不做 docking、不做多 OS 窗口）；
  - 自动纵向布局 + `same_line`、缩进、分隔线、可折叠的标题、简单的表格/列；
  - 控件：文字、按钮、勾选、单选、滑条、拖动数值、下拉、单行文本框、树节点、进度条、颜色块、折线图和直方图；
  - ID 栈（按标签或显式 id 哈希）保存跨帧状态（展开、焦点、拖动中）；
  - 一套内置的简单风格（`ImmStyle`），不依赖 `ui/theme`。
- 输入：每帧传入一个 `ImmInput`（光标位置、按键、滚轮、文本字符、修饰键），由 framework 从输入路由的第 2 层（UI 焦点）填写；`imm` 报告它是否想要鼠标或键盘，以便路由决定是否用掉事件。
- 使用者：dev overlay（现在的 `framework::dev` 迁移到它上面）、控制台和 cvar 面板、性能图、游戏里的调试菜单。Shipping 构建可以用 cfg 整体裁掉。

## 16. 测试

按 0014 的层级：

- **无头逻辑测试**（不需要 GPU，`ui/core` 提供测试驱动）：建控件树、跑布局，用模拟输入驱动（点击、拖动、滚轮、按键、文本、IME 组字），断言状态、布局矩形和产生的事务。控件按**角色 + 名字**查找（§9.3 的同一份数据）。docking 的拖放、虚拟化列表、焦点导航、撤销都用这种测试。
- **绘制列表快照**：把一个窗口的 `UiDrawList` 序列化成可读文本，和提交在仓库里的基准比较；改动外观时更新基准。主题、弹出层、dock 靶标用这种测试。
- 不为 UI 框架另做 GPU 参考图；渲染正确性由 ui_render 自己的 GPU 测试保证（0012 §14）。

## 17. 对其他文档和代码的修改

- **0001 §6.1**：`ui_core`、`ui_widgets`、`ui_theme` 搬到顶层 `ui/`，不再用 taffy；`ui_core` 不再要求 `core + alloc`；新增 `apps/launcher` 和 `editor/shell`。
- **0012 §13**：引擎 UI 字体（`content/engine/fonts` 的 cook 资产）只给游戏和 `imm` 用；editor/launcher 用 `resources/fonts`（§11.3）。
- **0020 §10.7**：项目浏览器移到 launcher；**0020 §11.3**：去掉材质的内嵌代码视图。
- **0003 §13**：引擎 DDC 和只读预填 DDC 层、EngineAssociation 由本文落实（§11.4、§13）。
- **Host（0019 §13.1）**：PIE/预览 World 的模拟可以跨多个 Host 帧（§5.2）；`WindowRequest` 增加 popup 窗口和无边框窗口的选项。
- **`marlin-app`**：非客户区命中测试回调、无边框窗口保留系统行为、popup 窗口、拖动中跨窗口捕获和窗口移动、窗口不透明度、每显示器 DPI 变化事件、TSF 文本输入、系统字体回退查询（DirectWrite / fontconfig）。
- **`marlin-cook-font`**：TrueType Collection 支持。

## 18. 落地顺序

| 步 | 内容 |
|---|---|
| T1 | `ui/core`：signal 运行时、控件树、布局（flex、stack、grid、scroll）、绘制缓存、无头测试驱动；搬迁空 crate |
| T2 | `ui/theme`（token、样式、深色主题）、基础控件（label、button、checkbox、文本框、菜单）、焦点和键盘导航、`marlin-i18n` |
| T3 | 平台：自绘标题栏、多窗口根、popup 窗口、TSF IME |
| T4 | docking：`DockArea`、拖出成真窗口、靶标、布局持久化 |
| T5 | 虚拟化列表/树/表格/网格、属性编辑器框架 |
| T6 | `editor/core` 的事务和撤销、按路径失效；`editor/shell` 用新框架重建 Level Editor（Outliner、Details、Content Browser、Output Log、视口面板） |
| T7 | `resources/`（SVG 图标、字体、系统字体回退、.po）、引擎 DDC 和预填包；editor 不再从 cookd 拿 UI 字体和 UI shader |
| T8 | `marlin-launcher` |
| T9 | PIE/预览 World 与 UI 解耦（Host 改动） |
| 随时 | `ui_render::imm` 和 dev overlay 迁移（与 T1–T9 无依赖） |

## 19. 第一版不做

- 宏 DSL、标记文件布局、可视化 UI 设计器；CSS 式选择器和级联；用户自定义主题文件。
- taffy 或其他第三方布局库。
- UI 独立线程（UI 和文档分开）。
- Windows UI Automation（读屏）；Linux 的 IME。
- editor 内的代码编辑器（含材质代码视图）。
- 动态加载的 editor 插件。
- 时间轴/关键帧式 UI 动画。
- `imm` 的 docking 和多 OS 窗口。
- UI 的 GPU 参考图测试。

## 20. 待定

- signal 运行时的细节：arena 的回收策略、Memo 的相等比较（`PartialEq` 还是版本号）、异步（后台任务的 future 和信号的衔接）。
- 布局文件和已安装引擎表的具体格式（大概率是 toml，和 `config/` 一致）。
- 只读预填 DDC 包的打包方式（哪些引擎内容进包、按平台分几个包）。
- 系统字体回退在 Linux 上的行为，以及 CFF 轮廓是否要支持。
- 模板的格式（复制目录 + 替换名字，还是有清单）。
- 正式游戏 UI 的文档（§1.3）。
