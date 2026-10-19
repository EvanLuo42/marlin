# 0018 — Marlin script：VM 语言

状态：草案 · 2026-10-18

前置：
- [0004](0004-shader-language.md)：Marlin shader 语法（数学类型、借来的语法）
- [0015](0015-language-frontend.md)：C# 系语言的共享前端（超集文法、`Dialect`、`hir_def`、query 引擎）

## 0. 决定摘要

总原则：**语法和 C# 几乎 1:1，有 GC，跑在寄存器式解释器上**。除了 gameplay，也要能写一些 compute 代码：这部分走一个受检的数据并行子集，由解释器按 SIMD lane 批量执行。性能目标是"解释器能达到的最好水平"，所以语义上凡是会挡住解释器优化的地方（运行时生成代码、弱可空性、UTF-16、开放世界）都改掉了。

| 维度 | 决定 | § |
|---|---|---|
| 名字 | Marlin script，扩展名 `.mscript` | 1 |
| 执行 | **只有解释器**：寄存器式、按类型拆开的宽指令、尾调用串接分派、超级指令、quickening、inline cache；不做 JIT，也不做 AOT | 9 |
| compute | kernel 由解释器**按 lane 批量执行**：一条指令处理一批元素，分支用掩码，分派开销按 batch 宽度摊薄 | 7.4 |
| GC | 精确、分代：nursery 是 bump 分配加复制回收（STW，亚毫秒）；老年代是 Immix 式 mark-region，标记在 worker 上并发进行（SATB 写屏障），只做机会性的局部整理；按帧预算调度 | 10 |
| 线程 | 托管堆只在一个 gameplay 线程上修改；并行只通过受检 job 进行 | 7 |
| 并行数据 | 主要用非托管容器（`NativeArray<T>` 等）并声明读写访问，由调度器检查冲突；gameplay 线程 `await` job 期间，job 可以只读访问冻结的托管堆 | 7.2 |
| kernel 写法 | 标量写法（`Parallel.For`、`IJobFor`），编译器自动做 lane 批量化 | 7.3 |
| 异常 | 保留 `try`/`catch`/`finally`，用零开销异常表；kernel 里不能 throw 或 catch | 4 |
| 可空性 | 可空引用类型是**硬性的类型规则**，不只是警告 | 3.3 |
| 泛型 | 具体化：引用类型实参共享一份代码，值类型实参在 cook 时各自特化 | 3.4 |
| 值类型 | struct、ref 局部变量/返回值、`in`/`out`/`ref`、`Span<T>`、`ref struct`、`stackalloc`；没有 `unsafe` | 3.2 |
| 字符串 | 不可变 UTF-8；`char` 是 32 位 Unicode 标量值 | 3.5 |
| 异步 | `async`/`await` 加引擎的帧调度器；挂起时保存寄存器窗口，不生成状态机类 | 5 |
| 借自 shader | namespace 级函数、`const` 泛型参数和定长数组 `T[N]`、必须写 file-scoped `namespace`、带接口实现的 `extension` 块 | 2.3 |
| 去掉的 C# 特性 | `dynamic`、`Reflection.Emit`、终结器、LINQ 查询语法、`lock`/`Thread`/`Monitor`、`unsafe`、P/Invoke 等。保留 `goto`、`decimal`、只读的 `Expression<T>`、`yield return`；`Interlocked` 只用在 kernel 里 | 2.2 |
| 字符串下标 | `Length` 是字节数，没有 `s[i]` | 3.5 |
| 集合与哈希 | `Dictionary`/`HashSet` 按插入顺序遍历；`object.GetHashCode` 是惰性分配的序号 | 11.1、15 |
| 装箱 | 隐式装箱允许但警告，`[NoBoxing]` 里是错误 | 3.1 |
| 非可空数组 | 元素非可空的数组必须给出全部元素 | 3.3 |
| 整数除零 | 抛 `DivideByZeroException` | 3.6 |
| 浮点 | 始终严格 IEEE，没有 fast-math | 15 |
| 引用宽度 | 32 位压缩引用，托管堆上限 32 GB | 8 |
| 弱引用 | 只在确定的帧边界、完成一轮老年代标记后批量清空 | 10.4 |
| 未捕获异常 | 终止这一次进入脚本的调用，记录日志后继续 | 4 |
| kernel 函数 | 包内推断；跨包调用的 public 函数要标 `[Kernel]` 作为契约 | 7.2 |
| job 冲突检查 | kernel 规则在编译期保证；容器级的跨 job 冲突只在开发构建里于运行时检查 | 7.2 |
| 共享数学 | 双语言包 `.mcommon`：只允许两种 dialect 的交集，两个编译器各自编译；内建数学库也是这种包 | 11.2 |
| mod | 受限的扩展点：只能实现 `[Moddable]` 类型、只能调用 `[ModApi]`，加载时验证字节码，可以限制 CPU 和内存 | 9.7 |
| 超级指令 | 用引擎自带的 benchmark 和 sample 统计，表随引擎版本固定；设计上不排除以后按游戏定制 | 9.5 |
| lane 宽度 | 每个 kernel 在加载时选 `W`（默认 SIMD 宽度 ×4，寄存器压力大时减半），`[LaneWidth(n)]` 可以覆盖 | 7.4 |
| 热重载的挂起帧 | 挂起的帧用旧字节码跑完当前这次调用 | 13 |
| 数学 | 和 Marlin shader 同名、同语义、同布局（`float3`、`float4x4`、`Dot`、swizzle、`M * v`） | 11.2 |
| 互操作 | 在 Rust 侧加标注，构建时生成脚本侧的声明和寄存器级的调用 thunk；调用引擎函数就是一条 `call.native` 指令 | 12 |
| 引擎对象 | 访问已销毁的对象抛 `ObjectDestroyedException`，用 `IsAlive` 检查；不重载 `==`。对象模型等 gameplay 框架设计时再定 | 12.3 |
| 热重载 | 保留状态：函数体立即替换；类增删字段时迁移堆上已有的对象；只在帧边界切换 | 13 |
| 反射 | 完整的只读反射（枚举、读写字段、调用方法），没有 Emit | 14 |
| 确定性 | 跨平台位级确定：严格 IEEE、自己实现超越函数、确定的哈希和遍历顺序、固定的归约顺序 | 15 |
| 世界 | shipping 是封闭世界：整程序去虚化、内联、裁剪；开发期用不做这些优化的链接方式；mod 以后按沙箱包另议 | 9.6 |
| 越界检查 | 所有构建都开，编译器能证明安全时消除 | 3.6 |
| 标准库 | 按 BCL 命名的精简子集，加上游戏向的部分：Native 容器、Job/Parallel、帧调度、确定性数学、对象池；方法链 LINQ 用零分配实现。没有文件、网络和线程 API | 11 |

## 1. 定位

- 写 gameplay 逻辑，以及偶尔需要的 CPU compute（大批量的模拟、寻路预处理、程序化生成）。很重的 compute 仍然交给 GPU 的 Marlin shader。
- 所有平台都只跑解释器。主机禁止运行时生成代码，所以 JIT 用不了；AOT 不做。因此运行时和字节码的设计都只为解释器服务，不需要给原生代码留余地。
- 源码 `.mscript`，按包组织，规则和 shader 一样（0015 §3）。包清单是 `ScriptPackage` 资产，由宿主解析。
- cook 产物是一个已经链接好的字节码镜像（§9.6）；开发期由编辑器里的编译器增量产出、热重载进去。

## 2. 语法：C# 与 Dialect

解析走 0015 的超集文法。Marlin script 的 `Dialect` 定义在 `compiler/vm/syntax` 里。

### 2.1 保留的 C#

照 C# 原样（以 C# 14 为准）：`class`（单继承、`abstract`、`sealed`、`virtual`/`override`）、`interface`（含默认接口方法、static abstract 成员）、`struct`、`record`/`record struct`、`enum`、委托、事件、lambda（含 `static` lambda）、局部函数、属性、索引器、运算符重载、自定义转换、元组与解构、模式匹配与 switch 表达式、`is`/`as`、`?.`/`??`/`??=`、`using` 语句和声明、`foreach`、范围与索引（`a[1..^1]`）、集合表达式、插值字符串、`nameof`、`typeof`、`default`、`checked`/`unchecked`、`params`、命名参数和可选参数、`protected` 和 C# 的默认可见性。

### 2.2 去掉的 C#

parser 照样解析（以便给出好的提示），validation 报错：

| 构造 | 原因 |
|---|---|
| `dynamic`、`Reflection.Emit`、`Expression<T>.Compile` | 运行时生成代码；破坏封闭世界 |
| 终结器 `~T()` | 让 GC 复杂化且时机不确定；清理用 `IDisposable`/`using` |
| LINQ 查询语法 `from … select` | 方法链形式已经够用（§11.3） |
| `lock`、`Thread`、`Monitor`、`volatile` | 托管堆是单线程的；并行只走 job（§7） |
| 托管代码里的 `Interlocked` | 同上；kernel 里可以对 Native 容器的元素用（§7.2） |
| `unsafe`、指针、`fixed`、`stackalloc` 到指针 | 脚本永远不能踩坏内存；`Span<T>` 够用 |
| `extern`/P/Invoke、COM | 互操作只走生成的绑定（§12） |
| `Finalize`、`GC.KeepAlive` 等 GC API | 只保留 `WeakReference<T>`（§10.4）和 `GC.Collect`（开发用） |

另外几项保留，但有说明：
- `goto`、`goto case`：照 C#。
- `decimal`：照 C#（128 位十进制）。纯软件实现，天然确定；它是 unmanaged struct，kernel 里也能用。
- `Expression<T>`：lambda 可以转成只读的表达式树（编辑器绑定、查询 DSL 用），可以遍历、可以解释执行，但没有 `Compile`。
- `yield return`：照 C#。实现和 `async` 共用挂起帧的机制（§5）；`foreach` 直接消费、且迭代器不逃逸时，编译器把迭代器内联成循环，不分配。

### 2.3 从 Marlin shader 借来的

让两种语言在同一份代码里写起来一样：
- **namespace 级函数和字段**：`namespace Game; public float Damp(float a, float b, float k) => …;`
- **`const` 泛型参数和定长数组**：`struct Ring<T, const int N> { T[N] items; }`。`T[N]` 是值类型，长度是类型的一部分，和 shader 的 `float[4]` 同一个东西。普通的 `T[]` 仍然是 C# 的托管数组。
- **必须写 file-scoped `namespace`**。
- **`extension` 块**（C# 14），并且可以 `: IFoo` 给已有类型补接口实现。

## 3. 类型系统

### 3.1 值类型和引用类型

照 C#。值类型不装箱的规则比 C# 更严格：
- 值类型通过接口约束调用方法时（`where T : IFoo`）走特化，不装箱（§3.4）；
- 隐式装箱（把 struct 赋给 `object` 或接口类型的变量）允许但会给出警告，`[NoBoxing]` 的作用域里是错误。

### 3.2 ref 与 Span

`ref`/`in`/`out` 参数、ref 局部变量、ref 返回值、`ref struct`、`Span<T>`/`ReadOnlySpan<T>`、`stackalloc`（结果是 `Span<T>`），转义规则照 C#（`scoped`、ref safety）。解释器里 ref 是"基址 + 偏移"两个寄存器（§9.2），GC 能扫描并在移动对象时修正它。

### 3.3 可空性

语法照 C#（`Player?`），语义是硬性的：
- 非可空引用类型的值永远不是 `null`。从可空类型转到非可空类型要经过 `!`、模式匹配或流分析证明，`!` 在运行时会检查。
- 字段的初始化规则比 C# 严格：构造函数结束时，每个非可空引用字段都必须已经赋值（对 `this` 做确定赋值分析）；构造期间把 `this` 交出去时，交出去的那一刻也要求已经赋值。
- 数组 `new Player[n]` 的元素类型必须可空；元素非可空时必须给出全部元素：`new Player[] { … }`、集合表达式、`Array.Create(n, i => …)`。值类型和可空引用类型的数组照常 `new T[n]`。
- 结果：解释器只在 `?.`、`!` 和可空类型的解引用处做 null 检查，其余的成员访问都不检查。

### 3.4 泛型

照 .NET 的具体化：
- 引用类型实参共享同一份字节码，类型参数从隐藏的类型字典里取（`typeof(T)`、`new T[]`、`is T` 用得到）；
- 值类型实参在 cook 时各自生成特化的字节码，寄存器布局是确定的，不装箱；
- `const` 泛型参数总是特化；
- 支持 C# 的约束，包括 `static abstract` 接口成员（泛型数学用得到）。

### 3.5 字符串与 char

- `string` 是不可变 UTF-8，和 Rust 的 `&str` 零拷贝互通；字面量放在常量区，不分配。
- `char` 是 32 位 Unicode 标量值（Rust 的 `char`）。
- `s.Length` 是字节数；`s[i]` 不提供（validation 报错，建议用 `foreach (char c in s)`、`s.Bytes[i]` 或 `s.Runes`）。切片 `s[a..b]` 按字节，切在字符中间时抛异常。
- `StringBuilder` 和插值字符串用零分配的处理器（C# 10 的 interpolated string handler），直接写进 UTF-8 缓冲区。

### 3.6 整数与检查

- 整数默认是 unchecked 语义，`checked` 照 C#。
- 整数除零抛 `DivideByZeroException`（和 C# 一样；shader 里是 0，两者不同）。
- 数组和 Span 的越界检查在所有构建里都开着；编译器在能证明安全时消除（`for (i = 0; i < a.Length; i++)`、kernel 的下标、`foreach`）。

## 4. 异常

- `try`/`catch`/`finally`/`throw`、异常过滤器 `when`，照 C#。异常类型继承 `Exception`。
- 实现用**零开销异常表**：每个函数有一张 `(pc 区间 → handler)` 表，不抛异常时没有任何开销。抛出时解释器沿寄存器栈展开。
- 脚本里没被捕获的异常只终止当前这次从引擎进入脚本的调用（一次 `Update`、一个事件回调），经 marlin-diag 记录后引擎继续运行。开发构建可以配置成在抛出处断下来。
- kernel 里不能写 `throw`/`try`。kernel 里的运行时错误（越界、除零）终止整个 job，作为异常在 `await` 这个 job 的地方抛出。

## 5. async/await

- 语法照 C#。`Task`/`Task<T>` 换成引擎的 `Task`（值类型的 awaitable，类似 `ValueTask`/UniTask），可等待的东西由引擎提供：`await Frame.Next()`、`await Frame.Physics()`、`await Time.Delay(2.0)`、`await asset.Load()`、`await job`。
- **不生成状态机类**。`async` 函数在解释器里有自己的帧；挂起时把这个帧的寄存器窗口搬到池化的帧块里，恢复时再搬回来。帧块按大小分级放在对象池里，稳态下不分配。
- 帧调度器在 gameplay 线程上，按引擎规定的帧阶段恢复挂起的 `async` 函数，顺序是确定的（§15）。
- 取消用 `CancellationToken`；引擎对象销毁时自动取消挂在它身上的 `async` 函数（具体的绑定方式随对象模型定，§12.3）。

## 6. 委托、事件、lambda

照 C#。捕获变量的 lambda 分配闭包对象；`static` lambda 和不捕获的 lambda 是缓存的单例，不分配。委托调用是一条 `call.delegate` 指令。

## 7. 并行：受检 job

### 7.1 线程模型

- 托管堆只有 **gameplay 线程**能分配和修改。GC 因此不需要在多个线程之间同步 safepoint，写屏障也不需要原子操作。
- 并行只通过 job 进行。job 跑在引擎的 job 系统上（和渲染、物理共用 worker）。

### 7.2 job 能访问的数据

kernel（job 的函数体）是 Marlin script 的一个**受检子集**：
- 不能分配托管对象（`new` 一个 class、装箱、闭包、字符串拼接都是错误），不能 throw/catch，只能调用 kernel 函数：
  - 同一个包里的函数由编译器推断是否满足 kernel 规则；
  - 别的包里的函数必须标 `[Kernel]`。这是 public API 的契约（和 shader 的 `[Requires]` 同一个思路）：标了以后编译器检查它满足规则，改实现时不会悄悄破坏别的包里的 kernel；
- 可以对 Native 容器的元素用 `Interlocked`（`Add`、`CompareExchange`、`Max`……）。注意原子操作的结果顺序依赖调度，用它算出来的值不在确定性保证（§15）之内，除非运算满足交换律和结合律（整数加、max）；
- 数据来源有三类：
  1. **非托管容器**：`NativeArray<T>`、`NativeList<T>`、`NativeHashMap<K, V>`、引擎的组件存储，`T` 必须是 unmanaged struct。job 声明对每个容器的访问：`[ReadOnly]`、`[WriteOnly]`、读写；
  2. **冻结的托管堆**：gameplay 线程 `await` 一个 job 时，托管堆被冻结（不分配、不写、不 GC），这期间 job 可以**只读**访问托管对象。编译器检查 kernel 对托管引用只有读操作；
  3. job struct 自己的值字段（按值拷贝进 job）。
- 检查分两层：
  - kernel 内的规则（不分配、只写 `[i]`、对托管堆只读）是编译期保证的，所有构建都成立；
  - 容器级的跨 job 冲突（两个可能并发的 job 一个写、另一个读写同一个容器，同 Unity 的 safety system）在开发构建里于 `Schedule` 时检查，报告缺了哪条依赖；shipping 里去掉。漏掉的冲突在 shipping 里是数据竞争，但仍然不会越界或破坏 GC 堆。
- 并行写同一个容器的不同元素：`Parallel.For` 的 `i` 只能写 `[i]`（编译器检查下标就是循环变量）；其他写法用 `NativeArray<T>.ParallelWriter`（原子追加）或者归约（§7.5）。

### 7.3 写法

```csharp
namespace Game.Boids;

public void Step(NativeArray<Boid> boids, NativeArray<float3> forces, float dt)
{
    Parallel.For(boids.Length, i =>
    {
        ref var b = ref boids[i];
        b.Velocity += forces[i] * dt;
        b.Position += b.Velocity * dt;
    });
}

// 或者 struct job，可以存起来、组合依赖
public struct Integrate : IJobFor
{
    [ReadOnly] public NativeArray<float3> Forces;
    public NativeArray<Boid> Boids;
    public float Dt;

    public void Execute(int i) { … }
}

var handle = new Integrate { … }.Schedule(boids.Length, dependsOn: prev);
await handle;
```

- `Parallel.For` 的 lambda 必须是 kernel，它捕获的变量按值拷贝（不分配闭包：编译器把它降低成一个隐式的 job struct）。
- `Parallel.For` 是同步的：gameplay 线程在上面等待并参与执行；`Schedule` 返回 `JobHandle`，可以 `await`。

### 7.4 lane 批量执行

编译器把 kernel 编译成两份字节码：普通的标量版本（调试用、批量的余数也用它），和**批量版本**。批量版本的寄存器是 `W` 个 lane 的 SoA 数组，每条指令一次处理 `W` 个元素。

`W` 的选择：
- 字节码里的批量版本与 `W` 无关，加载时按平台和 kernel 选定 `W`；
- 默认 `W` 是平台 SIMD 宽度的 4 倍（AVX2 的 f32 是 32，NEON 是 16）；kernel 的寄存器总量（寄存器数 × `W` × 4 字节）超过 L1 的一个预算时，`W` 逐次减半；
- `[LaneWidth(n)]` 可以覆盖；
- 结果不依赖 `W`：lane 之间没有交互（归约见 §7.5），所以换 `W` 不影响确定性。

执行方式：

- 指令分为 uniform（所有 lane 的值相同，比如 `dt`、循环外的常量）和 varying 两类。编译器做 uniformity 分析（和 shader 编译器同一个思路），uniform 的计算只算一次；
- 分支变成掩码：`if` 的两边都执行、按掩码写回；全部 lane 走同一边时跳过另一边（一条 `any`/`all` 测试）；
- 连续访问 `a[i]` 变成整块 load/store，不连续的变成 gather/scatter；
- 循环按 lane 的活跃掩码一直跑到所有 lane 都退出；
- handler 内部是原生的 SIMD 循环（`core::simd` 或者 per-arch intrinsics），分派开销按 `W` 摊薄。

### 7.5 归约与确定性

- 归约用 `Parallel.Sum`、`Parallel.Reduce(n, identity, map, combine)`；
- 为了确定性（§15），工作的切分方式只取决于元素个数，不取决于 worker 数量和调度；归约树的形状固定。所以多线程的结果和单线程的结果位级相同。

## 8. 对象布局

- 对象头 8 字节：类型指针的压缩下标（32 位）+ GC 标记位、hash 状态等（32 位）。没有 sync block。
- `GetHashCode` 的默认值是惰性分配的序号（§15）：第一次调用时从一个全局计数器取号，hash 状态记为"已取号"，号先放在按对象地址索引的旁表里（GC 移动对象时更新）；对象下次被移动时在新位置末尾多分配 4 字节存这个号，状态改为"号在对象里"，并从旁表删除。没调用过 `GetHashCode` 的对象不多占任何空间。
- 字段按大小排序紧排；引用字段集中在一起，方便 GC 扫描。shipping 封闭世界下字段偏移直接写进字节码（§9.6）；开发期按字段 id 链接，第一次执行时 quicken 成偏移。
- 数组头：对象头 + 长度（32 位）。
- 引用压缩：托管堆放在一块预留的 32 GB 虚拟地址空间里，引用是 32 位的偏移（左移 3 位），所有引用字段都是 4 字节。

## 9. 执行模型：解释器

### 9.1 目标

在只有解释器的前提下拿到最好的性能。gameplay 代码的目标是原生代码的 1/3 到 1/8；kernel 的批量版本目标是接近原生 SIMD。

### 9.2 寄存器与帧

- 每个函数有固定数量的虚拟寄存器，每个寄存器 8 字节；struct 占连续的若干个寄存器，`float4` 占 2 个。寄存器的类型在每个 pc 都是静态已知的，所以 GC 的栈图是现成的，按 safepoint 存一张位图。
- 寄存器栈是一块连续内存，调用时新帧的寄存器窗口紧接在调用者的参数寄存器后面：**参数不拷贝**，调用者把实参算进自己窗口末尾的寄存器，被调用者直接把它们当作自己的前几个寄存器。
- ref 是两个寄存器：托管对象的基址（GC 能看到、能修正）和偏移；指向栈或非托管内存时基址是 0。

### 9.3 指令

- 三地址、按类型拆开：`add.i32 d, a, b`、`add.f32x4 d, a, b`、`ld.field.i32 d, obj, off`。操作数默认 8 位寄存器下标，`wide` 前缀把它们扩到 16 位。
- 立即数形式和常用组合有专门的指令：`add.i32.imm`、`cmp.lt.i32.br`、`ld.field.f32x3`；
- 数学库的大部分函数（`Dot`、`Cross`、`Normalize`、`Lerp`、矩阵乘法、四元数运算……）是单条指令，一次分派做完一整块工作；
- 调用：`call`（静态目标）、`call.virt`（vtable 槽）、`call.iface`（带 inline cache）、`call.delegate`、`call.native`（引擎函数，§12）。

### 9.4 分派

- 每条指令的 handler 是一个 Rust 函数，结尾用 `become`（nightly 的 `explicit_tail_calls`）跳到下一条指令的 handler。热状态（pc、寄存器窗口基址、分派表）作为参数一直放在机器寄存器里。每个 handler 末尾各有一个间接跳转，分支预测器可以按 handler 分别学习。
- 后备方案：如果某个平台的工具链对 `become` 生成的代码不理想，就在那个平台上把分派循环改成一个大 `match` 加手工的跳转表；指令语义只写一份（宏生成两种形态）。

### 9.5 编译期和运行期的优化

在编译器里（cook 时，或开发期的增量编译时）：
- 寄存器分配（合并 move，减少寄存器数量，缩小帧）；
- 内联（小函数、属性访问器、shipping 里所有能去虚化的调用）；
- 越界检查消除、null 检查消除（§3.3）；
- **超级指令**：用引擎自带的 benchmark 和 sample 项目做动态统计，找出高频的指令序列，合成一条指令。超级指令表由工具生成、提交进仓库、随引擎版本固定，所有游戏共用同一个解释器。设计上不排除以后按游戏的 profile 定制超级指令表（那需要为该游戏重新编译运行时），现在不做。

在运行时：
- **quickening**：开发期按名字或 id 链接的指令（字段访问、调用）第一次执行后，原地改写成直接偏移和直接目标的形式；
- **inline cache**：`call.iface` 和 shipping 里没能去虚化的 `call.virt` 带单态/多态的 inline cache。

### 9.6 封闭世界与链接

- **shipping**：cook 时知道全部代码。整程序分析做去虚化（没有被 override 的 `virtual`、只有一个实现的接口）、跨包内联、未用代码和元数据裁剪；字段偏移、vtable 槽、静态字段地址都直接写进字节码。产物是一个已经链接好的镜像，加载时只做重定位，不做验证。
- **开发期**：每个包单独编译，指令里存符号 id，按需 quicken。这样热重载（§13）只需要替换改了的包。

### 9.7 mod

mod 是受限的扩展点，不破坏游戏本体的封闭世界优化：
- 游戏把允许 mod 扩展的接口和基类标 `[Moddable]`，把允许 mod 调用的 API 标 `[ModApi]`。cook 时，`[Moddable]` 类型上的虚调用和接口调用保持动态（不去虚化），其余照常整程序优化；
- mod 包单独编译成开发期那种按符号 id 链接的字节码，只能引用 `[ModApi]` 的符号；
- 加载时**验证**字节码：类型安全、寄存器类型一致、跳转目标合法、只引用允许的符号；验证失败拒绝加载；
- 运行时可以给每个 mod 设 CPU 预算（解释器在回边和调用处计数）和托管内存上限，超出时终止那次调用并报告；
- mod 不能写 kernel 之外的并行代码，规则和本体一样。

## 10. GC

### 10.1 结构

- **精确**：寄存器的栈图、对象的引用位图、ref 寄存器的基址，都是静态已知的。
- **nursery**：每帧的短命对象从 bump 指针分配（一次比较加一次加法）。nursery 满了，或者引擎在帧边界要求时，做一次 STW 的复制回收，把存活对象晋升到老年代。nursery 的大小按目标暂停时间自适应，目标是亚毫秒。
- **老年代**：Immix 式的 mark-region（32 KB block、128 B line），标记**在 worker 线程上并发进行**，gameplay 线程继续运行（SATB 写屏障记录被覆盖的旧引用）。回收按 line 粒度复用空间；碎片严重的 block 在下一次 nursery 回收时机会性地疏散。只有很短的 STW：开始标记（扫根）和结束标记（处理 SATB 缓冲）。
- **大对象**：超过阈值的对象（大数组）放在单独的大对象空间，不移动。
- 写屏障：分代的 card marking 加并发标记期间的 SATB，都是非原子的（只有一个 mutator 线程）。

### 10.2 调度

- 引擎每帧给 GC 一个预算和一个时机（通常是帧尾的空闲时间）；nursery 回收尽量放在帧边界做。
- 冻结期间（job 只读访问托管堆，§7.2）不做 GC，也不分配；必要时 gameplay 线程等 job 完成后再回收。
- 开发构建里有 GC 统计接入 stats overlay：每帧分配量、暂停时间、晋升量。

### 10.3 引擎持有的引用

引擎持有脚本对象用句柄表（GC 根），不直接存指针；句柄在对象移动时不变。

### 10.4 弱引用

`WeakReference<T>` 只在**确定的点**批量清空：引擎指定的帧边界，且该帧之前完成了一轮老年代标记。nursery 回收不清空弱引用（目标对象在 nursery 里死掉时，弱引用记下它，等下一个确定点再清）。

开启确定模式（锁步联机、回放）时，老年代的标记轮次由帧号驱动（比如每 N 帧开始一轮），而不是由内存压力驱动，所以弱引用清空的时刻在所有机器上相同。内存压力过大时仍然会强制回收，但被强制的那一轮不清空弱引用。

## 11. 标准库

### 11.1 范围

按 BCL 命名的精简子集加游戏向的部分，都在 `Marlin` 和 `System` 下：
- **核心**：`object`、基元类型、`string`、`Span<T>`、`Math`/`MathF`（确定性实现）、`Random`（底层是 `marlin_math::Rng` 的同一个算法）、`Exception` 层次、`IDisposable`、`IEquatable<T>`、`IComparable<T>`、泛型数学接口；
- **集合**：`List<T>`、`Dictionary<K, V>`（确定的遍历顺序：按插入顺序）、`HashSet<T>`、`Queue<T>`、`Stack<T>`、`PriorityQueue<T, P>`、`SortedList`；全部提供 struct 枚举器；
- **文本**：`StringBuilder`、插值字符串处理器、数字与字符串的格式化和解析（invariant culture，没有本地化 culture）；
- **并行**：`Parallel`、`IJob`/`IJobFor`、`JobHandle`、Native 容器（§7）；
- **游戏向**：`Task`/帧调度（§5）、对象池 `Pool<T>`、确定性数学、`Marlin.Math`（§11.2）；
- **没有**：文件、网络、进程、线程、时钟（这些都通过引擎 API）、本地化。正则不进标准库，需要时由引擎绑定一个 `Regex` 类（底层是 Rust 的 regex crate，线性时间，没有回溯）。

### 11.2 数学

和 Marlin shader 同名、同语义、同内存布局：`float2`/`float3`/`float4`、`int3`、`float4x4`、`quaternion`，`Dot`、`Lerp`、`Saturate`，swizzle `v.xyz`，矩阵 `M * v`（列向量）。

两种语言共用的代码放在**双语言包**里（源文件扩展名 `.mcommon`，包清单是 `CommonPackage`）：
- 只允许两种 dialect 的**交集**：struct、enum、接口（只作泛型约束）、值类型、namespace 级函数、`const` 泛型、数学类型。没有 class、`new`、异常、`async`、托管数组、string；
- 两个编译器各自编译：`.mscript` 和 `.mshader` 的包都可以依赖 `.mcommon` 包；
- 内建的数学库本身就写成这种包；
- 交集的 dialect 定义在 `front` 里（它不是哪一种语言专有的）；
- 语义差异照各自的语言：整数除零在 shader 里是 0、在 script 里抛异常；浮点在 shader 里默认宽松、在 script 里严格。双语言包里的代码要按两种语义都正确来写，编译器在能看出差异时（比如常量除零）给出警告。

### 11.3 LINQ

方法链 LINQ（`Where`/`Select`/`OrderBy`/`Sum`/`Any`/`ToList`……）照 BCL 的名字，实现成零分配：每个操作返回一个 struct 枚举器，组合出来的枚举器类型被泛型特化，lambda 是不分配的静态委托或者被内联。只有 `ToList`、`ToArray` 这类物化操作分配。

## 12. 和引擎互操作

### 12.1 绑定生成

- Rust 侧在类型和函数上加 attribute（`#[script]`），构建时生成：
  - 脚本侧的声明包（只有签名的 `.mscript`，标 `[Native]`）；
  - 每个函数一个寄存器级的 thunk：直接从调用者的寄存器窗口读参数、把返回值写回寄存器，没有 marshal。
- 调用引擎函数是一条 `call.native idx` 指令。
- 值类型（`float3`、`Entity`……）两边布局相同，按值传递；string 以 UTF-8 切片传给 Rust。

### 12.2 回调

引擎调脚本（事件、生命周期函数）通过句柄表和函数 id；进入脚本是 gameplay 线程上一次普通的解释器调用。

### 12.3 引擎对象

- 脚本里对引擎对象的引用在对象销毁后**仍然非 null**（不破坏 §3.3 的可空性规则）。访问已销毁对象的成员抛 `ObjectDestroyedException`；用 `obj.IsAlive` 检查。不重载 `==`，没有 Unity 的"假 null"。
- 实现：引擎对象的脚本代理里存着句柄和代数；每次 `call.native` 先校验代数（一次比较）。
- **待定**：对象模型（脚本类是否继承引擎类型、还是挂在 ECS 上）等 gameplay 框架设计时再定。VM 只提供通用的绑定机制。

## 13. 热重载

- 编辑器里改了 `.mscript`，编译器增量重编改动的包（0015 的 query 引擎），在**帧边界**切换：
  - 函数体改动：替换字节码，新的调用立即走新代码。正在执行的帧（只可能是挂起的 `async` 函数和 `yield` 迭代器）用旧字节码一直跑完当前这次调用；编辑器提示哪些长期运行的协程还在跑旧代码。没有帧再引用的旧字节码随后回收；
  - 旧字节码访问的字段在开发期是按字段 id 链接再 quicken 的（§9.6），布局迁移后把旧字节码里已经 quicken 的指令恢复成按 id 的形式，重新解析到新布局；字段已被删掉时，那条指令抛异常，终止那个旧帧；
  - 类型布局改动（增删字段、改字段类型）：遍历堆，把旧布局的对象迁移成新布局。名字和类型都对得上的字段拷过去，新字段取初始值，删掉的字段丢弃；类型变了且有可用的转换就转，否则取初始值并警告；
  - 删掉的类型还有存活实例时：实例保留旧类型，报告警告。
- 开发期的链接方式（§9.6）保证替换只影响改过的包。
- shipping 里没有热重载。

## 14. 反射

- 运行时可以枚举类型、字段、属性、方法、attribute，读写字段，调用方法和构造函数；`Type`、`FieldInfo`、`MethodInfo` 照 BCL 的形状。没有 Emit。
- 元数据本来就是解释器需要的，开销很小。shipping 默认裁掉没用到的名字和方法；要在 shipping 里通过反射访问的成员（序列化、存档）标 `[Preserve]`，或者由序列化系统自动保留它用到的类型。

## 15. 确定性

同样的字节码加同样的输入，所有平台上的结果位级相同：
- 浮点是严格 IEEE：不收缩 fma，不重结合，没有近似倒数（和 shader 的默认宽松语义不同）；handler 里的 SIMD 指令只用位级确定的那些；
- `Sin`、`Exp`、`Pow` 等超越函数用自己的实现（不调 libm）；
- `Dictionary`/`HashSet` 的遍历顺序是插入顺序；字符串 hash 用固定种子（shipping 里抗 DoS 不是问题）；`object.GetHashCode` 是惰性分配的序号而不是地址（§8）；
- kernel 里的 `Interlocked` 只有满足交换律和结合律的运算（整数加、min/max、位运算）结果确定（§7.2）；
- job 的切分和归约树只由元素个数决定（§7.5）；
- 帧调度器恢复 `async` 函数的顺序固定（§5）；
- GC 的时机不影响程序语义：没有终结器，`WeakReference` 只在确定的点清空（§10.4）。

## 16. 编译器与运行时的结构

```
compiler/
  vm/                     包名 marlin-vmc-*
    syntax    Marlin script 的 Dialect、Language 描述
    hir_ty    类型检查（类、继承、可空性、泛型、ref safety）
    mir       控制流图，确定赋值、可空流分析、kernel 规则检查、uniformity 分析
    opt       内联、去虚化、检查消除
    codegen   寄存器分配、标量和批量两份字节码、超级指令
    link      开发期的包链接、shipping 的镜像链接
runtime/
  script/                 解释器、GC、帧调度器、job 桥接、反射、热重载
```

- `compiler/vm` 依赖 `front` 和 `common`，不依赖 `shader`（0015 §1）。uniformity 分析和数学类型的定义如果两边都要，下沉到 `front` 或单独的共享 crate。
- 诊断码在 `E2xxx`（0015 §2.3）。

## 17. 待定

- 对象模型：脚本类与引擎对象的关系，以及引擎对象销毁时怎么取消挂在它身上的 `async` 函数（§5、§12.3）。等 gameplay 框架设计时定。
- 按游戏 profile 定制超级指令表（§9.5）：现在不做。
