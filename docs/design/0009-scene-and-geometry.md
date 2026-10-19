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

R1i（0008 §8）做完时（2026-10-18）定下的细节（离散 LOD 的常驻几何）：
- **cook 侧不用再做**：R2 的 cook 侧已经产出离散 LOD 链的页（每个 LOD 一页，含量化的位置、32 位切线框架、meshlet、meshlet 顶点和三角形、按 section 的范围，以及给不用 mesh shader 的路径的索引）。meshlet 的上限沿用 cooker 的 64 顶点、124 三角形（每个 meshlet 的三角形数据 372 字节，各厂商的 mesh shader 输出都放得下），不是计划里写的 128；visbuffer 的 7 位三角形下标照样够用。量化的往返测试也已经在 cook 侧（`positions_quantize_over_their_bounds` 等）。
- **运行时只认 cook 出来的格式**：`Renderer::upload_mesh(device, &MeshView, pages)` 把每个 LOD 的页原样上传，所有 LOD 常驻；运行时不再有 f32 顶点的 `MeshData`/`Vertex`/`cube`，测试用 `marlin-cook-mesh` 现场 cook 网格（dev 依赖，`tests/support/cook.rs`）。`MeshView::decode_lod` 留作 CPU 参考解码和工具用。framework 的加载器不再在 CPU 上解码，`LoadedMesh` 就是原地的 cook 数据（`InPlace<Mesh>`），页在上传时检查一次（`check_page`，GPU 不检查）。
- **一个网格一个 buffer**（`marlin_render::geometry`），一个 raw 视图：开头每个 LOD 一条 16 个字的记录（`Marlin.Engine.Scene.LodStreams`：顶点、三角形、meshlet 数，误差，索引宽度，各段的位置，section 表的位置和数量），然后是各 LOD 的 section 表，然后是各页（16 字节对齐）。记录里的位置都是相对 buffer 开头的字节偏移，shader 不需要知道页的基址。cluster DAG 的页和 RT LOD 留在 CPU 上，等 R2 的流送。页池（§3.2）是 R2 的事；现在每个网格自己一个 buffer，和"全部常驻"一致。
- **网格表**（GPU Scene，§2.1）每行 16 个字（`Marlin.Engine.Scene.MeshEntry`）：几何视图、LOD 数、`MeshFlags`、每顶点字节数（位置 | 属性 << 8 | 蒙皮 << 16）、位置的解码偏移和比例、是否驻留、UV 套数、局部包围球。`ResidentMesh` 由 `Renderer::resident_mesh(handle)` 给出。
- **解码在 GPU 上**：`content/engine/shaders/Scene/Geometry.mshader` 里的 `Lod`、`DecodePosition`（16 位量化或 3×f32）、`OctDecode`/`DecodeNormal`（切线框架的 bit 0–19）、`LoadIndex`，前向渲染器和测试 shader 都调用它们。前向 pass 用 LOD 自己的索引列表画（索引 buffer 就是几何 buffer 里的那一段），VS 按顶点下标从页里解码；push constant 变成 8 个字（`FORWARD_PUSH_BYTES = 32`），多了网格表、几何视图和 LOD。
- **离散 LOD 的选择**（每个实例，CPU 上）：取误差投影后不超过 `Renderer::lod_error_pixels`（默认 1 像素）的最粗一级；投影用实例包围球近侧的距离和实例的最大缩放。draw 按 (网格, LOD) 分组。`RenderStats` 多了 `triangles`。
- `sample_first_frame` 因为 LOD 选择而改变：中间那个物体的轮廓上有 118 个像素不同（LOD 1 的误差在那个距离上不到 1 像素）。把预算设成 0（总是 LOD 0）时，GPU 解码的画面和旧参考图在容差内一致，所以差别只来自 LOD。参考图单独提交重新生成。
- **测试**（`marlin-render/tests/gpu/geometry.rs`）：用引擎自己的解码函数（测试 shader `Render.DecodeVertices`，crate 的测试包现在可以依赖 `Marlin.Engine`）在 GPU 上解码一个球的每个 LOD 的所有顶点，和 CPU 的 `decode_lod` 比较：位置相差不超过 1e-6（GPU 可能用 FMA），法线点积 > 0.99999；量化和 f32 两种精度都测。往返：LOD 0 的位置都在球面上，误差不超过半个量化步长（f32 时为 0）。LOD：距离增大时三角形数单调不增、最远处是最后一级，预算为 0 时总是 LOD 0。3 张 golden 不变。开窗口跑 `samples/basic` 10 秒，开着验证层，没有报错。

R1j（0008 §8）做完时（2026-10-18）定下的细节（§4.1、§4.3 的离散 LOD 版本）：
- **范围**：一遍（没有 §4.2 的 HZB 两遍遮挡剔除，那是 R2）、只有硬件光栅（没有软光栅分流）、只有离散 LOD（没有 DAG 遍历）。`marlin_render::Visibility::add_passes(g, renderer, scene, gpu, camera, w, h)` 返回 `VisibilityOutput { visbuffer, depth, clusters, counters, capacity }`。游戏的帧里还没有接上：输出没人读，graph 会把这些 pass 剔除，等 R1k 的材质解算来读 visbuffer。
- **pass**（`content/engine/shaders/Visibility/Visibility.mshader`，`Marlin.Engine.Visibility`）：`VisClear` → `Reset`（计数清零）→ `InstanceCull`（每个实例行一个线程：视锥测试世界包围球，再选 LOD，可见的追加到 `VisibleInstances`）→ `InstanceArgs`（可见实例数写成下一步的 indirect dispatch）→ `ClusterCull`（每个可见实例一个 64 线程的 group：逐个 meshlet 做视锥和法线锥背面测试，追加到 `VisibleClusters`）→ `FinishArgs`（cluster 数截到容量，写成 mesh dispatch：一行最多 65535 个 group，超出用第二维）→ `VisRaster`（mesh shader 每个 workgroup 一个 cluster，128 线程；pixel shader 对 `R64Uint` 做原子 max；没有颜色和深度附件）→ `VisDepth`（深度拆进 `R32Float`）。
- **visbuffer 的位**照 §4.3：高 32 位是 reverse-Z 深度的 float 位，然后 25 位可见 cluster 下标、7 位三角形下标。三角形下标经 mesh shader 的逐图元输出 `[PrimitiveId]` 带给 pixel shader，值就是 `cluster << 7 | triangle`。可见 cluster 表每项两个字：实例行、meshlet 下标 | LOD << 24。
- **计数和 indirect 参数分两个 buffer**：一个 pass 不会既对一个 buffer 做原子计数又从它取 indirect 参数。RHI 的 indirect dispatch 记录是 16 字节（先是一个常量，即 push 的第 31 个字，再是 x、y、z），一开始按 12 字节写，cluster 剔除一个 group 都没跑；直接 dispatch 对照后才找到。
- **容量**：可见 cluster 表按"每个可绘制实例最大一级的 meshlet 数之和"取上界，取 2 的幂、至少 1024、至多 2^25（25 位），所以不会溢出，场景小变化时 graph 的 plan 也不变。
- **GPU 上选 LOD**：公式和 CPU（前向渲染器）的完全一样：实例包围球近侧的距离、最大缩放、`lod_error_pixels`，取最粗的合格一级。各级误差从几何 buffer 的 LOD 记录里读。
- **剔除细节**：视锥平面在 CPU 上从 view-projection 抽出（左右下上和近平面，reverse-Z 的近平面是 z = w，远平面在无穷远不测），按世界空间传入。法线锥用 cooker 写的 snorm8 轴和 cutoff（cutoff 为 1 是退化锥，不剔除）；实例有非均匀缩放时不做法线锥测试（锥在非均匀缩放下不再保守）。被删除的实例行由 scatter 写成 `HIDDEN`，表重建时所有行都重写，所以 GPU 剔除只看标志，不需要存活位图。
- **网格表里放句柄**：Marlin shader 的资源就是 32 位 bindless 下标，可以放在 buffer 的结构体里，所以 `MeshEntry.Geometry` 直接是 `ReadOnlyByteBuffer`，剔除和 mesh shader 从网格表拿到几何 buffer。
- **顺带的修正**：
  - RHI：`RenderPassDesc` 加了 `area`（没有附件的 render pass 必须给，比如只写 storage 视图的 visbuffer pass），graph 的 raster pass 对应 `b.area(w, h)`（进拓扑键）。
  - shader 编译器：pixel 入口读 `PrimitiveId` 时声明 `Geometry` capability（DXC 对 `SV_PrimitiveID` 也这样做；mesh 入口已经有 `MeshShadingEXT`），并且和其他整数输入一样加 `Flat`。自己的 SPIR-V 校验器也检查这两条（以前对内建变量跳过了 `Flat` 规则，验证层报错后补上），`shaderc` 加了回归测试。语法没有变化。
  - `Renderer::add_passes` 返回 `ForwardOutput { color, depth, stats }`，深度改为存储（测试拿它和 visbuffer 比较）。
  - 测试 shader 的 crate 包现在可以依赖 `Marlin.Engine`（R1i 已改）；framework 加了 `visibility_shaders(library)`，按 stage 和 push 大小检查，端到端测试用真实的 cook 结果建一次 `Visibility`。
- **测试**（`marlin-render/tests/gpu/visibility.rs`，同一帧里同时跑前向和可见性 pass）：旋转、缩放、非均匀缩放、相互穿插的立方体和球，视锥外和相机背后的实例被剔除（可见实例数正确）；visbuffer 的覆盖、深度和前向的深度缓冲**逐像素一致**（RTX 5060 上 0 个像素不同，测试也按 0 容差），拆出的深度纹理和 visbuffer 的高 32 位逐位相同，每个像素的 cluster 都属于前向画出那个颜色的实例；把一个实例移出视野后下一帧再比一次。法线锥：正对相机的球 47 个 meshlet 只画 31 个，画面不变。LOD：5 个距离上的球，GPU 选的 LOD 的三角形总数等于前向渲染器在 CPU 上选的。空场景 visbuffer 全 0。3 张 golden 和 `sample_first_frame` 不变。开窗口跑 `samples/basic` 10 秒，没有报错。

R1k（材质最小集，细节见 0011 §14）对本文的影响：场景多了材质表（`MaterialId`、每个实例一个材质），GPU Scene 多了材质表和每个 permutation 一张的参数表，scatter 改成通用的（op 带目标字偏移，一张表一次 dispatch）。材质分箱（§5）和运动向量（§4.6）照本文实现：分箱的 indirect 记录的常量带着 bin 的起点，所有 permutation 的解算在一个 graph pass 里。`InstanceTransforms` 一度改成 6 个 `float4`（驱动问题），编译器绕开之后恢复成两个 `Rows3x4`（0016 §4.2）。

R2a（0008 §8 R2 的第一步，2026-10-18）定下的细节：cluster DAG 全部常驻，GPU 上遍历 DAG，取代离散 LOD（§3.2、§4.1）：
- **页池**（`marlin_render::Meshes`）：一个 `PAGE_SIZE`（128 KiB）一槽的持久 buffer，一个 raw 视图。容量在创建时定（`Meshes::new(device, pool_pages)`），引擎用 `DEFAULT_POOL_PAGES = 4096`（512 MiB，桌面 AAA 渲染器流送池的常见起点），测试用小的。上传网格时每页占一个空闲槽，页原样拷进槽里（页头里的偏移都相对槽起点），放不下就是 `OutOfMemory`；`release_mesh` 把槽还回去。这一步还没有淘汰，所有页常驻；流送（R2b）在同一个池上做。
- **DAG buffer**：每个网格一个，放常驻部分：4 个字的头（group、part、页表的字节偏移和节点数），然后是层级节点、group、part（都是 cooked 的原样），最后是页表（每页一个槽号）。网格表（GPU Scene）的 `Geometry`/`Lods` 两个字换成 `Pages`（页池）和 `Dag`（DAG buffer）两个句柄。
- **cluster 的地址**：`slot << 12 | 页内下标`。上传时检查每页的 cluster 数小于 4096（一页 128 KiB，cluster 记录就要 64 字节，实际到不了）。可见 cluster 表每项是（实例行，地址），visbuffer 的 25 位 cluster 下标仍然指这张表。
- **遍历按层级**，不用持久线程：每一层节点一个 `NodeArgs` + `NodeCull`（每个节点 8 个线程，一个线程一个子节点：视锥测试，再把子树里最大的 group 误差按子节点的球投影，不超过预算就整个跳过；叶子是 part，追加到 part 表，内部节点追加到节点表），层数是这一帧要画的网格里最深的层级（至少 1）。Vulkan 不保证不同 workgroup 之间的前进性，持久线程等别的 workgroup 写队列可能挂住 GPU（这台机器上的 NVIDIA 驱动挂住过一次就要重启），所以选了逐层的 indirect dispatch，代价是每层一次空跑的可能和一次 barrier。
- **LOD 判断在 `ClusterCull`**（每个 part 一个 32 线程的 group）：part 所在 group 的误差投影不超过预算就整个跳过（这块表面由更粗的画）；否则 cluster 自己的 LOD 误差投影不超过预算、过了视锥和法线锥的才画。投影用 `ProjectedError`：球心经实例变换，距离取球近侧（减去半径乘最大缩放，下限 1e-4），误差乘最大缩放和每单位像素数再除以距离。group 的判断和由它简化出的 cluster 的判断用的是同一个函数、逐位相同的球和误差（cooker 把 group 的球和误差原样写进 cluster 的 LOD 球和误差），所以两者互补，切割水密。层级里的跳过留了 1% 的余量（`<= 预算 × 0.99`），因为它和 `ClusterCull` 是两个 shader，驱动的浮点收缩可能不同；余量保证层级不会跳过一个 `ClusterCull` 会判为"要细化"的 group。
- **缓冲的容量**：节点、part、可见 cluster 表各按"每个可绘制实例整棵 DAG 的节点数、part 数、cluster 数之和"取 2 的幂，至少 1024，上限是 `Visibility` 的预算（`max_nodes = max_parts = 2^21`，`max_clusters = 2^22`，和 Nanite 的默认值同一量级；visbuffer 的 25 位是硬上限）。超出的丢掉，计数器照样计（`counters` 模块：可见实例、节点、part、找到的 cluster、画了的 cluster）。
- **mesh shader**：每个 workgroup 一个 cluster，最多 128 顶点 128 三角形；从页里读 cluster 记录、解码位置、三角形的三个页内顶点下标。材质解算用同样的函数从页里重建三角形。`Geometry.mshader` 的 `Lod`/`LodStreams`/`LoadIndex` 换成 `LoadPage`、`LoadCluster`、`LoadTriangle`、`LoadGroup`、`LoadPart`、`PageSlot`、`LoadChild`。
- **离散 LOD 在运行时完全不用了**：`Meshes::lod_indices`、`LodIndices`、`meshlet_capacity` 删掉；没有 DAG 的网格上传时是 `InvalidInput`。`VisibilityShaders` 多了 `node_args`、`node_cull`、`part_args`，少了 `instance_args`，并有 `VisibilityShaders::ENTRIES`/`from_fn`。
- **测试**（`marlin-render/tests/gpu`）：参考光栅器改成画测试在 CPU 上按同一规则算出的切割（每个 cluster 一个实例、每实例 128 个三角形，多余的退化），GPU 画的 cluster 必须都在 CPU 切割里且各一次，visbuffer 的覆盖和深度与参考逐像素一致，每个像素的实例正确；预算 0、0.5、1、4、16 像素都比一遍。法线锥：预算 0 的球画的全是第 0 层，背面剔除后不到 3/4。距离：5 个距离上的球，切割的三角形数随距离单调不增（2046、510、510、302、302；最远处是根 group 的 cluster）。解码：每页的每个顶点 GPU 和 CPU 解码一致，位置都在球面上（量化半步以内）。页池：释放后槽回来、再上传拿到同样的槽；池满时 `OutOfMemory`。材质的 3 张 golden 不变。`sample_first_frame` 有 93 个像素不同（球和圆环的轮廓上，1 像素误差预算下的切割和原来的离散 LOD 不一样），参考图单独提交重新生成。开窗口跑 `samples/basic` 10 秒，开着验证层，没有报错。
- **cooked 格式只剩 DAG**（同属 R2a，单独提交）：离散 LOD 链在 cook 侧也删掉了。`CookedMesh` 直接内嵌一个 `ClusterDag`（不再是 0 或 1 个的 `RelSlice`），`lods`、`rt_lod`、`MeshLod`、`LodSection`、`Meshlet`、`MESHLET_MAX_*`、`check_page`、`decode_lod` 删掉；源设置 `Mesh` 去掉 `Lods`、`RtLod`、`ClusterDag` 三项（RT 只用 DAG 的 RT LOD，`RtFraction` 保留）。bulk blob 现在是 DAG 的各页加最后一个 RT LOD。网格 cooker 版本升到 3。原来 LOD 页的检查里有蒙皮关节下标的检查，搬到 `PageView::new` 和 `check_rt`（`PageLayout` 多了 `joints`：有权重的关节下标必须小于它）。`cook_mesh::meshlet` 留着，它是分 cluster 用的生成器；`optimize` 只剩焊接在用，缓存和取用顺序优化留给以后不走 mesh shader 的路径。

R2b（2026-10-18）定下的细节：cluster 页的流送和反馈（§6.1）：
- **范围**：GPU 驻留的流送：网格上传时只有根页进页池，其余页按可见性 pass 的请求进来，页池满了按 LRU 淘汰。页的字节还来自内存里的整个单元（加载器仍然整单元加载）；按需从磁盘读单个 blob（`Package::read_blob` 已经有了，缺异步 IO 和加载器的改造）是下一步 R2c。
- **页表搬进页池**：页池 buffer 是"槽 + 页表"，页表每页一个 u32（槽号或 `0xFFFFFFFF`），每个网格占一段连续的表项（按需分配、释放后合并）。DAG buffer 头里的第三个字从"页表偏移"改成"本网格页表在页池里的字节偏移"，所以页表只有一份、一个视图、一个 graph 导入。`PagePoolDesc` 默认 4096 槽（512 MiB）、2^18 个表项；槽数上限 `MAX_POOL_SLOTS = 32768`，保证字节偏移是 32 位。
- **写页池只在 graph 里**：`Meshes::take_uploads` 收集这一帧要写的页和表项（同一槽、同一表项以最后一次为准），`Meshes::add_passes` 导入页池，`Marlin.Engine.Streaming.PageUpload` 一个 group 一个写操作（页的字节或一个表项）把上传 ring 里的数据拷进去，在可见性之前。读页池的 pass（`NodeCull`、`ClusterCull`、`VisRaster`、材质解算、测试的参考光栅）都声明对它的读，graph 插屏障。`ClearWords` 每帧清零"槽被用过"的标记。网格的常驻部分（DAG buffer）仍在上传时阻塞拷贝，之后不变。
- **GPU 侧**：`NodeCull` 到叶子时，叶子所在 group 的所有页都驻留才把 part 交给 `ClusterCull`，否则把缺的页写进请求表（页表项的字节偏移 + 投影误差作为优先级，计数器第 8 个字）。`ClusterCull` 读一个槽就在 `Used` 里标记它；一个 cluster 自己的误差超过预算时，只有它的来源 group 整个驻留才让给更细的，否则画它自己。所以缺页只会让画面变粗，不会有洞。
- **group 要整个画**：页装满才换页，group 常常跨页，跨页的 group 只有所有页都驻留才画（GPU 逐 part 检查）。为此 cooked 格式把两种关系分开（网格 cooker 版本 4）：`ClusterPage` 的依赖是"本页各 group 的父 group 的所有页"（父 group 就是本页 group 简化出的更粗 cluster 所在的 group，总在更前的页），后面接 `sibling_count` 个兄弟页（本页各 group 的其他页）。原来的依赖把兄弟页和"更粗 cluster 所在的页"混在一起：兄弟关系沿着装满的页一页接一页传下去，一个请求的闭包就成了整个网格；而只有更粗 cluster 所在的页不够，那些 cluster 所在的 group 也要整个驻留才能画。
- **CPU 侧的不变式**：页驻留 ⇒ 它的依赖都驻留，所以一个整个驻留的 group 的父 group 也整个驻留，切割照样水密。请求一页时加载它和它的兄弟页，再加上这些页的依赖闭包，要么全部进来要么都不进来；淘汰一页时连同所有依赖它的驻留页一起淘汰。常驻（钉住）的是开头的根页、所有根 group 的页和它们的依赖闭包，所以最粗的 group 总是完整的，任何区域都有可画的退路。
- **反馈的节奏**：`Meshes::apply_feedback` 读回一帧的请求和用过的槽：用过的页更新最后使用帧；请求去重后按优先级从高到低，每个连同闭包找空槽，没有就淘汰最久没用的页（只淘汰没钉住、在最新一帧反馈里没被用过、不是这一帧正要加载的）；一帧最多加载 `max_page_uploads`（默认 64 页，8 MiB）页，比预算大的闭包可以单独占一帧。framework 的 `RenderSide` 每帧把计数器、请求和用过标记三个读回存下来，下一帧开始时用 `FrameContext::readback` 非阻塞地取已经完成的（两帧前的一定完成），`RenderSide::collect_feedback` 交给 `Meshes`。`streaming_idle()`：没有待写的，上一次反馈要的页都已驻留。
- **偏离 §6.1**：预算不够时没有逐级提高误差阈值；缺页时退到驻留的更粗层级（画面变粗、不缺块），阈值自适应留到有大场景压测时再做。请求没有在 GPU 上去重，CPU 去重（每帧最多 `max_requests = 4096` 条）。
- **`Meshes::prefetch(mesh)`**：一次加载网格的所有页（放不下就都不加载），给加载画面、过场特写和工具用；材质测试用它让画面和流送无关。
- **顺带的修正**：`MeshView::new` 先检查 UV 套数不超过 4 再算属性步长，随机损坏的 UV 套数原来会在乘法上溢出 panic（cooker 测试的损坏用例换了布局后撞上了）。
- **测试**（`marlin-render/tests/gpu/streaming.rs`）：上传后只有根页；近处的球第一帧只画根页并发出请求，之后每一帧都和参考光栅画的同一切割（CPU 按同样的驻留状态、同样的整 group 规则算）逐像素一致，直到流送追上；追上以后再 `prefetch` 全部页，画面深度逐像素不变（没加载的页都是切割用不到的）；每帧最多 1 页时追上要的帧数比 64 页时多；两个球、页池只够一个球的非根页：相机从 A 移到 B，A 的页被淘汰回到根页、B 的全部进来；每一帧都检查驻留页的依赖都驻留、根页一直在。释放网格还回槽；根页放不下时 `OutOfMemory`。可见性测试改成每帧检查直到流送追上。cooker 的测试检查依赖是父 group 的全部页且都在前面、兄弟页正好是本页 group 的其他页。`sample_first_frame` 在流送追上后的那一帧和 R2a 的参考图一致，没有重新生成。开窗口跑 `samples/basic` 10 秒，开着验证层，没有报错。

R2c（2026-10-18）定下的细节：网格页按需从磁盘读，加载器不再整单元加载（资产这一层的细节见 0003 末尾"按 blob 流送"）：
- **cook**：网格的根页（`root_page_count` 之前的页，也就是 R2b 钉住的那些，加上它们的依赖闭包仍在其中，因为依赖总在前面）照常随单元加载；其余的页和 RT LOD 都标成流送 blob。网格 cooker 版本升到 5。`samples/basic` 的包从 1.3 MiB 原始数据里，加载关卡时只读单元头和根页。
- **`MeshData` 加了 `request` 和 `take`**：`bulk(i)` 只给内存里有的 blob；`request(i)` 开始读一个没有的；`take(i)` 把读到的字节交出来（数据那边不留副本），读失败是 `Err`。framework 的 `LoadedMesh` 现在是 `Arc<StreamedMesh>`：单元里的根页原地用，流送页通过引擎的 `BlobStreamer` 读，每个 blob 至多一个在途的读。`StreamedMesh::view` 用 cook 时记下的 blob 大小（流送页不在单元里，不能拿切片长度当大小）。
- **`Meshes` 的异步加载**：一个请求的闭包里内存里没有的页，先 `request`，标记"正在读"；以后每次 `apply_feedback` 先把读到的取回来，放进网格的"已到达"表；闭包的所有页都到齐的那一帧才分槽位、写页（这一帧上传用的就是这些字节，传完就丢）。读到的页 120 帧内没被用上（相机移走了）就丢掉，以后需要再读。读失败或检查不过的页标成失败，不再读，由更粗的 cluster 代替；失败的页不算"缺页"，所以流送仍然能追上。`prefetch` 对不在内存的页也是先读，读齐之前返回 `false`，`poll_reads` 取回读到的。`streaming_idle` 还要求没有在读的页。
- **顺带的修正**：加载器给 `InPlace` 记 blob 范围时，不在单元里的流送 blob 是一个空切片，指针不在单元里，原来的减法会溢出；空切片现在记成空范围。fuzz 的种子生成器本来就落后于包格式（没有名字表），一起改了，`asset_package` 的种子按新格式重新生成。
- **测试**：asset（`tests/unit/cooked.rs`）：流送 blob 可以不在、非流送的不行，标志往返。cook_core（`tests/unit/package.rs`）：带一个常驻 blob 和一个流送 blob 的资产打包后，单元的读取范围和大小都不到全部的四分之一，加载器看到常驻的在、流送的不在；`BlobStreamer` 经过 `IoQueue` 读回流送 blob 的原始字节，只发一次读；发读前丢掉的请求不读；同步读；`MemorySource::insert_streamed` 同样可用。render GPU（`tests/gpu/streaming.rs`）：一个像包一样加载的测试 `MeshData`（根页在内存，其他页在 `request` 之后隔一次 `take` 才到）：每一帧都和参考光栅的切割逐像素一致，每页只读一次，追上以后的深度和所有页都在内存的网格逐像素相同；一页读失败时那块表面停在更粗的层级，页不再被读。framework 端到端：样例包里确实有流送 blob，流送追上以后的画面和 `sample_first_frame` 一致（没有重新生成）。开窗口跑 `samples/basic` 10 秒，开着验证层，没有报错。

R2d（2026-10-18）定下的细节：两遍 HZB 遮挡剔除（§4.2）：
- **HZB**：`R32Float`、带完整 mip 链的视图历史资源（`g.history("VisHzb", 视图 id, …)`），每个 texel 存它覆盖的屏幕里最远的深度（reverse-Z 下最小的值）。mip 0 每个轴是 2 的幂、至少半个屏幕（`hzb_size`：`next_power_of_two(w) / 2`），一个 texel 每个轴最多盖 2 个像素（小数边界时 3 个），`HzbFirst` 取这些像素的最小值；之后每一级（`HzbDown`）取下一级 2×2 的最小值，直到 1×1。shader 在 `Visibility/Hzb.mshader`。
- **遮挡测试**（`Occluded`）：把球的包围盒 8 个角投影，任一角在相机后面（w ≤ 0）或越过近平面就算可见；否则取屏幕矩形和最近的深度，选矩形在其中最多盖 2×2 个 texel 的最粗一级，读这 4 个 texel 的最小值（最远的遮挡物），最近深度比它更远才算被遮挡。
- **两遍**（Nanite 的做法）：第一遍的实例和 cluster 用上一帧的 HZB 和上一帧的 view-projection 测试，被挡住的实例记进 `OccludedInstances`，被挡住的 cluster（层级遍历和 LOD 判断都通过了的）记进 `OccludedClusters`，其余照常画。第一遍画完拆出深度、建 HZB；第二遍用这一帧的 view-projection 和新 HZB 重测：被挡的实例重新从根节点遍历（同样的逐层 pass，节点和 part 表接着第一遍往后追加），被挡的 cluster 直接重测（`ClusterRetest`），通过的追加到可见 cluster 表、第二次光栅（mesh dispatch 只覆盖这一遍追加的那一段，计数器第 10 个字是这一遍的第一个 cluster）。最后用全部的深度再建一次 HZB，留给下一帧。第一遍只是"按上一帧可能看不见"地推迟，错推迟的第二遍都会补画，所以画面和不做遮挡剔除逐像素相同；visbuffer 的 cluster 下标在两遍之间连续。
- **没有上一帧时**（第一帧、换了分辨率使历史描述不同、相机切换：`ViewDesc::previous` 为 `None`）第一遍不做遮挡测试，也没有第二遍，只建下一帧的 HZB。`Visibility::occlusion` 可以关掉遮挡剔除（调试、对比）。
- **不测遮挡的地方**：层级节点只做视锥和误差测试，不做 HZB 测试（实例和 cluster 两级已经挡掉了大部分；节点级的遮挡和"节点被挡则子树推迟到第二遍"留到大场景压测时再看）。
- **容量**：被挡实例和被挡 cluster 表的容量按"可绘制实例数"和"cluster 总数"中大的那个取 2 的幂，上限 `max_occluded`（2^20）；放不下的直接画（保守）。计数器扩到 16 个字，indirect 参数 5 条记录（加上实例重测和 cluster 重测）。
- **接口**：`Visibility::add_passes(g, meshes, scene, gpu, pool, &ViewDesc { id, camera, previous, width, height })`；`VisibilityOutput` 多了 `hzb`（最终的 HZB）和 `two_pass`。`VisibilityShaders` 多了 `instance_retest_args`、`instance_retest`、`cluster_retest_args`、`cluster_retest`、`hzb_first`、`hzb_down`，cull 的 push constant 是 16 个字（`CULL_PUSH_BYTES = 64`），HZB 的 6 个字。
- **测试**（`marlin-render/tests/gpu/occlusion.rs`，以及原有的可见性和流送测试：它们现在每一帧都在两遍下和参考光栅逐像素一致）：一堵铺满视野的墙后面三个球：稳定以后第一遍挡掉三个实例，第二遍仍然挡住，只画墙；关掉遮挡剔除画的 cluster 更多，画面完全相同。墙挪走的那一帧：第一遍按旧 HZB 挡掉三个球，第二遍把它们画出来，画面和参考一致；之后相机转动、移动的几帧同样一致。半边墙后面一个大球（预算 0，最细的 cluster）：实例可见、左半边的 cluster 被挡（`OccludedClusters` 非空）；墙挪走那一帧第二遍画出这些 cluster。HZB 的每一级和 CPU 按同样规则从深度图算出的逐值相等，最后一级是 1×1。材质的 golden 和 `sample_first_frame` 不变。开窗口跑 `samples/basic` 10 秒，开着验证层，没有报错。

R2e（2026-10-18）定下的细节：软光栅（§4.1、§4.3）：
- **分流**（`Software`，在 `ClusterCull` 和 `ClusterRetest` 里）：按 cluster 球的投影直径（近侧距离）和三角形数估计屏幕上三角形的平均边长（直径 × √(2 / 三角形数)），小于 `Visibility::sw_edge_pixels`（§4.1 的 `r.geo.sw_raster_edge_px`，默认 32 像素）的走软光栅；碰到近平面的 cluster（球心到近平面的距离不超过半径）一律走硬件，软光栅不裁剪。设成 0 就全走 mesh shader。
- **可见 cluster 表分两半**：容量翻倍，硬件光栅的 cluster 从 0 起，软光栅的从 `capacity` 起（各自的计数器：第 15、16、17 个字是软光栅找到的、画了的、这一遍的第一个）。visbuffer 里的 cluster 下标就是表里的下标，所以材质分箱和解算不用区分两种光栅；每半的容量上限是 2^24，两半加起来仍在 25 位以内。两遍遮挡剔除的第二遍在两半里都接着往后追加。
- **软光栅**（`SwRaster`）：每个 cluster 一个 128 线程的 group，一个线程一个三角形，直接从页里解码三个顶点（不共享顶点，簇小，重复的解码比 groupshared 同步便宜）。顶点吸附到 1/256 像素（8 位子像素，和这里的 GPU 一样），64 位整数的边函数，像素中心采样，top-left 填充规则，按硬件一样剔除背面（y 向上时顺时针）；深度按屏幕空间线性插值 NDC 的 z，用 64 位原子 max 写 visbuffer，值和 `VisPs` 的一样。每个三角形遍历它的像素包围盒。
- **和硬件对比**：同一场景全走硬件和全走软件，覆盖的像素逐个相同、每个像素属于同一个实例；深度相差在 5×10⁻⁴（相对）以内（硬件按自己的方式插值，最多见到 1.5×10⁻⁴）。所以和参考光栅的逐像素比较里，深度改成相对 5×10⁻⁴ 或绝对 10⁻⁶ 的容差，覆盖仍然逐像素相同。
- **测试**（`marlin-render/tests/gpu/software.rs`，以及所有可见性、流送、遮挡测试：64 像素的视图里大多数 cluster 走软光栅，它们照样和参考光栅一致）：全硬件和全软件逐像素对比（如上）；默认阈值下小三角形走软件、近处的大立方体走硬件；相机在一个大球里面时，穿过近平面的 cluster 留在硬件上、其余走软件，画面和参考一致。材质的 golden 和 `sample_first_frame` 不变。开窗口跑 `samples/basic` 10 秒，开着验证层，没有报错。

R2f（2026-10-18）定下的细节：太阳的虚拟阴影贴图（0010 §5.3 的基本链路；静态页缓存和 SMRT 软阴影留到后面两步）：
- **clipmap**（`marlin_render::shadows`，`VsmSettings`）：`levels` 级，每级 `virtual_resolution`² 个 texel，`page_size`² 一页，第 k 级的范围是第 0 级的 2^k 倍。桌面默认：16K² 的级、128² 的页、8K² 的物理页池（64×64 页），第 0 级 texel 半毫米（8 米宽），14 级到 65 公里，光照深度范围 2000 米。每级的中心是相机在垂直于光的平面上的位置，按这一级的页大小吸附，相机移动时 texel 落在同一处世界位置；光照深度在相机处为 0.5、朝光增大。
- **一帧的 pass**（`Marlin.Engine.Shadows`，`Shadows/Vsm.mshader`）：`VsmMark` 从相机深度重建每个像素的世界位置，选 texel 不小于像素大小的最细一级（超出最后一级就用最后一级，texel 比像素细只是更清晰），标记滤波核四个角所在的页；`VsmAllocate` 给每个被标记的虚拟页按顺序分一个物理页（池满了的页不映射，那里不投影），写页表；`VsmClearArgs` + `VsmClear` 把分出去的页清成深度 0；每一级用可见性 pass 的正交模式画（`Visibility::add_shadow_view`）；`VsmMask` 按同样的规则找级，3×3 PCF，写屏幕空间的太阳阴影遮罩（`R16Float`，1 为照亮），直接光照把太阳乘上它。
- **阴影视图**复用可见性的剔除 pass：`View` 多了第 6 个平面（正交的远平面；相机的远平面在无穷远，写成 `(0, 0, 0, 1)`）和正交模式：LOD 的投影误差不除以距离（每单位 texel 数固定），不做法线锥（两面都画）、不分流软光栅、不做遮挡测试、不发页请求（读到的槽仍然标记"用过"）；cluster 剔除多一步：只画覆盖到已映射页的 cluster。光栅用 `VisMesh` 加新的 `ShadowPs`（不剔除背面）：把虚拟 texel 经页表换到物理页，原子 max 写入离光最近的深度（`R32Uint` 存 float 的位）。
- **偏移**：被测点先沿表面法线（材质缓冲的 proxy 法线）推 `normal_offset` 个 texel，深度再朝光偏 `depth_bias` 个 texel（默认 2 和 1.5）。没有材质缓冲时（render crate 的测试）只有深度偏移。
- **接口**：`Vsm::add_passes(g, vis, meshes, scene, gpu, pages, used, depth, material, env, camera, w, h) -> VsmOutput { mask, used, page_table, pool, counters }`；`Lighting::add_passes` 多了一个可选的阴影遮罩。framework 的帧：可见性 → 材质 → VSM → 光照；流送反馈读的是阴影视图之后的"用过"标记。
- **还没做**：静态页跨帧缓存（现在每帧所有被标记的页都重画；物理页池是瞬态资源）、动态实例和太阳方向变化的失效、L1–L2 的 SMRT 软阴影、半透明和体积雾的页请求、阴影视图的页流送请求。性能没有测：默认设置下每帧 14 级各跑一遍剔除和光栅。
- **测试**（`marlin-render/tests/gpu/shadows.rs`，测试用 4 级、1024² 的小 clipmap）：一块地面和一个悬空的盒子，每个像素在 CPU 上从表面朝太阳射线和场景里的盒子求交，和 GPU 的遮罩（< 0.5 为阴影）比较：离开阴影边缘一个像素以外的像素全部一致，开阔地面没有一个 texel 被自己完全挡住（没有 acne）；太阳换方向、盒子移动以后的下一帧同样一致；被映射的页正好是被标记的页、各自一个不同的物理页、只占虚拟页的一小部分。可见性的其他测试不变。`sample_first_frame` 因为有了阴影而改变（物体在地面上的影子、圆环在球下半部的影子），参考图单独提交重新生成。开窗口跑 `samples/basic` 12 秒，开着验证层，没有报错。

R2g（2026-10-18）定下的细节：VSM 的页跨帧缓存（0010 §5.3 的"缓存"一条）：
- **持久资源**（按视图，`g.persistent(…, view, …)`）：物理页池、缓存表（每个虚拟页上次用的物理页）、每个物理页的标签（级、跨光方向的绝对页坐标 x/y、代数）。资源刚创建时（`fresh`），缓存表和标签都填成 `0xFFFFFFFF`（无效）。
- **页按世界锚定**：每级的第一页是绝对页 `Origin`，第 p 页放在表槽 `(p + Origin mod n) mod n`（n 为每边页数）。相机移动时，级跟着平移，仍在级内的页保持原槽，移进来的页绕回到移出去的槽上，标签对不上，就是未命中。光照深度的中心按 `depth_range / 8` 吸附，不再每帧跟着相机。
- **代数**：太阳方向、深度中心所在的格、设置三者任何一个变了，视图的代数加一，所有页的标签都失效。CPU 端每个视图记一份（`Vsm` 里一个按视图的表，用 `marlin_jobs::sync::Mutex`，因为 pass 是并行录制的）。
- **失效球**：`RenderScene::shadow_changes` 记下这一帧里阴影变了的世界包围球：实例移动、新增、删除、隐藏或显示、换网格、网格换了 GPU 副本，各取变化前后的位置（每行每帧旧位置只记一次）。另外，`Meshes::changed_meshes` 给出上一次 `take_uploads` 改了驻留页的网格，它们的实例的球也算（LOD 切法可能变了）。颜色和材质的变化不影响阴影，不失效。
- **一帧的 pass**：`VsmMark` → `VsmInvalidate`（每个物理页一个线程，标签所在的页和任何一个失效球在光空间相交（多留一个 texel），就清掉标签）→ `VsmKeep`（被标记、缓存的物理页标签对得上的，保留）→ `VsmFreeList`（没保留的页分两张表：没有有效标签的在前，有效但这一帧没人标记的缓存页在后，后者只在前者用完时才被拿走）→ `VsmAllocate`（被标记的页：保留的直接用；否则按顺序从空闲表取一页、写标签和缓存表，页表里带上 `RENDER_BIT`）→ 只清新分到的页 → 各级只画带 `RENDER_BIT` 的页（cluster 剔除和 `ShadowPs` 都只认这些页，缓存页不被覆盖）→ `VsmMask`。
- **计数**（`shadows::counters`）：本帧画的页、两张空闲表的长度、命中缓存的页。
- **还没做**：静态和动态分开缓存（现在动态物体经过的页连同静态内容一起重画）；失效球在 CPU 上算，流送变化时要遍历实例（场景大了改到 GPU 上做）；SMRT 软阴影（R2h）。
- **测试**（`marlin-render/tests/gpu/shadows.rs`）：静止的下一帧不画任何页，遮罩和上一帧逐像素相同；相机平移以后，画的页少于标记的一半，遮罩仍和 CPU 射线一致；盒子移动只重画它下面的页（其余命中缓存），停下以后的一帧不再画页；看天空的一帧不标记任何页，转回来以后所有页仍命中缓存；太阳换方向以后没有页命中；改颜色不画页；隐藏盒子以后它的影子消失。`sample_first_frame` 不变。开窗口跑 `samples/basic` 12 秒，开着验证层，没有报错。
- 实现里遇到的一个坑：标准库只有 `AsUInt(float)`，`AsUInt(int)` 会先把 int 隐式转成 float 再取位；int 和 uint 之间要用 `(uint)`/`(int)` 转换。

性能（2026-10-18）：`samples/basic` 在 RTX 5060 上不到 60 fps，GPU 一帧 16 ms，其中 `ShadowRaster` 占 13.7 ms。原因是阴影光栅在每级 16K² 的虚拟分辨率上进行，`ShadowPs` 到了像素才丢掉不画的页：覆盖整级的地面每级都要着色 2.7 亿个像素。
- **按页光栅**：`ClusterCull` 在阴影视图里不再只问"有没有画的页"，而是给 cluster 覆盖到的每个本帧要画的页追加一个绘制（`ShadowDraws`：cluster 在 `Clusters` 里的下标、页在本级的坐标 `x | y << 16`；计数器第 18 个字）。页少的 cluster 遍历自己的页矩形查页表；页多的（比如地面）改为遍历本级本帧画的页的列表，取两者中短的那个。这个列表由 `VsmAllocate` 给每个带 `RENDER_BIT` 的页追加（`VsmDrawn`：每级 `PoolPages + 1` 个字，先是个数，再是页）。
- **`ShadowMesh`**：每个绘制一个 mesh group，把虚拟 texel 平移整数个 texel，让这一页正好铺满 `page_size`² 的光栅区域，硬件按视口裁剪三角形，只给这一页的 texel 着色；图元 id 带上物理页号，`ShadowPs` 直接写物理页，不再查页表。平移是整 texel 的，覆盖的 texel 和虚拟分辨率上一样，`sample_first_frame` 逐像素不变。
- **容量**：每级的绘制表 `4 × (cluster 数 + 物理页数)`，上限 `Visibility::max_shadow_draws`（2^20）；超出的绘制丢掉（计数仍然计）。
- **结果**：`ShadowRaster` 13.7 ms → 0.18 ms，开 vsync 跑满 165 fps，关掉约 370 fps。剩下的 GPU 时间是约 180 个依次执行的小 pass（14 级每级约 10 个），每个 10–20 µs，主要是 barrier 和间接参数的等待，不是计算量。
- **`r.stats`**：每秒在日志里打印帧率、GPU 一帧的跨度和各队列的忙碌时间、CPU 区段和 GPU pass 的平均耗时。
- **各级一起剔除**（`Visibility::add_shadow_views`）：14 级不再各自声明一串 pass，而是每个阶段一个 pass、里面每级一次 dispatch（`cull_passes`）：各级的表各自独立，共用的只有读的东西和只置 1 的"用过"标记，所以一个 pass 里的 dispatch 之间不需要 barrier，可以并行跑。shader 不变。阴影的 pass 从约 126 个降到约 9 个。
- **原生 indirect**（0016 §6）：剩下每个小 pass 10–20 µs 的大头其实是 Vulkan 的 device-generated commands。graph 的 indirect 命令不送记录里的 constant，改用原生命令以后 GPU 一帧 2.0 ms → 1.0 ms，关 vsync 约 750 fps，开始受 CPU 限制（录制、每帧新建的 readback 缓冲、流送反馈、present）。
