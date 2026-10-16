# 0008 — UI 渲染、字体与文字

状态：草案 · 2026-10-16

前置：[0004](0004-rendering-overview.md)、[0005](0005-rhi-and-render-graph.md)。

参考：Wessel Mast，*UIPainter: Tile-Based Rendering in One Draw Call*（GDC 2024）。下面是 Decima UIPainter 做法的要点，以及 Marlin 在它基础上做的改动。

| Decima UIPainter | Marlin |
|---|---|
| 图元是 SDF：圆角矩形、圆、扇形、线、三角形，外加文字和图像 | 保留，另加贝塞尔描边、任意填充路径、渐变和图案填充 |
| SDF 的 1 px 衰减提供抗锯齿 | 保留；填充路径改用精确面积覆盖率 |
| 一个 primitive group 共用阴影和发光 | 保留 |
| 所有命令按顺序放进一个 buffer，一个 pass，在寄存器内混合 | 保留：fine raster 阶段是一个 compute pass，每像素在寄存器里混合 |
| 8×8 tile，与 wave 对齐 | 保留 |
| **CPU** 端按包围盒分配 tile，线段用 Wu 算法 | **改到 GPU**：两级 binning（64×64 → 8×8），精确的覆盖测试在 GPU 上做。演讲里提到 CPU 是主要瓶颈，下一步的方向是把分配搬到 GPU |
| 字形按精确尺寸光栅化，用 skyline 打包进缓存 | 保留，加 LRU；世界空间的文字用 MSDF |
| 每帧全量重绘 | 改为：控件缓存命令段，GPU 按 tile 哈希跳过没有变化的 tile |
| 用全屏三角形加 pixel shader | 改为 compute：可以跳过 tile，可以按 wave 一致地加载命令，binning 的写入也需要 compute |

## 0. 决定摘要

| 维度 | 决定 |
|---|---|
| 分层 | `font`（字体和文字）→ `ui_render`（立即模式绘制 API 和 GPU 管线）← `ui_core`（保留模式，编辑器和游戏共用） |
| binning | GPU compute：先按 64×64 粗 bin（保持命令顺序），再细分到 8×8 tile 列表（精确覆盖测试、tile 内遮挡剔除、tile 哈希） |
| 着色 | 每个 tile 一个 8×8 线程组，在寄存器里按顺序混合，在 sRGB 空间混合，带裁剪和图层栈 |
| 图元 | Decima 的那套，加二次和三次贝塞尔描边、填充路径（nonzero / evenodd）、线性、径向和锥形渐变、图案填充 |
| 文字 | 按精确像素尺寸光栅化字形（1/4 px 子像素相位），skyline 图集加 LRU；大字号和 3D 文字用 MSDF |
| shaping | 自研完整 OpenType shaping，包括 GSUB、GPOS、bidi、复杂文字脚本、断行和字体回退 |
| 缓存 | CPU：命令段按控件保留在 GPU 上，只上传有变化的段；GPU：每个 tile 算哈希，和上一帧一样的就跳过 |
| 色彩 | UI 内部在 sRGB 空间混合；合成时转换到输出空间，HDR 下按 paper-white 缩放 |
| 游戏 UI | 复用 `ui_core`，布局写在 .masset 里，逻辑通过 VM 绑定 Signal |

## 1. 分层与数据流

```
ui_core（保留树、布局、Signal）
  └─ 脏控件 paint(&mut Painter) ──▶ CommandSegment（每控件一段，CPU arena + GPU 持久段）
ui_render
  ├─ UiFrame：段顺序表 + 脏段上传 + 字形请求
  ├─ font：shaping（布局缓存）、字形光栅（job）、图集上传
  └─ render graph passes：Expand → PathFlatten → CoarseBin → FineBin(+Hash) → FineRaster(dirty tiles) → Compose
```

- `ui_render` 不依赖 `ui_core`。只要能产出 `Painter` 命令就可以直接用 `ui_render`，例如编辑器的调试叠加层和游戏内的开发工具。
- 所有坐标在**录制时**就转换成物理像素，DPI 缩放和控件变换都在 CPU 上预先乘好。所以 SDF 的距离单位始终是像素，1 px 的抗锯齿衰减在任何缩放下都成立，节点图缩放时也不需要特殊处理。只有旋转需要每条命令带一个 2×2 矩阵，像素位置先变换到图元的局部坐标再求 SDF。

## 2. 字体与文字（`marlin-font`，L3，no_std）

### 2.1 解析

- 字体格式：
  - TrueType（`glyf`，二次曲线）；
  - CFF / CFF2（三次曲线，自研 charstring 解释器）；
  - 可变字体（`fvar`、`gvar`、`avar`、`HVAR`），放到第二阶段。
- 读取的表：`cmap`、`hmtx`/`vmtx`、`OS/2`、`name`、`GDEF`、`GSUB`、`GPOS`、`kern`（旧格式）。
- 字体文件作为 bulk 数据加载，就地 mmap 解析，不做拷贝。`cook_font` 可以按游戏实际用到的字符集做子集化，这对 CJK 字体能显著减小体积，但它是可选的。

### 2.2 文字处理流水线

```
文本 + 样式 ─▶ 分段：bidi（UAX #9）、脚本（UAX #24）、字体回退（按 cmap 覆盖查回退链）
           ─▶ 每段 shaping：GSUB（ccmp/locl/liga/rlig/脚本特性…）→ GPOS（kern/mark/mkmk/curs）
              复杂脚本 shaper：Arabic（joining）、Indic、USE、Hangul、Thai/Lao、Khmer、Myanmar
           ─▶ 断行：UAX #14 + CJK 禁则；对齐与两端对齐
           ─▶ TextLayout：glyph run 列表（字体、字号、glyph id、位置、cluster 映射）
```

- Unicode 数据表在构建期由 UCD 生成，存成紧凑的 trie，no_std 可用。
- **布局缓存**：键是 (文本哈希, 样式, 宽度约束)，值是 `TextLayout`，按 LRU 淘汰。编辑器里大部分文字每帧都是一样的，所以几乎每帧都命中缓存。
- 编辑需要的接口也由 `TextLayout` 提供：光标、选区、命中测试（cluster 到字符位置的映射），代码编辑器控件会用到。

### 2.3 光栅化与字形缓存

- **光栅器**：用精确面积覆盖率做扫描线累加，在 f32 缓冲上进行，SIMD 实现。二次和三次曲线按误差细分后再累加。小字号可选 stem darkening。不实现 TrueType 字节码 hinting。
- **缓存的键**是 (字体, glyph, 像素尺寸量化到 1/4 px, 水平子像素相位 1/4 px, 可变实例哈希)。
- **图集**：
  - R8 纹理数组，每页 2048²。
  - 每页用 skyline bottom-left 打包（Decima 的做法）。
  - 每个槽位记录"最后使用的帧号"。
  - 页满时先回收这一页里过期的槽位；回收后还是放不下，就整页清空，选最久没用的页，然后重新打包。
  - 页数按档位和平台设上限。
- **新字形**在 job 里并行光栅化，结果通过上传 ring 写进图集。写图集的 copy pass 排在 UI 的 render graph 之前。
- **MSDF**：字号大于阈值（例如 96 px）、带旋转或透视（3D 文字）时，改用 MSDF 字形：32 px 基准、4 px 距离范围、RGB8 图集。生成方式有两种：运行时在 job 里按需生成；或者由 `cook_font` 为指定的字体预先生成。
- **彩色字形**：先支持 bitmap emoji（CBDT/sbix），它们放进图像图集；COLR v0/v1 放到以后。

## 3. 绘制 API 与命令编码（`marlin-ui-render`）

### 3.1 Painter

```rust
let mut p = frame.painter(segment);            // 录到某个 CommandSegment
p.rounded_rect(rect, Radii::all(6.0), Paint::solid(BG)).border(1.0, Paint::solid(EDGE));
p.group(Effects { shadow: Shadow { offset, blur: 8.0, color }, glow: None }, |p| {
    p.rounded_rect(handle_track, Radii::all(3.0), Paint::solid(ORANGE));
    p.circle(knob_center, 7.0, Paint::solid(WHITE));
});
p.arc(center, r_inner, r_outer, a0, a1, paint);          // 扇形 / 圆环段
p.line(a, b, width, Cap::Round, paint);
p.polyline(points, width, Join::Round, paint);
p.cubic_stroke(p0, p1, p2, p3, width, paint);             // 节点图连线
p.fill_path(&path, FillRule::NonZero, paint);             // SVG 图标
p.image(rect, ImageRef::atlas(icon) | ImageRef::texture(view), tint);
p.nine_slice(rect, image, insets);
p.text(&layout, origin, paint);
p.push_clip(ClipShape::rounded_rect(r, radii)); … p.pop_clip();
p.push_layer(opacity, blend_mode); … p.pop_layer();
```

`Paint` 的种类：
- 纯色；
- 线性、径向、锥形渐变（色带存在渐变图集的一行里，最多 16 个色标，第一版只支持在 sRGB 空间插值）；
- 图案（图像加 2D 变换，可平铺）；
- UI 材质（§4.5）。

### 3.2 命令编码

- 每条命令 = 16 字节的头 + 变长的几何负载，存在一个 ByteAddressBuffer 里。
- 头部：
  - `kind`（8 bit）、`flags`（8 bit）、`group`（16 bit）；
  - `paint` 下标（u32），指向 paint 表；
  - `clip` 下标（u32），指向 clip 表；
  - 包围盒（4×i16，物理像素）。
- 负载举例：
  - 圆角矩形：中心、半宽高、4 个角的半径、描边宽度和描边 paint，共 32 字节；
  - 线段：两个端点、宽度、端点样式；
  - 三次贝塞尔：4 个控制点、宽度；
  - glyph run：字号、glyph 实例区间。glyph 实例是 (图集矩形 u16×4, 位置用 24.8 定点数)。
- 填充路径的负载是路径段区间：直线、二次和三次曲线，写在单独的路径段 buffer 里。CPU 不展平曲线。
- 控制类命令：`GroupBegin`/`GroupEnd`（带效果参数）、`PushClip`/`PopClip`、`PushLayer`/`PopLayer`。

### 3.3 命令段（CommandSegment）

- 一个段是一串连续的命令，通常对应一个控件。
- 段在 CPU 侧录在 arena 里，录完后上传到 **GPU 持久命令池**：一个大 buffer，按段分配，空闲块合并。
- 每帧的绘制顺序由一个**段顺序表**描述，里面是若干 (段 ID, GPU 偏移, 命令数)。
- 段没有变化时不需要上传，只要顺序表引用它即可。界面静止的那一帧，CPU 只需要上传一张很小的顺序表。

## 4. GPU 管线

整个管线由一组 render graph pass 构成。除了最后的 Compose，其余都是 compute，可以放到异步计算队列上，和场景的光栅 pass 重叠执行。

### 4.1 Expand

- 按段顺序表做前缀和，得到全局命令序号，然后为每条命令写一个间接引用 `cmd_ref[i] = (段内偏移)`。
- glyph run 在这一步**展开成单个字形命令**，这样后面的 binning 看到的都是单个字形，一个 tile 只会遍历落在它上面的那几个字。
- 填充路径的路径段在这一步被送进 PathFlatten 的工作队列。

### 4.2 PathFlatten 与路径覆盖准备（只处理填充路径）

1. **展平**：二次和三次曲线按屏幕误差（0.25 px）细分成线段，计算路径的包围盒，回填到命令头。
2. **线段分 tile**：把每条线段分配到它穿过的 8×8 tile。
3. **backdrop**：对每条路径，沿 tile 行做前缀和，得到每个 tile 左边界处的 winding 数。这是 Vello 的思路。
4. 结果：对每个 tile，得到"这个路径在这里有哪些线段"和"backdrop"。整块被路径内部覆盖的 tile，在后面会被标记成"实心填充"，fine raster 阶段不需要再算覆盖率。

### 4.3 CoarseBin（64×64 px bin，保持顺序）

- 每个工作组处理 256 条命令，每个线程负责一条，算出它覆盖哪些 bin。
- 工作组内为每个 bin 维护一个 256 位的掩码（共享内存，用 atomicOr 写入），然后统计每个 bin 的命令数。
- 对全局 (chunk, bin) 计数做前缀和，得到每个 bin 的命令列表。列表先按 chunk、再按掩码位排序，**正好就是命令的原始顺序**。

### 4.4 FineBin（8×8 tile 列表）

- 每个 bin 一个工作组，64 个线程分别对应 bin 里的 64 个 tile。
- 按顺序遍历 bin 里的命令，每个线程对自己的 tile 做**精确的覆盖测试**：

  | 图元 | 测试 |
  |---|---|
  | SDF 图元 | 图元到 tile 中心的有符号距离 ≤ tile 半对角线 + AA 宽度 + 效果外扩（阴影和发光的半径）。线段只会落到它真正穿过的 tile 上，效果等同于 Decima 在 CPU 端用 Wu 算法做的那件事，但是在 GPU 上并行完成 |
  | 贝塞尔描边 | 用控制多边形的凸包求保守距离，然后逐段细化 |
  | 填充路径 | tile 里有线段，或者 backdrop ≠ 0 |
  | 字形和图像 | 矩形相交 |

- 命中的命令追加到这个 tile 的命令列表（PTCL），顺序与原命令一致。
- **tile 内遮挡剔除**：如果某条命令不透明，并且覆盖了整个 tile（实心矩形、实心路径的内部 tile），而且没有裁剪和图层，就清空这个 tile 已经积累的列表，从这条命令重新开始。编辑器里大量面板相互叠在一起，这一步能省下大部分像素工作。
- **tile 哈希**：把这个 tile 最终命令列表里每条命令的内容哈希和顺序组合起来，与上一帧的 tile 哈希 buffer 比较。不同的 tile 进入"脏 tile 列表"，用于后面的 indirect dispatch。
  - 命令的内容哈希在 Expand 阶段由 GPU 根据命令字节计算。
  - 引用动态纹理的图像命令（比如编辑器的视口），负载里带一个版本号，这样纹理内容一变，哈希也会变。
  - 窗口大小变化、字形图集的某一页被清空时，会强制整帧都标脏。
- PTCL 的存储：每帧用一个 GPU 线性分配器分配。容量取上一帧实际用量的 1.5 倍。如果溢出，就设一个溢出标志并读回：编辑器会扩容后把这一帧重做一次；游戏会提前按峰值估算容量。

### 4.5 FineRaster

- 对脏 tile 做 indirect dispatch，每个 tile 一个 8×8 的线程组，每个线程对应一个像素。
- 每个像素按顺序遍历 PTCL。命令在整个 tile 内是一致的，所以加载命令走 wave 一致的标量读取。具体做法：
  - 求覆盖率：
    - SDF 图元用 `clamp(0.5 - d, 0, 1)` 的 1 px 衰减；
    - 填充路径用精确面积覆盖率，加上 backdrop，再按填充规则处理；
    - 字形用图集里的覆盖率；
    - MSDF 用 median 加屏幕距离换算。
  - 求颜色：按 paint 类型计算。
  - 混合：在寄存器里做预乘 alpha 的 over 混合，**在 sRGB 编码空间进行**。
- **group**：
  - 遇到 `GroupBegin` 时，先遍历一遍组内成员，算出它们 SDF 的并集（取 min），用来计算阴影和发光并混合进去；
  - 然后再按顺序绘制各个成员的填充。
  - 这和 Decima 的语义一致：整个形状只算一次阴影。
  - 阴影的半径有上限，因为 binning 的外扩量依赖这个上限。
- **裁剪**：
  - 矩形裁剪在 CPU 上直接求交，合并成命令的 clip 矩形，GPU 上只是一次比较；
  - 圆角裁剪和路径裁剪用 `PushClip`/`PopClip`，在寄存器里维护一个覆盖率栈，深度 4，超过 4 层就溢出到 groupshared 内存。
- **图层**：`PushLayer`/`PopLayer` 维护一个颜色栈，用来实现组透明度和混合模式，同样是深度 4，可以溢出。
- **UI 材质**（UI 域，0007 §6）：cook 时把构建里所有 UI 域材质的 `UiFill` 函数，编译进 FineRaster shader 的一个 `switch` 分支里，所以 UI 管线只有一个 PSO。这类材质的数量有预算，超出时报警。
- **输出**：写进持久的 UI 目标纹理，格式是 RGBA8Unorm，存 sRGB 编码的预乘颜色。没有标脏的 tile 保持上一帧的内容。

### 4.6 Compose

- **编辑器窗口**：一个全屏 pass 把 UI 目标拷进 swapchain 并做格式转换。D3D12 的 flip-model swapchain 不能当 UAV 用，所以没法直接写。
- **游戏**：在输出编码 pass 里把 UI 合成到 tonemap 之后的场景上（§6.2）。

## 5. 缓存策略汇总

| 层 | 机制 | 静止帧的成本 |
|---|---|---|
| ui_core | Signal 驱动，只有脏控件重新 paint | 0 |
| 命令段 | 没变的段留在 GPU 上，只上传顺序表 | 几 KB |
| 文字 | 布局缓存、字形缓存 | 0 |
| GPU | 每帧仍要做 Expand、Bin 和哈希比较，但脏 tile 数为 0，所以 FineRaster 不执行 | binning 的固定成本，预计 < 0.1 ms（4K） |
| Compose | 编辑器窗口没有任何脏 tile、也没有视口更新时，可以跳过 present（看窗口系统是否允许） | 0 |

## 6. 与渲染器的集成

### 6.1 Render graph

- `ui_render::add_passes(g, &ui_frame, UiTarget) -> UiOutput`。
- Expand、Bin 和 Raster 加了 `async_compute` 提示，Compose 在 Graphics 队列上。
- 一帧可以有多个 UI 根（多个窗口、world-space UI），每个根有自己的一组 pass 和目标纹理。图集和命令池是共享的。

### 6.2 色彩与 HDR

- UI 在 sRGB 编码空间内混合，所以半透明和抗锯齿边缘的效果与 Figma、Photoshop 一致。
- **SDR 输出**：`out = ui + (1 - ui.a) · scene_srgb`。这里的 `scene_srgb` 是 tonemap 加 OETF 之后的场景，ui 是预乘颜色。
- **HDR 输出**：
  1. 把 UI 解码到线性空间，乘以 `paper_white` 亮度（nits，玩家可以调）；
  2. 和线性的场景（nits）做 over 混合；
  3. 编码成 PQ 或 scRGB。
  
  半透明 UI 叠在 HDR 场景上的效果和 SDR 下略有不同，这是有意接受的。
- 编辑器窗口固定使用 SDR 的 sRGB swapchain。编辑器里的 HDR 预览，是视口内部的场景按 HDR 管线输出，再做一次显示映射。

### 6.3 视口、world-space UI 与 3D 文字

- **编辑器视口**：场景渲染到纹理，用 `image(ImageRef::texture(view))` 显示，所在 tile 每帧都是脏的。
- **world-space UI**：
  - `UiSurface` component 持有一个 UI 根和一张目标纹理，UI 按 §4 渲染进去。
  - 场景里用一个材质（Surface 域，Unlit 或自发光）采样这张纹理。纹理分辨率按屏幕上的投影大小选择。
  - 交互：射线与平面求交得到 UV，再交给这个 UI 根的命中测试。
- **场景内的 3D 文字**：直接作为场景的前向半透明图元来画，材质库里有一个 `msdf_text()` 采样函数。

## 7. `ui_core` 与游戏 UI

- `ui_core` 只依赖 core 和 alloc，包含：
  - 保留树；
  - 布局：flexbox 和 grid，用 taffy（no_std + alloc），按 0001 §6.1；
  - 细粒度响应式的 Signal、Memo、Effect；
  - 输入路由、焦点和导航图（手柄、键盘）；
  - 动画：属性 tween 驱动 Signal；
  - 图层：主层、弹出层、提示层、拖拽层，各层的段顺序依次拼接。
- **命中测试**在 CPU 上用布局树完成，不读 GPU。
- **编辑器**：控件用 Rust 编写（`ui_widgets`）；节点图画布直接用贝塞尔描边和 group。
- **游戏 UI**：
  - `UiLayout` .masset 描述控件树、样式和绑定表达式；
  - VM 提供 view model，Signal 暴露给 VM；
  - 支持本地化：文字走 §2 的完整 shaping；
  - 支持安全区和手柄导航。
- **立即模式调试 UI**：游戏内开发工具直接调用 `Painter`，外加一层很薄的 IM 控件库。不经过 `ui_core`。

## 8. 性能目标与调试

| 场景 | CPU（录制与上传） | GPU |
|---|---|---|
| 4K 编辑器，5 万条命令，整屏重绘（窗口大小变化） | < 2 ms（并行 paint） | < 1 ms |
| 同上，典型交互（悬停或拖动一个控件） | < 0.2 ms | < 0.2 ms |
| 静止 | ≈ 0 | < 0.1 ms |
| 节点图里 10 万条线段的网格线框 | < 3 ms | < 2 ms |

调试视图：
- 每个 tile 的命令数热度图（Decima 也有同样的工具）；
- 脏 tile 叠加；
- tile 内遮挡剔除的命中率；
- 字形图集查看器；
- 段缓存统计；
- PTCL 用量。

## 9. 测试

- **binning**：Rust 里有一份参考实现（覆盖测试和遮挡剔除的逻辑与 shader 共用同一套规则），与 GPU 产出的每个 tile 的列表做精确比较。随机生成命令做模糊测试。
- **图元像素**：
  - 轴对齐、并且对齐到像素的形状，覆盖率只会是 0 或 1，可以精确断言；
  - 斜边、曲线和路径的抗锯齿与 CPU 参考光栅器比较，每个通道允许 ±1/255 的误差（GPU 与 CPU 浮点运算顺序不同）。
- **tile 哈希**：命令不变时脏 tile 为 0；修改一条命令，脏 tile 集合恰好等于它新旧覆盖范围的并集。
- **shaping**：用离线生成的期望数据（glyph id 和位置）回归，覆盖拉丁、CJK、阿拉伯、天城文、泰文；bidi 和断行用 Unicode 官方测试文件。
- **字形光栅**：与参考覆盖率比较，误差 ≤ 1/255；图集的回收和重新打包后，内容保持一致。

## 10. 待定问题

1. 圆角裁剪和路径裁剪栈的深度上限，以及溢出到 groupshared 时的性能。需要实测。
2. FineRaster 要不要在两个 8×8 tile 之间共享一个 wave64，在 32 宽的 wave 上又怎么排布，需要按厂商调优。
3. COLR v1 彩色字体，以及可变字体的优先级。
4. 编辑器在无改动时跳过 present 的策略，各个窗口系统的行为不一样。
