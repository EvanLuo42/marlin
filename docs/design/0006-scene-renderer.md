# 0006 — 场景渲染器

状态：草案 · 2026-10-16

前置：[0004](0004-rendering-overview.md)、[0005](0005-rhi-and-render-graph.md)。材质相关细节见 [0007](0007-material-system.md)，环境、地形、粒子、后处理见 [0009](0009-environment-and-effects.md)。

## 1. 渲染世界与 GPU Scene

### 1.1 所有权

- 模拟侧（world/ECS）**从不**接触渲染侧的数据结构。渲染侧也**从不**读取 ECS。
- 两侧之间只有一条通道：每帧一份不可变的 `RenderDelta`，从帧 arena 分配，由 Render 阶段消费。

### 1.2 Proxy 与 ID

- 渲染侧对象统一叫 proxy，类型有 `MeshInstance`、`Light`、`Decal`、`FogVolume`、`ParticleSystem`、`ReflectionCapture`、`TerrainCell`、`Camera`/`View`。
- proxy ID 由**模拟侧**通过原子分配器预留，形式是下标加世代号。这样模拟侧在同一帧里就能引用刚创建的 proxy，比如 attach 关系、动态材质实例，不用等渲染侧往返一次。
- 渲染侧拿到 ID 后，下标直接作为 GPU 表的槽位。

### 1.3 Extract

extract 是 Game 阶段末尾的一组 ECS system。它只读 world，并行运行，并依赖 ECS 的 change detection（0004 §4）：

| 变化 | 产出 |
|---|---|
| 新增或删除带渲染 component 的实体 | `created: [(ProxyId, ProxyDesc)]`、`destroyed: [ProxyId]` |
| world transform 有变化（依赖 transform 层级） | `transforms: [(ProxyId, Affine3x4)]`，按 ID 排序，SoA 存放 |
| 材质、可见性、光源参数有变化 | `params: [(ProxyId, ParamPatch)]` |
| 动态材质实例的写入 | `material_patches`（0007 §8.3） |
| 骨骼姿势 | `skin_palettes: [(ProxyId, range)]` + 一块连续的矩阵数据 |
| 相机与 view | `views`（每帧全量，数量很少） |

### 1.4 应用 delta（Render 阶段第一步）

1. 更新 CPU 侧表：proxy 槽位、AABB 的 BVH（光源和 decal 的 CPU 剔除要用）、变化集合。
2. 把 transform、参数和新建的 proxy 打包进上传 ring。由一个 **GPU scatter pass** 写入持久 buffer。这个 pass 同时把旧的 transform 拷到 `prev_transform`，供运动向量使用。
3. 有变化的实例，会把它覆盖到的 VSM 静态页标记为失效（§5.3），同时标记它的 BLAS 和 TLAS 需要更新（§6.1）。

### 1.5 GPU Scene 的持久表

GPU Scene 由若干持久的 structured buffer 组成，容量按需倍增：

| 表 | 每项内容 |
|---|---|
| `instances` | transform 3×4、prev transform 3×4、局部 AABB、mesh ID、材质槽起点、flags（cast shadow、visible in RT、WPO、两面等）、LOD bias、自定义数据偏移 |
| `meshes` | LOD 数组（每级：meshlet 范围、顶点流位置、屏幕误差、驻留状态）、包围球、RT 用的 LOD 下标 |
| `material_slots` | 实例的第 i 个 section 用哪个材质实例 |
| `material_instances` | 材质类型（决定 bin）、参数块偏移、flags（0007 §8） |
| `lights` | 类型、颜色和强度、位置和方向、半径、锥角、IES 下标、VSM 下标、light function 下标 |
| `decals`、`fog_volumes`、`reflection_captures` | — |

- 这些表都常驻显存，各个 pass 通过 bindless 下标访问（下标放在 per-frame 常量里）。
- 规模目标：GPU Scene 能装 **一百万个实例**。CPU 每帧的成本只和**变化量**成正比。

## 2. 网格数据（`cook_mesh` 输出）

### 2.1 顶点流

为了按 pass 需要读取，顶点数据拆成三条流：

| 流 | 内容 | 使用者 |
|---|---|---|
| position | 每个 LOD 一块，坐标相对 mesh AABB 做 16 位量化（3×u16，按 8 字节对齐）。精度不够的大物件用 float32 | 光栅、VSM、BLAS 构建、命中重建 |
| attribute | 法线和切线用八面体编码，打包成 32 位；UV0–3 用 2×f16 或 unorm16 加范围；顶点色 RGBA8 可选 | 材质 resolve |
| skin | 4 或 8 个骨骼影响：u16 下标 + unorm8/16 权重 | 计算蒙皮 |

### 2.2 Meshlet

- 上限 64 个顶点、124 个三角形。三角形用 3×u8 打包，顶点用 u32 下标间接引用 LOD 的顶点流（顶点去重）。
- 每个 meshlet 带以下数据：
  - 包围球
  - 法线锥（轴和截止角，用 i8 量化）
  - 所属 LOD
  - **DAG 预留字段**：`group`、`lod_error`、`parent_error`、`error_sphere`。离散 LOD 下，`lod_error` 填所在 LOD 的误差，`parent_error` 填 +∞。以后升级成 cluster DAG 时，运行时的 LOD 判断逻辑不需要改（0004 §0）。
- cook 期间做的优化，全部自研（0003）：
  - 顶点缓存和局部性优化；
  - meshlet 聚类，优先保证空间紧凑和法线锥集中；
  - 二次误差度量的简化器，用来生成 LOD 链，边界顶点锁定，带属性误差权重。

### 2.3 LOD 与驻留

- 每个 LOD 是一个流送单元（页），对应 0003 的 bulk 数据。
- 最粗一级常驻内存。
- 选哪一级 LOD 在 GPU 上决定，依据是屏幕投影误差。如果目标 LOD 还没加载，就退回到已驻留的最近一级，同时写入流送反馈（§8.2）。

### 2.4 RT 代理

- 每个 mesh 指定一个 RT LOD，默认是"屏幕误差 ≈ 1 px 时对应的那一级"，再往粗调一档。BLAS 用的就是这一级的 position 流。
- 植被等对象可以单独指定更粗的 RT LOD。

## 3. 剔除与 Visibility Buffer

### 3.1 视图与输出

- 所有 view 共用同一套剔除和光栅代码，只是参数不同：主相机、编辑器视口、VSM 光源页面，以及反射捕获。
- 主视图的输出：
  - `VisBuffer`：R32Uint，内容是 `visible_cluster_index << 7 | triangle_index`。
  - `SceneDepth`：D32，reverse-Z。
  - `VisibleClusters`：一个 buffer，每项是 (instance, meshlet, view)，供 resolve 阶段解码。
- 如果某一帧可见 cluster 超过 2²⁵（约 3300 万），会报警并截断。

### 3.2 两阶段遮挡剔除

```
Phase 1:  上一帧可见的实例和 cluster（visibility bit）
          → 实例剔除（视锥 + 上一帧 HZB）→ cluster 剔除 → 光栅 → 深度
          → 用本帧深度生成 HZB
Phase 2:  上一帧不可见的部分 → 用新 HZB 测试 → 光栅新出现的部分 → 更新 visibility bit
          → 生成最终 HZB（下一帧 Phase 1 用）
```

**实例剔除**（compute）做这几件事：
- 视锥测试
- HZB 测试
- 距离和 LOD 选择
- 跳过不投影阴影的实例（VSM view 时）

通过的实例按 cluster 数量展开，输出 cluster 候选。

**cluster 剔除**可以在 amplification shader 里做，也可以用 compute 加 indirect mesh dispatch 实现，按平台实测选择：
- 视锥
- 法线锥背面
- HZB
- 选出的 cluster 按**光栅 bin** 写入不同的列表（§3.3）

**三角形剔除**在 mesh shader 里做：
- 背面
- 零面积，以及落不到采样点上的小三角形
- 视锥

### 3.3 光栅 bin

- 不需要材质参与光栅的 cluster 走**快速路径**：一条共用的 mesh pipeline，只写 visbuffer 和深度。
- 材质实现了 `opacity_mask` 或 `deform`（WPO）的 cluster，进入该材质自己的光栅 pipeline（0007 §6.1）。每个 bin 一次 `dispatch_mesh_indirect`。
- bin 的分配：先计数，再求前缀和，然后 scatter。CPU 只为"当前 GPU Scene 里存在的光栅 bin"录制 dispatch，空 bin 的参数为 0。
- **以后的可选项**：微小三角形用 compute 软件光栅写 64 位原子（需要 ext `atomic_int64`）。这个结构也能支撑以后的 cluster DAG。

### 3.4 计算蒙皮与形变

- 骨骼蒙皮和 morph target 在 compute 里写进每帧的 position 流和 attribute 流，并保留上一帧的 position，用来算运动向量。
- 后续的光栅、VSM、BLAS refit 和命中重建都读蒙皮之后的流。这样"蒙皮几何"在下游看起来和静态几何一样，不需要为它单独做 permutation（0007 §7.2）。
- 布料等模拟以后也写进同一个流。

### 3.5 运动向量

- 不单独做 velocity pass，在材质 resolve 里从 visbuffer 重建：同一个三角形分别用 `prev_transform` 和上一帧的 position 流投影，两者之差就是运动向量。
- WPO 材质由 resolve 再调用一次 `deform`，并把时间参数换成上一帧的。
- 运动向量写进 `Velocity` 纹理（RG16F），供 TSR、运动模糊和 ReSTIR 的时间重投影使用。

## 4. 材质 resolve（摘要，细节见 0007 §6）

```
VisBuffer ─▶ 分类：每像素得到 material_type → 按材质计数、求前缀和 → 像素列表（按 8×8 tile 顺序 scatter，保持局部性）
          ─▶ 每个材质一次 dispatch_indirect：重建属性（解析导数）→ material.surface() → Substrate 展平
          ─▶ 写入 Substrate material buffer + Velocity + Emissive
          ─▶ tile 复杂度分类：Simple / Single / Complex / Special
```

## 5. 阴影：虚拟阴影贴图（VSM）

### 5.1 结构

- 每个投影光源有一张虚拟的 16k × 16k 阴影贴图，切成 128 × 128 texel 的页。
- 方向光用 clipmap：多级，以相机为中心，每级覆盖范围翻倍。
- 局部光：点光源用 6 面 cube，聚光灯用单面。远处的光源用更粗的 mip。
- 物理页池是一张大深度纹理（R32Float，用 UAV 原子 max 或 min 写入，以兼容 reverse-Z）。大小按档位设置：低档约 4k²，中档和高档 8k² 到 16k²。
- 页表是每个光源的一张 texture，记录物理页坐标、驻留位和脏位。

### 5.2 每帧流程

1. **标记**：根据主视图的深度，算出每个可见像素在各光源中需要哪一页、哪一级 mip（按屏幕 texel 密度）。用 clustered 光源列表把局部光的范围限制住。
2. **分配**：没驻留的页从空闲列表里分配。空闲列表不够时，按 LRU 回收不可见的页。
3. **渲染**：只渲染脏页。每个光源的每个脏页集合作为一个 VSM view，走 §3 的剔除和光栅，并共用光栅 bin。
4. **采样**：光照 pass 用 SMRT 风格的软阴影过滤。在页表上做 ray march，用多个样本近似半影。

### 5.3 缓存

- 静态页一旦渲染，就一直保留，直到失效。
- 失效来源有三种：
  - 实例变化：§1.4 第 3 步按 AABB 失效重叠的页；
  - 光源移动：整张贴图失效；
  - WPO 材质：每帧失效，并且可以按距离限制失效范围。
- 动态物体与静态物体分开缓存（双层页）：静态层只在失效时重绘，动态层每帧重绘后与静态层合成。

### 5.4 与 RT 的关系

- 高档的直接光可见性由 ReSTIR DI 的阴影光线负责。但半透明、体积雾、毛发和低成本路径仍然采样 VSM，所以高档也保留 VSM，只是页预算更小。

## 6. 光线追踪基础（ext `ray_query`）

### 6.1 加速结构

- **BLAS**：
  - 每个 mesh 的 RT LOD 一份，加载时构建，并做 compact。
  - 蒙皮和 WPO 实例每帧 refit。refit 的预算按距离排优先级；远处的实例降频更新，或者直接用静态姿势。
  - 材质可以标记"RT 中忽略 WPO"（0007 §9）。
- **TLAS**：每帧在 GPU 上从 GPU Scene 生成实例描述，然后完整重建（不做 update），把 RT 可见性和距离剔除也算进去。
- 构建放在异步计算队列上，与 visbuffer 光栅并行。

### 6.2 Wavefront 命中着色

所有需要在光线命中点处知道材质的路径都走这条流程，包括 ReSTIR GI/PT、探针更新、RT 反射和 SHaRC 更新：

```
trace kernel（ray query）──▶ HitRecord { instance, primitive, barycentrics, ray_cone, ray_dir, payload_slot }
  ─▶ 按材质分桶（与 §4 同一套分类代码）
  ─▶ 每个材质跑它的 resolve kernel（RT 入口：用 ray cone 选 LOD，path = RayTracing）
  ─▶ HitSurface（与 material buffer 同一种 Substrate 编码）
  ─▶ 光照 kernel（NEE、SHaRC 查询……）以及下一跳
```

- 未命中的光线直接查天空（大气 LUT）。
- 透明物体和 alpha 测试的处理见 0009 §5.3。

## 7. 光照

### 7.1 光源表示

- **解析光源**：方向光、点光源、聚光灯、矩形和圆盘面光源、天空光。支持 IES 和 light function，后者是 Light Function 域的材质，结果烘焙进 light function 图集（0007 §5）。
- **自发光几何**：带 `emissive` 标记、并且 `emissive_as_light` 打开的材质，所在三角形会进入一张**发光三角形表**。表在 cook 期预计算功率，运行时再乘上材质的强度参数。这张表供 ReSTIR 做光源采样。
- **Clustered 光源列表**：
  - 每个 view 一份 froxel 网格，例如 32 × 18 × 48，深度方向按指数分布。
  - 每个 cluster 存光源和 decal 下标的 bitmask，或者紧凑列表。
  - 使用者包括：低中档的直接光、前向半透明、体积雾、Toon 材质的光照样本（0007 §5.4）。
- **全局光源采样结构**（高档）：用功率 alias table 加空间光源层级（light BVH），为 ReSTIR DI 提供初始候选。

### 7.2 直接光（低档、中档）

- 光照 kernel 按 tile 复杂度类别各跑一个，读 material buffer。
- 对 clustered 列表里的每个光源，用 Substrate slab BSDF 求 `eval`，再乘以 VSM 可见性。方向光另加云阴影和大气透射率。
- 输出拆成 diffuse 和 specular 两部分，后面的 GI、SSR 和 TSR 都要用。

### 7.3 ReSTIR DI（高档）

1. **初始候选**：每个像素做 M 次 RIS 采样，从全局光源采样结构和发光三角形中抽取，目标函数是 `BSDF·Le·G`（暂不算可见性）。
2. **时间复用**：用运动向量重投影找到上一帧的 reservoir，按法线和深度做相似性检查，再合并。
3. **空间复用**：从邻域取 k 个 reservoir 合并，用 MIS 权重保证无偏，或者选用偏差可控的 pairwise MIS。
4. **最终着色**：对选中的样本发一条阴影光线（ray query），然后计算 `BSDF eval`。
5. 输出解调后的 diffuse 和 specular radiance，加上命中距离，送进降噪器（§7.7）。

参数按档位设置，例如 M、k 和是否做可见性复用。不能 lower 的材质（Toon）从 reservoir 里读样本作为光照输入（0007 §5.4）。

### 7.4 GI：探针（低档、中档）

- **探针体**：
  - 以相机为中心的多级 clipmap，例如 4 级，每级 32³ 个探针，间距按级翻倍。
  - 每个探针存两样东西：一是辐照度，用八面体映射，8×8 RGB11F；二是可见性，用八面体映射，16×16 RG16F，存距离的均值和平方均值，用 Chebyshev 测试防漏光。这是 DDGI 的格式。
  - 关卡里也可以手动放置局部体积（室内）。
  - **低档和中档用同一种格式**。
- **更新**：
  - **中档**：每帧按预算选一部分探针（优先选新进入视野、光照变化大的），每个探针发 N 条 RT 光线。光线命中后走 wavefront 着色，命中点的直接光来自光源采样加阴影光线，多次反弹则查询 SHaRC。最后用滞后混合（hysteresis）更新探针。
  - **低档**：cook 期用参考 PT 烘焙初值（cook_level）。运行时每帧只用很少的 RT 光线更新一小部分探针，来应对昼夜变化和开关门。也可以把 RT 完全关掉，只用烘焙值。
- **采样**：光照 kernel 对每个像素做三线性插值，带法线偏移和可见性权重。镜面部分用探针加 SSR。中档另有低粗糙度表面的 RT 反射，复用 §7.6 的镜面光线路径，但预算更低，并且不做 ReSTIR 复用。

### 7.5 SHaRC：世界空间辐射缓存

- 一张空间哈希网格，体素大小按到相机的距离分级。每项存累积的 radiance 和样本数。
- **更新**：每帧从屏幕上稀疏采样一部分像素（约 1/16），发射短路径，最多 4 跳。每一跳的 NEE 结果都累加进路径经过的各个体素，然后 resolve，并按时间衰减。
- **查询**：
  - 探针更新时，命中点的间接光直接查 SHaRC，不再继续追踪；
  - ReSTIR GI 的次级命中点也查 SHaRC，以此终止路径。
- 中档和高档共用同一个实现。

### 7.6 GI 与反射（高档）：ReSTIR GI

1. 每个像素从 GBuffer 出发，按 BSDF 采样一个方向（可以用 cosine 或 GGX VNDF 两种策略做 MIS），发射一条光线。
2. 命中点走 wavefront 着色，得到 radiance。这里的 radiance 包括命中点的直接光（复用 ReSTIR DI 的光源采样，并发阴影光线），以及 SHaRC 提供的多次反弹。
3. reservoir 里存命中点的位置、法线和 radiance，做时间复用和空间复用。重连时要做 Jacobian 校正。
4. 光滑表面的 specular（粗糙度低于阈值）单独发镜面光线，用 ReSTIR 做 1 spp 的镜面反射，同样走 wavefront 着色。
5. 输出送进降噪器。

### 7.7 降噪器（自研）

- 输入：解调后的 diffuse 和 specular radiance、命中距离、法线、粗糙度、深度、运动向量，以及历史累积长度。
- 结构类似 ReBLUR/SVGF：
  1. 时间累积，按命中距离做视差修正，镜面另做虚像重投影；
  2. 历史长度不足的地方加强空间滤波；
  3. 多级 à-trous 或泊松盘滤波，边缘停止函数考虑法线、深度和粗糙度；
  4. 防止拖影的历史截断。
- 最后与 albedo 重新调制，然后送进 TSR。
- **厂商 ext**：DLSS Ray Reconstruction 会替代"降噪 + TSR"这两步，输入按它的契约提供（guide buffer、specular albedo、hit distance）。

### 7.8 参考档：ReSTIR PT

- 全路径 reservoir，用 hybrid shift mapping（随机重放加重连），复用 §6.2 的 wavefront 机器。
- 编辑器的"参考视图"和照片模式在静止时逐帧累积，作为 0004 §6 测试里的收敛目标。
- 体积（雾和云）作为参与介质，只在这一档支持完整散射。

### 7.9 屏幕空间补充

- GTAO 用于低档和中档。
- SSR（Hi-Z 追踪）用于低档和中档，miss 时回退到探针或 SHaRC。
- 接触阴影（屏幕空间短光线）在所有档位上可选，补 VSM 的精度不足。

## 8. 流式加载

### 8.1 纹理 mip 流送

- 每张可流送纹理有一个 `streaming_id`。material resolve 计算每次采样需要的 mip（来自解析导数），对 `feedback[streaming_id]` 做原子 min。为了控制开销，每个像素每帧只抽查一次纹理采样，按像素哈希轮换。
- 反馈 buffer 每帧拷贝到 readback，CPU 侧延迟 2 到 3 帧读取，算出优先级：所需 mip 与当前已驻留 mip 的差，乘以屏幕占比。
- **升级**流程：
  1. 创建一张 mip 更多的新纹理（placed 资源，放在纹理池 heap 里）；
  2. 用 Copy 队列上传新 mip，再用 copy 把旧的 mip 拷过去；
  3. `rebind_view` 把原来的 bindless 下标改指到新纹理（0005 §1.5）；
  4. 旧纹理进退休队列。
- **降级**按 LRU 和内存预算执行，流程与升级对称。
- 材质参数块里存的是 bindless 下标，整个过程中下标不变。

### 8.2 网格 LOD 流送

- 实例剔除阶段，把每个 mesh 期望的最细 LOD 写进 `mesh_feedback[mesh_id]`（原子 min）。
- CPU 读回后发起 LOD 页的加载。加载完成后，GPU Scene 的 `meshes` 表更新驻留状态。

### 8.3 虚拟纹理（软件 VT）

- 用于两类内容：
  1. 地形 RVT：运行时由材质合成（0009 §4.4）；
  2. 超大的 cooked 纹理，例如世界地图、超大贴花，以 VT 页为单位 cook。
- 结构：
  - 每个 VT 一张页表纹理（R16Uint 或 RG16Uint，按 mip 排列）；
  - 共享的物理页缓存图集，按格式分组（BC1、BC3、BC5……），页大小 128 加 4 px 边框；
  - 物理页 LRU。
- 反馈：material resolve 写入 (vt_id, page, mip)。按 tile 去重后写到一个追加 buffer，CPU 读回后发起加载，或者交给 RVT 合成。
- 采样时多一次间接：先查页表，再算物理 UV。这一步封装在 shader 库的 `VirtualTexture2D` 类型里，材质代码写法和普通纹理一样。

### 8.4 上传通道

- 所有流送上传都走 **Copy 队列**，用独立的 timeline fence 通知完成。Graphics 队列只在真正使用新资源时等待这个 fence。
- bulk 数据按 0003 §4.3 对齐，可以直接 DMA。主机和 PC 上的 DirectStorage、GDeflate 走 `marlin_io` 的 ext，解压到 GPU 内存。
- 每帧上传量有预算（MB/帧，按档位设置），防止流送占满 PCIe 带宽或者统一内存带宽。

## 9. 半透明

- **对象**：Translucent、Additive、Modulate、AlphaComposite 这几种混合模式的材质，以及粒子（0009 §7）。
- **排序**：按物体到相机的距离从后往前排，同一物体内部不排序。材质可以设置"排序优先级"，强制改变顺序。
- **着色**：
  - 用材质的前向入口（0007 §6.4）：mesh shader 加 pixel shader，在 pixel shader 里调用 surface() 并做前向光照。
  - 光照输入：clustered 光源 + VSM、探针或 SHaRC、大气透射、体积雾（froxel 采样）。高档可以加 RT 反射，这是一个材质标记，只对玻璃这类少量材质开放，用 ray query 发一条反射光线，命中点用简化光照。
- **折射**：采样一份场景颜色的拷贝（带 mip，用于粗糙折射），偏移量由材质给出。
- **OIT**（可选）：对头发卡片、密集粒子这类不好排序的层，用 MBOIT（矩基 OIT），分两遍：先累积矩，再着色合成。
- **水体**：单层水面用 Substrate 的 SingleLayerWater，走 opaque 的 visbuffer 路径。水下的介质在光照阶段处理，不经过半透明流程。
- **与 TSR 的关系**：半透明会写一张"reactive mask"，告诉 TSR 降低这些区域的历史权重。

## 10. 主视图帧图（中档示例）

```
[每帧一次]  ApplyDelta(scatter) → Skinning → (RT: BLAS refit → TLAS build, async)
[每 view]   CullPhase1 → RasterBins → HZB → CullPhase2 → RasterBins → HZBFinal
            MaterialClassify → MaterialResolve×N → TileClassify
            VSM: Mark → Alloc → CullRaster(dirty pages) 
            ClusteredLights(async) → GTAO(async) → ProbeUpdate(RT, async) → SHaRC update/resolve
            DirectLighting(per tile class) → ProbeGI apply → SSR → Composite
            Sky/Atmosphere → VolumetricFog(froxel) → Clouds
            Translucency(sorted forward, MBOIT) 
            PostChain（0009 §6）：DOF → TSR → MotionBlur → Exposure/Bloom → Tonemap
[合成]      UI（0008）→ Compose → Present
```

按档位替换的部分：
- 高档：DirectLighting 换成 ReSTIR DI；ProbeGI 和 SSR 换成 ReSTIR GI 加降噪器。
- 低档：ProbeUpdate 只做稀疏更新；VSM 降分辨率。

## 11. 编辑器与调试

- **拾取**：读回 visbuffer 在鼠标处的像素，解码成 instance，再映射回 entity。半透明物体和 gizmo 另外用一个 ID pass。
- **选中描边**：对选中实例的 cluster 做一个 mask pass，再做屏幕空间描边。
- **调试视图**：
  - visbuffer 三角形 ID 和 cluster ID
  - 材质 bin 与 tile 复杂度
  - overdraw（光栅 bin）
  - VSM 页、缓存命中
  - 探针可视化
  - SHaRC 体素
  - reservoir 有效性
  - 流送请求热度
  - 光源复杂度
  
  调试视图通过 0005 §2.7 的捕获机制和一个调试合成 pass 实现。
- **调试绘制**：
  - CPU 立即模式 API 画线和形状，用于 3D 调试；
  - GPU 侧 shader 可以往调试 buffer 里追加线段，相当于 shader 版的 printf/draw。
  
  两者都在合成前用一个统一的线段 pass 画出来。

## 12. 测试要点

- 剔除：构造已知的遮挡场景，断言可见 cluster 集合精确等于期望（GPU 确定性）。
- visbuffer：对固定几何，断言三角形 ID 图逐像素等于期望。
- VSM：断言页标记集合；静态缓存失效后重绘结果与无缓存时逐字节相同。
- 光照：
  - 每档对固定场景做 FLIP 比较；
  - ReSTIR DI/GI 的期望值和参考 PT 的误差，随帧数单调下降到阈值以下。
- 流送：强制低预算下的升降级循环，检查结果不黑屏、无撕裂，bindless 下标保持不变。
