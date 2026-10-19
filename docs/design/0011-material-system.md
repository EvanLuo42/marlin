# 0011 — 材质系统：Substrate、开放的 BSDF、材质缓冲、实例与 cook

状态：草案 · 2026-10-16

前置：
- [0004](0004-shader-language.md)：§8 泛型、关联类型、扩展；§13 编译期；§15.3 隐式导数
- [0005](0005-shader-compiler.md)：§6 反射与 `[Export]`
- [0009](0009-scene-and-geometry.md)：visbuffer、材质分箱、GPU Scene
- [0010](0010-lighting.md)：ReSTIR 的 target、最终着色、降噪

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 编写方式 | **只用代码**：材质就是实现了 `ISurfaceMaterial` 的 Marlin shader struct，没有节点图 | 2 |
| 参数 | struct 的普通字段就是参数块，`const` 字段是静态参数（0004 §13.3）；另有每实例数据和全局参数集 | 2.3、7 |
| 模型 | 完整的 Substrate：Slab（含 haze、fuzz、SSS、薄膜、glint、specular profile……）和四种算子 Horizontal、Vertical、Add、Weight；另有 Hair、Eye、SingleLayerWater、Unlit 这几个专用 BSDF | 3 |
| 拓扑 | **类型即拓扑**：用泛型组合类型表达树，关联类型 `Tree` 就是拓扑；`Flatten` 是 const 函数，在编译期从类型算出拓扑 | 3.4 |
| 扩展 | **开放的 `IBsdf` 接口**：Slab 只是其中一个实现。新 BSDF（例如 Toon）写在任何 shader 包里，引擎核心不用改 | 5 |
| 分发 | 语言里没有动态分发（0004 §8.5）。cook 把 closure 类型收成一个闭集：分配 id，生成"按类型特化"和"全类型 switch"两类分发 struct | 5.2–5.4 |
| 光照接口 | ReSTIR 的重采样只用 proxy lobe，不分发；只有最终着色、合成和波前式命中才按 closure 类型分发 | 5.5 |
| 风格化 | 每个 BSDF 可以覆盖 `Compose`，对降噪后的总光照做非线性处理（Toon 的阶梯化）；默认实现是普通的反照率 × 辐照度 | 5.6 |
| 材质缓冲 | 稠密部分每像素 16 B（header + proxy lobe），复杂像素从一个溢出池里按 tile 分配 closure 数据 | 4 |
| 预算 | 每个平台有每像素字节数和 closure 数上限；超出时 **cook 按固定规则自动简化**；运行时再按像素丢掉权重为 0 的 closure | 4.3、8.3 |
| 实例 | 四层：材质类型 → 材质实例资产（可继承，可覆盖静态参数）→ 每实例自定义数据 → 全局参数集 | 7 |
| 域 | Surface（不透明、masked、半透明）、Decal、PostProcess（含显示变换）、LightFunction、Volume、UI 效果 | 2.1、10 |
| 次级命中 | 简化材质图集（运行时烘焙）；波前式完整材质给 L4、高档镜面和参考路径追踪器 | 9 |
| 编译 | game 只用 cook 好的 permutation；editor 通过 cook 服务按需编译，编译完成之前用默认材质 | 8 |

> **命名说明**：本文代码里的接口名、attribute 名（`ISurfaceMaterial`、`[PerInstance]`、`[Global]`……）都是草稿。按 shader 语言的约定，它们属于语言和标准库的命名，要单独确认后再定（§13）。

## 1. 概念

| 层 | 是什么 | 在哪里 |
|---|---|---|
| 材质类型 | 一个实现了某个材质域接口的 struct | `.mshader` |
| 材质实例 | 参数值，加上可选的静态参数覆盖；可以继承另一个实例 | `.masset` |
| 每实例数据 | 某个物体上的 N 个 float（受击闪白、染色……） | GPU Scene（0009 §2） |
| 全局参数集 | 天气、湿度、时间这类全局量 | `.masset`，每帧上传一份 |
| closure 类型 | 一个实现了 `IBsdf` 的 struct | `.mshader` |
| closure 注册表 | 工程里所有 closure 类型和它们的 id，只追加 | `.masset`，cook 维护 |

## 2. 材质类型与域

### 2.1 域

| 域 | 接口（草稿名） | 入口 | § |
|---|---|---|---|
| Surface | `ISurfaceMaterial<TSelf>` | `ResolvePixel<M>`、光栅变体、`ForwardTranslucent<M>`、`BakeSimple<M>`、`ResolveHit<M>` | 2.2、6 |
| Decal | `IDecalMaterial<TSelf>` | `ProjectDecal<M>` | 10.1 |
| PostProcess | `IPostProcessMaterial<TSelf>`、`IDisplayTransform` | `PostProcess<M>` | 10.2 |
| LightFunction | `ILightFunction<TSelf>` | `BakeLightFunction<M>` | 10.3 |
| Volume | `IVolumeMaterial<TSelf>` | `InjectVolume<M>` | 10.4 |
| UI 效果 | `IUiEffect<TSelf>` | UIRenderer 着色里的分发 | 10.5 |

### 2.2 Surface

```csharp
public interface ISurfaceMaterial<TSelf> where TSelf : ISurfaceMaterial<TSelf>
{
    type Tree : ISubstrateNode<Tree>;              // 拓扑（§3.4）
    Tree Surface(in SurfaceContext ctx);

    // 可选方法（默认实现表示"没有"；反射会列出实现了哪些，0005 §6.1）
    float Opacity(in SurfaceContext ctx) => 1;     // masked 和半透明用
    float3 Deform(in DeformContext ctx) => 0;      // 顶点偏移（WPO、风）

    const BlendMode Blend = BlendMode.Opaque;      // Opaque / Masked / Translucent / TranslucentOit
    const float MaskThreshold = 0.5;
    const float MaxDisplacement = 0;               // Deform 的最大位移，剔除时放大包围盒（0009 §4.4）
    const SimpleBake Bake = SimpleBake.Uv0;        // 简化材质怎么烘（§9.2）
}
```

- 实现了 `Opacity` 并且 `Blend == Masked` 的材质，或者实现了 `Deform` 的材质，会有自己的光栅 permutation（0009 §4.4）。
- `Blend` 是静态参数，所以同一个材质类型的不同实例可以分别选择不透明或半透明，代价是不同的 permutation。
- 2026-10-19（R4e）：`Opacity` 和 `Blend` 已经加到 `ISurfaceMaterial`（`content/engine/substrate/Substrate.mshader`，`BlendMode` 有 `Opaque`、`Masked`、`Translucent`、`TranslucentOit`），用户确认采用这种接口成员的写法。简化材质的烘焙在 `Blend` 不是 `Opaque` 时把 `Opacity` 写进图集（`SimpleMaterial.Opacity`）。`Deform`、`MaskThreshold`、`MaxDisplacement`、`Bake` 还没有加。

### 2.3 一个材质

```csharp
using Marlin.Substrate;

public struct Rock : ISurfaceMaterial<Rock>
{
    [Group("Base")] public Texture2D<float4> BaseColor;
    [Group("Base")] public Texture2D<float4> NormalMap;
    [Group("Base")] public Sampler Sampler;
    [Range(0, 2)]   public float RoughnessScale = 1;

    [Group("Moss")] public Texture2D<float4> MossColor;
    [Range(0, 1)]   public float MossAmount = 0.5;
    [Global("Weather.Wetness")] public float Wetness;     // 来自全局参数集
    [PerInstance(0)]            public float Tint;        // 来自每实例数据

    public const bool UseMoss = true;                     // 静态参数

    public type Tree = Horizontal<Slab, Slab>;

    public Tree Surface(in SurfaceContext ctx)
    {
        var c = ctx.Sample(BaseColor, Sampler, ctx.Uv0);
        var n = ctx.SampleNormal(NormalMap, Sampler, ctx.Uv0);
        var rock = Slab.Default with
        {
            DiffuseAlbedo = c.rgb * Lerp(1, 0.6, Wetness),
            Roughness = Saturate(c.a * RoughnessScale * Lerp(1, 0.3, Wetness)),
            Normal = n,
        };
        var moss = Slab.Default with { DiffuseAlbedo = ctx.Sample(MossColor, Sampler, ctx.Uv0).rgb, Roughness = 0.9 };
        var mask = UseMoss ? Saturate(ctx.WorldNormal.y * MossAmount * 2) : 0;
        return new() { A = rock, B = moss, Mix = mask };
    }
}
```

- **struct 本身就是参数块**：普通字段按 scalar 布局（0004 §16）存进材质表（§7.5）；资源字段是 32 位 bindless 下标。这里取代 0004 §8.2 示例里的 `type Params` 写法，那个写法只是示例。
- 字段上的 `[Group]`、`[Range]` 由反射原样输出，editor 的属性面板用它们（0005 §6.1）。
- 采样要通过 `ctx.Sample`：它在调用纹理的 `Sample` 之外，还会顺带写采样反馈（0009 §6.2）。这符合 0004 §17 的约定：采样反馈属于材质库，不属于语法。
- 静态开关不改变拓扑类型：`UseMoss = false` 时 `Mix` 折叠成常量 0，运行时这一路 closure 被丢掉（§4.4），但编译期的拓扑仍然按两个 closure 保守计算（§13）。

## 3. Substrate 模型

### 3.1 Slab

| 字段 | 含义 | 特性位 |
|---|---|---|
| `DiffuseAlbedo` | 漫反射反照率 | — |
| `F0`、`F90` | 镜面的法向和掠射反射率；金属就是高 F0、零漫反射 | — |
| `Roughness`、`Anisotropy`、`Tangent` | GGX 粗糙度和各向异性 | `Aniso` |
| `Normal` | 着色法线 | — |
| `SecondRoughness`、`SecondWeight` | 第二镜面瓣（haze） | `Haze` |
| `FuzzAmount`、`FuzzColor`、`FuzzRoughness` | 布料的绒毛层 | `Fuzz` |
| `SssMfp`、`SssMfpScale`、`SssPhaseG`、`SssProfile` | 次表面散射（平均自由程，或者皮肤 profile） | `Sss` |
| `ThinFilmThickness`、`ThinFilmIor` | 薄膜干涉 | `ThinFilm` |
| `Glint`、`GlintUv` | 闪点（车漆、雪） | `Glint` |
| `SpecularProfile` | 用 LUT 自定义镜面颜色随角度的变化 | `SpecProfile` |
| `Emissive` | 自发光（cd/m²） | `Emissive` |
| `Thickness`、`Transmission` | 作为上层时的厚度和吸收；薄面透射 | `Transmission` |

- 字段都有默认值，`Slab.Default with { ... }` 只写需要的部分。
- 特性位是编译期的：`Flatten` 通过常量传播看出哪些字段被设成了非默认值，只为用到的特性分配 payload 和生成光照代码（§3.4）。

### 3.2 专用 BSDF

| BSDF | 用途 |
|---|---|
| `Hair` | Marschner 加双散射近似；strand 和 card 都用它（0013 §7） |
| `Eye` | 角膜和虹膜的两层，带焦散近似 |
| `SingleLayerWater` | 水面加水下体积的吸收和散射（0013 §6） |
| `Unlit` | 只有自发光 |

它们和 Slab 一样，都只是 `IBsdf` 的实现（§5），放在引擎的 `Marlin.Substrate` 包里。

### 3.3 算子

| 类型 | 字段 | 含义 |
|---|---|---|
| `Horizontal<A, B>` | `A`、`B`、`Mix` | 按权重横向混合：`(1 − Mix)·A + Mix·B` |
| `Vertical<Top, Bottom>` | `Top`、`Bottom`、`Coverage` | 垂直分层：`Top` 盖在 `Bottom` 上；`Bottom` 的贡献乘上穿过 `Top` 的透射 |
| `Add<A, B>` | `A`、`B` | 相加，不做能量守恒 |
| `Weight<A>` | `A`、`W` | 整体乘一个权重（RGB） |

例：车漆是 `Vertical<Slab, Slab>`（清漆盖在底漆上），雨后的布料是 `Vertical<Slab, Horizontal<Slab, Slab>>`。

### 3.4 展平

```csharp
public interface ISubstrateNode<TSelf> where TSelf : ISubstrateNode<TSelf>
{
    static abstract ClosureInfo Info();                         // 编译期：拓扑（必须能在 CTFE 中求值）
    void Emit(ref ClosureWriter w, float3 weight, in EmitContext e);   // 运行时：写出 closure
    float3 Transmittance(float3 wo);                            // 作为 Vertical 的上层时
    Slab ToSlab(float3 wo);                                     // 预算不够时合并成一个 Slab（§8.3）
}

public struct Vertical<TTop, TBottom> : ISubstrateNode<Vertical<TTop, TBottom>>
    where TTop : ISubstrateNode<TTop> where TBottom : ISubstrateNode<TBottom>
{
    public TTop Top;
    public TBottom Bottom;
    public float Coverage = 1;

    public static ClosureInfo Info() => ClosureInfo.Concat(TTop.Info(), TBottom.Info());

    public void Emit(ref ClosureWriter w, float3 weight, in EmitContext e)
    {
        Top.Emit(ref w, weight * Coverage, e);
        var through = Lerp(float3(1), Top.Transmittance(e.Wo), Coverage);
        Bottom.Emit(ref w, weight * through, e);
    }
    // Transmittance、ToSlab 略
}
```

- **编译期**：`Substrate.Flatten<T>()` 是一个 `const` 函数，沿类型递归（编译期递归是允许的，0004 §6），得到 `ClosureInfo`：closure 数、每个 closure 的类型、特性位、payload 字节数。入口模板用 `[Export] const ClosureInfo Topology = Flatten<M.Tree>();` 把它写进反射（0005 §6.2）。
- **运行时**：`Emit` 沿同一棵树递归，算出每个叶子的 RGB 权重：
  - Horizontal 按 `Mix` 分配；
  - Vertical 让下层乘上上层的透射。透射在视线方向 `wo` 上计算，对所有光照方向都沿用这个值，和参考实现的近似一样；
  - Weight 相乘；Add 不变。
- 所有泛型在编译期单态化，`Emit` 最后会被完全内联展开，没有递归调用。
- 依赖的语言能力：接口里的 `static abstract` 方法要能在 CTFE 中求值（`TTop.Info()`）。0004 §13.4 规定 `const` 函数只能调用 `const` 函数；"接口方法的实现必须是 const"这一点要在语言里确认（§13）。

### 3.5 拓扑导出

```
exports["Topology"] = {
    Count = 2,
    Types = [Slab, Slab],
    Features = Aniso | Haze,
    PayloadBytes = 36,
    HasEmissive = false,
}
```

host 端用它决定：
- 材质缓冲的预算是否超出（§8.3）；
- 实例是否进入自发光三角形表（0010 §5.2）；
- 这个材质需要哪些 closure 类型的光照 bucket（§5.4）。

## 4. 材质缓冲

### 4.1 布局

| 部分 | 大小 | 内容 |
|---|---|---|
| 稠密：header | 4 B / 像素 | closure 数（3 位）、快路径标志、自发光、SSS、法线表项数、溢出池偏移（单位 16 B，20 位） |
| 稠密：proxy lobe | 12 B / 像素 | 法线（八面体 2×16 位）；漫反射反照率 RGB8 + 粗糙度 8 位；F0 RGB8 + proxy 种类 8 位 |
| 溢出：法线表 | 0–8 B | 最多 2 个额外的法线或切线（八面体） |
| 溢出：closure | 变长 | 每个 closure 一个 4 B 的头（类型 id 5 位、法线表下标 2 位、特性位）+ 4 B 权重（R11G11B10）+ payload |

- **快路径**：像素只有一个默认的 Slab（没有任何特性位），并且 F90 取默认值时，proxy lobe 就是它的全部数据，不占溢出池。场景里大部分像素走这条路，开销和传统 GBuffer 相当。
- **溢出池**：复杂像素的 closure 数据从一个池里分配。分配按 8×8 tile 做：tile 内先用 wave 前缀和算出总量，一次原子加分配一段连续空间，像素的偏移写进 header。这样同一个 tile 的数据在内存里是挨着的。
- 池满了：像素退化成只有 proxy lobe（画面变简单，但不会出错），同时记一个计数，editor 里会提示。
- 稠密部分是两张 `R32G32B32A32_UINT` 纹理；溢出池是一个 `ByteBuffer`。它们都是瞬态资源，可以和别的 pass 别名（0007 §4.5）。

### 4.2 proxy lobe

- 每个 closure 都必须提供一个 proxy（§5.1）：一个漫反射加一个 GGX 瓣。
- 像素的 proxy 是所有 closure 的 proxy 按权重合成的：反照率和 F0 加权求和，粗糙度按镜面亮度加权，法线取权重最大的那个 closure。
- proxy 的用途：
  - ReSTIR 的 target 和 BSDF 采样（0010 §2）；
  - 降噪的解调和引导；
  - 镜面追踪按粗糙度 τ 划分；
  - 空间复用的邻居拒绝。

### 4.3 预算

| 平台 / 预设 | 溢出每像素上限 | closure 上限 | 溢出池容量（按屏幕像素数的比例） |
|---|---|---|---|
| NS2 | 16 B | 2 | 25% |
| XSS、PC Low | 24 B | 2 | 25% |
| PS5/XSX、PC Medium | 48 B | 4 | 50% |
| PS5 Pro、PC High/Ultra | 64 B | 6 | 50% |
| Cinematic | 112 B | 8 | 100% |

- 预算是 target 描述里的编译期常量，`Flatten` 用它决定是否简化（§8.3）。
- 1080p 下，稠密部分 33 MB，PS5 的溢出池 50 MB。数值是起点，实测后再调。

### 4.4 tile 分类

材质解算之后，一个 compute pass 对每个 8×8 tile：
- 把 tile 内所有像素的 closure 类型掩码求 OR（u32，所以 closure 类型最多 32 种，§5.8）；
- 判断是否全部走快路径；
- 按结果把 tile 分进一个 bucket，写 tile 列表和 indirect 参数（§5.4）。

运行时，权重小于阈值的 closure 不写入（`ClosureWriter.Write` 检查），所以像素实际的 closure 数可以少于拓扑的上限。

## 5. 开放的 BSDF 扩展

这一节说明为什么不用改引擎就能加一个 Toon BSDF，以及 host 和 device 两边具体怎么走通。

### 5.1 `IBsdf`

```csharp
public interface IClosureId { static abstract uint ClosureId { get; } }    // 由 cook 生成，见 §5.2

public interface IBsdf<TSelf> : ISubstrateNode<TSelf>
    where TSelf : IBsdf<TSelf>, IClosureId
{
    type Packed;                                            // 材质缓冲里的格式；大小编译期已知
    static abstract Packed Pack(in TSelf b, ref NormalTable n);
    static abstract TSelf Unpack(in Packed p, in NormalTable n);

    float3 Eval(float3 wo, float3 wi);                      // f · cosθ，方向是世界空间
    BsdfSample Sample(float3 wo, float3 u);
    float Pdf(float3 wo, float3 wi);
    ProxyLobe Proxy(float3 wo);                             // 必须提供

    // 有默认实现的部分
    float3 Transmittance(float3 wo) => 0;                   // 默认不透光
    SimpleMaterial Simplify(float3 wo) => SimpleMaterial.FromProxy(Proxy(wo));
    float3 Compose(in ComposeInput i) => i.Diffuse * i.DiffuseAlbedo + i.Specular * i.SpecularAlbedo;
    void ApplyDecal(in DecalSample d) { }                   // 贴花，§10.1
    Slab ToSlab(float3 wo) => Slab.FromProxy(Proxy(wo));    // 预算不够时
    static virtual ClosureFeatures Features => ClosureFeatures.None;   // 需要引擎的哪些额外 pass，§5.7

    // 作为 Substrate 树的叶子
    static ClosureInfo ISubstrateNode<TSelf>.Info() => ClosureInfo.Leaf<TSelf>();
    void ISubstrateNode<TSelf>.Emit(ref ClosureWriter w, float3 weight, in EmitContext e) => w.Write(this, weight, e);
}
```

- 一个 BSDF 只要实现这些方法，就能作为 Substrate 树的叶子，和 Slab 放在同一棵树里（比如 `Vertical<Slab, ToonBsdf>`：Toon 的底色上盖一层清漆）。
- 这里用到的都是已有的语言特性：`static abstract`、`static virtual`、默认接口方法、关联类型（0004 §8.2–8.3）。

### 5.2 closure 类型 id

问题在于，材质解算要把"这是哪种 closure"写进材质缓冲，光照再按 id 解码，所以每种 closure 需要一个**稠密、稳定**的 id。

- 工程里有一个 **closure 注册表** 资产（`ClosureRegistry.masset`），记录完整类型名到 id 的映射，**只追加不重排**。
- cook 编译材质之前，先从所有材质的 `Tree` 类型里收集叶子类型。遇到注册表里没有的类型，就追加一个新 id（editor 里由 cook 服务自动完成）。
- cook 根据注册表生成一个 shader 包 `Marlin.Generated.Closures`，里面用 0004 §8.4 的扩展块给每种类型补上 `IClosureId`：

```csharp
// 生成的代码
namespace Marlin.Generated;
extension(Marlin.Substrate.Slab)  : IClosureId { public static uint ClosureId => 0; }
extension(Marlin.Substrate.Hair)  : IClosureId { public static uint ClosureId => 1; }
// ……
extension(Game.Toon.ToonBsdf)     : IClosureId { public static uint ClosureId => 7; }
```

- 每个 `Session` 都包含这个生成的包（0005 §5.1）。`IBsdf` 对 `TSelf : IClosureId` 的约束在单态化时检查，由生成的扩展满足。
- 因为 id 只追加，加一个新的 BSDF 不会改变已有类型的 id，已有材质的 shader 缓存也不会失效。只有光照的分发 shader 需要重编（§5.4）。

### 5.3 写入：材质解算

```csharp
public ref struct ClosureWriter
{
    public void Write<B>(in B b, float3 weight, in EmitContext e) where B : IBsdf<B>, IClosureId
    {
        if (Max3(weight) < MinWeight) return;                  // 按像素丢掉权重为 0 的 closure
        var p = B.Pack(b, ref Normals);
        AppendHeader(B.ClosureId, Normals.LastIndex, B.Features, weight);
        AppendPayload(p);                                      // SizeOf<B.Packed>() 是编译期常量
        AccumulateProxy(b.Proxy(e.Wo), weight);                // §4.2
    }
}
```

`ResolvePixel<M>` 里的 `Emit` 是对 `M.Tree` 的单态化展开，所以每个叶子的类型在编译期已知：这一步不需要任何分发。

### 5.4 读取：光照

光照这一侧，同一个 pass 要处理多种 closure 类型，这里才需要分发。做法是 **cook 生成分发 struct**：

```csharp
public interface IClosureDispatch
{
    static abstract float3 Eval(in ClosureRef c, float3 wo, float3 wi);
    static abstract float3 Compose(in ClosureRef c, in ComposeInput i);
    static abstract SimpleMaterial Simplify(in ClosureRef c, float3 wo);
}

// 生成的代码：每种类型一个特化版本，没有分支
public struct Only_ToonBsdf : IClosureDispatch
{
    public static float3 Eval(in ClosureRef c, float3 wo, float3 wi) => c.Unpack<ToonBsdf>().Eval(wo, wi);
    public static float3 Compose(in ClosureRef c, in ComposeInput i) => c.Unpack<ToonBsdf>().Compose(i);
    // ……
}

// 生成的代码：全类型 switch，给混合 tile 和半透明用
public struct All : IClosureDispatch
{
    public static float3 Eval(in ClosureRef c, float3 wo, float3 wi) => c.TypeId switch
    {
        0 => c.Unpack<Slab>().Eval(wo, wi),
        1 => c.Unpack<Hair>().Eval(wo, wi),
        7 => c.Unpack<ToonBsdf>().Eval(wo, wi),
        _ => 0,
    };
    // ……
}
```

光照的入口对分发 struct 泛型：

```csharp
[Compute(8, 8, 1)]
public void ShadeTiles<D>([Push] in ShadeArgs a, [GroupId] uint3 g, [GroupThreadId] uint3 t)
    where D : IClosureDispatch
{
    var px = a.Tiles.Load(a.Bucket, g.x).Origin + t.xy;
    var m = MaterialBuffer.Open(a.Material, px);
    var s = Reservoirs.Resolve(a.Restir, px);                  // 0010 §2.6
    float3 diffuse = 0, specular = 0;
    foreach (var c in m.Closures)                              // 运行时个数 ≤ 拓扑上限
    {
        var f = D.Eval(c, s.Wo, s.Wi);
        Split(c, f * c.Weight * s.Radiance * s.W, ref diffuse, ref specular);
    }
    Demodulate.Write(a.Out, px, m.Proxy, diffuse, specular, s.HitDistance);
}
```

- 稠密的快路径 tile 用 `ShadeTiles<OnlySimpleSlab>`，它只读 proxy lobe。
- 一个 tile 只有一种 closure 类型时用 `ShadeTiles<Only_T>`；类型混合时用 `ShadeTiles<All>`。混合的 tile 通常只出现在材质的边界上，switch 的分歧限于那里。
- **bucket 的数量是固定的**：1（快路径）+ 2N（每种类型分简单和复杂两档）+ 1（混合），N 是注册表里的类型数，cook 时就确定了。

host 侧：

```rust
pub struct ClosureRegistry {
    pub types: Vec<ClosureType>,       // id、完整类型名、特性位、payload 字节数
    pub buckets: Vec<ShadeBucket>,     // 每个 bucket 对应的 pipeline：ShadeTiles<D>、Compose<D>
}

fn add_shading<'g>(g: &mut RenderGraph<'g>, reg: &'g ClosureRegistry, io: &ShadeIo<'g>) -> ShadeOut<'g> {
    let classes = g.add_compute("ClassifyTiles", |b| { /* 写每个 bucket 的 tile 列表和 indirect 参数 */ });
    g.add_compute("ShadeTiles", |b| {
        let args = b.read_buffer(classes.args, Access::Indirect);
        let tiles = b.read_buffer(classes.tiles, Access::ComputeRead);
        let out = b.write_texture(io.lighting, Access::ComputeWrite);
        // ……其余输入
        (out, move |ctx: &mut PassContext| {
            for (i, bucket) in reg.buckets.iter().enumerate() {
                ctx.set_pipeline(&bucket.shade);
                ctx.push(&ShadeArgs { bucket: i as u32, tiles: ctx.srv(tiles), /* …… */ });
                ctx.dispatch_indirect(args, i as u64 * DISPATCH_ARGS_SIZE);
            }
        })
    })
}
```

- 每个 bucket 都是同一个 pass 里的一次 indirect dispatch。它们写的像素互不相交，彼此之间不需要 barrier。
- bucket 数量在 cook 时固定，所以 graph 的拓扑稳定，编译缓存照样命中（0007 §5）。
- tile 里没有某个 bucket 的像素时，对应的 dispatch 是空的。

### 5.5 哪些地方需要分发

| 用途 | 是否分发 | 用什么 |
|---|---|---|
| ReSTIR 初始采样、时域和空间复用（0010 §2.2–2.5） | 否 | proxy lobe |
| 最终着色（0010 §2.6） | 是 | `ShadeTiles<D>`，按 bucket |
| 合成（§5.6） | 是 | `Compose<D>`，按 bucket |
| 降噪 | 否 | proxy lobe |
| 半透明前向（0009 §8） | 是 | 寄存器里的 closure，用 `All`（树是材质自己的，所以也可以直接用单态化的叶子，不经过 id） |
| 波前式命中（§9.3） | 是 | 命中点的 closure 写进 hit 材质缓冲，然后同上 |

这个设计成立的关键：**ReSTIR 里最频繁的部分（每个候选、每个邻居）只碰 proxy lobe**。proxy 是近似的，所以只影响噪声，不影响无偏性（0010 §2.6）；只有每个像素最后一次着色才求值精确的 BSDF。

### 5.6 合成钩子

降噪器输出解调后的漫反射和镜面辐照度。合成 pass 按 bucket 分发，对每个 closure 调用 `Compose`：

```csharp
public struct ComposeInput
{
    public float3 Diffuse, Specular;                    // 降噪后、解调的
    public float3 DiffuseAlbedo, SpecularAlbedo;        // 这个 closure 的（已乘权重）
    public float SunShadow;                             // L0–L2 的 VSM 遮罩（0010 §5.3）
    public float3 Wo;
    public ViewInfo View;
}
```

- 默认实现：`Diffuse · DiffuseAlbedo + Specular · SpecularAlbedo`。像素的解调用的就是各 closure 反照率的总和，所以默认合成正好还原线性的结果。
- 风格化 BSDF 可以覆盖它，对总光照做非线性处理：按亮度阶梯化、把间接光压平、加边缘光。
- 自发光在合成时另外加上。

### 5.7 特性位与额外的 pass

有些 BSDF 需要引擎做光照之外的事。BSDF 用 `Features` 声明，引擎在 tile 分类时汇总，只在需要的 tile 上运行对应的 pass：

| 特性 | 引擎做什么 |
|---|---|
| `SubsurfaceDiffusion` | 合成之前，对这些像素的漫反射做屏幕空间的 Burley 扩散（按 MFP） |
| `HairShading` | 毛发的双散射需要的深度不透明度（0013 §7） |
| `WaterVolume` | 需要水面以下的场景颜色和深度（0013 §6） |
| `NeedsTangent` | 法线表里存切线 |

这些 pass 都是引擎内置的。自定义 BSDF 只能组合使用这些特性；要加全新的 pass，就用 host 侧的 `RenderFeature`（0008 §5.5）。

### 5.8 Toon 示例

```csharp
namespace Game.Toon;

public struct ToonBsdf : IBsdf<ToonBsdf>
{
    public float3 Albedo;
    public float3 Normal;
    public Texture2D<float3> Ramp;          // N×1 的 ramp
    public Sampler RampSampler;
    public float3 SpecColor;
    public float SpecSize = 0.9, SpecSoftness = 0.02;
    public float IndirectFlatten = 0.7;     // 合成时间接光压平的程度

    public type Packed = ToonPacked;        // Albedo RGB8、ramp 下标、SpecSize/Softness 各 8 位……
    public static ToonPacked Pack(in ToonBsdf b, ref NormalTable n) => ...;
    public static ToonBsdf Unpack(in ToonPacked p, in NormalTable n) => ...;

    public float3 Eval(float3 wo, float3 wi)
    {
        var ndl = Dot(Normal, wi);
        var band = Ramp.Sample(RampSampler, float2(ndl * 0.5 + 0.5, 0.5), lod: 0);
        var h = Normalize(wo + wi);
        var spec = SpecColor * SmoothStep(SpecSize - SpecSoftness, SpecSize + SpecSoftness, Dot(Normal, h));
        return (Albedo * band / Pi + spec) * Saturate(ndl);
    }

    public BsdfSample Sample(float3 wo, float3 u) => BsdfSample.CosineHemisphere(Normal, u);
    public float Pdf(float3 wo, float3 wi) => Saturate(Dot(Normal, wi)) / Pi;
    public ProxyLobe Proxy(float3 wo) => ProxyLobe.Diffuse(Albedo, Normal);

    public float3 Compose(in ComposeInput i)
    {
        // 对总光照做阶梯化；间接光向平均值压平
        var lum = Luma(i.Diffuse);
        var stepped = Ramp.Sample(RampSampler, float2(Saturate(lum), 0.5), lod: 0) * lum;
        var flat = Lerp(i.Diffuse, float3(lum), IndirectFlatten);
        return Lerp(flat, stepped, 0.5) * i.DiffuseAlbedo + i.Specular * i.SpecularAlbedo;
    }
}

public struct ToonCharacter : ISurfaceMaterial<ToonCharacter>
{
    public Texture2D<float4> BaseColor;
    public Texture2D<float3> Ramp;
    public Sampler Sampler;
    public type Tree = ToonBsdf;
    public Tree Surface(in SurfaceContext ctx) => new()
    {
        Albedo = ctx.Sample(BaseColor, Sampler, ctx.Uv0).rgb,
        Normal = ctx.Normal,
        Ramp = Ramp,
        RampSampler = Sampler,
        SpecColor = float3(1),
    };
}
```

接入这个 BSDF 的全部工作：
1. 在游戏的 shader 包里写这两个 struct；
2. cook 自动把 `ToonBsdf` 追加进注册表，生成 `Only_ToonBsdf` 并更新 `All`，重编光照 shader；
3. **引擎代码一行都不用改**。

描边不属于 BSDF，用一个后处理材质（§10.2）或者 `RenderFeature` 实现。

### 5.9 限制

- closure 类型最多 32 种（tile 掩码是 u32）。不够时换成 u64，只影响分类 pass。
- proxy 和精确 BSDF 差别很大时（非常激进的 ramp），ReSTIR 的噪声会变大，但结果仍然无偏。
- `Compose` 拿到的是像素的总光照，没有按光源分开，也没有把直接光和间接光分开（统一蓄水池里它们本来就混在一起）。需要单独处理直接光的风格化，在 `Eval` 里按每盏光做。直接光和间接光分开的通道见 §13。
- 自定义 BSDF 的 `Sample` 和 `Pdf` 只在波前式命中和 L4 的路径延续时用到；L0–L3 的延续按 proxy 采样。

## 6. 材质解算

### 6.1 入口

```csharp
[Compute(64, 1, 1)]
public void ResolvePixel<M, TBudget>([Push] in ResolveArgs a, [DispatchThreadId] uint3 id)
    where M : ISurfaceMaterial<M> where TBudget : IMaterialBudget
{
    [Export] const ClosureInfo Topology = Substrate.Flatten<M.Tree>();

    if (id.x >= a.Bin.Count) return;
    var px = a.Bin.Pixel(id.x);
    var hit = VisBuffer.Reconstruct(a.Scene, a.View, px);     // 三角形、重心坐标、解析导数
    var ctx = SurfaceContext.FromRaster(hit, a.Scene, a.View); // 内部调用 Derivatives.Seed
    var m = a.Scene.Materials.Load<M>(hit.MaterialSlot);
    var tree = m.Surface(ctx);

    var w = ClosureWriter.Open(a.MaterialBuffer, px);
    Substrate.Emit<M.Tree, TBudget>(tree, ref w, EmitContext.From(ctx, a.DBuffer.Load(px)));
    w.Finish();
    Velocity.Write(a.Velocity, px, hit);                       // 0009 §4.6
}
```

- `TBudget` 是平台的预算常量（§4.3），作为类型实参传入，`Emit` 用 `const if` 决定是否简化（§8.3）。
- 每个 permutation 对应一次 indirect dispatch（0009 §5）。

### 6.2 SurfaceContext

| 字段 | 说明 |
|---|---|
| `Position`、`PrevPosition` | 世界位置，带导数源 |
| `Normal`、`GeometricNormal`、`Tangent` | 插值后的法线和切线 |
| `Uv0`…`Uv3` | 纹理坐标，带导数源 |
| `VertexColor` | 顶点色 |
| `WorldNormal` | 便捷属性 |
| `InstanceId`、`CustomData` | 每实例数据（§7.3） |
| `View` | 相机位置、视线方向、屏幕坐标、时间 |
| `Sample(...)`、`SampleNormal(...)` | 带采样反馈的采样（§2.3） |

- 导数：visbuffer 解算时由重心坐标的解析导数给出，射线命中时由 ray cone 给出；都通过 `Derivatives.Seed` 标记，材质代码里的 `Ddx`、隐式 LOD 采样都能用，并且不需要标 `[Differentiable]`（0004 §15.3）。

### 6.3 其他入口

| 入口 | 用在 | 和 `ResolvePixel` 的区别 |
|---|---|---|
| `RasterMasked<M>`、`RasterDeform<M>` | 可编程光栅（0009 §4.4） | 只调用 `Opacity` 和 `Deform`，不调用 `Surface` |
| `ForwardTranslucent<M>` | 半透明（0009 §8） | closure 留在寄存器里，直接做前向光照 |
| `ResolveHit<M>` | 波前式命中（§9.3） | 上下文来自射线命中，导数来自 ray cone，写进 hit 材质缓冲 |
| `BakeSimple<M>` | 简化材质图集（§9.2） | 在 UV 网格上求值，输出简化材质 |

它们都由同一个材质类型实例化，cook 按材质的 `Blend` 和实现了哪些可选方法来决定生成哪几个。

## 7. 实例与参数

### 7.1 四层

1. **材质类型**：`.mshader` 里的 struct。
2. **材质实例**：`.masset` 资产，覆盖参数值；可以继承另一个实例；可以覆盖 `const` 静态参数（会产生新的 permutation，§8.1）。
3. **每实例数据**：GPU Scene 里每个实例的 N 个 float。字段标 `[PerInstance(i)]` 时从这里读第 i 个，不占参数块。改它不需要新建材质实例，用来做受击闪白、染色、溶解进度。
4. **全局参数集**：`.masset` 里定义的一组命名参数（例如 `Weather.Wetness`）。字段标 `[Global("Weather.Wetness")]` 时从全局的 uniform buffer 读；每帧只上传一份，cook 时把名字解析成偏移。

### 7.2 材质实例资产

```
MaterialInstance {
    parent: "materials/rock_base.mi"      // 或者直接指向材质类型
    material: "Game.Rock"
    params: { RoughnessScale: 1.2, MossAmount: 0.8, BaseColor: "textures/granite_d" }
    statics: { UseMoss: true }            // 覆盖静态参数
}
```

- cook 把继承链展平：最终得到 (材质类型, 静态参数值, 参数块字节)。
- 静态参数被覆盖时，cook 报告每个材质类型的 permutation 数。工程可以设上限，超过就报错。

### 7.3 每实例数据

- 每个实例最多 `MAX_CUSTOM_DATA`（默认 8）个 float，放在 GPU Scene 的 `CustomData` 表里（0009 §2.1）。
- 更新走场景的更新流（0009 §1.2），gameplay 直接设置，不经过材质实例。

### 7.4 动态材质实例

- 运行时从一个实例派生出一个新实例，然后单独改参数（类似参考实现里的动态材质实例）。会分配一个新的材质表槽位。
- 动态实例只能改参数值，不能改静态参数，因为那需要新的 permutation，而 game 里不编译 shader。

### 7.5 GPU 上的材质表

- 一个 `ByteBuffer`，每个材质实例占一个槽位，槽位里是它的参数块，布局来自反射。
- `a.Scene.Materials.Load<M>(slot)` 按 `M` 的布局读出参数，struct 里的资源字段就是下标。
- 参数被修改时，对应的行通过 scatter 上传（0009 §2.2）。

### 7.6 host API

```rust
pub struct MaterialInstance { /* 槽位、permutation、参数块 */ }

impl MaterialInstance {
    pub fn set_scalar(&mut self, name: ParamId, v: f32);
    pub fn set_vector(&mut self, name: ParamId, v: Vec4);
    pub fn set_texture(&mut self, name: ParamId, t: &TextureAsset);
    pub fn derive(&self, scene: &mut RenderScene) -> MaterialInstance;   // 动态实例
}

pub struct ParameterCollection { /* 名字 → 偏移 */ }
impl ParameterCollection { pub fn set(&mut self, name: ParamId, v: f32); }
```

- `ParamId` 在加载时从名字解析一次，之后只用下标。
- 参数的布局校验沿用 0005 §9：Development 构建里比对反射和 Rust 端的期望。

## 8. permutation、cook 与编译

### 8.1 permutation 键

(材质类型, 静态参数值, 入口, target)。

- 入口：`ResolvePixel`、光栅变体、`ForwardTranslucent`、`ResolveHit`、`BakeSimple`。
- target 里包含平台的材质预算（§4.3），所以不同平台的简化结果不同。
- 画质档位**不**产生材质 permutation：档位影响的是光照 shader，材质解算在所有档位上都一样。

### 8.2 cook 流程

```
收集用到的材质实例（关卡、包） ─▶ 展平继承链 ─▶ 去重得到 permutation
  ─▶ 编译入口（并行，0005 §5.5）─▶ 反射里的 Topology
  ─▶ 汇总叶子类型 ─▶ 更新 closure 注册表（只追加）─▶ 生成 Marlin.Generated.Closures
  ─▶ 编译光照的分发 permutation（ShadeTiles<D>、Compose<D>……，每个 bucket 一个）
  ─▶ cooked 单元：permutation 表、参数布局、Topology、ClosureRegistry
```

- 注册表变化时，只有分发 shader 需要重编，材质 shader 不受影响（§5.2）。
- cook 报告：每个材质的 permutation 数、拓扑、payload 字节数、简化情况（§8.3）、指令统计（0005 §7）。

### 8.3 平台简化

`Substrate.Emit<Tree, TBudget>` 在编译期比较 `Flatten<Tree>()` 和预算，超出时按固定顺序简化，每一步都是 `const if`，不满足就继续下一步：

1. 去掉昂贵的特性：glint → 薄膜 → specular profile → haze（并进主瓣）→ fuzz（并进漫反射）；
2. 把最深的 `Vertical` 烘成一个 Slab：上层的镜面保留，下层的漫反射乘上上层的透射；
3. 把 `Horizontal` 合并成一个 Slab，参数按 `Mix` 插值；
4. 还是超出就整棵树 `ToSlab`。

- 自定义 BSDF 参与合并时用它的 `ToSlab`（默认取 proxy）。
- 简化的结果写进 cook 报告。editor 可以把视口切到"按某个平台的预算"预览简化后的效果，因为只是换一个 `TBudget` 再编一次。

### 8.4 editor 与热重载

- editor 通过 cook 服务（0003 §6）按需编译：视口里第一次出现某个 permutation 时发请求。编译完成之前，这个实例用默认材质（灰色棋盘格）渲染，不阻塞画面。
- 改 `.mshader` 文件后，编译器的 `Session` 只重算受影响的 query（0005 §5.1），pipeline 缓存原地替换。
- 新增 closure 类型时，cook 服务追加注册表、重新生成分发包、重编光照 shader，然后热替换。

### 8.5 pipeline 缓存

- 主机：所有 pipeline 都在 cook 时离线编译好。
- PC：cook 输出这个包用到的 pipeline 列表，加载关卡时在后台按优先级预编译。运行时碰到没编译的就用默认材质顶替，同时记录日志，不卡顿。

## 9. 简化材质与光线命中

### 9.1 简化材质

`SimpleMaterial`：反照率（RGB）、粗糙度、F0（或金属度）、自发光、不透明度。没有法线贴图。

用在：
- GI 路径和 L0–L2 反射的命中点（0010 §2.2、§2.7）；
- masked 几何在 ray query 里的 alpha 测试（0009 §7）；
- 自发光三角形的功率估计（0010 §5.2）；
- radiance cache 更新路径的命中点（0010 §4.1）。

### 9.2 运行时烘焙

- 材质行加入或参数改变时，调度一次 `BakeSimple<M>`（`Material/Simple.mshader`，每个材质类型一个 permutation，和解算的 permutation 一起注册）：材质行 r 在图集里固定占第 r 块 32²（图集 2048²，一行 64 块，最多 4096 个材质行），在 UV0 的网格上对每个纹素求值 `Surface` → `Emit`（放在朝 +Z 的平面上），把合成的 proxy lobe 写进图集。
- 图集是三张持久纹理：`SimpleAlbedo`（平方根反照率 + 粗糙度，RGBA8）、`SimpleSpecular`（平方根 F0 + 不透明度，RGBA8）、`SimpleEmission`（自发光，RGBA16F）。`ClosureWriter` 为此多了 `Emission`，合成方式和反照率一样按权重求和。
- 在运行时烘焙而不是在 cook 时烘焙：cook 不需要 GPU，动态实例的参数改了也能更新。只烘焙这一帧变化的行（`RenderScene::simple_changes`），图集或系数表是新建的那一帧全部重烘。
- **每实例数据用一个常量系数**（用户的选择）：每个实例行在 `SimpleFactors` 里有两个 `float4`，反照率系数和自发光系数，等于在 4 个 UV 上用它的 `CustomData` 求值、除以用 (1,1,1,1) 求值的结果（分母接近 0 的通道取 1）。命中点读出的值 = 图集 × 系数。对"基色乘实例颜色"这类线性用法是精确的，非线性的用法（比如按 `CustomData` 切换贴图）只是近似。全局参数不进图集，简化材质看不到湿度这类全局变化（§13）。
- 不按 UV 映射的材质（世界空间、三平面投影）：现在在 `Position = 0` 处求值，得到的只是原点那一点的值；`SimpleBake.Constant`（几个位置取平均）以后需要时再加。
- 命中点：`SimpleOfHit(hit, sources, simple)`：实例行 → 材质行 → 图集块，UV 由 RT LOD 的顶点 UV0 和重心坐标插值得到（`SurfaceOfHit`，0009 §3.1），按最近纹素读。没有材质的实例是中灰（反照率 0.18）。每个 section 的材质槽等网格 section 接上再做。

### 9.3 波前式完整材质

L4、L3 的镜面（粗糙度 < 0.05）、参考路径追踪器使用：

```
射线 ─▶ hit buffer（实例、三角形、重心坐标、射线方向、ray cone）
     ─▶ 按材质 permutation 分箱（和 0009 §5 同一套分箱代码，只是输入是 hit 列表）
     ─▶ ResolveHit<M>：同一个 Surface，写进 hit 材质缓冲
     ─▶ 按 closure bucket 着色和继续采样（§5.4）
```

- 这就是自己实现的"按材质重排"：命中点按材质排好以后，每个 dispatch 里都是同一个材质，没有分歧。
- 每一层弹射多一轮分箱，只在需要完整材质的档位上做。

## 10. 其他域

### 10.1 Decal

```csharp
public interface IDecalMaterial<TSelf> where TSelf : IDecalMaterial<TSelf>
{
    DecalSample Decal(in DecalContext ctx);     // 反照率、法线、粗糙度、金属度、自发光，各带一个不透明度
    const DecalChannels Channels = DecalChannels.All;
}
```

- **DBuffer**：可见性之后、材质解算之前，贴花按材质分箱，每个贴花材质的投影盒一次 dispatch，把结果混合进 DBuffer（3 张纹理：反照率+不透明度、法线+不透明度、粗糙度/金属度/镜面+不透明度）。
- 材质解算时，`EmitContext` 带着这个像素的 DBuffer 值，`Emit` 在打包每个叶子之前调用它的 `ApplyDecal`。Slab 实现了它，其他 BSDF 默认忽略贴花，可以自己覆盖。
- 这样材质解算的 shader 不需要知道有哪些贴花材质，也就不需要分发。
- 网格贴花（贴花形状的几何体）也写 DBuffer。
- 写进 RVT 的贴花（路面标线这类）走地形的 RVT（0013 §4），不经过 DBuffer。

### 10.2 PostProcess

```csharp
public interface IPostProcessMaterial<TSelf> where TSelf : IPostProcessMaterial<TSelf>
{
    float4 Process(in PostContext ctx);         // 可以读场景颜色、深度、速度，以及材质缓冲的 proxy
    const PostInjection At = PostInjection.BeforeDisplay;
}
```

- 注入点：`AfterLighting`、`BeforeDisplay`、`AfterDisplay`（0010 §8.2）。同一个注入点上的多个材质按优先级排序。
- **显示变换** `IDisplayTransform`（0010 §8.3）也属于这个域：它是一个特殊的后处理，可编程，默认实现是 ACES 2.0。
- 后处理体积（0009 §1.1 的 `Volumes`）决定哪些后处理材质生效、权重多少。

### 10.3 LightFunction

```csharp
public interface ILightFunction<TSelf> where TSelf : ILightFunction<TSelf>
{
    float3 Evaluate(in LightFunctionContext ctx);   // 光源空间的方向或位置
}
```

- 不在光照 shader 里分发。每帧把光函数**烘进一个图集**：聚光和矩形光烘成 2D 投影，点光烘成立方体的六个面；静态的光函数只烘一次。
- ReSTIR 的 target 和着色里，光函数就是一次图集采样（0010 §5.1）。

### 10.4 Volume

```csharp
public interface IVolumeMaterial<TSelf> where TSelf : IVolumeMaterial<TSelf>
{
    VolumeSample Sample(in VolumeContext ctx);     // 消光、散射反照率、自发光、相函数 g
}
```

- 用在局部雾体积和粒子的体积注入：按材质分箱，在 froxel 网格里覆盖到的格子上调度 `InjectVolume<M>`（0013 §3）。
- 体积云使用自己固定的模型（0013 §2），不走这个域。

### 10.5 UI 效果

```csharp
public interface IUiEffect<TSelf> where TSelf : IUiEffect<TSelf>
{
    float4 Shade(in UiEffectContext ctx);          // 返回预乘 alpha 的颜色；ctx 里有局部坐标、SDF 距离、时间
}
```

- UIRenderer 的 tile 着色是一个 kernel，所以 UI 效果和 closure 一样，由 cook 生成一个 switch 分发（0012 §9）。
- 限制：不能写资源，不能读场景缓冲。

## 11. 调试

- 材质缓冲的可视化：每个 closure 单独显示；proxy lobe；快路径和复杂像素；溢出池占用。
- bucket 的可视化：每个 tile 落在哪个 bucket。
- 简化材质：把视口切到"RT 命中点看到的材质"。
- 按平台预算预览（§8.3）。

## 12. 测试

- **平台无关**：
  - closure 注册表只追加：新增类型不改变已有 id；生成的扩展包内容稳定（快照测试）；
  - 拓扑的 CTFE：各种算子组合的 `ClosureInfo` 和手算的一致；
  - 平台简化规则：给定拓扑和预算，简化的步骤和结果与期望一致；
  - 材质实例继承链的展平、静态参数覆盖产生的 permutation 键；
  - 参数块布局和 Rust 端的比对（0005 §9）。
- **平台相关**（真实硬件）：
  - 单个默认 Slab：快路径和"强制走溢出池"的结果逐像素一致；
  - `Only_T` 和 `All` 两种分发对同一个 tile 的输出一致；
  - Toon 示例：只在测试的 shader 包里定义 `ToonBsdf`，不改引擎代码，渲染结果和 CPU 上用同一套公式算出的参考图一致；
  - 溢出池耗尽时像素退化成 proxy，不出现垃圾数据；
  - 简化材质图集：主射线命中点读出的简化材质和材质 pass 在同一像素写出的 proxy lobe 一致，实例改变后增量烘焙仍然一致（§14 的 R4a 记录）。

## 13. 待定

**已确认**（2026-10-18，R1k 之前）：
- 表面材质的接口叫 `ISurfaceMaterial<TSelf>`，上下文叫 `SurfaceContext`；
- 材质从一开始就用关联类型 `Tree`（只有 Slab 时写 `type Tree = Slab;`），R5 加算子时材质代码不用改；
- Tree 的约束接口叫 `ISubstrateNode<TSelf>`，写材质缓冲的类型叫 `ClosureWriter`；
- Substrate 的类型放在单独的包 `Marlin.Substrate` 里（`content/engine/substrate/`），引擎和游戏的材质都依赖它；
- 引擎的默认材质类型叫 `Marlin.Engine.DefaultMaterial`。

**需要和 shader 语言一起确认的**（0004 的规则：命名和语义先问再定）：
- 接口和类型名：`ISurfaceMaterial`、`ISubstrateNode`、`IBsdf`、`IClosureId`、`IClosureDispatch`、`ClosureWriter`、`SurfaceContext`、各个域的接口名；
- attribute 名：`[PerInstance(i)]`、`[Global("...")]`，以及 `[Group]`、`[Range]` 是否进标准库；
- **接口的 static abstract 方法能否在 CTFE 中求值**（`TTop.Info()`，§3.4）：需要语言规定"这类接口方法的实现必须是 `const`"，或者引入 `static abstract const` 这样的写法；
- 生成的扩展包（§5.2）作为每个 `Session` 的隐式依赖：0005 的 `SourceProvider` 要能提供"生成的包"；
- 静态开关能否改变拓扑类型（比如 `type Tree = UseMoss ? Horizontal<Slab, Slab> : Slab;`），现在只能保守计算（§2.3）；
- `dyn IBsdf`（0004 §8.5）将来实现时，可以替代 §5.4 里生成的 `All`。

**数值**：§4.3 的预算和溢出池比例。

**功能**：
- 直接光和间接光分开的合成通道（§5.9）：给需要的 BSDF 一个特性位，降噪器只在那些 tile 上多处理一路；
- 简化材质要不要带上每实例数据的影响（§9.2）；
- 每像素位移（POM）和曲面细分位移。

## 14. 实现记录

R1k（0008 §8，材质最小集）做完时（2026-10-18）定下的细节：
- **范围**：只有默认 Slab（`DiffuseAlbedo`、`F0`、`F90`、`Roughness`、`Normal`、`Emissive`），只有材质缓冲的稠密部分（§4.1，每像素 16 字节），没有溢出池、closure 注册表和分发（§5）、材质实例资产（§7.2）、每实例自定义数据之外的参数来源（`[PerInstance]`、`[Global]`）、平台简化（§8.3）。这些随 R5 的完整 Substrate 一起做。
- **`Marlin.Substrate` 包**（`content/engine/substrate/`，`Library = true`）：`ISubstrateNode<TSelf>`（`Emit`）、`ISurfaceMaterial<TSelf>`（`type Tree : ISubstrateNode<Tree>`、`Tree Surface(in SurfaceContext)`）、`SurfaceContext`（位置、插值法线、几何法线、`Uv0`、实例行、`CustomData`）、`EmitContext`、`Slab`、`ClosureWriter`，以及打包和解包（八面体法线 2×16 位；反照率和 F0 是平方根编码的 RGB8，暗部精度更高；粗糙度 8 位）。`ShaderPackage` 加了 `Library`：只给别的包用的类型和函数，cooker 不再警告它没有入口。
- **快路径**：一个默认的 Slab、权重 1、`F90` 为 1、没有自发光，header 的快路径位置 1。多个 closure 时 proxy 按权重合成：反照率和 F0 加权求和，粗糙度按镜面亮度加权，法线取权重最大的那个（§4.2）。自发光现在只有 header 的标志位，颜色要等溢出池。
- **`Marlin.Engine.DefaultMaterial`**：`BaseColor`、`Roughness`、`Metallic`，基色乘上实例颜色（`CustomData`）；金属的漫反射为 0，F0 是基色。marlin-material 的 `DefaultMaterial` 按着色器的布局打包它的参数块（5 个字）。
- **材质表**（GPU Scene，0009 §2.1）：场景多了材质表（`MaterialId`、`MaterialUpdate`、`MaterialDesc { permutation, params }`），实例带一个材质（每个 section 一个材质等网格的 section 接上再做），`InstanceData` 的第 3 个字是材质行，没有时是 `0xFFFFFFFF`。**参数块按 permutation 分表**（偏离 §7.5 的"一个 `ByteBuffer`"）：shader 用 `ReadOnlyBuffer<M>[row]` 按材质的 struct 类型读参数，Marlin shader 现在没有从字节 buffer 按类型读 struct 的写法，所以同一个 permutation 的参数块放在一张行宽相同的表里。材质表每行是 (permutation, 在表里的行)。GPU Scene 的 scatter 因此改成通用的：每个 op 带上目标表里的字偏移，一张表一次 dispatch，表的个数不再固定。
- **分箱和解算**（`Marlin.Engine.Material`，0009 §5）：`Classify` 按像素查出 permutation 并计数，同时给没有表面的像素清空材质缓冲、写相机运动的速度；`Bins` 算出每个 bin 的起点，写成 RHI 的 indirect 记录（常量 = bin 的起点，即 push 的第 31 个字）；`Scatter` 把像素（x \| y << 16）写进 bin；`ResolvePixel<M>` 在一个 graph pass 里对每个 permutation 一次 indirect dispatch。解算从 visbuffer 重建三角形：解码三个顶点、按当前和上一帧的变换算世界位置、用 clip 坐标求透视正确的重心坐标，再插值位置、上一帧位置和法线。uv 和导数源（§6.2 的 `Derivatives.Seed`）还没有接：现在的材质不采样纹理，等纹理上传到来时一起做。
- **运动向量**（0009 §4.6）：解算时用实例上一帧的变换和上一帧的相机算出上一帧的屏幕位置，速度 = 当前 − 上一帧，UV 单位（y 向下），`Rg16Float`。天空用无穷远点在两帧相机下的投影差。
- **host 端**：新的 `marlin-material` crate：`Materials::new(device, &MaterialShaders)`、`register(device, name, resolve)`（按注册顺序给出 permutation，默认材质先注册，`DEFAULT_PERMUTATION = 0`）、`add_passes(g, gpu, &vis, camera, previous, w, h) -> MaterialOutput { visbuffer, material, velocity, counts }`、`add_debug_pass(g, &out, MaterialDebug)`（反照率、法线、粗糙度/F0/快路径、速度）。framework 的 `Engine` 建一个默认材质实例，所有静态网格都用它。R1l 起游戏的帧跑材质 pass（见 0008 §8 的 R1 记录）。
- **编译器的修正**（语法没有变化）：
  - 关联类型的约束可以提到它自己（`type Tree : ISubstrateNode<Tree>;`）：别名环检查跳过没有目标的关联类型，`AliasTargetQuery` 对它们不再去降级签名（那会在自己的约束上成环）。
  - 实现接口时，签名里的关联类型按实现者的取值比较（`Tree Surface(...)` 和 `Slab Surface(...)` 匹配）。
  - 经类型参数调用接口方法时，接口的关联类型变成 `M.Tree`；`M.Tree` 类型的值能找到它约束接口上的成员（`tree.Emit(...)`）。`hir_ty` 和 `lower` 各加了测试（后者在解释器里跑单态化后的结果）。
  - `marlin-gpu-test-support` 的测试 shader 能给泛型入口传类型实参，session 里多了 `Marlin.Substrate`。
- **驱动问题**：NVIDIA 驱动经 descriptor heap 读 buffer 时，忽略类型是 struct 的成员的 offset（0016 §4.2）。`InstanceTransforms` 因此改成 6 个 `float4` 字段，`Current`、`Previous` 是拼出 `Rows3x4` 的属性。之后 SPIR-V 后端绕开了这个问题，`InstanceTransforms` 恢复成两个 `Rows3x4` 字段（0016 §4.2）。
- **测试**（`marlin-material/tests/gpu/materials.rs`，两种材质：默认材质和测试包里的 `Material.Stripes`，所以有两个 permutation）：每个被覆盖的像素恰好进一个 bin、恰好解算一次，closure 数为 1 且走快路径，反照率、F0、粗糙度是各自材质给的值（8 位编码的误差内）；天空全 0；正对相机的面法线正确；静止时速度为 0，实例右移 0.1 时速度和投影算出的一致，相机转动时天空的速度和 tan(θ)·P00/2 一致。三张调试视图（反照率、法线、粗糙度/F0/快路径）有 golden。GPU Scene 的测试（render crate）检查材质表和每个 permutation 的参数表，包括换 permutation 和删除。3 张前向 golden 和 `sample_first_frame` 不变。开窗口跑 `samples/basic` 10 秒，没有报错。

R4a（0008 §8，光线命中和简化材质）做完时（2026-10-19）定下的细节：
- **命中点的表面**（`Scene/RayTracing.mshader`）：几何池多了一张 RT 网格表（`RtMeshTable`，每个 mesh slot 8 个字：RT LOD 的位置、属性、索引在几何池里的偏移，每顶点属性的字节数，UV 套数，顶点数，三角形数）。`SurfaceOfHit(hit, ray, sources)` 从它和 GPU Scene 的实例、网格、变换表读出三角形，按重心坐标插值世界位置、着色法线（八面体编码）、几何法线（都翻到射线这一侧）和 UV0（f16）。`HitSources` 是这些表的 bindless 索引，`RtScene::read_hits` 声明它们的读取。
- **简化材质**：见 §9.2。host 端是 `Materials::add_simple_passes(g, scene, gpu) -> SimpleOutput`，`register` 改成同时接收解算和烘焙两个模块；framework 按名字加载默认材质的烘焙变体（`DEFAULT_BAKE`，`content/engine/shaders/DefaultMaterialBake.masset`），`RenderSide` 每帧在 GPU Scene 之后调度烘焙。现在还没有 pass 读它，R4b（SHaRC）开始用。
- **测试**：render crate 的 `raytracing.rs`：命中点的位置和法线、压扁的立方体保持面法线、UV0 插值。marlin-material 的 `simple.rs`：从相机穿过每个像素中心发射主射线，在表面内部（四邻的 proxy lobe 相同）的像素上，命中点读出的简化材质和材质 pass 写出的 proxy lobe 一致（反照率、F0、粗糙度，8 位编码误差内；一个带颜色的电介质立方体和一个金属立方体）；改实例颜色后只重烘这个实例的系数，结果仍然一致（把增量烘焙去掉时这个测试会失败）。
