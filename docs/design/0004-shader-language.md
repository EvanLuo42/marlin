# 0004 — Marlin shader 语言：语法

状态：草案 · 2026-10-16

前置：
- [0002](0002-shader-interface.md)：shader ABI（bindless、push constants、坐标约定）
- [0003](0003-asset-pipeline.md)：资产文本格式与 cook

## 0. 决定摘要

总原则：**语法跟着 C# 走，但没有 `new`**。shader 需要而 C# 没有的东西，从参考实现那里借，但只借好的、需要的设计（自动微分、关联类型、值泛型、capability、泛型入口）。参考实现为了兼容 HLSL/GLSL 而留下的设计一律不要：预处理器、语义字符串、`register`、`cbuffer`、C 式数组、宽松精度类型等。

| 维度 | 决定 | § |
|---|---|---|
| 命名 | 完全照 C# 惯例：类型、方法、属性、公开字段用 PascalCase，局部变量和参数用 camelCase；内建函数也用 PascalCase（`Dot`、`Lerp`、`Saturate`） | 2 |
| 扩展名 | `.mshader` | 2 |
| 类型声明 | 只有 `struct`、`interface`、`enum`；没有 `class` 和 `record`；所有类型都是值类型 | 3 |
| 向量/矩阵 | `float3`、`int2`、`half4`、`float4x4` 这类短名，底层是 `Vector<T, N>` 和 `Matrix<T, R, C>` | 3.2 |
| 数组 | `float[4]`，值类型，长度是类型的一部分；用集合表达式 `[a, b, c]` 初始化 | 3.3 |
| enum | C# 原样，支持 `[Flags]`；对非 Flags enum 做 switch 时要求穷尽 | 3.5 |
| 构造 | 三种写法：构造函数调用 `T(...)`、对象初始化器 `T { ... }`、`with`；另有 `default` | 4 |
| 字段默认值 | C# 11 语义：初始值用于 `T()`、`T { }` 和反射；`default` 永远全零 | 4 |
| 可变性 | C# 原样：默认可变，`readonly` 可标在字段、方法或整个 struct 上；对只读值调用非 readonly 方法是错误，不会偷偷拷贝 | 5 |
| 成员 | 保留属性、运算符重载、自定义转换、索引器和默认接口方法 | 5 |
| 参数 | C# 的 `in`、`out`、`ref`，调用处必须写 `out`/`ref`；实现是 copy-in/copy-out | 6 |
| 函数 | 允许 namespace 顶层函数 | 6 |
| 递归 | 只允许编译期递归 | 6 |
| 组织 | 包 + file-scoped `namespace` + `using`；可见性 `public`/`internal`/`private`；包清单是 `ShaderPackage` 资产 | 7 |
| 泛型 | C# 写法加值参数 `const int N`；约束用 `where` | 8 |
| 关联类型 | 接口里写 `type Name : IBound;`，实现者写 `type Name = X;` 或者同名嵌套 struct | 8.2 |
| Self 类型 | C# 的 CRTP：`interface IArithmetic<TSelf> where TSelf : IArithmetic<TSelf>` | 8.3 |
| 扩展 | C# 14 的 `extension(x)` 块，额外允许 `: IFoo` 补实现接口 | 8.4 |
| 动态分发 | 不做：接口只能当泛型约束用 | 8.5 |
| 表达式 | 保留 switch 表达式与模式匹配、元组与解构、lambda 与局部函数、`foreach` 与范围 | 9 |
| 字面量 | 不带后缀的浮点字面量按上下文定类型，没有上下文时是 `float` | 9.3 |
| 向量转换 | 标量广播是隐式的，截断必须显式；swizzle 用小写 `v.xyz` | 9.4 |
| 矩阵 | `*` 是矩阵乘法（列向量，`M * v`）；元素用 `m[r, c]`，整列整行用 `Column(i)`/`Row(i)`；没有 `m[i]` | 9.5 |
| 资源 | C# 式的只读/可写成对命名（`ReadOnlyBuffer<T>` 和 `Buffer<T>`……）；资源类型本身就是 32 位 bindless 下标 | 10 |
| NonUniform | 编译器自动推断，允许手工覆盖 | 10.3 |
| 指针 | 暂不支持 | 10 |
| 入口 | 按 stage 分的 attribute：`[Compute(8, 8, 1)]`、`[Vertex]`、`[Pixel]`、`[Mesh(...)]` | 11 |
| 系统值 | 写在参数或字段上的 attribute：`[DispatchThreadId]`、`[Position]`…… | 11.2 |
| push constants | 入口参数标 `[Push]`，最多一个 | 11.3 |
| 模板入口 | 泛型入口，cook 时给出类型实参 | 11.4 |
| groupshared | namespace 级的 `[GroupShared]` 字段 | 12.1 |
| wave、barrier、原子 | 按功能分 namespace：`Wave.Sum`、`Barrier.Group`；原子操作是资源上的方法：`buf.AtomicAdd(i, v)` | 12.2 |
| capability | 由编译器推断；public API 可以用 `[Requires]` 声明成契约 | 12.3 |
| 预处理器 | 完全没有 | 13 |
| 编译期分支 | 普通 `if` 遇到常量条件时保证折叠；`const if` 被裁掉的分支不做 capability 检查 | 13.2 |
| 静态参数 | 带默认值的 `const` 字段，实例化时可以覆盖 | 13.3 |
| attribute | 用户可以定义，有类型检查；内建 attribute 也用同一套机制声明 | 14 |
| 自动微分 | 前向和反向的语法都定下来，编译器先实现前向；用 `[Differentiable]`、`[NoDiff]`、`Detach` 标记，用 `ForwardDiff(f)`/`BackwardDiff(f)` 求导，值和导数打包成 `DiffPair<T>`；材质代码里的屏幕空间导数由编译器自动传播 | 15 |
| 自定义导数 | 两个方向的标注都支持：`[ForwardDerivative(nameof(F))]` 和 `[ForwardDerivativeOf(nameof(G))]` | 15.4 |
| 内存布局 | 全部用 scalar 布局；`Uniform<T>` 另外要求字段不跨 16 字节行 | 16 |
| 16 位与 bool | `half`/`short`/`ushort` 是真正的 16 位类型；bool 不能放进 buffer | 3.1 |
| 64 位与 8 位 | 有 `long`/`ulong`（需要 Int64 capability）；没有 `byte`/`sbyte` | 3.1 |
| 整数语义 | C# 的 unchecked 语义；整数除零的结果固定为 0 | 9.6 |
| lambda 参数 | 直接写 `Func<...>`/`Action<...>`，编译器把它当作隐式的泛型参数 | 9.2 |
| 标准库导入 | 核心部分隐式导入，另支持 `global using` | 7.1 |
| 纹理 API | 一个 `Sample`，用命名参数区分采样方式；用索引器读 mip 0；尺寸用属性拿；纹理和采样器保持分离；可写纹理的类型不带格式 | 10.4 |
| 渲染目标 | 返回 struct，字段上标 `[Target(i)]` 和 `[Depth]` | 11.5 |
| mesh 输出 | 一个 `MeshOutput<TVertex, TPrimitive>` 对象；amplification 用 `Mesh.Dispatch` 发 payload，mesh 用 `[Payload]` 参数接收 | 11.6 |
| ray query | 对象式的 `RayQuery`，另有便捷方法 `TraceAny`/`TraceClosest` | 12.4 |
| 调试 | `Debug.Print` 和 `Debug.Assert` 接受插值字符串，只在 Development 构建里生效 | 12.5 |
| 可微类型 | 声明 `IDifferentiable<T>`，由编译器合成 `Differential` | 15.1 |
| 导数源 | 用 `Derivatives.Seed(x, dx, dy)` 标记 | 15.3 |
| 数值接口 | 精简的几层：`IArithmetic<T>`、`IInteger<T>`、`IFloat<T>`、`IComparable<T>` | 8.6 |
| 浮点语义 | 默认宽松（允许重结合、fma 收缩、近似倒数），`[Precise]` 关闭这些变换 | 9.7 |
| 编译期函数 | `const` 函数在编译期求值；`[Export]` 常量的值写进反射 | 13.4 |

## 1. 从 C# 去掉的东西

| C# 特性 | 处理 | 原因 |
|---|---|---|
| `new` | 没有 | 用构造函数调用、初始化器和 `with` 代替（§4） |
| `class`、`record` | 没有；写 `class` 会报错，并提示改成 `struct` | GPU 上没有引用类型和 GC |
| null、`?.`、`??`、可空类型 | 没有 | 没有引用 |
| `string` | 只能出现在 attribute 实参和 `nameof` 里 | — |
| 异常、`try`/`catch`/`throw` | 没有 | — |
| `async`/`await`、`yield`、`lock`、`unsafe` | 没有 | — |
| 类型模式（`is T x`） | 没有 | 没有动态类型 |
| 预处理器 | 没有 | §13 |
| 指针 | 暂不支持 | D3D12 SM 6.x 没有 buffer device address；以后支持了再加 |

## 2. 词法与命名

- 源文件扩展名是 `.mshader`，编码 UTF-8。
- 注释、标识符、关键字、运算符和优先级都和 C# 一致。
- 命名规范**完全照 C#**：
  - 类型、方法、属性、公开字段、enum 成员、namespace 用 PascalCase；
  - 局部变量和参数用 camelCase；
  - 接口用 `I` 前缀；
  - 内建函数也遵守这套规范：`Dot`、`Cross`、`Lerp`、`Saturate`、`Sqrt`。
- 例外：向量分量和 swizzle 用小写（§9.4），向量和矩阵的短类型名也是小写（`float3`），和 `float`、`int` 这些关键字保持一致。
- 和资产的关系：材质参数在 shader 和 `.masset` 里**同名**，都是 PascalCase，中间没有转换。Rust 端手写的参数结构用 snake_case，做布局校验时按 0003 §1.2 的规则对应（0005 §9）。

## 3. 类型

### 3.1 标量

C# 的数值关键字：`bool`、`int`、`uint`、`float`、`double`，另有 16 位的 `half`、`short`、`ushort`。

- `half`、`short` 和 `ushort` 是**真正的** 16 位类型，需要 native 16-bit capability（§12.3）。没有 `min16float` 这类宽松精度类型。
- `bool` 只能用在寄存器里。要放进 buffer 的 struct 如果含有 `bool` 字段就报错，应改用 `uint` 或 `[Flags]` enum，这样就不存在 bool 在内存里占几个字节的歧义。
- `long` 和 `ulong` 需要 Int64 capability，以后用 compute 软件光栅写 visbuffer 时会用到 64 位原子操作。`double` 需要 Float64 capability。
- 没有 `byte` 和 `sbyte`，因为 D3D12 没有 8 位算术。打包数据用 `uint` 加位运算，或者用标准库里的打包类型（例如 `Unorm8x4`）。

### 3.2 向量与矩阵

```csharp
float3 p = float3(1, 2, 3);
half4 c;
uint2 id;
float4x4 m;              // 4 行 4 列
float4x3 n;              // 4 行 3 列

// 泛型代码直接写底层类型
T Sum<T, const int N>(Vector<T, N> v) where T : IArithmetic<T> { ... }
```

- 短名是 `{标量}{N}` 和 `{标量}{R}x{C}`，其中 N、R、C ∈ 2..4。它们是 `Vector<T, N>` 和 `Matrix<T, R, C>` 的别名。
- 矩阵在内存里默认按列主序存储，跟 0002 §4 和 marlin-math 一致。

### 3.3 数组

```csharp
float[4] w = [0.1, 0.2, 0.3, 0.4];
float3[N] taps;                    // N 可以是 const 泛型参数
const int[3] Offsets = [-1, 0, 1];
var n = w.Length;                  // 编译期常量
```

- 数组是值类型，赋值时整体拷贝，长度是类型的一部分。
- 用 C# 12 的集合表达式初始化。
- 变长的数据放在 buffer 里（§10），语言里没有变长数组。

### 3.4 struct 与元组

- 所有用户类型都是 `struct`，都是值类型。
- 元组是匿名 struct：`(float3 Wi, float Pdf)`。

### 3.5 enum

```csharp
public enum Lobe : uint { Diffuse, Specular, Haze, Fuzz }

[Flags] public enum Features : uint { Aniso = 1, Fuzz = 2, Haze = 4 }

float w = lobe switch
{
    Lobe.Diffuse => 1,
    Lobe.Specular => 2,
    Lobe.Haze or Lobe.Fuzz => 0.5,
};
```

- 写法和 C# 一样，底层类型默认为 `int`。
- 非 `[Flags]` 的 enum 不允许出现成员以外的值。所以对它做 switch 时，编译器要求覆盖所有成员（这一点比 C# 严格），覆盖全了也不需要再写 `_`。

## 4. 构造与初始化

没有 `new`，构造值的写法有下面几种：

```csharp
var p = float3(1, 2, 3);                    // 构造函数调用
var l = Light(c, 2);
var m = Light { Color = c, Intensity = 2 }; // 对象初始化器
Light k = { Color = c };                    // 目标类型推断
var n = l with { Intensity = 4 };           // 拷贝并修改字段
var z = default(Light);                     // 全零
Light d = default;
```

- 写法上就是把 C# 的 `new T(...)`、`new T { ... }`、`new() { ... }` 去掉 `new`。
- `with` 对所有 struct 都可以用，不限于 record。
- 字段默认值按 **C# 11 语义**：

```csharp
public struct Light
{
    public float3 Color = 1;
    public float Intensity = 1;
}

Light()                  // Color = 1, Intensity = 1
Light { Intensity = 4 }  // Color = 1
default(Light)           // 全零，不应用初始值
```

- 字段初始值同时也是反射里的参数默认值（§14）。

## 5. 成员与可变性

```csharp
public struct Rng
{
    uint State;

    public float Next() { State = Hash(State); return ToUnorm(State); }
    public readonly float Peek() => ToUnorm(Hash(State));
}

public readonly struct Ray
{
    public float3 Origin;
    public float3 Dir;
    public float3 At(float t) => Origin + Dir * t;
}
```

- 规则和 C# 一样：字段和方法默认可以修改 `this`。`readonly` 可以标在字段上、方法上，也可以标在整个 struct 上。
- `in` 参数、`readonly` 字段和 `readonly struct` 的值只能调用 `readonly` 方法。违反时**报错**；C# 遇到这种情况会偷偷拷贝一份，我们不这样做。
- 下面这些成员种类都保留：
  - 属性：`get`、`set` 和表达式体 `=>`，接口里也可以有属性；
  - 运算符重载：`static T operator +(T a, T b)`；
  - 自定义转换：`implicit operator` 和 `explicit operator`；
  - 索引器 `this[...]`，资源和矩阵的 `[]` 就是用它实现的；
  - 静态成员：`static` 方法、属性和 `const`。struct 里没有可变的 static 字段。

## 6. 函数与参数

```csharp
namespace Marlin.Math;

public float Luma(float3 c) => Dot(c, float3(0.2126, 0.7152, 0.0722));

public void Decode(in GBuffer g, out float3 n, ref float w) { ... }

// 调用处
Decode(g, out var n, ref w);
```

- 函数可以写在 namespace 顶层，内建函数也是顶层函数。
- 参数传递完全照 C#：
  - `in` 只读；
  - `out` 必须在函数里赋值；
  - `ref` 可读可写；
  - 调用处必须写上 `out` 或 `ref`。
- 实现方式是 copy-in/copy-out，**不承诺**真正的别名。需要别名语义的场景，比如原子操作，用资源上的方法来做（§12.2）。
- 局部函数和 lambda 见 §9.2。
- **递归**只允许编译期递归：在泛型参数或 const 参数上递减、单态化之后能完全展开的递归是合法的（例如 `Layer<N>` 调用 `Layer<N - 1>`）。运行时递归会报错，并给出形成环的调用链。

## 7. 代码组织

### 7.1 包与 namespace

```csharp
// shaders/substrate/Slab.mshader
namespace Marlin.Substrate;

using Marlin.Math;

public struct Slab { ... }
internal float3 F0FromSpecular(float s) => ...;
```

- **包**是编译单元：一个目录加一份清单，包里的所有文件一起编译，这和 csproj 一样。文件之间不需要 import。
- 每个文件用 file-scoped `namespace` 声明，用 `using` 引入别的 namespace，还支持 `using static` 和 `using` 别名。
- 可见性：
  - `public`：其他包可见；
  - `internal`：同一个包内可见，是默认值；
  - `private`：只在所在类型内可见。
- 编译器在名字解析完成后，报告每个入口**实际用到的源文件**，供 cook 键（0003 §4.1）和热重载使用。
- **标准库导入**：
  - 核心部分在根 namespace `Marlin` 里，每个文件都会隐式导入。核心部分包括标量、向量、矩阵、资源类型、数值接口和常用数学函数（`Dot`、`Lerp`、`Saturate`……）。
  - `Wave`、`Barrier`、`Mesh`、`Debug`、`Derivatives` 这些子 namespace 调用时要写前缀。
  - 包里可以用 C# 10 的 `global using` 给所有文件统一导入。

### 7.2 包清单

清单用 0003 的资产文本格式：

```text
// shaders/substrate/package.masset
ShaderPackage
{
    !Uuid = "…"
    Name = "Marlin.Substrate"
    Dependencies = [<shaders/math:Marlin.Math #…>]
}
```

## 8. 泛型、接口与扩展

### 8.1 泛型

- 写法和约束（`where T : IFoo`）与 C# 一样。
- C# 没有的**值参数**写成 `const int N`：

```csharp
public struct Vector<T, const int N> { ... }
public T Sum<T, const int N>(Vector<T, N> v) where T : IArithmetic<T> { ... }
```

- 所有泛型都在编译期单态化。

### 8.2 关联类型

C# 没有关联类型，这是从参考实现借来的，材质系统这类库需要它：

```csharp
public interface IMaterial
{
    type Params : IParams;
    SurfaceOutput Surface(in SurfaceContext ctx);
}

public struct Rock : IMaterial
{
    public type Params = RockParams;
    // 或者：public struct Params : IParams { ... }
}
```

- 在泛型代码里用 `M.Params` 引用它。
- 关联类型可以带约束，也可以在 `where` 里约束：`where M.Params : IFoo`。

### 8.3 Self 类型

照 C# 的 CRTP 写法，不引入新关键字：

```csharp
public interface IArithmetic<TSelf> where TSelf : IArithmetic<TSelf>
{
    static abstract TSelf operator +(TSelf a, TSelf b);
    static abstract TSelf Zero { get; }
}
```

- 保留 C# 11 的 `static abstract` 和 `static virtual` 接口成员，包括运算符。
- 保留默认接口方法，例如 `IBsdf.Albedo(wo)` 可以有一个默认实现。

### 8.4 扩展

以 C# 14 的 `extension(x)` 块为基础，另外允许在块后面加 `: IFoo`，事后给已有类型补实现接口：

```csharp
extension(float3 c) : ILerpable<float3>
{
    public float Luma => Dot(c, float3(0.2126, 0.7152, 0.0722));
    public static float3 Lerp(float3 a, float3 b, float t) => a + (b - a) * t;
}

extension<T>(Vector<T, 3> v) where T : IArithmetic<T>
{
    public T Sum => v.x + v.y + v.z;
}
```

### 8.5 没有动态分发

- 接口只能用作泛型约束，`IBsdf b;` 这样的声明会报错。
- 以后真需要时，可以加 `dyn IBsdf`，用"已知的 N 个实现组成的 tagged union"来实现。

### 8.6 数值接口

不照搬 .NET generic math 那一整套细粒度接口，只有精简的几层：

| 接口 | 内容 |
|---|---|
| `IArithmetic<T>` | `+ - *`、一元 `-`、`Zero`、`One` |
| `IInteger<T>` | 在 `IArithmetic<T>` 之上加 `/ %` 和位运算 |
| `IFloat<T>` | 在 `IArithmetic<T>` 之上加 `/` 和 `Sqrt`、`Exp`、`Log`、`Sin`…… |
| `IComparable<T>` | `< <= > >=`、`Min`、`Max` |

- 所有标量都实现了对应的接口。
- 向量按分量实现这些接口，比较运算返回 bool 向量。
- 泛型数学代码一般只需要写 `where T : IFloat<T>`。

## 9. 表达式与语句

### 9.1 保留的 C# 特性

- **switch 表达式和模式匹配**：支持常量模式、关系模式、`and`/`or`/`not` 和属性模式，没有类型模式。
- **元组和解构**：`var (wi, pdf) = Sample(...);`
- **`foreach` 和范围**：`foreach (var i in 0..n)`，也可以对定长数组和实现了枚举接口的类型做 `foreach`。
- 其他语句（`if`、`for`、`while`、`do`、`switch`、`break`、`continue`、`return`）都和 C# 一样。像素着色器里丢弃像素用内建函数 `Discard()`。

### 9.2 lambda 与局部函数

```csharp
float Integrate(int n, Func<float, float> f)
{
    float s = 0;
    foreach (var i in 0..n) s += f((i + 0.5) / n);
    return s / n;
}

var r = Integrate(64, x => Exp(-x * k));
```

- 局部函数和 C# 一样。
- 接收 lambda 的参数直接写 C# 的 `Func<...>` 或 `Action<...>`。编译器把它当作隐式的泛型参数，对每个传进来的 lambda 或方法组单态化一份。
- `Func` 和 `Action` 只能用作参数类型，不能存进字段或局部变量，也不能作为返回值。
- lambda 按值捕获，在编译期内联。

### 9.3 字面量

```csharp
float a = 0.5;          // float
half h = 0.5;           // half
var x = 0.5;            // float（不是 double）
double d = 0.5;         // double
float3 c = c0 * 0.5;    // float
```

- 不带后缀的浮点字面量，类型由上下文决定；没有上下文时是 `float`。
- 带后缀的写法 `1.0f`、`1.0h`、`1.0d` 照样可以用。
- 非字面量之间的转换照 C# 规则：加宽是隐式的，收窄要显式。

### 9.4 向量转换与 swizzle

```csharp
float3 c = 1;           // 标量广播：隐式
var d = c * 2;
float3 p = pos4;        // 错误：截断必须显式
float3 p = pos4.xyz;    // ok
```

- 分量名和 swizzle 用小写，`xyzw` 或 `rgba` 都可以，但不能在同一个 swizzle 里混用。
- 向量 `*` 向量是逐分量相乘。

### 9.5 矩阵

```csharp
float4 clip = viewProj * float4(p, 1);
float4x4 mvp = proj * view * model;
var h = CompMul(a, b);          // 逐分量相乘

float m01 = m[0, 1];            // 第 0 行第 1 列
float3 t = m.Column(3).xyz;
float4 r0 = m.Row(0);
```

- 矩阵 `*` 矩阵、矩阵 `*` 向量都是线性代数里的乘法，向量按列向量处理（0002 §4）。
- **没有** `m[i]`：HLSL 里它取的是行，GLSL 里取的是列，写成 `m[i]` 意思不明确。

### 9.6 整数语义

- 照 C# 的 unchecked 语义：
  - 溢出时回绕；
  - 移位次数按位宽取模，这一点 C# 和 GPU 的行为一致；
  - 有符号整数的 `/` 和 `%` 向零截断。
- 整数除以 0 的结果**固定为 0**：除数不是常量时，编译器插入检查。这是因为 SPIR-V 上除零是未定义行为，D3D12 上得到 0xFFFFFFFF；这样规定之后，各平台的结果一致。
- 没有 `checked`。

### 9.7 浮点语义

- 默认是宽松的：允许重结合、fma 收缩和近似倒数。这是 GPU 的常规做法，也和 HLSL 的默认行为一致。
- 但不假设没有 NaN 和 Inf，所以 `IsNan`、`IsInf` 是可靠的。
- `[Precise]` 可以标在函数、局部变量或字段上，被标记的计算不做上面这些变换。用在不允许裂缝的位置计算、补偿求和这类地方。

## 10. 资源与 bindless

0002 定了只有 bindless 这一种模型。资源类型的值**本身就是一个 32 位 bindless 下标**，可以放进 struct、push constants 和 buffer。

| RHI 视图（0002 §2.3） | 只读 | 可写 |
|---|---|---|
| `Uniform` | `Uniform<T>` | — |
| `Structured` | `ReadOnlyBuffer<T>` | `Buffer<T>` |
| `Raw` | `ReadOnlyByteBuffer` | `ByteBuffer` |
| `D2` | `Texture2D<T>` | `StorageTexture2D<T>` |
| `D2Array` | `Texture2DArray<T>` | `StorageTexture2DArray<T>` |
| `Cube` | `TextureCube<T>` | — |
| `Sampler` | `Sampler`、`ComparisonSampler` | — |
| 加速结构视图（0007 §9） | `AccelerationStructure` | — |

```csharp
public struct RockParams
{
    public Texture2D<float4> Albedo;   // 4 字节
    public Sampler Sampler;            // 4 字节
    public float RoughnessScale;
}

var c = p.Albedo.Sample(p.Sampler, uv);
```

- 命名照 C# 的 Span/ReadOnlySpan 规则：不带前缀的可写，带 `ReadOnly` 前缀的只读。不用 `RW`、`Structured`、`ByteAddress` 这类历史名字。
- 反射里，资源字段就是一个 u32 下标（对应 `ShaderView::index()` 或 `Sampler::index()`）；Rust 端对应的字段也写成 `u32`。
- 不支持指针。

### 10.1 采样器表

- `Sampler` 和 `ComparisonSampler` 的下标指向采样器表，资源类型的下标指向资源表。
- 编译器按类型决定从哪张表取（0002 §2.2）。

### 10.2 存储纹理

- `StorageTexture*` 的视图只覆盖单个 mip（0002 §2.3）。
- 类型参数 `T` 只是 shader 这一侧看到的类型，类型里不写格式，格式由视图决定。
  - Vulkan 上要求支持不带格式的读写（`shaderStorageImageReadWithoutFormat`、`shaderStorageImageWriteWithoutFormat`）。
  - 这和"只有 bindless、所有 pipeline 共用一个布局"的模型是一致的。

### 10.3 非一致下标

- 编译器做 uniformity 分析：资源下标在 wave 内可能不一致时，自动加 NonUniform 标记。
- 作者可以手工覆盖：
  - 用 `[Uniform]` 断言某个值一致，消除保守分析带来的多余开销；
  - 用 `NonUniform(x)` 强制标为不一致。

### 10.4 纹理 API

用 C# 的命名参数区分不同的采样方式，不用 HLSL 那一串 `SampleXxx` 方法名：

```csharp
var c = t.Sample(s, uv);                     // 隐式 LOD（导数见 §15.3）
var c = t.Sample(s, uv, lod: 2);
var c = t.Sample(s, uv, grad: (dx, dy));
var c = t.Sample(s, uv, bias: -0.5, offset: int2(1, 0));
float v = depth.SampleCompare(cs, uv, reference);
float4 r = t.Gather(s, uv, Channel.R);
float4 x = t[int2(px)];                      // 读 mip 0 的纹素
float4 y = t.Load(int2(px), mip: 3);
uint2 size = t.Size;
uint mips = t.MipCount;
```

- `lod`、`grad` 和 `bias` 三者互斥，同时写两个就报错。
- 纹理和采样器**保持分离**，各自是一个 4 字节的下标，采样时传入采样器。这样采样器可以在 cook 时单独去重。
- 存储纹理用索引器读写：`dst[px] = c;`。

## 11. 入口与 stage

### 11.1 入口声明

每个 stage 有一个 attribute，stage 的参数写在 attribute 里：

```csharp
[Compute(8, 8, 1)]
public void Blur(...) { }

[Vertex]
public VsOut Vs(...) { }

[Pixel]
public float4 Ps(in VsOut i) { }

[Mesh(128, 1, 1, MaxVertices = 64, MaxPrimitives = 126, Topology = Triangle)]
public void Ms(...) { }

[Amplification(32, 1, 1)]
public void As(...) { }
```

### 11.2 系统值与 stage I/O

系统值写成入口参数或 stage I/O struct 字段上的 attribute。编译器检查它的类型和 stage 是否匹配：

```csharp
public struct VsOut
{
    [Position] public float4 Pos;
    public float2 Uv;
    [Flat] public uint Material;
}

[Vertex]
public VsOut Vs([VertexId] uint vid, [InstanceId] uint iid, [Push] in DrawArgs args) { ... }

[Compute(8, 8, 1)]
public void Blur([DispatchThreadId] uint3 id, [GroupIndex] uint gi, [Push] in BlurArgs args) { ... }
```

- 没有标系统值的 stage I/O 字段，由编译器按字段名分配 location。
- `[Flat]`、`[NoPerspective]` 等插值修饰也写成 attribute。
- 0002 §3 的约定照旧：`[VertexId]` 包含 base vertex，`[InstanceId]` 从 0 开始。

### 11.3 Push constants

```csharp
public struct BlurArgs
{
    public Texture2D<float4> Src;
    public StorageTexture2D<float4> Dst;
    public Sampler Linear;
    public float Radius;
}
```

- 每个入口最多一个 `[Push]` 参数，类型可以是任意 struct。
- 编译器检查它不超过 128 字节（0002 §2.1），并把布局放进反射。
- 各个入口的 push 布局互不影响。

### 11.4 泛型入口

引擎模板写成泛型入口。cook 给出类型实参，编译器 API 用"入口全名 + 类型实参"取出一个单态化的入口。这就是"一份源码生成多个入口"的实现方式：

```csharp
namespace Marlin.Material.Entries;

[Compute(64, 1, 1)]
public void ResolvePixel<M>([DispatchThreadId] uint3 id, [Push] in ResolveArgs a)
    where M : IMaterial
{
    ...
}

// cook: compile("Marlin.Material.Entries.ResolvePixel", ["Game.Rock"])
```

- 材质库 API 的具体形状（`Surface` 怎么拿到参数块等）由材质库的设计来定，不属于语法。

### 11.5 渲染目标

```csharp
public struct PsOut
{
    [Target(0)] public float4 Color;
    [Target(1)] public float2 Velocity;
}

[Pixel]
public PsOut Ps(in VsOut i, [IsFrontFace] bool front) { ... }
```

- 只有一个渲染目标时，直接返回 `float4`，它就是 target 0。
- 有多个渲染目标时，返回一个 struct，字段标 `[Target(i)]`。要写深度时，字段标 `[Depth]`。
- 这和 stage I/O 用的是同一套 attribute 写法。

### 11.6 mesh 与 amplification

```csharp
public struct PrimOut { [Flat] public uint Cluster; }

[GroupShared] ClusterPayload Payload;

[Amplification(32, 1, 1)]
public void As(...)
{
    ...
    Mesh.Dispatch(count, 1, 1, Payload);
}

[Mesh(128, 1, 1, MaxVertices = 64, MaxPrimitives = 126, Topology = Triangle)]
public void Ms([GroupThreadId] uint3 tid, [Payload] in ClusterPayload p,
               MeshOutput<VsOut, PrimOut> o)
{
    o.SetCounts(nv, np);
    o.Vertices[i] = v;
    o.Triangles[j] = uint3(a, b, c);
    o.Primitives[j] = PrimOut { Cluster = id };
}
```

- mesh 的输出是一个入口参数 `MeshOutput<TVertex, TPrimitive>`，它是一个特殊的句柄。
  - 容量来自 `[Mesh]` 的 `MaxVertices` 和 `MaxPrimitives`。
  - 不需要逐图元属性时，写成 `MeshOutput<TVertex>`。
  - 在 `SetCounts` 之前写输出会报错。
- amplification 把 payload 写进一个 `[GroupShared]` 字段，然后调用 `Mesh.Dispatch(x, y, z, payload)`。mesh 入口用 `[Payload] in T` 参数接收。
- 编译器检查：两边的 payload 类型要一致；`Mesh.Dispatch` 在所有控制流路径上恰好调用一次。

## 12. GPU 特有的部分

### 12.1 groupshared

```csharp
namespace Marlin.Post;

[GroupShared] float4[18 * 18] Tile;

[Compute(16, 16, 1)]
public void Blur([GroupIndex] uint gi, ...)
{
    Tile[gi] = ...;
    Barrier.Group();
    ...
}
```

- namespace 级的可变变量只能是 `[GroupShared]` 字段，`const` 不受这个限制。
- 编译器按入口实际用到的字段分配 groupshared 内存。工具函数可以直接访问这些字段。

### 12.2 wave、barrier 与原子操作

内建函数按功能分 namespace 组织：

```csharp
var total = Wave.Sum(v);
if (Wave.IsFirstLane) counters.AtomicAdd(0, total);
Barrier.Group();
uint old = counters.AtomicExchange(1, 5);
Tile.AtomicMax(i, v);
```

- `Wave.*`：`Sum`、`Ballot`、`ReadLane`、`IsFirstLane`……
- `Barrier.*`：`Group`、`Device`……
- 原子操作是资源和 groupshared 数组上的方法。它们都按下标访问，所以不需要对 buffer 元素取真正的 `ref`（§6）。

### 12.3 capability

```csharp
[Requires(Capability.WaveMatrix)]
public void MatMul16(...) { ... }

const if (Target.Has(Capability.WaveMatrix)) { MatMul16(...); }
else { ... }
```

- 编译器从函数体推断出每个函数需要哪些 capability，并沿调用图向上传播。
- public 函数可以标 `[Requires(...)]` 作为契约，推断结果超出声明就报错。
- 当 target 不满足入口需要的 capability 时报错，并给出调用链。
- `Target.Has(...)` 和 `Target.Tier` 等是编译期常量，配合 `const if` 使用（§13.2）。

### 12.4 ray query

只有 inline ray query，没有 RT pipeline（0002 §1）。`AccelerationStructure` 和其他资源一样，是一个 4 字节的下标。

```csharp
var q = RayQuery(accel, ray, RayFlags.CullBackFace, mask: 0xFF);
while (q.Proceed())
{
    if (AlphaTest(q.Candidate)) q.CommitTriangle();
}
if (q.Committed.Kind == HitKind.Triangle)
{
    float t = q.Committed.T;
    float2 bary = q.Committed.Barycentrics;
    uint inst = q.Committed.InstanceIndex;
}

bool blocked = accel.TraceAny(ray, RayFlags.Opaque);
RayHit h = accel.TraceClosest(ray, RayFlags.Opaque);
```

- 完整版是 `RayQuery` 对象，带 candidate 循环，用来写 alpha test 这类逻辑。
- 常见情况用便捷方法 `TraceAny` 和 `TraceClosest`，它们内部用的是同一套实现。
- flags 是普通参数，在编译期是常量时自动折叠。

### 12.5 调试输出与断言

```csharp
Debug.Assert(pdf > 0, $"pdf = {pdf} at {id.xy}");
if (id.x == 0) Debug.Print($"count = {n}");
```

- 只在 Development 构建里生效：输出写进一个调试 buffer，引擎读回后通过 marlin-diag 输出。Shipping 构建里整条语句被删掉。
- 格式用 C# 的插值字符串 `$"..."`。这是字符串在语言里除了 attribute 实参和 `nameof` 以外唯一的用处（§1）。
- 字符串在编译期变成格式 id，GPU 上只写参数。

## 13. 编译期

### 13.1 没有预处理器

- 语言里没有 `#if` 和 `#define`。
- permutation 用静态参数（§13.3）、泛型和编译期常量来表达；平台和档位的差异用编译期常量做条件。
- 这样工具和反射看到的是完整的源码，编译器也能推导出实际依赖的 permutation 维度。

### 13.2 编译期分支

```csharp
if (UseMoss) { ... }                    // 条件是常量，保证折叠；两边都做完整检查

const if (Target.Has(Capability.WaveMatrix))
{
    WaveMatrixMultiply(...);            // 别的 target 上不报错
}
else { ... }
```

- 普通 `if` 的条件是编译期常量时，编译器保证把它折叠掉，但两个分支都要通过完整检查。
- `const if` 要求条件必须是编译期常量，被裁掉的分支不做 target capability 检查（类似 C++ 的 `if constexpr`）。

### 13.3 静态参数

```csharp
public struct Rock : IMaterial
{
    [Group("Base")] public Texture2D<float4> Albedo;
    [Range(0, 2)]   public float RoughnessScale = 1;
    public const bool UseMoss = false;
    ...
}
```

- 带默认值的 `const` 字段就是静态参数。它的含义比 C# 多了一层：实例化时可以覆盖的编译期常量。
- 反射把它列为静态参数，它不占参数块的空间。
- 改变它的值会改变 permutation。

### 13.4 编译期函数与导出

```csharp
public const ClosureInfo Flatten<M>() where M : IMaterial { ... }

[Compute(64, 1, 1)]
public void ResolvePixel<M>(...) where M : IMaterial
{
    [Export] const ClosureInfo Topology = Flatten<M>();
    ...
}
// 反射：exports["Topology"] = { Count = 2, Features = Aniso | Fuzz }
```

- 用 `const` 修饰的函数可以在编译期求值。它只能调用其他 `const` 函数，不能访问资源、groupshared、系统值和 `Debug.*`。C# 没有这个特性，它借自 C++ 的 constexpr。
- `const` 字段和局部 `const` 的初始化表达式可以调用 `const` 函数。
- 标了 `[Export]` 的 `const`，求出的值会写进反射（0005 §6.2），用来导出 Substrate 拓扑这类"库自己定义的编译期信息"。

## 14. Attribute 与反射

```csharp
[AttributeUsage(AttributeTargets.Field)]
public struct RangeAttribute : IAttribute
{
    public float Min, Max;
    public RangeAttribute(float min, float max) { Min = min; Max = max; }
}

[Range(0, 2)] public float RoughnessScale = 1;
```

- attribute 是实现了 `IAttribute` 的 struct。和 C# 一样，用的时候可以省掉 `Attribute` 后缀，用 `[AttributeUsage]` 限制它能标在什么上。
- 实参必须是编译期常量，并且要做类型检查。
- 反射原样输出所有用户 attribute。
- 内建 attribute（`Compute`、`Push`、`Differentiable`……）也用同一套机制声明，只是语义由编译器实现。

## 15. 自动微分

这部分借自参考实现，改写成 C# 的风格。前向模式和反向模式的语法现在都定下来，编译器先实现前向模式。

### 15.1 标记

```csharp
[Differentiable]
public float3 Shade(float3 n, float3 l, [NoDiff] float roughness)
{
    var ndl = Saturate(Dot(n, l));
    return ndl * Albedo(Detach(n));
}

public struct Slab : IDifferentiable<Slab> { ... }
```

- `[Differentiable]` 标记可微函数。可微函数调用不可微函数时，编译器报错。
- `[NoDiff]` 标记不参与微分的参数；`Detach(x)` 在表达式里截断导数。
- 可微类型实现 `IDifferentiable<TSelf>`，它带一个关联类型 `type Differential`。标量、向量、矩阵和数组都内建实现了这个接口。
- 用户 struct 声明 `: IDifferentiable<Slab>` 后，编译器**逐字段合成** `Differential` 和它的运算（`Zero`、`Add`、`Scale`）：

```csharp
public struct Slab : IDifferentiable<Slab>
{
    public float3 DiffuseAlbedo;
    public float Roughness;
    [NoDiff] public Features Flags;
    public Texture2D<float4> Lut;   // 自动跳过
}
// 合成出：struct Slab.Differential { float3 DiffuseAlbedo; float Roughness; }
```

- 不可微的字段（整数、资源、enum）和标了 `[NoDiff]` 的字段会被跳过。
- 特殊类型可以手写 `Differential` 和这些运算，覆盖合成的版本。

### 15.2 求导调用

```csharp
var r = ForwardDiff(Shade)(DiffPair(n, dn), DiffPair(l, float3(0)), rough);
float3 value = r.Value;
float3 dShade = r.Derivative;

var dn2 = DiffPair(n);
var dl = DiffPair(l);
BackwardDiff(Shade)(ref dn2, ref dl, rough, dOut);
// dn2.Derivative 和 dl.Derivative 就是梯度
```

- `ForwardDiff(f)` 和 `BackwardDiff(f)` 是内建的高阶函数，参数是一个方法组，返回导数函数。
- `DiffPair<T>` 包含 `Value` 和 `Derivative`。

### 15.3 隐式的屏幕空间导数

- `Ddx`、`Ddy`、`Fwidth`，以及隐式选择 LOD 的 `Sample`，在任何 stage 都能用。
  - pixel stage 里用硬件 quad 导数；
  - compute 里（visbuffer resolve、光线命中点），编译器从入口声明的导数源出发，对用到的代码**自动**做前向微分。导数源是 UV 和位置的解析导数，或者 ray cone。
- 材质代码**不需要**标 `[Differentiable]`。
- 不可微的操作导数视为 0，并给出警告。
- **导数源**用 `Derivatives.Seed(x, dx, dy)` 标记。入口或材质库的适配器调用它，它返回的值类型仍然是 `T`：

```csharp
var uv = Derivatives.Seed(uv0, dx: dUvDx, dy: dUvDy);
var wp = Derivatives.Seed(worldPos, dx: dPdx, dy: dPdy);
var ctx = SurfaceContext { Uv0 = uv, WorldPos = wp, ... };
var o = material.Surface(ctx);   // 内部的 Ddx(...) 和 Sample(...) 都能用
```

- 从导数源算出来的任何表达式都能求 `Ddx`/`Ddy`，采样也能隐式选 LOD。没有 seed 的值导数为 0。
- 编译器只传播确实被用到的导数。
- pixel stage 里不需要 seed，用的是硬件导数。

### 15.4 自定义导数

```csharp
[ForwardDerivative(nameof(SafeSqrtFwd))]
public float SafeSqrt(float x) => Sqrt(Max(x, 1e-8));

public DiffPair<float> SafeSqrtFwd(DiffPair<float> x) => ...;

// 给别人库里的函数（包括内建函数）事后补上导数
[ForwardDerivativeOf(nameof(Marlin.Math.Acos))]
internal DiffPair<float> AcosFwd(DiffPair<float> x) => ...;
```

- 反向模式对应的是 `[BackwardDerivative]` 和 `[BackwardDerivativeOf]`。
- 编译器会检查导数函数的签名是否和原函数匹配。

## 16. 内存布局

- 只有一种布局：**scalar 布局**，也就是自然对齐。比如 `float3` 占 12 字节、4 字节对齐。
- 同一个 struct 在任何地方都是同一种布局，Rust 端也只需要手写一份对应的 struct（0005 §9）。
- `Uniform<T>` 另外要求字段不能跨 16 字节行，这是 D3D12 cbuffer 的规则。不满足时报错，并建议怎么重排字段。
- 需要精确控制时，可以用 C# 的 `[StructLayout(LayoutKind.Explicit)]` 和 `[FieldOffset(n)]`。
- 资源字段占 4 字节（§10）；`bool` 不能放进 buffer（§3.1）。

## 17. 待定

- 编译器 API 和反射：见 0005。
- 标准库的完整清单：各个 namespace 里有哪些函数，打包类型有哪些。
- 采样反馈属于材质库的 `SurfaceContext` 采样函数，不属于语法。
