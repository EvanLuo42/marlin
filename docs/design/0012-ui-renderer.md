# 0012 — UIRenderer：SDF 图元与 GPU tile 渲染

状态：U1 完成 · 2026-10-18（草案 2026-10-16）

前置：
- [0007](0007-render-graph.md)：render graph
- [0008](0008-rendering-overview.md) §6：视口与 UI 的合成

参考：Wessel Mast，*UIPainter: Tile-Based Rendering in One Draw Call*，GDC 2024（Guerrilla，Decima 编辑器的 UI 渲染器）。

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 定位 | runtime 的立即模式 2D 渲染器（`marlin-ui-render`）：只画图元和文字。editor 的保留模式和 MVVM 框架建在它之上（0001） | 1 |
| 使用者 | editor 的所有窗口、游戏 HUD 和菜单、世界内 UI（渲染到纹理）、调试用的 2D 绘制 | 1 |
| 基本思路 | 沿用 UIPainter：每个图元是一条绘制命令，用 SDF 算覆盖率，自带抗锯齿；按屏幕 tile 分箱，每个像素只遍历自己 tile 的命令；混合在寄存器里完成 | 2–5 |
| 分箱 | **在 GPU 上做**：CPU 只按顺序写命令，GPU 依次做包围、粗分箱和细分箱（Decima 说 CPU 分箱是它的主要瓶颈，把分箱搬到 GPU 是它列出的最大优化） | 5 |
| 执行 | 全部是 compute：包围 → 粗分箱 → 细分箱 → 损伤检测 → 着色；每个 16×16 的 tile 一个 workgroup | 5 |
| 图元 | Decima 的 SDF 图元全套，加上任意路径填充（贝塞尔轮廓，non-zero 和 even-odd） | 4 |
| 文字 | 按实际像素字号栅格化（水平 1/4 像素的子像素位置），存进字形缓存；大字号和任意变换下改走路径填充 | 6 |
| 变换 | 图元组上可以带 2D 仿射变换；SDF 在局部空间求值，抗锯齿宽度按雅可比修正 | 3.2 |
| 色彩 | 在 sRGB 编码空间混合，预乘 alpha；渐变可以选 Oklab 插值；HDR 窗口按 paper white 缩放 | 8 |
| 重绘 | **tile 级损伤检测**：tile 的命令列表哈希和上一帧相同就不着色；整个窗口都没变时整帧跳过，连 present 也不做 | 5.5 |
| 视口 | 视口图像是一个引用外部纹理的 image 命令（0008 §6） | 7 |
| 多线程 | 绘制列表由多个 chunk 组成，chunk 可以并行录制，也可以跨帧复用；分箱都在 GPU 上，CPU 只负责写命令 | 2.3、11 |

## 1. 定位

- **runtime crate**：`marlin-ui-render` 是 no_std 的，game 和 editor 共用。
- **立即模式**：每帧（或者在内容变化时）调用 `Painter` 的方法写一个绘制列表。控件树、布局、MVVM 都在 editor 的 `ui_core` 里（0001 §6.1），不在这里。
- **使用者**：
  - editor 窗口：面板、节点图、曲线编辑器、时间轴、视口上的叠加 UI；
  - 游戏：HUD、菜单、字幕；
  - 世界内 UI：屏幕、告示牌等，渲染到纹理再给材质用（§10）；
  - 调试 2D：统计、图表、文字标注。

## 2. API

### 2.1 Painter

```rust
pub struct Painter<'a> { /* 写进一个 UiDrawList */ }

impl Painter<'_> {
    // 图元
    pub fn rect(&mut self, r: Rect, s: &RectStyle);             // 填充、四个角的半径、描边宽度和颜色
    pub fn circle(&mut self, c: Vec2, radius: f32, s: &ShapeStyle);
    pub fn arc(&mut self, c: Vec2, r_inner: f32, r_outer: f32, a0: f32, a1: f32, s: &ShapeStyle);
    pub fn line(&mut self, a: Vec2, b: Vec2, s: &StrokeStyle);   // 宽度、端点样式、虚线
    pub fn polyline(&mut self, pts: &[Vec2], s: &StrokeStyle);   // 连接样式
    pub fn quad_bezier(&mut self, p: [Vec2; 3], s: &StrokeStyle);
    pub fn cubic_bezier(&mut self, p: [Vec2; 4], s: &StrokeStyle);
    pub fn triangle(&mut self, p: [Vec2; 3], s: &ShapeStyle);
    pub fn fill_path(&mut self, path: &Path, rule: FillRule, paint: &Paint);
    pub fn stroke_path(&mut self, path: &Path, s: &StrokeStyle);
    pub fn text(&mut self, layout: &TextLayout, origin: Vec2, paint: &Paint);
    pub fn image(&mut self, r: Rect, src: ImageSource, s: &ImageStyle);   // UV、色调、圆角
    pub fn effect(&mut self, r: Rect, effect: UiEffectId, params: &[u32]); // §9
    pub fn backdrop_blur(&mut self, r: Rect, radius: f32, corners: Corners); // §5.6

    // 状态
    pub fn push_clip(&mut self, r: Rect, corners: Corners);
    pub fn pop_clip(&mut self);
    pub fn begin_group(&mut self, g: &GroupDesc);                // 变换、不透明度、阴影、发光
    pub fn end_group(&mut self);

    // 并行与复用（§2.3）
    pub fn chunk(&mut self) -> ChunkSlot;
    pub fn replay(&mut self, chunk: &RetainedChunk, offset: Vec2);
}
```

- 坐标单位是逻辑像素；`UiSurface` 的 DPI 缩放在命令写入时就乘进去。
- `Paint` 是纯色、渐变（线性、径向、角度）或图像。
- 所有方法只是往绘制列表里追加数据，不做任何栅格化。

### 2.2 图元组

`begin_group` / `end_group` 之间的图元组成一个组，组和 Decima 的 primitive group 是同一个概念：

- 组里的所有图元共享同一个阴影和发光：效果是对组的**合并形状**算的（§4.3），比如一个由多个图元拼成的图标，阴影只算一次，看起来是一个整体。
- 组可以带一个 2D 仿射变换和一个不透明度。
- 组可以嵌套：CPU 在写命令时把嵌套的变换乘起来，GPU 只看到展平后的变换；阴影和发光只作用在声明它们的那一层。

### 2.3 绘制列表与 chunk

- `UiDrawList` 由有序的 chunk 组成。`painter.chunk()` 在当前位置预留一个槽，返回的 `ChunkSlot` 可以交给另一个 job 去填，最后按槽的顺序拼起来。这样多个面板可以并行录制，前后顺序不变。
- `RetainedChunk`：录好的 chunk 可以保留到下一帧，用 `replay` 加一个平移重新插入，代价只是一次内存拷贝。editor 的控件树可以按子树缓存 chunk，只重录变了的子树。
- chunk 里的命令不引用字形缓存的位置，只引用字形 id（§6.3），所以保留的 chunk 在字形缓存重排之后依然有效。

## 3. 命令格式

### 3.1 GPU 数据

| buffer | 元素 | 说明 |
|---|---|---|
| `Commands` | 16 B 的头：类型（6 位）、标志、组下标、裁剪下标、paint 下标、payload 偏移 | 每个图元一条 |
| `Payload` | 按类型不同 | 矩形的尺寸和四个角的半径、圆弧的角度和半径、线段端点…… |
| `Groups` | 2×3 仿射变换和它的逆、不透明度、阴影（偏移、模糊半径、颜色）、发光（半径、颜色）、成员命令的范围 | |
| `Clips` | 圆角矩形（在某个组的空间里），加上父裁剪的下标 | 裁剪链 |
| `Paints` | 纯色（sRGB8 + alpha）、渐变（渐变表里的行号、几何参数、插值空间）、图像（纹理下标、UV 变换、采样器） | |
| `Paths` | 路径的线段（直线、二次贝塞尔；三次在 CPU 上拆成二次） | §4.2 |
| `Glyphs` | 字形 id → 字形缓存里的矩形 | 每帧更新的间接表，§6.3 |

- 每帧 CPU 把绘制列表的这些部分写进上传 ring（0007 §3.2 的 `ctx.upload`）。
- 渐变的色标烘进一张渐变表纹理（每行一个渐变），着色时只需一次采样。

### 3.2 变换与抗锯齿

- 图元的几何参数在组的局部空间里给出。着色时先把像素中心用逆变换映射到局部空间，在那里算 SDF。
- 抗锯齿宽度：局部空间里的距离要换算成屏幕像素，乘上逆变换雅可比的范数（对非均匀缩放，按梯度方向算）。这样任意缩放和旋转下边缘都正好是一个像素的过渡。
- 节点图的平移和缩放只改变组的变换，不用重新生成命令。

## 4. 图元

### 4.1 SDF 图元

| 图元 | 距离函数 | 备注 |
|---|---|---|
| 圆角矩形 | 四个角各自的半径 | 可带描边（`|d| − w/2`） |
| 圆、椭圆 | 椭圆用近似 SDF | |
| 扇形、圆环 | 角度范围加内外半径 | Decima 例子里拼 "G" 用的就是它 |
| 线段 | 胶囊或平头 | 虚线按弧长取模 |
| 折线 | 拆成线段，连接处用圆角或斜接 | |
| 二次贝塞尔描边 | 解三次方程得到精确距离 | 三次贝塞尔在 CPU 上拆成二次 |
| 三角形 | 精确 SDF | |

覆盖率：`clamp(0.5 − d / aa, 0, 1)`，`aa` 是 §3.2 算出的像素宽度。和 Decima 的 `1 − d` 一样，边缘有一个像素的过渡。

### 4.2 路径填充

- 路径由直线和二次贝塞尔组成（三次在 CPU 上拆分），填充规则可以是 non-zero 或 even-odd。
- 覆盖率用**带符号面积累积**的方法算，不用 SDF：
  - 分箱时，每条线段只分进它实际穿过的 tile；
  - 每个 tile 的"左边界上的环绕数"（backdrop）沿 tile 行做前缀和得到；
  - 着色时，像素的覆盖率 = backdrop + tile 内线段贡献的带符号面积，再按填充规则换算。
- 用途：SVG 图标、矢量 HUD、大字号的文字（§6.4）。

### 4.3 阴影与发光

- 组的阴影：在组的第一个成员之前插入一条"组阴影"命令，它的覆盖区域是组的包围盒加上模糊半径。着色时对组内所有成员的 SDF 取最小值，得到合并形状的距离，再用这个距离算一个高斯近似的衰减。
- 组很大时（成员超过 16 个），阴影改用组包围盒的圆角矩形近似，由 `GroupDesc` 的一个标志控制。
- 发光（悬停高亮）同理，只是没有偏移，衰减曲线不同。

### 4.4 文字与图像

文字见 §6，图像见 §7。它们的命令很简单，只需要 UV 和 paint。

## 5. GPU 管线

```
上传 ─▶ UiBounds ─▶ UiBin（粗）─▶ UiCoarse（细）─▶ UiDamage ─▶ UiFine ─▶ 编码到 swapchain
```

每个 `UiSurface`（窗口或渲染目标纹理）一组。都是 compute pass，在同一个 graph 里（0008 §6.1）。

### 5.1 UiBounds

- 一个线程处理一条命令：应用组变换，按抗锯齿宽度、阴影和发光的半径向外扩，再和裁剪链求交，得到屏幕上的包围盒。
- 包围盒为空的命令（完全在裁剪之外或者在屏幕之外）在这里就被剔除。

### 5.2 UiBin（粗分箱）

- 屏幕按 256×256 像素划分成 bin（每个 bin 是 16×16 个 tile）。
- 命令按 256 条一组（chunk）处理：一个 workgroup 处理一组，为每个 bin 生成一个 256 位的掩码，表示这一组里哪些命令碰到了这个 bin。
- 掩码保持了命令的顺序，所以后面不需要排序。4K 分辨率、10 万条命令时，掩码大约 1.7 MB。

### 5.3 UiCoarse（细分箱）

- 每个 bin 一个 workgroup，按顺序遍历所有 chunk 的掩码，对每条碰到这个 bin 的命令算出它覆盖的 tile（16×16 像素），追加到这些 tile 的命令列表里：
  - 一般的图元：包围盒覆盖的 tile；
  - 线段和描边：沿线段做保守的 DDA，只分进真正穿过的 tile。这对应 Decima 后来用 Wu 算法做的优化，网格线框那种几十万条线段的情况靠它才撑得住；
  - 路径：线段分进它们穿过的 tile，同时为路径算每个 tile 的 backdrop（§4.2）。
- 一个 bin 里的 tile 列表只由这一个 workgroup 写，而它按命令顺序遍历，所以列表天然有序。
- **不透明遮挡**：一条命令完全不透明地盖住整个 tile 时（面板背景就是这样），清空这个 tile 在它之前的列表。底层被盖住的命令不再着色。
- tile 列表的存储按 bin 从一个全局池里成块分配。

### 5.4 tile 大小

- 16×16 的 tile，每个 tile 一个 256 线程的 workgroup，一个线程一个像素。
- Decima 选 8×8 是因为像素着色器按 8×8 调度。这里是 compute，16×16 让分箱的开销降到四分之一，而一个 tile 内所有线程遍历同一份命令列表，命令数据可以用标量加载，wave 的利用率不受影响。

### 5.5 UiDamage

- UiCoarse 同时为每个 tile 算一个哈希：tile 列表里每条命令的内容哈希（CPU 写命令时算好）、外部图像的版本号、字形缓存的版本号，按顺序合并。
- 和上一帧这个 tile 的哈希（每个 surface 一个持久 buffer）相同时，这个 tile 被标为干净：UiFine 跳过它，UI 目标里保留上一帧的像素。
- UI 目标是持久资源（0007 §8 的 `g.persistent`，键是 surface）。
- **整帧跳过**：CPU 端比较整个绘制列表的哈希和所有外部图像的版本。都没变时，这个窗口这一帧不声明任何 UI pass，也不 present。editor 空闲时 GPU 几乎不工作，笔记本上省电。
- 视口图像每帧都在变的话，只有它覆盖的 tile 是脏的。

### 5.6 UiFine

- 每个脏 tile 一个 workgroup。线程遍历 tile 列表：
  - 读命令（tile 内一致，标量加载）；
  - 算覆盖率和颜色；
  - 在寄存器里按预乘 alpha 混合。
- 最后写进 UI 目标。SDR 窗口是 `Rgba8Unorm`（存的是 sRGB 编码值），HDR 窗口是 `Rgba16Float`（§8.3）。
- **背景模糊**（`backdrop_blur`）要读它下面已经画好的内容，所以它是一个分层点：CPU 按模糊命令把列表切成若干层，每层一次 UiFine，层与层之间对模糊区域做一次降采样加高斯模糊，再接着画。每帧的模糊命令通常只有几个。

### 5.7 编码

- 最后一个 pass 把 UI 目标转换成 swapchain 的格式：sRGB、PQ 或 scRGB（§8）。swapchain 支持 UAV 时用 compute，否则用一个全屏三角形。
- swapchain 的图像每帧轮换，所以即使 UI 没有脏 tile，只要这一帧要 present，就要做这一次拷贝。

## 6. 文字

### 6.1 管线

```
TextLayout（shaping、换行，marlin-font）─▶ painter.text ─▶ 字形命令（字形 id、位置）
字形请求 ─▶ 栅格化 job（CPU，并行）─▶ 字形缓存（GPU）─▶ 字形间接表
```

### 6.2 栅格化

- 按**实际的像素大小**栅格化：字号 × DPI 缩放 × 组变换里的均匀缩放。
- 水平方向有 4 个子像素位置（0、¼、½、¾），垂直方向对齐到整像素。
- 8 位灰度覆盖率，小字号可以选做 stem darkening。不做 LCD 子像素抗锯齿。
- 栅格化在 `marlin-font` 里用 CPU 做，按字形分给多个 job。新字形在 Render 阶段建图之前栅格化完，然后用一个 copy pass 写进缓存。

### 6.3 字形缓存

- `R8` 的纹理数组，每页 2048²，用 skyline bottom-left 打包（和 Decima 一样）。彩色字形（emoji）用单独的 `Rgba8` 页。
- 每个字形条目以 (字体, 字形, 像素大小, 子像素位置) 为键。
- 页满了，就按页做 LRU：把最久没用的那一页整页清空重新打包，不做逐个字形的碎片回收。
- 命令里只存字形 id。id 到缓存矩形的映射是一张每帧更新的间接表（`Glyphs`，§3.1）。字形被重新打包时只需要更新这张表，命令和保留的 chunk 都不用改。

### 6.4 大字号与任意变换

- 下面这些情况，文字改用路径填充（§4.2），不占字形缓存：
  - 有效像素大小超过阈值（默认 64 px）；
  - 组变换带旋转、斜切或非均匀缩放；
  - 调用方标记了"正在动画缩放"。
- 字形轮廓（字体单位）按需上传到一个 GPU buffer，每个字形只传一次；路径命令引用它，再加上变换。
- 节点图连续缩放时，栅格化文字的像素大小每帧都在变，会不停地生成新字形。缩放过程中走路径填充，停下来之后再换回栅格化的清晰版本。

### 6.5 shaping

- shaping 和换行在 `marlin-font` 里：OpenType 的 GSUB/GPOS、双向文本、断行规则。它的设计不在本文范围内（§15）。
- `TextLayout` 是 shaping 的结果：字形 id 和它们的位置。它可以缓存，文字不变时跨帧复用。

## 7. 图像

| 来源 | 用途 | 采样 |
|---|---|---|
| 图标图集 | 小图标 | cook 好的图集，每个图标有多个 DPI 版本 |
| 独立纹理 | 大图、缩略图、视频帧 | 自己的 bindless 下标 |
| 外部图像 | 视口、渲染到纹理的结果 | `ExternalImageId`，建图时映射成 graph 句柄 |

- `ImageSource::External(id)`：录制时 graph 还不存在，所以只写 id。`UiRenderer::add_passes` 接收一张 `ExternalImageId → TextureHandle` 的表，在 pass 里声明读取（0008 §6.1）。
- 采样是双线性的，mip 级别按变换的缩放选择；颜色是预乘 alpha 的。
- 图像命令可以带圆角，按圆角矩形的 SDF 裁剪。

## 8. 色彩

### 8.1 混合空间

- 在 **sRGB 编码空间**里混合，使用预乘 alpha。这和浏览器、Figma 一致，设计稿和实际画面对得上，半透明和抗锯齿边缘的观感也是设计师预期的样子。
- paint 的颜色按 sRGB 给出。

### 8.2 渐变

- 默认在 sRGB 编码空间里插值。
- 可以选 Oklab 插值：中间色不会发灰。
- 渐变在渐变表纹理里预先采样，插值空间在烘表时就处理好了。

### 8.3 HDR 窗口

- UI 目标是 `Rgba16Float`，存的是"相对于 paper white 的扩展 sRGB 编码值"：1.0 就是 paper white，超过 1 的值表示比 paper white 更亮。
- 视口图像（已经是窗口的显示空间，0008 §6.3）在采样时转换成同样的表示，所以半透明的 UI 叠在 HDR 高光上，混合规则和 SDR 一样。
- 编码 pass：扩展 sRGB → 线性 → 乘以 `paper_white_nits` → PQ 或 scRGB。
- 游戏 HUD 用同一套：HUD 的亮度跟随玩家的 paper white 设置。

## 9. UI 效果

- 自定义效果是 `IUiEffect` 材质（0011 §10.5）：给定局部坐标、SDF 距离和时间，返回预乘 alpha 的颜色。
- UiFine 是一个 kernel，所以 cook 和 closure 一样生成一个 switch 分发（0011 §5.4），UiFine 实例化成 `UiFine<UiEffectDispatch>`。没有效果命令的 tile 不会进入这个 switch。
- 效果的参数放在命令的 payload 里。
- 限制：不能写资源，不能读场景缓冲（读视口图像要通过 image 命令）。

## 10. 世界内 UI

- `UiSurface` 的种类是 `Texture` 时，目标是一张持久的、带 mip 的纹理，它的 bindless 下标是稳定的。
- 材质把它当作普通纹理采样，比如作为自发光贴图。
- 内容变化时才渲染（§5.5 的整帧跳过同样适用），也可以限制更新频率。渲染之后生成 mip。
- 输入（射线和面板的交点换算成 UV）属于 gameplay，不在渲染器里。

## 11. 多线程与 CPU 开销

- CPU 只做录制：写命令、写 payload、算命令的内容哈希。不做分箱，也不做栅格化（字形除外）。
- 面板可以并行录制成多个 chunk；没变的子树复用保留的 chunk（§2.3）。
- 新字形的栅格化在 job 里并行。
- 目标：复杂的 editor 界面（上万条命令）每帧 CPU 开销小于 0.5 ms。

## 12. 调试

- 每个 tile 的命令数热图。Decima 正是靠这种热图发现了斜线的分箱问题。
- 损伤叠加层：显示这一帧着色了哪些 tile。
- 分层视图：显示背景模糊切出的各层。
- 统计：命令数、tile 列表的总长度、平均每 tile 的命令数、被不透明遮挡剔除的命令数、字形缓存的占用。

## 13. 落地

| 阶段 | 内容 |
|---|---|
| U1 | 矩形、圆、线段、三角形、文字（栅格化）、图像；GPU 分箱的完整流水；单窗口 |
| U2 | 路径填充；组的变换、阴影和发光；裁剪链；损伤检测和整帧跳过；多窗口；视口图像 |
| U3 | UI 效果；背景模糊；渲染到纹理；HDR 窗口；大字号文字走路径 |

U1 完成后，editor 的窗口外壳就可以换成它；U2 完成后，视口嵌进 UI。

**U1 的记录**（2026-10-18 完成）：U1 分成 U1a–U1g 七步，每步一个提交，各带测试。定下的细节：

- **字体资产（U1a）**：`Font` 是按名字注册的资产类型（`marlin_font::Font`），新 crate `marlin-cook-font` 自己解析 TrueType（`head`、`maxp`、`hhea`、`hmtx`、`OS/2`、`cmap` 格式 4 和 12、`loca`、`glyf` 含组合字形、`name`），运行时只读烘焙后的格式，不解析字体文件。烘焙格式：
  - 竖直度量：`OS/2` 设了 `USE_TYPO_METRICS` 时取 typo 的三个值，否则取 `hhea`；另存 cap height、x height。
  - 字符映射存成区间（连续字符对应连续字形的合成一段）；每个字形存 advance、左侧距和包围盒。
  - 轮廓是闭合的二次曲线段，坐标用 **半字体单位** 的 `i16`：TrueType 两个 off-curve 点之间隐含的 on-curve 点正好在半单位上，所以每个点都精确。一个 n 段的轮廓存 2n 个点（起点、各段的控制点和终点，最后一段回到起点）；直线段的控制点等于起点。
  - 组合字形在 cooker 里展平，分量的点在变换之后取整到整字体单位。CFF 轮廓（`OTTO`）暂不支持，cook 时报错并说明。
  - 引擎的 UI 字体是 Inter 4.1 Regular（SIL OFL），放在 `content/engine/fonts/`（字体文件走 Git LFS），名字 `Marlin.Engine.Fonts.Inter`，按名字加载（framework 的 `load_ui_font`）。
- **栅格化和字形缓存（U1b）**：
  - 栅格化用精确的面积累积（font-rs 的做法）：每条线段把它扫过的带符号面积加到经过的像素上，按行前缀和得到覆盖率，取绝对值截到 1（相当于 non-zero 规则）。二次曲线按拍平误差不超过 1/64 像素切成直线。
  - 字形 id = key 的序号 × 4 + 子像素位置。key 是（字体，字形，以 1/64 像素计的字号）。`GlyphIds` 带锁，多个 painter 并行录制时共用一份。
  - 页是 2048² 的 `R8`，默认最多 4 页（16 MB），纹理至少 2 层，保证视图总是数组；字形四周留 1 像素空白。新字形在 job 里并行栅格化（8 个一组）。
  - LRU 的保护规则：本帧画到的字形如果在帧开始前就在某页里，这一页不淘汰；本帧刚放进某页的新字形不保护这一页，这页被淘汰时，这些字形重新排队放置。淘汰之后仍放不下的字形本帧不画（`PrepareStats::dropped`），下一帧再试。
  - `TextLayout` 是最小版：按字符映射和 advance 排，`\n` 换行，没有字距、连字和其他 shaping（§6.5、§15）。
- **命令格式（U1c）**，`marlin_ui_render::command`：
  - 头 16 字节：kind（6 位）、flags（10 位）、组（16 位）、裁剪（16 位）、paint 下标、payload 偏移（字）。组和裁剪在 U1 里都是 0。payload 是 32 位字，各图元的布局写在模块文档里。
  - paint 4 个字：种类、颜色（sRGB、直通 alpha 的 RGBA8），图像 paint 另有纹理视图下标和采样器下标。纹理放在 paint 里，shader 可以按类型读到 `Texture2D` 和 `Sampler`。图像的采样器默认是 `DEFAULT_SAMPLER`，`prepare` 时换成 renderer 自己的双线性 clamp 采样器。相邻的同色命令共用一个 paint。
  - 坐标是物理像素，DPI 缩放在写命令时乘上。
  - chunk：`Painter::chunk` 预留位置，`ChunkSlot` 可以交给别的线程录制，再用 `UiDrawList::fill` 放回。`RetainedChunk::replay` 的偏移对齐到整物理像素，所以文字的子像素位置和字形 id 不变（§2.3 说的"只是一次内存拷贝"，外加坐标平移）。
- **图元的细节（U1c）**：
  - 描边以边缘为中心，`|d| − w/2`；填充在描边下面，按预乘 alpha 合成。
  - 圆角逐角给出，每个角最多取短边的一半。虚线一律是平头，和端点样式无关。
  - 不透明内部（§5.3 的遮挡用）：矩形按最大的圆角向内缩；圆取内接正方形。只有填充和描边都不透明的矩形和圆才标 `OPAQUE`。
  - 图像：默认按 sRGB 视图采样（得到线性值、直通 alpha），编码回 sRGB 后再预乘；`ImageColor::SrgbPremultiplied` 的纹理原样使用。mip 用显式的 lod（纹素与像素之比的 log2）。
- **CPU 参考（U1c）**：`shape` 和 shader 逐个表达式对应，`reference` 用它分箱和着色。包围盒只用加减、乘常数、min、max、floor、ceil，CPU 和 GPU 的结果逐位相同。线段是否碰到 tile 要开方和做除法，两边的舍入可能不同，所以离阈值 1/64 像素以内的条目在参考里标为可选（GPU 可以有也可以没有，这样的条目在 tile 里本来就没有墨迹）。
- **GPU 管线（U1d、U1e）**：shader 是 `content/engine/shaders/Ui/Ui.mshader`，namespace `Marlin.Engine.Ui`，入口 `GlyphUpload`、`Bounds`、`Bin`、`Coarse`、`Fine`、`EncodeVs`、`EncodePs`（命名方案是用户定的，graph 里的 pass 名仍叫 `UiBounds`……）。
  - `UiBounds` 每个线程处理一条命令，把包围盒和不透明内部打包成 16 位的坐标。
  - `UiBin` 用全局原子 OR 写掩码，布局是 chunk × bin × 8 个字。
  - `UiCoarse` 每个 bin 一个 16 × 16 的 workgroup，每个线程负责一个 tile，按命令顺序遍历掩码（`FirstBitLow`）。线段的判定是：tile 中心到线段的距离 ≤ 半线宽（方头乘 √2）+ tile 的半对角线 + 0.5，相当于保守 DDA。遇到不透明遮挡时把计数清零，已有的块原样复用。
  - tile 列表的块是 32 个字：31 条命令，加上下一块的下标。池的大小按读回的已用块数乘 1.5 增长，溢出了就翻倍。读回用 renderer 自己的 readback buffer，等这个帧槽再轮到时读，不需要 `FrameContext`。
  - `UiFine` 每个 tile 一个 workgroup，写 `Rgba8Unorm` 的 UI 目标（sRGB 编码值，预乘）。`EncodePs` 用全屏三角形写进 swapchain，目标是 sRGB 视图时先解码成线性。
  - 新字形由一个 compute pass 写进 `R8` 存储纹理数组。这要求 `R8Unorm` 支持 `STORAGE_WRITE`，`UiRenderer::new` 会检查。页纹理由 `UiRenderer` 自己持有，每帧导入 graph。
  - 命令、payload、paint 和字形表用 `ctx.upload_slice` 在每个用到它们的 pass 里各传一份（每帧几百 KB 的拷贝）。以后改成一次上传到 GPU buffer。
- **接入（U1f、U1g）**：framework 的 `ui_shaders` 检查每个入口的 stage 和 push 大小（`UI_PUSH_BYTES`，另有 GPU 测试拿它和编译出的 shader 对比），`InPlace<Font>` 原地加载字体。`marlin-editor <project> [--frames N]` 在进程内运行项目的 cook 服务并挂载，按名字加载引擎 shader 和 UI 字体，每帧用 UIRenderer 画窗口外壳（`marlin_editor::shell`：菜单、工具栏、大纲、细节、内容浏览器、视口占位、状态栏，带悬停高亮）。U1 没有裁剪，所以先画视口，再画面板，盖住视口溢出的网格线。
- **验收**：
  - GPU 的 tile 列表和 CPU 参考逐条一致：示例场景在三种尺寸下（含不是整 bin 的尺寸）、三轮 900–1500 条随机命令、空列表、池溢出后增长。
  - 着色和参考光栅比较：场景每通道最多差 3 级，差 1 级以上的像素不超过 1%；文字最多差 1 级；图像最多差 3 级（硬件双线性的权重是 8 位小数）；编码进 sRGB 目标最多差 1 级。
  - 新的参考图 `ui_scene`、`editor_shell`。
  - editor 开着验证层跑 `samples/basic` 300 帧，没有报错。
- **没做**（U1 的范围之外，或者要先定设计的）：
  - §2.1 的渐变 paint：§13 的阶段表里没有列，建议放进 U2。
  - 圆弧、折线和贝塞尔描边：§4.1 有，§13 的 U1 没有列。
  - 裁剪和组、损伤检测和整帧跳过：属于 U2。
  - 字距和 shaping、CFF 字体、cook 好的图标图集。

## 14. 测试

- **平台无关**：
  - 分箱的 CPU 参考实现：同一份绘制列表，CPU 算出的每个 tile 的命令列表就是期望值（GPU 测试用它比对）；
  - 每种 SDF 图元的距离函数和解析覆盖率（在 CPU 上超采样积分）对比；
  - skyline 打包器、字形缓存的 LRU、间接表；
  - chunk 的并行录制和拼接保持顺序；
  - 保留的 chunk 在字形缓存重排后依然正确。
- **平台相关**（真实硬件）：
  - GPU 分箱输出的 tile 列表和 CPU 参考逐条一致；
  - 着色结果和 CPU 参考光栅化（同样的公式、同样的混合顺序）在容差内一致；
  - 开着和关着损伤检测，连续若干帧的输出逐像素相同；
  - 路径填充和参考光栅化器（高倍超采样）在容差内一致；
  - 不透明遮挡剔除不改变结果。

## 15. 待定

- `marlin-font` 的 shaping 设计（单独的文档）：自研 OpenType shaping 的范围，复杂文字放在第几阶段。
- 输入法的候选窗和文字光标的渲染细节。
- 视频播放的解码路径，在本文里只是一个"独立纹理"。
- 无障碍（屏幕阅读器）：属于 editor 的 UI 框架，不在渲染器里。
- 损伤检测之外的进一步省电：editor 在后台时整体降低帧率。
