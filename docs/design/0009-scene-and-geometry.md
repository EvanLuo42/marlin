# 0009 — 场景与几何：RenderScene、GPU Scene、cluster DAG、visbuffer

状态：草案 · 2026-10-16

前置：
- [0007](0007-render-graph.md)：render graph，§9 加速结构
- [0008](0008-rendering-overview.md)：总览、帧流水、视图

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 场景表示 | 渲染器持有独立的 `RenderScene`：按种类分的 SoA 表，句柄稳定；只通过 extract 的更新流写入 | 1 |
| GPU Scene | 持久的 GPU 表（实例、变换、网格、材质、光源……），每帧只用一个 scatter pass 上传有变化的行 | 2 |
| 几何资产 | **全平台 cluster DAG**：每个 cluster 最多 128 个三角形，组合成层级 DAG 做连续 LOD；按页流送；平台之间只差误差阈值 | 3 |
| 可见性 | GPU-driven：实例剔除 → DAG 遍历与 LOD 选择 → cluster 剔除 → 大三角形走 mesh shader、小三角形走软光栅；两遍 HZB 遮挡剔除 | 4 |
| visbuffer | 64 位：深度 32 位 + 可见 cluster 25 位 + 三角形 7 位，用 64 位原子 max 写入 | 4.3 |
| 可编程光栅 | masked 和顶点形变的材质有自己的光栅 permutation；其余材质共用一个固定光栅 | 4.4 |
| 蒙皮 | compute 预蒙皮，写进持久的顶点 buffer；同一份结果给光栅、运动向量和 BLAS refit 用 | 4.5 |
| 材质分箱 | visbuffer 按材质 permutation 分箱，每个 permutation 一次 indirect dispatch 解算；具体见 0011 | 5 |
| 流送 | cluster 页按 GPU 的反馈流送，根页常驻；纹理按材质解算时写的采样反馈流送 mip；地形和贴花混合用 RVT | 6 |
| 光追场景 | 每个网格的 BLAS 用一个固定的粗 LOD 构建；TLAS 实例由 GPU 生成（0007 §9）；masked 材质在 ray query 的候选循环里做 alpha 测试 | 7 |
| 半透明 | 按物体排序的前向 pass，光照来自 clustered 光源加世界 radiance cache；可选的加权混合 OIT | 8 |

## 1. RenderScene

### 1.1 表

| 表 | 一行是什么 | 主要字段 |
|---|---|---|
| `Instances` | 一个可渲染的实例 | 网格、材质槽（每个 section 一个）、变换、上一帧变换、包围、标志（投影、可见性掩码、RT 掩码、是否 masked/形变）、自定义数据（N 个 float，0011 §7.3）、所在层 |
| `Meshes` | 一个驻留的网格资产 | DAG 根、页表范围、包围、section 数、RT LOD 的 BLAS 引用 |
| `MaterialInstances` | 一个材质实例 | permutation id、参数块在材质表里的槽位、简化材质图集里的矩形（0011 §9） |
| `Lights` | 一个光源 | 类型、颜色和强度（物理单位）、形状、范围、IES/cookie/光函数、投影标志 |
| `Decals` | 一个贴花 | OBB、贴花材质实例、排序优先级 |
| `Volumes` | 局部雾、后处理体积 | 形状、体积材质或后处理参数、混合权重 |
| `Skinned` | 一个蒙皮实例 | 骨骼矩阵的范围、蒙皮顶点 buffer 的槽位、BLAS 组里的条目 |
| `Environment` | 单例 | 天空、大气、云、太阳、月亮、曝光参数 |

- 每张表是 SoA，用 `marlin-containers` 的稳定句柄（下标加代数）。删除时留下空位并放进空闲表，GPU 行的下标不变。
- 实例的 GPU 下标就是它在表里的下标，所以 ray query 命中返回的 `InstanceIndex` 就是 GPU Scene 的实例下标（0007 §9.6）。

### 1.2 更新流

```rust
pub struct SceneUpdates {
    pub instances: UpdateStream<InstanceId, InstanceUpdate>,   // Add / Remove / Transform / Material / Flags / CustomData
    pub lights: UpdateStream<LightId, LightUpdate>,
    pub decals: UpdateStream<DecalId, DecalUpdate>,
    // ……每张表一个
    pub environment: Option<EnvironmentDesc>,
}
```

- extract 的 job 各自写一段更新流，最后按表合并，不需要锁。
- 句柄在 extract 时预分配，用 `RenderScene` 的分配器（`SceneIds`，extract 那边持有一份）；删除的回收推迟到应用阶段，所以 extract 期间不会复用正在使用的下标。分配新行是一次原子加，复用空闲行要拿一把 fiber 感知的锁（R1h 的实现，不是无锁的；extract 每帧分配的句柄不多，等成为瓶颈再换）。
- 应用阶段（0008 §3.3）按表并行，每张表内部再按行区间分给多个 job。
- 只改变换的实例走一条快路径：不做别的校验，直接记脏行。

### 1.3 和 ECS 的关系

- `RenderScene` 不依赖 ECS。editor 的预览场景、资产缩略图、材质球都直接构造 `RenderScene`，不需要一个 ECS 世界。
- ECS 那边的组件（`StaticMesh`，以后的 `Light`、`Decal`……）和 extract 系统放在 `marlin-framework` 里，`marlin-render` 只定义更新流。

## 2. GPU Scene

### 2.1 GPU 表

| buffer | 元素 | 说明 |
|---|---|---|
| `InstanceData` | 实例的静态部分 | 网格、材质槽基址、标志、自定义数据偏移 |
| `InstanceTransform` | 当前帧和上一帧的 3×4 变换 | 每帧都可能变，单独放 |
| `InstanceBounds` | 世界空间包围球和 AABB | 剔除用 |
| `MeshTable` | 网格 | DAG 根页、层级包围、RT LOD 的 BLAS 引用（0007 §9.6） |
| `MaterialTable` | 材质参数块 | 按材质类型的布局，下标是槽位（0011 §7） |
| `LightTable` | 光源 | 0010 §5.1 |
| `DecalTable` | 贴花 | 0011 §6.4 |
| `CustomData` | 每个实例的自定义 float | 0011 §7.3 |

- 它们都是持久资源，以导入资源的身份进入 graph（0007 §2.3），状态由 graph 跟踪。
- 容量按 2 的幂增长；增长时新建 buffer，用一个 copy pass 把旧内容拷过去，旧 buffer 进退休队列。

### 2.2 scatter 上传

- 每帧只有一个 scatter pass：CPU 把脏行写进上传 ring（`ctx.upload`），并附上一张 (目标表, 行号, 源偏移) 的列表。一个 compute pass 按这张列表把所有表的脏行拷到位。
- 只改变换的实例只写 `InstanceTransform` 和 `InstanceBounds`。
- 上一帧的变换在 GPU 上保存：scatter pass 先把当前帧的值移到"上一帧"，再写新值。没有更新的实例也需要这一步，所以由一个"本帧哪些实例动过"的位图驱动；静止实例的两个变换本来就相等，不用碰。

## 3. cluster DAG

### 3.1 构建（cook）

`marlin-cook-mesh` 对每个网格：

1. 把三角形分成 cluster：每个最多 128 个三角形、128 个顶点。按空间位置和法线一致性分组（为背面剔除的法线锥服务）。
2. 把相邻的 cluster 组成 group（8–32 个），在 group 内锁住外边界做简化（QEM，加属性误差），简化到一半三角形，再重新切成 cluster，作为父层。
3. 重复，直到只剩一个根 group。
4. 每个 group 记录简化误差和包围球；误差沿 DAG 单调（父 ≥ 子），这样运行时的 LOD 选择在每个 group 上独立判断，结果自然没有裂缝。
5. 顶点量化：位置按网格包围盒量化，法线和切线用八面体编码，UV 按范围量化。
6. 按 group 打成**页**（128 KiB）。页是流送和驻留的单位。用 GDeflate 压缩，主机和 DirectStorage 可以硬件解压（每页一个 bulk blob，由打包器压缩，§13）。
7. 另选一个**RT LOD**：DAG 中某一层的切割（默认取三角形数约为原网格 1/4 的那一层），单独存一份不量化的顶点和索引，用来构建 BLAS（§7）。

- cluster 最多 128 个三角形，同时满足 mesh shader 的输出限制和 DXR 2.0 CLAS 的上限（256），以后可以直接把 cluster 交给 CLAS（§10）。
- 每个 cluster 带一个材质 section 下标的小表（通常只有 1 个），三角形上存 section 下标。

### 3.2 运行时数据

- **页池**：一个大的持久 buffer，按页大小切成槽。网格的根页常驻，其余按需。
- **页表**：每个网格的每个页 → 页池槽位或"不驻留"。
- **层级**：group 的 BVH（每个节点 4–8 个子节点，带包围球和最大误差），放在常驻部分。遍历从根开始，遇到未驻留的页就停在驻留的最细层，同时请求流送（§6.1）。

## 4. 可见性管线（每个视图）

### 4.1 流程

```
实例剔除（视锥、上一帧 HZB、距离）
  └─▶ DAG 遍历（compute，持久线程 + 工作队列）
        · 节点：视锥/HZB 测试；投影误差 < 阈值就停止细分
        · 叶子：输出候选 cluster
  └─▶ cluster 剔除（视锥、HZB、法线锥背面剔除）
        · 屏幕上的三角形平均边长 > 阈值 → 硬件光栅队列（mesh shader）
        · 否则 → 软光栅队列（compute）
  └─▶ 光栅：写 visbuffer（§4.3）
```

- LOD 的判断：一个 group 选用自己而不选它的父，当且仅当"自己的误差投影 ≤ 阈值 < 父的误差投影"。阈值就是档位里的几何误差（0008 §2.3）。
- DAG 遍历是一个 compute pass，用持久线程从一个全局队列里取节点。每个视图一次，所有实例一起遍历。
- 软光栅：每个线程一个三角形（大一点的三角形一个 wave 一个），用定点数做扫描线，深度测试和写入都是 64 位原子 max。
- 硬件光栅：mesh shader 每个 workgroup 一个 cluster，pixel shader 同样用原子 max 写 visbuffer，不用颜色附件和深度附件。
- 硬件和软件的分界阈值是 cvar（`r.geo.sw_raster_edge_px`，默认 32 像素），按平台调。

### 4.2 两遍遮挡剔除

1. 第一遍：用上一帧的 HZB 测试实例和 cluster，可见的画进 visbuffer。
2. 用这一遍的深度建 HZB。
3. 第二遍：第一遍被判为遮挡的实例和 cluster，用新的 HZB 再测一次，通过的补画。
4. 用最终深度再建一次 HZB，留给下一帧。

两遍之间的被剔除列表放在瞬态 buffer 里。HZB 是视图的历史资源（0007 §8）。

### 4.3 visbuffer

| 位 | 内容 |
|---|---|
| 63–32 | 深度：reverse-Z 的 float 位模式，按 uint 比较；越大越近，正好用原子 max |
| 31–7 | 可见 cluster 下标（25 位，每帧最多约 3300 万个可见 cluster） |
| 6–0 | cluster 内的三角形下标（7 位，对应 128 个三角形） |

- 可见 cluster 表（瞬态）：实例下标、cluster 在页池里的地址。
- 写入要求 64 位 typed UAV 原子（0008 §1.3）。RHI 要补这个特性的查询和对应的视图格式（§11）。
- 深度从 visbuffer 拆出来，写一张普通的深度纹理（`R32Float`），给后续 pass 和半透明的深度测试用。

### 4.4 可编程光栅

大多数材质的光栅不需要执行材质代码。下面两类需要：

- **masked**：要采样 alpha 来决定像素留不留；
- **顶点形变**：WPO、植被的风、顶点动画。

这两类材质在 cook 时多生成一组光栅 permutation（mesh shader + pixel shader，以及软光栅的 compute 版本），由材质里的方法提供（0011 §2.2）。cluster 剔除时按实例的标志把 cluster 分到"固定光栅"或某个材质的"可编程光栅"队列。

- 可编程光栅的 permutation 数量受控：只有实际实现了 `Opacity` 或 `Deform` 方法的材质类型才有。
- 顶点形变的包围盒要放大，放大量由材质声明（`MaxDisplacement`），剔除时使用。
- 形变材质的上一帧位置：形变函数以时间为参数，计算运动向量时用上一帧的时间再算一次。

### 4.5 蒙皮与形变

- 蒙皮是场景级的 compute pass：读骨骼矩阵和绑定姿势，把当前帧的顶点写进每个实例的持久顶点 buffer。上一帧的结果留在另一半双缓冲里，用来算运动向量。
- 蒙皮网格同样有 cluster DAG，拓扑来自绑定姿势；LOD 判断用的包围球按骨骼的最大位移放大。
- cluster 的位置从蒙皮 buffer 里读，而不是从页池。
- 同一份蒙皮结果给 BLAS refit 用（0007 §9.8）。
- morph target 在蒙皮之前做，同一个 pass。

### 4.6 运动向量

- 不单独做速度 pass。材质解算重建三角形时，同时算出当前帧和上一帧的屏幕位置（上一帧的变换、上一帧的蒙皮顶点），写一张速度纹理。
- 没有几何的像素（天空）用相机运动算。

## 5. 材质分箱

```
visbuffer ─▶ "Classify"：每个像素 → 材质 permutation id，统计每个 id 的像素数
          ─▶ 前缀和 ─▶ "Scatter"：写每个 id 的像素列表（打包成 2×16 位）和 indirect 参数
          ─▶ "Resolve"：对每个驻留的 permutation 发一次 dispatch_indirect（0011 §6）
```

- permutation id 是材质实例对应的 shader permutation，从实例的材质槽查得。
- Resolve 是一个 graph pass，在执行闭包里循环所有驻留的 permutation。像素数为 0 的 permutation 只是一次空的 indirect dispatch。permutation 的集合只在流送加载新材质时改变，所以 graph 的拓扑在稳态下不变。
- 空的 indirect dispatch 在命令处理器上也有固定开销。驻留的 permutation 很多时（上千个），按上一帧的统计跳过空 bin 会漏掉这一帧刚出现的材质，所以不这么做；改为在 Classify 里额外输出一个"本帧出现过的 permutation"位图，读回后供下一帧决定是否声明一个"稀疏"版本的 Resolve（§12）。

## 6. 流送

### 6.1 cluster 页

- DAG 遍历遇到"想细分但子页未驻留"的节点时，把页请求写进一个反馈 buffer（页 id + 优先级，优先级是投影误差）。
- 反馈读回后（帧槽 fence 到了以后），CPU 去重并排序，交给 `marlin-io` 异步读取，解压后写进上传 ring，在下一帧的 graph 里用一个 copy pass 写进页池，同时更新页表。
- 页池满了按 LRU 淘汰：每页记录最后一次被遍历用到的帧号。
- 显存预算按平台设定；预算不够时逐级提高几何误差阈值，而不是让画面出现缺块。

### 6.2 纹理 mip

- 材质解算时（0011 §6.5），每个 wave 把它采样的纹理和需要的 mip 写进采样反馈 buffer：按纹理 id 用原子 min 记录最细的 mip。
- 这是全分辨率的反馈，不需要单独的 pass，开销几乎为零。
- 半透明、RT 命中点、粒子的采样也写同一份反馈。
- CPU 读回后，按最细 mip 决定每张纹理要驻留到哪一级，流送 mip 尾部。纹理的 bindless 视图在 mip 变化时重建（新视图的 `MinLod` 指向驻留的最细级），旧视图进退休队列。

### 6.3 RVT

- 运行时虚拟纹理只用于地形和"写进 RVT 的贴花"（0013 §4）：材质混合的结果按页缓存，渲染时一次采样。
- 页的请求来自地形材质解算时写的反馈，和 §6.2 是同一类机制。
- 全局的分页虚拟纹理不做（0008 §0），采样路径保持一次 bindless 采样。

## 7. 光追场景

- **BLAS**：静态网格用 cook 时选好的 RT LOD 构建（§3.1），流程按 0007 §9.7：按预算构建、compaction。蒙皮网格按 0007 §9.8 refit。
- **TLAS**：由 GPU 根据 GPU Scene 生成实例描述（0007 §9.6）。实例的 RT 掩码位：

| 位 | 含义 |
|---|---|
| 0 | 不透明 |
| 1 | masked（需要 alpha 测试） |
| 2 | 毛发代理（0013 §7） |
| 3 | 粒子代理 |
| 4 | 只投影、不可见于 GI 和反射（例如第一人称手臂的影子替身） |
| 5 | 地形 |

- **masked 几何**：ray query 用 `RAY_FLAG_NONE`，在候选循环里对 masked 实例的三角形做 alpha 测试。alpha 来自简化材质图集里的 opacity 通道（0011 §9），只是一次纹理读取，不调用材质代码。
- **RT LOD 与主视图的几何不一致**：自遮挡错误用法线偏移加"起点前移到 RT LOD 包围"来避免；软光栅出来的细节在光线里看不到，这是有意接受的。

## 8. 半透明

- **排序前向**：半透明实例按包围中心到相机的距离从后往前排序（GPU 上的基数排序），每个实例按 cluster DAG 选好 LOD 后用 mesh shader 绘制。同一网格内部的三角形不排序。
- **光照**：
  - 直接光：clustered 光源网格（0010 §5.4）加 VSM 的太阳阴影；L3 以上太阳阴影改用一条 RT 阴影射线；
  - 间接光：查世界 radiance cache（0010 §4），不打 GI 射线；
  - 镜面反射：L2 以下用天空和 cache 的近似；L3 以上，粗糙度低于阈值的玻璃打一条反射射线，命中点用简化材质。
- **材质**：半透明用同一套 Substrate 树，在前向 pass 里直接求值，用 closure 分发里的 `All` 版本（0011 §5.4）。
- **折射**：先把不透明的场景颜色拷一份（带 mip），半透明按粗糙度从对应的 mip 采样。
- **OIT**：材质可以选择加权混合 OIT（McGuire–Bavoil），用于粒子、大量重叠的玻璃。OIT 的物体在一个单独的累积 pass 里画，最后合成。
- **低分辨率半透明**：粒子这类可以画进半分辨率的目标，再按深度感知上采样合成。
- **TSR 的 reactive mask**：半透明和粒子写进一张反应遮罩，告诉 TSR 少用这些像素的历史。

## 9. 调试

- 0008 §7 里列出的几何类调试视图：实例、cluster、三角形、LOD、HW/SW 比例、过度绘制。
- 页池的占用和请求队列长度；LOD 被预算强制抬高时在屏幕上提示。
- 冻结剔除：锁住某一帧的剔除结果，然后自由移动相机观察。

## 10. 测试

测试放在各 crate 的 `tests/` 里。

- **平台无关**：
  - DAG 构建：误差单调、group 边界锁定、任何一种 LOD 切割都没有裂缝（在 CPU 上随机挑误差阈值，检查切割结果是水密的）；
  - 页打包和量化的往返误差；
  - 更新流的合并和应用：句柄稳定，空闲表回收被推迟到应用阶段；
  - scatter 列表的生成。
- **平台相关**（真实硬件）：
  - 软光栅和硬件光栅对同一组三角形写出的 visbuffer 逐像素一致（只看覆盖，不看三角形 id 的分配）；
  - 两遍遮挡剔除不丢物体：和不剔除的结果比较 visbuffer；
  - 流送：页池很小时画面只会变粗，不会出现缺块；
  - masked 材质的 alpha 测试在光栅和 ray query 两边的结果一致。

## 11. RHI 需要的改动

- 64 位 typed UAV 原子：特性查询，加上 `R64Uint` 的纹理和视图格式（D3D12 的 `AtomicInt64OnTypedResourceSupported`，Vulkan 的 `shaderImageInt64Atomics`）。已做（2026-10-18）：`Features::texture_atomic_int64` 和 `Format::R64Uint`，shader 侧是存储纹理的 `AtomicMax` 等方法（0004 §12.2、0016 §3）。
- 带计数 buffer 的 indirect（`ExecuteIndirect` 的 count buffer，Vulkan 的 `vkCmdDispatchIndirectCount` 等价物），可见 cluster 数要用。
- mesh pipeline 允许没有颜色附件和深度附件、只写 UAV。
- 加速结构：按 0007 §11.3。

## 12. 待定

- 曲面细分和位移贴图（Nanite tessellation 那一类）：先不做，DAG 的格式给它留了位置。
- DXR 2.0 的 CLAS、partitioned TLAS：可以去掉单独的 RT LOD，直接用流送进来的 cluster 构建；等 PC 驱动和主机都支持后再评估。
- Work graphs：可以取代"DAG 遍历 → 分箱 → indirect"这条链；先看主机的支持情况。
- §5 的稀疏 Resolve。
- 材质 section 很多的网格（> 64 个）怎么处理。
- 植被的远景 LOD（体素或聚合体），见 0013 §5。

## 13. 实现顺序

R2（0008 §8）里只有 cook 侧（§3.1）不依赖 R1 的 GPU Scene 和可见性管线，先做了。每步单独提交、带测试：

| 步 | 内容 |
|---|---|
| 1 | 分 cluster（`cook_mesh::cluster`） |
| 2 | DAG：分组、组内锁边简化、重新分 cluster、误差和包围球（`cook_mesh::dag`） |
| 3 | 顶点量化（`cook_mesh::quantize`，解码在 `marlin_render::asset::cluster`） |
| 4 | 打页、页依赖、group 层级（`cook_mesh::page`）；第 6 步的材质 section 表也在这一步，因为它就在三角形的编码里 |
| 5 | RT LOD（`cook_mesh::rt`） |
| 7 | cooked 格式：`CookedMesh::cluster_dag`，运行时读取和校验（`MeshView`、`ClusterDagView`、`PageView`） |

R2 的运行时部分（页流送和反馈、两遍 HZB、软光栅、VSM）要等 R1h–R1l 的 GPU Scene、meshlet 可见性管线和 visbuffer。

cook 侧做完时（2026-10-18）定下的细节：
- **分 cluster** 直接用 meshlet 生成器（§3.1 第 1 条要的空间加法线锥的生长），参数换成 128 顶点、128 三角形。每次调用先把顶点压缩成局部编号，所以高层可以一个 group 一个 group 地重新切。在 2 万三角形的球上平均每个 cluster 112 个三角形。
- **不产生新顶点**：简化是半边折叠，顶点只会移到已有的顶点上，所以 DAG 的每一层都引用源网格的顶点。页里每个 cluster 有自己的一份顶点（两个 cluster 共用的顶点存两次），这样一个 cluster 只靠自己的页就能解码。
- **分组**：每层把 cluster 按共享边（按位置）建图，边权是共享边的条数，递归二分到每份不超过 32 个。二分是平衡的（按最终份数分配两边的大小），所以除了顶部剩下不多的几层，group 都在 16–32 个之间。二分先从最长轴一端的 cluster 开始按割边增益贪心生长，不连通时接最近的 cluster，再成对交换改进割边。没有用 METIS（第三方，0003）。
- **组内简化**：只把这个 group 的三角形交给简化器，group 的外边界就是简化器眼里的开放边界，所以被锁住。目标是一半三角形，误差不设上限。少于 15% 的削减算简化失败，这个 group 成为根（误差 +∞，永远不被替换）；它的邻居继续简化时共享边仍然锁住，所以切割照样水密。材质 section 的边界和 LOD 链一样锁住，section 很多的网格因此会有较多的根（§12 已列为待定）。
- **误差**：group 的误差 = 它的 cluster 中最大的 LOD 误差 + 这次简化新增的几何误差（距离）。属性误差只参与选择折叠，不计入记录的误差，因为运行时比较的是屏幕上的几何误差。group 的包围球包住它的 cluster 的 LOD 包围球；简化出来的 cluster 的 LOD 误差和 LOD 包围球就是这个 group 的。这样误差和包围球都从子到父单调，任何"投影误差和阈值比较"的单调判据得到的切割都一致、都水密。
- **选择规则**（写在 `ClusterRecord` 的文档里）：cluster 所在 group 的投影误差超过阈值（这个 group 不够好，要细化），而 cluster 自己的投影 LOD 误差不超过阈值，或者生成它的 group（`source_group`）没有驻留时，画这个 cluster。
- **量化**：位置按整个网格的包围盒 16 位量化（每顶点 8 字节，`Float` 精度时 3×f32），不按 cluster 量化，这样两个 cluster 共用的位置解码后逐位相同，切割才水密。法线和切线沿用网格已有的 32 位编码：八面体法线 2×10 位，切线是在法线平面里的角度（11 位），比八面体编码切线在同样位数下更准。UV 按每个 cluster、每个 UV 集的范围做 unorm16（`UvRange`），平铺的 UV 也不丢精度。颜色 RGBA8，蒙皮和网格相同。往返误差：位置半步以内，法线 < 0.25°，切线 < 0.5°，UV 半步以内。
- **页**：最多 128 KiB。页头之后是 16 字节对齐的几段：cluster 记录（64 字节：剔除球、法线锥、LOD 球和误差、所在 group、来源 group、各段的偏移和数量、层级）、每个 cluster 每个 UV 集的范围、位置、属性、蒙皮、三角形、section 表。三角形是一个 u32：三个 cluster 内的顶点下标加上它在 cluster 的 section 表里的下标。section 表每项是 u16 的网格 section 下标。
- **打页的顺序**：先放根 group，再按层级从粗到细，同一层按 group 包围球中心的 Morton 序。页装满为止，装不下的 group 拆开，每一段是一个 part。常驻的根页是开头到最后一个含根 group 的页。页的依赖是它的 group 的其他页（拆开的 group 整体进出）加上这些 group 简化出的 cluster 所在的页（总在前面），所以流送从粗到细加载、从细到粗淘汰。
- **压缩不在页里做**：每页是一个单独的 bulk blob，打包器把它放在自己的 chunk 里，用平台的 bulk codec 压缩（0003 P4，blob 起点 4 KiB 对齐）。GDeflate 编码器在 `marlin-compress` 里已经有了，所以页格式不需要压缩标志。要让 GPU 或 DirectStorage 解压，平台的 `bulk-codec` 要设成 `gdeflate`；Windows 现在是 `zstd`，等流送实现时再改。
- **group 层级**：每一层的 part 建一棵 8 叉 BVH（按球心最长轴连续三次对半分成 8 份），各层的子树按从粗到细挂在根节点（节点 0）下，超过 8 层时再按 8 个一组嵌套。每个子节点记录包住下面所有 group 球的球和下面最大的 group 误差；叶子是 part。
- **RT LOD**：对象空间的均匀切割，在所有 group 误差里二分查找，取三角形数最接近 `RtFraction`（默认 0.25）乘源三角形数的阈值。均匀切割一定水密。存成不量化的 3×f32 位置、LOD 布局的属性（f16 UV）和蒙皮、按 section 排好的 u32 索引，每个 section 一段。`Mesh::rt_lod` 仍然只管离散 LOD 链。
- **格式**：`CookedMesh::cluster_dag` 是 0 或 1 个 `ClusterDag`（常驻部分：页表和依赖、group、part、层级节点、RT LOD）。bulk blob 的顺序是离散 LOD 页、DAG 页、RT LOD。`Mesh::cluster_dag` 默认打开。离散 LOD 链先保留，前向渲染器还在用，visbuffer 接手后删掉。网格 cooker 版本升到 2。
- **校验**：`MeshView::new` 检查常驻部分（页和 RT LOD 在 bulk 范围内、页不超过 128 KiB、依赖、part、层级是以节点 0 为根的树）；页在加载时用 `ClusterDagView::page` 检查一次（各段大小、每个 cluster 的范围、三角形下标、section 下标），RT LOD 用 `check_rt`。随机破坏的页和 payload 只返回错误。
- 测试（`tools/cook_mesh/tests/unit/clusters.rs`）：cluster 覆盖每个三角形一次且在限制内，法线锥保守；误差和包围球单调，闭合的球收敛到一个根；group 边界在简化前后不变；随机阈值的对象空间切割和随机相机的投影切割都水密；量化往返；页的限制和填充率、每个 cluster 恰好一次且解码回原三角形和 section、part 和依赖、层级的球和误差、只从页解码出来的切割水密、损坏的页被拒绝；RT LOD 约 1/4、水密、section 正确。
- 规模：基准 `cargo bench -p marlin-cook-mesh --bench cluster_dag`（分 cluster、DAG、打页、RT LOD 四段）。32.8 万三角形的球：DAG 1.8 秒（约 18 万三角形/秒）、6076 个 cluster、13 层，分 cluster 0.28 秒，打页 0.11 秒（79 页），RT LOD 3 毫秒。

运行时（R2 后半）要从这个格式用到的东西：
- 页池按 128 KiB 切槽；GPU 读页头找各段，cluster 下标 = 槽位基址 + 页内下标（visbuffer 的 25 位 cluster id 是全局可见 cluster 的编号，三角形 7 位正好放下 128）。
- 页表和每个 group 的驻留位：group 的所有 part 的页都驻留才算驻留，`source_group` 的判断要用。
- 请求一个页时把依赖一起请求，依赖没到就不用这个页；根页在网格加载时上传并钉住。
- 遍历从层级节点 0 开始，子节点的 `max_error` 投影后不超过阈值就跳过。
- BLAS 用 RT LOD 的 blob 构建（§7），按 section 分成几何。

R1h（0008 §8）做完时（2026-10-18）定下的细节（§1、§2，以及 framework 的 extract）：
- **范围**：这一步的表只有 `Instances`、`Meshes`、`Lights` 和 `Environment`，字段也只取前向渲染器现在用得到的（实例：网格、变换、上一帧变换、颜色、标志；网格：顶点视图、索引数、局部包围）。材质槽、自定义数据、贴花、体积、蒙皮等到用到它们的步骤再加。颜色是材质（0011，R1k）到来之前的占位。
- **`marlin-render::scene`**：
  - `Id<T>` 是 (下标, 代数)，`InstanceId`、`MeshId`、`LightId` 是它的别名。`IdAllocator` 的 `reserve` 可以在任何线程调用；`release` 只在 `apply` 里做，把代数加一放回空闲表。三个分配器合成 `SceneIds`（`Arc`），`RenderScene::with_ids(ids)` 用调用方给的那一份。
  - `RenderScene::apply_all(sets)` 是一帧的应用：一帧只调用一次，可以给几组更新（framework 先给 extract 的，再给渲染侧自己产生的网格驻留）。每组里先网格、再光源、再实例，各自按流的顺序。对不是活着的行（删掉的、没加过的、代数对不上的）的更新直接忽略。
  - 上一帧变换在 CPU 上也记一份：一行这一帧第一次 `Transform` 时把当前值存为上一帧，同一帧再改不动它；上一帧动过、这一帧没动的行，上一帧变换追上当前值，并标记为"停下"。
  - 网格表的一行先由 `MeshUpdate::Add` 建立（被引用了，还没上 GPU），再由 `MeshUpdate::Resident(ResidentMesh)` 填上 GPU 副本（重新加载时再来一次）。不驻留的网格的实例不画。
- **GPU Scene**（`RenderScene::add_passes(g, &ScatterShader) -> GpuScene`）：
  - 五张表是 graph 的持久资源（`g.persistent("GpuScene.*", 场景 id, …)`，R1c），每个 `RenderScene` 有唯一的 id 作为 scope。行的布局见 `scene::gpu` 的模块文档：实例数据 8 个字、变换 24 个字（当前和上一帧各一个 3×4，行主序）、包围 4 个字（世界空间包围球）、网格 8 个字、光源 16 个字。AABB 等剔除需要时再加。
  - 容量是至少 64 行的 2 的幂。**增长时不拷旧内容**（偏离 §2.1 第 2 条）：CPU 上有完整的表，新 buffer 拿到的是 `fresh`，scatter 把所有活着的行重写一遍，省掉一个 copy pass 和旧 buffer 的退休。
  - scatter 每帧一个 compute pass，没有脏行就不加。每个 op 是 `uint4`（表 | 种类 << 8、行、数据起点、字数），数据和 op 都用 `ctx.upload_slice`。种类：写（新值写进行）、移位（当前变换拷到上一帧）、移位加写（先移位再写新的当前变换）。所以动了的实例只上传新的 12 个字，§2.2 的"上一帧"移位在 GPU 上做；停下的实例只发一个移位，不上传数据；静止的实例什么都不发（§2.2 说的位图就是这份"上一帧动过的行"的列表）。新行把两个变换都写成同一个值。
  - shader 在 `content/engine/shaders/Scene/Scene.mshader`（`Marlin.Engine.Scene`）：`Scatter`，以及 `InstanceData`、`InstanceTransforms`、`Rows3x4`、`Apply` 给别的 shader 用。表在 scatter 里是 raw 视图（`ByteBuffer`），forward pass 读的是 structured 视图（`ReadOnlyBuffer<T>`），两者在 Vulkan 上都是同一种 storage buffer 描述符，所以同一个 buffer 两种读法都行；D3D12（Xbox）到时候要分开建视图。
- **前向渲染器**：`Renderer::add_passes(g, target, &scene, gpu, camera)`。变换和颜色从 GPU Scene 读；CPU 每帧只上传帧常量和按网格分组的实例行号列表（`InstanceList`），push constant 变成 6 个字（`FORWARD_PUSH_BYTES = 24`）。环境（光和清屏色）在场景里。旧的 `Scene`、`SceneInstance`、`MeshRenderer` 删掉了，`marlin-render` 不再依赖 `marlin-ecs`。`Renderer::resident_mesh(handle, &data)` 给出网格表要的 `ResidentMesh`（包围由顶点算出）。
- **framework 的 extract**（`Engine::frame` 里）：
  - 按 ECS 的变化检测产生更新：`Changed<StaticMesh>` 里没见过的实体是新实例（`Add`），见过的比较网格和颜色，变了才发 `Mesh`/`Color`；`Changed<Transform>`（带 `StaticMesh`）且不是这一帧新加的实体只发 `Transform`；`removed::<StaticMesh>()` 里现在已经没有 `StaticMesh` 的实体发 `Remove`。实体到 `InstanceId` 的映射在 `Engine` 里。没有变化的帧，`SceneUpdates` 是空的。
  - 网格按资产 UUID 对应一个 `MeshId`：关卡用到的网格在生成关卡时预约并发 `MeshUpdate::Add`，关卡不再用的发 `Remove`；游戏代码直接生成的实体用到没见过的网格时，在 extract 里同样处理。环境只在关卡（重新）生成时随更新发送。
  - `RenderDelta` 变成 `{ frame, dt, updates, uploads, camera }`，`uploads` 按 `MeshId` 给几何。`RenderSide` 持有 `RenderScene` 和 `ScatterShader`：上传网格、产生 `Resident`，网格 `Remove` 时把 GPU 副本放进退休队列，再 `apply_all([&delta.updates, &resident])`；`add_passes` 先 scatter，再前向、后处理。`Engine::scene_ids()` 交给 `RenderSide::new`，两边共享的只有这几个分配器。
- **测试**：CPU（`marlin-render/tests/unit/scene.rs`）：8 个线程各预约 1000 个句柄互不相同；删除在应用之后才回收、回来的代数加一、旧句柄对新行无效；对没加过的行的更新被忽略且不回收；上一帧变换（多次移动、停下）；多段流合并后按顺序应用；网格和光源的增删；包围。framework（`tests/unit/extract.rs`）：新实体、无变化的帧为空、移动只发变换、改颜色、换网格（新网格同时加入）、去掉组件、销毁实体。GPU（`tests/gpu/gpu_scene.rs`）：读回 GPU 表和 CPU 对比，包括移动、连续移动、停下后上一帧追上、同一帧多次移动、标志和光源，以及表增长后所有行都在；前向测试改为用更新驱动（改颜色、移开、隐藏、删除后行号复用、网格先引用后驻留、换 GPU 副本、网格删除）。3 张 golden 和 `sample_first_frame` 没有变。开窗口跑 `samples/basic` 10 秒，开着验证层，没有报错。
