# 0007 — 材质系统：对齐 UE Substrate，用文本 shader 代替节点图

状态：草案 · 2026-10-16

前置：
- [0002](0002-shader-interface.md)：shader ABI
- [0003](0003-asset-pipeline.md)：资产与 cook
- [0006](0006-scene-renderer.md)：visbuffer、wavefront

## 0. 决定摘要

| 维度 | 决定 |
|---|---|
| 总原则 | 概念和数据模型**对齐 UE**：Material、Material Instance、Dynamic Material Instance、Material Function、Material Layers、MPC、Domain、Blend Mode、Usage、Quality Switch。区别只有一个：**节点图换成 Marlin shader 文本** |
| 着色模型 | **Substrate**：Slab BSDF 加上分层算子（Vertical Layer、Horizontal Mix、Add、Weight、Select）；另有特殊 BSDF：Unlit、Hair、Eye、SingleLayerWater、VolumetricFogCloud，以及 Marlin 自加的 Toon |
| GBuffer | Substrate 式的变长 material buffer，按档位设定字节预算，超出预算时 cook 期自动参数混合并报警。低档固定为单 slab |
| PT/ReSTIR | 所有 Slab 和特殊 BSDF 都实现 `eval`、`sample`、`pdf`，所以能 lower 的材质会**自动**参与全部光照路径 |
| 不可 lower 的材质 | Toon BSDF 只消费引擎给的光照样本，同时**必须**提供 fallback slab，供光线命中时使用 |
| 执行 | visbuffer 按材质分桶，每个材质一个 resolve compute；光线命中点走 wavefront，复用同一套 resolve 代码 |
| 资产 | 材质类型 = shader 源里实现 `IMaterial` 的 struct，加一个 `Material` .masset；实例 = `MaterialInstance` .masset，通过链接继承父材质或父实例，只写覆盖的值 |
| Permutation | 只编译实际被用到的组合；运行时从不编译；开发期由 cookd 按需编译并热重载 |
| 几何阶段 | 支持 `opacity_mask`（alpha test）和 `deform`（WPO）；位移以后再做 |
| 第一版配套 | MPC、Material Layers、Dynamic Material Instance、非 Surface 域（Decal、PostProcess、LightFunction、Volume、UI） |

## 1. UE 概念对照

| UE | Marlin | 说明 |
|---|---|---|
| Material（节点图 + 属性） | `IMaterial` struct（`.mshader` 源）+ `Material` .masset（属性） | §3 |
| Material Function | shader 语言里的函数、泛型函数和模块（`import`） | 不需要专门的资产类型 |
| Custom 节点 | 不需要，本来就是代码 | — |
| Material Instance Constant | `MaterialInstance` .masset，带 `Parent` 链接 | §8.1 |
| Dynamic Material Instance | 运行时由 VM 创建的 `DynamicMaterialInstance` | §8.3 |
| Static Switch / Static Component Mask | struct 里标 `[Static]` 的字段，或者泛型参数 | 改变 permutation |
| Material Parameter Collection | `ParameterCollection` .masset + shader 端 `ParameterCollection<T>` | §8.4 |
| Material Layers / Layer Blend | 泛型 layer 参数：`IMaterialLayer` + `ILayerBlend` | §8.5 |
| Quality Switch / Feature Level Switch | `ctx.tier`（编译期常量） | 只有被引用时才会产生 permutation |
| Ray Tracing / Path Tracing Quality Switch | `ctx.path ∈ { Raster, RayTracing, PathTracing }`（编译期常量） | Toon 等材质用它在光追里给出简化版本 |
| Material Domain | `Domain { Surface, DeferredDecal, PostProcess, LightFunction, Volume, UI }` | §6 |
| Blend Mode | `Opaque, Masked, Translucent, Additive, Modulate, AlphaComposite, AlphaHoldout` | Opaque 和 Masked 走 visbuffer |
| Usage 标记（Used with Skeletal Mesh…） | `Usage = [StaticMesh, SkinnedMesh, Terrain, ParticleSprite, ParticleRibbon, ParticleMesh, …]` | 决定几何形态（§7.2） |
| Substrate 节点 | shader 库 `marlin.substrate` 的类型与函数 | §4 |
| Material Attributes 输出 | `SurfaceOutput`（Substrate 树 + 可选的 opacity 和 refraction） | §3 |
| World Position Offset | `deform()` | §6.1 |
| Pixel Depth Offset | 暂不支持（与 visbuffer 冲突） | — |
| Material Property Overrides | 实例里可以覆盖的属性白名单：TwoSided、BlendMode、OpacityMaskClip 等 | — |

## 2. 对 Marlin shader 语言的要求

材质系统依赖编译器提供以下能力。编译器另有设计文档，这里只列需求：

1. **接口、泛型、关联类型**：类似 Slang 的 `interface`、`<T: IFoo>`、`associatedtype`，并且全部在编译期单态化。材质系统**不需要**动态分发。
2. **模块与 import**，以及可见性控制。shader 库（`marlin.material`、`marlin.substrate`、`marlin.lighting`……）以模块形式提供。
3. **属性（attribute）**：`[Param(…)]`、`[Static]`、`[Material(…)]` 等，编译器把它们原样放进反射信息。
4. **反射输出**：
   - 参数布局：名字、类型、偏移、默认值、UI 元数据；
   - 静态参数；
   - 实现了哪些可选方法（`deform`、`opacity_mask`、`fallback`）；
   - Substrate 树的拓扑，也就是展平后的 closure 数量和特性位（§4.4）；
   - 每个入口用到的引擎特性，比如是否引用了 `ctx.tier`，用来推导 permutation 维度。
5. **前向自动微分**：用于在 compute resolve 中算出任意表达式的屏幕空间导数。
   - visbuffer resolve 是 compute 着色，没有硬件 quad 导数；光线命中点也没有。
   - 编译器要把 `ctx.ddx(e)`、`ctx.ddy(e)` 以及纹理采样隐含的导数，从 UV 和位置的解析导数（raster 路径），或者 ray cone（RT 路径）出发，用前向模式传播。相当于 UE Nanite 材质的 analytic derivatives，或 Slang 的 `fwd_diff`。
6. **一份源码生成多个入口**：编译器负责把同一个材质类型套进不同的引擎模板，生成各个入口（§6）：
   - 光栅 bin 的 mesh 和 pixel shader；
   - resolve 的像素版和命中版；
   - forward；
   - decal；
   - 以及其他域的入口。
7. **自动插入 `NonUniformResourceIndex`**：材质参数里的纹理下标在一个 wave 内可能不一致（同一个 bin 里混着多个实例）。
8. **为参数块生成 Rust 类型**（build.rs 用）：引擎 shader 的参数结构和材质参数块的布局都由编译器给出（0005 §2.2）。
9. **确定性**：同样的输入得到逐字节相同的输出（0003 §4.1 的 cook 键要求）。

## 3. 材质源长什么样

```text
// materials/rock.mshader
import marlin.material;
import marlin.substrate;

struct Rock : IMaterial
{
    [Param(group = "Base")]   Texture2D<float4> albedo;
    [Param(group = "Base")]   float3 tint = float3(1, 1, 1);
    [Param(group = "Surface"), Range(0, 2)] float roughness_scale = 1.0;
    [Param(group = "Surface")] Texture2D<float4> normal_map;
    [Static]                  bool use_moss = false;
    [Param(group = "Moss")]   Texture2D<float4> moss_albedo;
    [Param(group = "Moss")]   float moss_height = 0.6;

    SurfaceOutput surface(SurfaceContext ctx)
    {
        float4 a = ctx.sample(albedo, ctx.uv(0));
        float3 n = ctx.tangent_to_world(unpack_normal(ctx.sample(normal_map, ctx.uv(0))));

        Slab rock = Slab.from_metal_rough(a.rgb * tint, /*metallic*/ 0, a.a * roughness_scale);
        rock.normal = n;

        if (use_moss) {                                 // [Static] → 编译期分支
            Slab moss = Slab.from_metal_rough(ctx.sample(moss_albedo, ctx.uv(0)).rgb, 0, 0.9);
            moss.fuzz_amount = 0.5;
            float w = saturate((n.y - moss_height) * 8);
            return SurfaceOutput(horizontal_mix(rock, moss, w));
        }
        return SurfaceOutput(rock);
    }
}
```

```text
// materials/rock.masset
Material
{
    !Uuid = "…"
    !Name = "Rock"
    Shader = `source:materials/rock.mshader`
    Type = "Rock"
    Domain = Surface
    BlendMode = Opaque
    Usage = [StaticMesh, SkinnedMesh]
}

MaterialInstance
{
    !Uuid = "…"
    !Name = "RockMossy"
    Parent = <:Rock #…>
    Params = { Albedo = <textures/rock_a:RockA #…>, RoughnessScale = 0.8, UseMoss = true }
}
```

- shader 字段名用 snake_case，`.masset` 里用 PascalCase，按 0003 §1.2 的规则自动互转。
- 可选方法：
  - `float opacity_mask(MaskContext)`：Masked 混合模式需要；
  - `float3 deform(DeformContext)`：WPO，返回世界空间偏移；
  - `Slab fallback(SurfaceContext)`：Toon 必需，其他材质不需要；
  - `float opacity(SurfaceContext)`：Translucent 需要。
- `SurfaceContext` 提供：
  - UV 0–3、顶点色、世界位置和相机相对位置；
  - 几何法线和插值法线、TBN；
  - 实例自定义数据、时间；
  - `ctx.path`、`ctx.tier`；
  - 采样函数：自动选择 LOD 和导数，并自动写入流送反馈（0006 §8.1）。

## 4. Substrate 数据模型

### 4.1 Slab BSDF

参数集合与 UE Substrate 的 Slab 对齐，下面用的是 Marlin 的命名：

| 参数组 | 字段 | 阶段 |
|---|---|---|
| 基础 | `diffuse_albedo`、`f0`、`f90`、`roughness`、`anisotropy`、`normal`、`tangent` | R3 |
| 自发光 | `emissive` | R3 |
| 第二镜面（haziness） | `second_roughness`、`second_roughness_weight` | R3 |
| 绒毛（fuzz） | `fuzz_amount`、`fuzz_color`、`fuzz_roughness` | R3 |
| 次表面 / 透射 | `sss_mfp`（平均自由程）、`sss_phase_anisotropy`、`sss_profile`、`thickness`（薄模式） | R4 |
| 薄膜 | `thin_film_thickness`、`thin_film_ior` | R4 |
| 闪点（glint） | `glint_density`、`glint_uv` | 后续 |
| 镜面轮廓 | `specular_profile`（LUT 下标） | 后续 |

- 便捷构造函数：`Slab.from_metal_rough(base_color, metallic, roughness, specular = 0.5)`，按 UE 的经典约定换算出 `f0` 和 `diffuse_albedo`。
- **BSDF 实现**：
  - 漫反射：带粗糙度的能量守恒漫反射（如 EON），也可以选 Lambert；
  - 镜面：GGX，包括多次散射补偿和各向异性；
  - haziness：第二个 GGX 波瓣；
  - fuzz：Charlie sheen；
  - 薄膜：Belcour 方法。
  - 每个波瓣都提供 `eval`、`sample`（VNDF 或 cosine）、`pdf` 和 `albedo(wo)`（查预积分 LUT）。

### 4.2 特殊 BSDF

| BSDF | 用途 | 可 lower 到 PT | 阶段 |
|---|---|---|---|
| `Unlit` | 只有自发光，可带覆盖度和透射色 | 是（作为发光体） | R3 |
| `SingleLayerWater` | 水面：表面加水下介质 | 是 | R4 |
| `Toon` | 卡通着色（Marlin 扩展） | **否**，需要 fallback（§5.4） | R4 |
| `Hair` | Chiang 式的 R/TT/TRT | 是 | 后续 |
| `Eye` | 角膜和虹膜 | 是 | 后续 |
| `VolumetricFogCloud` | Volume 域的介质参数：吸收、散射、相函数 | 是（参考档） | R7 |

特殊 BSDF 不能和 Slab 混合，只能作为整个材质的唯一输出。这一点和 UE Substrate 的限制一致，也让 tile 分类保持简单。

### 4.3 算子

| 算子 | 语义 |
|---|---|
| `vertical_layer(top, bottom, thickness)` | `top` 盖在 `bottom` 上：`bottom` 的光照会乘上 `top` 的透射率和覆盖度 |
| `horizontal_mix(a, b, t)` | 在面积上按比例混合两个材质 |
| `add(a, b)` | 两者的能量直接相加，例如自发光叠加 |
| `weight(a, w)` | 缩放覆盖度 |
| `select(a, b, cond)` | `cond` 是 `[Static]` 时在编译期选择；是运行时值时等价于 `horizontal_mix(a, b, cond ? 1 : 0)` |

**树的拓扑必须在编译期确定**，也就是只能依赖 `[Static]` 参数和泛型参数。树里各个节点的参数可以是运行时值。这样每个 permutation 的 closure 数量都是固定的，cook 期就能检查预算。

### 4.4 展平

编译器把树展平成一个 closure 列表，长度不超过 N：

```
closure_i = { bsdf_params_i, weight_i (覆盖度 × 父级 mix 权重), top_transmittance_i (来自上层的颜色过滤与方向相关衰减近似) }
```

材质整体的 BSDF 是各个 closure 的加权和：

- `eval(wi, wo) = Σ weight_i · T_i(wi, wo) · f_i(wi, wo)`
- `sample(wo, u)`：先按 `weight_i · albedo_i(wo)` 的比例选一个 closure，再在这个 closure 内部采样。
- `pdf = Σ P(i) · pdf_i`，也就是 one-sample MIS。

以上是 PT 和 ReSTIR 看到的"lower 之后的 BSDF"。光栅的直接光循环则对每个 closure 分别调用 `eval` 后求和。

### 4.5 Material buffer 编码与预算

- **头部**：每像素 32 位，记录 closure 数量、特性位（aniso、fuzz、haze、SSS、thin film）、特殊 BSDF 类型和 tile 复杂度提示。
- **Simple 编码**：单个 slab，并且只用到基础参数。头部之外只需要 12 字节左右：`diffuse_albedo` RGB8、`f0` RGB8、`roughness` 8 bit、`normal` 八面体 2×12 bit 等，`f90` 默认为 1。
- **完整编码**：每个 closure 按特性位变长打包。
- **存储**：
  - 每像素的前 16 字节放在一张 RGBA32Uint 纹理里，这样 Simple 像素只需要一次纹理读；
  - 超出 16 字节的部分放在一个 overflow ByteAddressBuffer 里，按 tile 用前缀和分配空间，地址由头部和 tile 基址推算。

| 档位 | 最大 closure 数 | 每像素字节预算（示例，待测量） | 超出预算时 |
|---|---|---|---|
| 低 | 1 | 16 | 一律参数混合成单个 slab |
| 中 | 2 | 48 | 合并权重最小的两个 closure |
| 高 | 4 | 96 | 同上 |
| 参考 | 8 | 不限（PT 不经过 buffer） | — |

- **自动简化**：在 cook 期进行，根据反射出的展平结果。对超出预算的 permutation，编译器生成一个"参数混合"版本：把 closure 按权重混合参数后合并（Substrate 的 parameter blending），并在 cook 报告里报警。低档总是生成单 slab 版本。

### 4.6 单层材质的开销

设计目标是**单 slab、只用基础参数的材质，开销和传统 DefaultLit 基本持平**。靠以下几点保证：

1. **存储**：Simple 编码一共 16 字节，一次纹理读就能取完。UE 传统 GBuffer 大约是 4 个 32 位 RT 加深度，带宽在同一量级。
2. **光照**：
   - tile 分类把只含 Simple 像素的 tile 交给专门的 kernel，这个 kernel 只做单 slab 的基础 GGX 加漫反射，ALU 和 DefaultLit 等价；
   - 没有用到的特性（fuzz、haze、SSS……）根本不会编进这个 kernel，因为特性位在 tile 级是一致的。
3. **固定额外开销**：每帧一次 tile 复杂度分类，加上每种类别一次 indirect dispatch，与材质数量无关，量级是几十 μs。
4. **真正的额外开销**只出现在多 closure 像素上：要读更多字节，光照也要对每个 closure 各求一次。这部分受上面的预算限制。

R3 的验收项：同一场景分别用"全部单 slab 的 Substrate"和"DefaultLit 等价 kernel"渲染，比较 GPU 时间，记录差异，目标是 ≤ 5%。

## 5. 光照契约

### 5.1 BSDF 接口（shader 库）

```text
interface IBsdf
{
    float3 eval(float3 wi, float3 wo);                  // 不含 cos，调用方乘
    BsdfSample sample(float3 wo, float3 u);             // { wi, weight = f·cos/pdf, pdf, lobe }
    float  pdf(float3 wi, float3 wo);
    float3 albedo(float3 wo);                           // 方向反照率估计（降噪解调、RIS 目标函数）
    LobeMask lobes();
}
```

- `SubstrateBsdf`（§4.4 的加权和）、`SingleLayerWaterBsdf` 等都实现这个接口。
- 光照代码只认 `IBsdf`，从来不知道具体是哪个材质。光栅直接光、ReSTIR DI 的目标函数和最终着色、ReSTIR GI 的采样、PT、探针更新时的命中点，用的都是同一个接口。
- **能量守恒测试**：每个 BSDF 都在 shader 测试里跑 white furnace 测试（0004 §6）。

### 5.2 光栅路径

- **直接光**：0006 §7.2 和 §7.3 在 tile kernel 里，从 material buffer 解码出 `SubstrateBsdf`，然后对每个光源样本求 `eval`。
- **间接光**：探针给出辐照度。对漫反射波瓣用 `albedo` 调制；对镜面波瓣，用 split-sum 近似，或者 SSR、RT 的结果。

### 5.3 RT 与 PT 路径

- 光线命中后先经过 wavefront resolve，得到同样编码的 `HitSurface`，解码出 `SubstrateBsdf`，再做 `sample`、`eval`、`pdf`。
- `ctx.path` 让材质在光追里换一种实现（相当于 UE 的 RayTracing/PathTracing Quality Switch）。比如复杂的视差效果在 RT 里就可以退化掉。

### 5.4 不可 lower 的材质：Toon

Toon 是一个特殊 BSDF，不实现 `IBsdf`，而是实现 `ICustomLighting`：

```text
interface ICustomLighting
{
    float3 shade(CustomLightingContext ctx);   // ctx: 位置、法线、视线、材质参数
}
struct CustomLightingContext
{
    LightSampleIter lights;   // 直接光样本：{ dir, radiance, visibility }
    float3 irradiance_sh[…];  // 间接光：探针（低/中）或 SHaRC 查询（高）的辐照度
    float  ao;
}
```

- **直接光样本**：低档和中档来自 clustered 光源列表加 VSM；高档来自 ReSTIR DI 最终选中的样本（每像素 1 到 k 个，已经带可见性）。Toon 的 `shade` 自己决定怎么做阶梯化、边缘光等卡通效果。
- **间接光**：Toon **不**接收 ReSTIR GI 和 PT 的间接光，只接收探针或 SHaRC 的辐照度。所以它在各档之间的观感是稳定的。
- **fallback**：Toon 材质**必须**实现 `Slab fallback(ctx)`。如果没写，编译器会根据 Toon 的基础色自动生成一个 Lambert slab，并给出提示。次级光线命中 Toon 表面时，用 fallback 来计算它反射和透射的光。
- **tile 分类**：Toon 像素属于 Special 类，由一个专门的 kernel 处理，这个 kernel 调用该材质的 `shade`。因为它是材质自己的代码，Special 类 tile 内部还要按材质再分一次桶。

## 6. 执行：每个域和每种入口

| 入口 | 形态 | 生成条件 |
|---|---|---|
| `RasterBin` | mesh shader（带 `deform`）+ 可选 pixel shader（带 `opacity_mask`）；写 visbuffer 或 VSM 深度 | 材质实现了 `deform` 或 `opacity_mask` |
| `ResolvePixel` | compute；输入是 visbuffer 像素列表，输出写 material buffer、Velocity、Emissive | Opaque 和 Masked 的 Surface 材质 |
| `ResolveHit` | compute；输入是 HitRecord 列表，输出 HitSurface | 同上，并且场景开启了 RT |
| `Forward` | mesh shader + pixel shader；在 pixel shader 里调用 surface 并做前向光照 | Translucent、Additive、Modulate、AlphaComposite 等 |
| `DecalApply` | compute，作用于被 decal 覆盖的 tile | DeferredDecal 域 |
| `PostProcess` | compute | PostProcess 域 |
| `LightFunction` | compute，写入 light function 图集 | LightFunction 域 |
| `VolumeInject` | compute，把介质写进 froxel 或局部体积 | Volume 域 |
| `UiFill` | 编进 UI tile shader 的填充函数 | UI 域（0008 §4.5） |
| `CustomShade` | compute，处理 Special 类 tile | Toon |
| `RvtCompose` | compute，把分层材质的混合结果写进 RVT 物理页（0009 §4.4） | Usage 含 `Terrain` 的材质 |

### 6.1 光栅 bin

- 没有 `deform` 和 `opacity_mask` 的材质共用一条快速路径的 pipeline。
- 带 `deform` 的材质：mesh shader 调用 `deform`。`DeformContext` 只能访问顶点级的数据：位置、法线、UV、顶点色、实例数据、时间、MPC。
- 带 `opacity_mask` 的材质：pixel shader 调用 `opacity_mask`，结果小于阈值就 discard。
- VSM 渲染复用同一套 bin。

### 6.2 Resolve

- **像素版**：
  1. 从 visbuffer 解码出 (cluster, triangle)，读取三个顶点。
  2. 用屏幕位置求重心坐标，以及重心坐标的解析导数。
  3. 插值得到属性和属性的导数，构造 `SurfaceContext`。
  4. 调用 `surface()`，展平 Substrate 树，编码写出。
- **命中版**：重心坐标由 HitRecord 直接给出，导数来自 ray cone，其他步骤相同。
- 两个版本共用同一份 `surface()` 代码。区别只在编译期选择的输入和输出适配器。

### 6.3 分桶与 dispatch

1. **分类**：每个像素读出它的 `material_type`。这个值由 instance → material slot → material instance → type 一路查出来，查表在 cull 阶段的 cluster 记录里预先缓存。然后对每个 bin 做原子计数。
2. **前缀和**，然后 **scatter**：像素坐标按 8×8 tile 的顺序写进各个 bin 的像素列表。bin 内部再按实例粗略排序，这样可以减少非一致的描述符访问在 AMD 上触发的 waterfall 循环。
3. **dispatch**：CPU 为"GPU Scene 里所有驻留的材质类型"各录一次 `dispatch_indirect`，空 bin 的组数为 0。
   - 已知的成本：驻留材质类型很多时（上千个 permutation），空 dispatch 的前端开销会累积起来。UE Nanite 有同样的问题。
   - **缓解手段**：
     - 鼓励用"少量母材质 + 大量实例"的工作方式，因为实例不会产生新的 bin；
     - cook 报告里给出 permutation 统计；
     - 以后用 D3D12 work graphs 或 `VK_EXT_device_generated_commands`（可以在 GPU 上切换 pipeline）去掉 CPU 侧的全量录制（0004 §8）。

### 6.4 Forward（半透明）

- pixel shader 调用 `surface()` 得到 Substrate 树。展平时用 forward 预算：中档 2 个、高档 4 个 closure。
- 然后调用 `marlin.lighting.forward` 库，输入是 clustered 光源、VSM、探针或 SHaRC、雾。
- 混合模式由固定功能 blend state 实现。

### 6.5 Decal

- decal 是 DeferredDecal 域的材质，输出一个 slab 的参数，外加覆盖度。
- 处理时机：在 resolve 之后、光照之前。对每个 decal 材质 bin（clustered decal 列表里覆盖到的 tile），读出像素当前的**顶层 slab**，和 decal 的 slab 做参数混合，然后写回。这与 Substrate decal 的语义一致。
- 多 closure 像素只修改顶层 closure，Simple 像素保持 Simple 编码。

## 7. Permutation

### 7.1 维度

permutation 的键由以下几部分组成：

- 材质类型
- `[Static]` 参数的值
- 泛型参数，包括 layer 类型
- 入口种类
- 几何形态
- 档位变体（只有材质引用了 `ctx.tier`，或者入口模板本身随档位变化时才算）
- 调试变体（只在 Development 构建里有）

### 7.2 几何形态

几何形态相当于 UE 的 vertex factory，但种类少得多，因为：
- 蒙皮已经在 compute 里完成（0006 §3.4），蒙皮网格和静态网格的形态相同；
- 植被实例就是普通的 GPU Scene 实例。

| 形态 | 说明 |
|---|---|
| `Mesh` | 静态网格和蒙皮网格（读 position 流和 attribute 流） |
| `Terrain` | 地形 cell 网格，多一套 RVT 坐标和层权重（0009 §4） |
| `ParticleSprite`、`ParticleRibbon`、`ParticleMesh` | 读粒子 buffer（0009 §7） |

材质的 `Usage` 声明它支持哪些形态。cook 时如果发现材质被用在没有声明的形态上，就报错（UE 的做法是自动补上，我们选择显式报错，避免 permutation 悄悄增长）。

### 7.3 收集与编译

1. **收集**：`cook_shader` 通过资产索引（0003 §2）找到构建闭包里所有的 `MaterialInstance`，以及引用它们的网格、地形和粒子，从而推出每个材质实际用到的 (静态参数, 形态, 入口) 组合，再乘以平台支持的档位。
2. **预算**：每个材质设有 permutation 数量预算，超出时报警；整体统计写进 cook 报告。
3. **编译**：调用编译器库，同一个材质的不同 permutation 并行编译。结果按 cook 键缓存进 DDC（0003 §5）。
4. **运行时**：
   - 加载材质单元时，在 job 里批量创建 pipeline（接入 pipeline cache）。
   - 找不到 pipeline 时：Shipping 构建用引擎默认材质，并报一次错误；Development 构建向 cookd 请求编译，在完成之前先用默认材质。
5. **热重载**：`.mshader` 改动后，cookd 只重新编译受影响的 permutation，并推送失效通知。运行时在帧边界替换 pipeline，bin ID 保持不变。

## 8. 资产与运行时

### 8.1 Material 与 MaterialInstance

- `Material`（.masset）包含：
  - shader 源链接和类型名；
  - Domain、BlendMode、TwoSided、OpacityMaskClip、Usage；
  - 排序优先级（半透明）；
  - RT 标记（§9）；
  - 绑定的 MPC（§8.4）。
- `MaterialInstance`（.masset）包含：
  - `Parent`：链接到一个 `Material` 或另一个 `MaterialInstance`；
  - `Params`：只写需要覆盖的值；
  - `Overrides`：属性白名单内的覆盖；
  - 静态参数的覆盖（会导致 permutation 变化）。
- **cook 时把实例链展平**：沿着 Parent 一路合并覆盖，得到最终的 `(permutation 键, 参数块字节, 纹理 import)`。运行时不存在实例链，只有展平后的结果。编辑器里修改父实例时，cookd 会重新 cook 所有依赖它的实例：父实例是一个构建依赖（0003 §4.1）。

### 8.2 参数块与 GPU 表

- 参数块布局来自反射：
  - 标量和向量按 16 字节对齐打包；
  - `Texture*` 存 u32 bindless 下标；
  - `SamplerState` 存 u32 采样器表下标（cook 期去重）；
  - MPC 引用存 u32 下标。
- 运行时有一个大的 `material_params` ByteAddressBuffer。每个材质实例在里面占一个槽，GPU Scene 的 `material_instances` 表记录 `{ type/bin, param_offset, flags }`。
- 纹理下标加载时还没有确定：cooked 参数块里存的是 import 下标，加载时等纹理句柄解析完，再改写成 bindless 下标（0003 §4.3 的 import 修正）。流送期间 bindless 下标保持不变（0006 §8.1）。

### 8.3 Dynamic Material Instance

- VM API：`DynamicMaterialInstance.create(parent)`，`set_scalar`、`set_vector`、`set_texture`，以及把它赋给实体的某个材质槽。
- 实现方式：模拟侧预留一个材质实例槽，ID 规则和 proxy 相同（0006 §1.2），复制父实例的参数块。之后的写入以 `material_patches` 的形式进入 `RenderDelta`，渲染侧把这些补丁 scatter 到 GPU。
- 动态实例不能修改静态参数，因为那会改变 permutation。这一点和 UE 一致。

### 8.4 Material Parameter Collection

- `ParameterCollection`（.masset）声明一组标量和向量参数的名字与默认值。
- shader 侧这样声明：`struct Weather { float wetness; float4 wind; }`，材质字段写 `ParameterCollection<Weather> weather;`。`Material` .masset 用 `Collections = { Weather = <env/weather:Weather #…> }` 把它绑定到具体的集合资产。cook 时会校验集合资产的布局和 shader 结构体一致。
- 运行时每个集合是一个持久的 uniform buffer，它的 bindless 下标在加载时写进参数块。VM 调用 `collection.set("wetness", v)` 产生补丁，下一帧生效。
- 每个材质建议最多引用 4 个集合（UE 的限制是 2 个）。超出时报警，但不是硬限制。

### 8.5 Material Layers

用泛型来表达：

```text
interface IMaterialLayer { Slab layer(SurfaceContext ctx); }
interface ILayerBlend    { float weight(SurfaceContext ctx, int index); }

struct Layered<L0: IMaterialLayer, L1: IMaterialLayer, L2: IMaterialLayer, B: ILayerBlend> : IMaterial
{
    [Param] L0 base; [Param] L1 layer1; [Param] L2 layer2; [Param] B blend;
    SurfaceOutput surface(SurfaceContext ctx) {
        let s = horizontal_mix(base.layer(ctx), layer1.layer(ctx), blend.weight(ctx, 1));
        return SurfaceOutput(horizontal_mix(s, layer2.layer(ctx), blend.weight(ctx, 2)));
    }
}
```

- 实例通过泛型实参选择每一层的类型，这个选择是静态的，会产生 permutation；各层的参数则是普通的实例参数。`.masset` 的写法是 `Layers = { Layer1 = MossLayer { Height = 0.6 } }`。
- 地形材质就是这样一个分层材质（0009 §4.4）。
- 也可以用 `vertical_layer` 代替 `horizontal_mix`，得到真正的物理分层，比如湿润涂层、清漆。

## 9. 光追相关的材质标记

| 标记 | 作用 |
|---|---|
| `VisibleInRayTracing`（默认开） | 关闭后，实例不进入 TLAS |
| `RtIgnoreDeform` | BLAS 不做 refit，使用静态形状（植被默认开） |
| `RtOpacity` | alpha 测试在 RT 中的表示：`Auto` / `Opaque` / `TextureMask`（0009 §5.3） |
| `EmissiveAsLight` | 发光三角形进入 ReSTIR 的光源表（0006 §7.1） |
| `RtReflections`（只对半透明有效） | 高档的前向半透明是否发反射光线 |

关于 `RtOpacity = Auto`：编译器会分析 `opacity_mask`。如果它可以归约成"纹理通道 × 常量与阈值比较"的形式，就提取出 (纹理, UV 通道, 通道, 阈值, 缩放)，交给通用的 RT alpha 测试使用；如果不能归约，就退化成 Opaque，并给出警告。

## 10. Cook 产物

- **材质类型单元**：
  - 反射信息：参数布局、默认参数块、静态参数、展平拓扑与预算结果、入口列表；
  - bulk：各 permutation 的字节码。
- **材质实例单元**：
  - 展平后的参数块；
  - 纹理和 MPC 的 import；
  - permutation 键（指向类型单元里的某个 pipeline）；
  - RT alpha 提取结果。
- **引擎 shader 库**：render pass 自己的 shader（剔除、光照、后处理……）由同一个编译器编译，打成一个"引擎 shader 库"单元。它的 permutation 维度由 pass 代码静态声明。

## 11. 测试

- **编译器**：确定性测试（同一输入两次编译逐字节一致）；反射测试（参数布局、静态参数、可选方法检测）。
- **展平**：给定树的拓扑，断言 closure 数量、权重表达式、预算内外的简化结果。这是 CPU 测试，展平算法在 Rust 侧有一份参考实现。
- **BSDF**：
  - 每个 BSDF 跑 white furnace 测试（只做漫反射或只做镜面时，反照率 ≤ 1）；
  - `sample` 与 `pdf` 一致性的卡方检验；
  - `eval` 与 `sample` 的权重一致性。
- **光栅与 PT 一致性**：同一个材质、同一个光源，光栅直接光与参考 PT 的直接光部分做 FLIP 对比。
- **实例链**：多级覆盖展平的结果；修改父实例后，子实例被正确地重新 cook。
- **性能**：§4.6 的单 slab 与 DefaultLit 对比基准。

## 12. 待定问题

1. Toon 这类 Special 类材质在高档能拿到 ReSTIR DI 的几个样本（1 个还是 k 个），会影响画面稳定性。需要美术评估。
2. `ctx.ddx` 的自动微分范围：对循环和动态分支怎么处理，要和编译器设计一起确定。
3. 地形这类层数很多的分层材质，在中档预算下的简化质量。如果不够，就让地形强制走 RVT 合成（0009 §4.4），绕开 resolve 预算。
4. 是否支持"材质内编译期动态分发"，例如一个材质 bin 内按实例切换算法。目前不支持，都通过 permutation 处理。
