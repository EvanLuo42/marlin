# 0006 — Marlin shader IR：指令集

状态：草案 · 2026-10-16

前置：
- [0004](0004-shader-language.md)：语言
- [0005](0005-shader-compiler.md)：编译器架构与 pass 流水线

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 目标 | 同一套 IR 能 lower 到 DXIL、SPIR-V、Metal AIR，以及主机平台（Switch 2、PS5、Xbox）的格式 | 1 |
| 降低 | 按实例降低：每个实例（函数加具体的类型和 const 实参）直接从 HIR 降低成具体的 IR，IR 里没有泛型 | 2 |
| SSA | 用块参数表示汇合点的值，不用 phi；写出时机械地转成 phi | 4 |
| 控制流 | 结构化的终结指令：`cond_br`/`switch` 带 merge，`loop` 带 header、continue 和 merge | 5 |
| convergence | 由结构隐式决定，lane 在 merge 块汇合；convergent 操作带标记，不能移到控制依赖不同的位置 | 5.3 |
| `Discard` | 普通指令，语义是 demote-to-helper | 5.2 |
| 类型 | 标量 i1/i16/i32/i64、f16/f32/f64；向量、矩阵（legalize 时才降低）、定长数组、struct、逻辑指针，以及不透明类型。整数不分有符号和无符号，符号语义记在操作上 | 3 |
| 内存 | 逻辑指针：带地址空间，只能从变量或资源元素出发、用 access chain 派生；不能做算术、不能转换、不能存进内存、不能经过块参数 | 6 |
| 入口 I/O | push、系统值、stage I/O、渲染目标、mesh 输出、payload 都是模块级的地址空间变量，统一用 load/store 访问 | 6.3 |
| 资源 | 资源值是带种类的下标 `res<K>`，可以自由流动；使用之前用 `desc.get` 物化成 `handle<K>`，handle 不能经过块参数、select 和内存 | 7 |
| 内存模型 | Vulkan 内存模型式：原子操作带 scope 和 ordering；barrier 分成执行 scope、内存 scope 和要同步的地址空间 | 8.4 |
| 数学 | 核心集加可替换集；可替换集里的每个 op 都有一个用 Marlin 写的标准库实现 | 8.2 |
| 纹理 | 少数几个 op 加操作数包 | 8.6 |
| wave | 参数化的几类，可以带 cluster 大小 | 8.7 |
| ray query 和 mesh | 不透明对象加指针；mesh 输出是地址空间变量 | 8.9 |
| 编码 | 封闭的核心 op enum，再加一个 `target` op，它只允许出现在后端自己的 lower 阶段 | 9 |
| 未定义值 | 没有 poison；只有一个冻结的 `any` 常量；所有 op 都是全函数 | 10 |
| CTFE | 单态化之后，用 IR 解释器执行 | 11 |
| 表示 | 每个函数一个 arena，用 u32 下标引用；块内的指令是双向链表；每个值有 use 列表；类型和常量在 module 级 intern | 12 |
| 文本格式 | 能打印也能解析，可以往返；不做二进制序列化 | 13 |
| 文本里的名字 | 值带源码名作提示（`%acc`，重名时 `%acc.1`，没有名字时 `%5`）；函数、全局变量和 struct 用全名，不是标识符时加引号（`@"Game.Grid<4>"`）；名字不影响语义 | 13 |
| 解释器 | 一个 group 的所有 lane 按结构化控制流一起锁步执行，维护活动掩码，lane 在 merge 块汇合（maximal reconvergence）；wave 操作按 wave 分组计算。数学函数是自己实现的，在所有宿主上逐位一致 | 11 |
| 跨进程缓存 | 只缓存最终的 blob，用 manifest 模式核对依赖（0005 §8） | — |

## 1. target 约束

| target | 底层形态 | 对 IR 的约束 |
|---|---|---|
| DXIL（Xbox 类似，另有扩展） | LLVM 3.7 bitcode，标量，有 phi | 向量要标量化；资源要先变成 handle（`CreateHandleFromHeap` + `AnnotateHandle`）；handle 不能经过 phi；buffer 按（下标, 字节偏移）访问 |
| SPIR-V（Switch 2 的 NVN 走 SPIR-V 系） | 逻辑寻址，结构化控制流，原生支持向量 | 不能做指针运算；要有 merge 标注；opaque 类型不能经过 phi；内存访问用 access chain |
| Metal AIR | 较新的 LLVM，有真正的指针和地址空间 | 资源从 argument buffer 取；地址空间分 device、constant、threadgroup、thread |
| PS5（只按公开资料） | 官方离线工具链；描述符是内存里的数据 | 描述符从内存表里读出来；push constants 走 user data 寄存器 |

IR 停在比这几个 target 都高一层的位置：内存是逻辑的，资源是抽象的，控制流是结构化的。最后一步由各个后端各自 lower（§14）。

## 2. 按实例降低

（2026-10-17 改：原来是"泛型 MIR + 单态化"两层。）

- `lower` 从入口出发，收集用到的**实例**：函数加上具体的类型实参和 const 实参（包括 `Func` 参数传进来的 lambda，0004 §9.2）。
- 每个实例直接从带类型的 HIR 降低成具体的 IR，降低时就代入类型、把接口调用解析成具体的 `call`、把 const 表达式求出值（§11）。
- 每个实例是一个 query：热重载时只重算改动过的函数的实例。泛型函数的每个实例各降低一次，降低本身很便宜。
- 所以 IR 里没有类型参数、关联类型投影、符号形式的数组长度和 `call_iface`；builder、打印器、解析器和 verifier 只有一档。
- 降低时用 Braun 等人的算法直接构造 SSA，局部变量不经过 alloca 再 mem2reg。
- 编译期递归在收集实例时展开；运行时递归由 verifier 报错（0004 §6）。

## 3. 类型

| 类别 | 类型 | 说明 |
|---|---|---|
| 标量 | `i1`、`i16`、`i32`、`i64`、`f16`、`f32`、`f64` | 整数不分有符号和无符号，`sdiv`/`udiv`、`slt`/`ult` 这类区别记在 op 上；`i1` 是 bool |
| 向量 | `f32x4`、`i32x2`……（元素是标量，长度 2–4） | 一直保留到 legalize 的 5.8，由 target 决定要不要标量化 |
| 矩阵 | `f32x4x4`（R×C，列主序） | legalize 的 5.1 才降低成向量 |
| 数组 | `[T; N]` | 只有定长数组 |
| struct | `struct @Name { ... }` | 带 scalar 布局（0004 §16）；布局在 module 级计算并缓存 |
| 指针 | `ptr<space, T>`，可以带 `ro` | 逻辑指针（§6） |
| 资源下标 | `res<K>` | K 是资源种类和元素类型，比如 `res<Texture2D<f32x4>>`、`res<Buffer<@Light>>`、`res<Sampler>` |
| handle | `handle<K>` | 物化之后的描述符（§7） |
| 不透明类型 | `rayquery`、`accel` | 只能存在于变量里，或者作为 handle 的种类 |

## 4. 值与 SSA

- 值有三种：指令的结果、块参数、常量。函数参数就是入口块的参数。
- 汇合点用**块参数**：跳转指令传实参。没有 phi，也没有关键边的问题。

```text
block3(%x: f32, %y: f32):
    %s = fadd %x, %y
    cond_br %c, block4(%s), block5(%x) merge block6
```

- 常量：标量和向量的立即数、struct 和数组的常量聚合、`zero`、`any`（§10）。
- 写出 DXIL 和 SPIR-V 时，块参数机械地转换成 phi。

## 5. 控制流

### 5.1 终结指令

| 终结指令 | 说明 |
|---|---|
| `br bN(args)` | 无条件跳转 |
| `cond_br %c, bT(args), bF(args) merge bM` | 结构化的选择 |
| `switch %v [k: bK(args), ...] default bD(args) merge bM` | 结构化的多路选择 |
| `loop header bH continue bC merge bM` | 进入循环，它所在的块是循环的前置块 |
| `return (vals)` | |
| `unreachable` | |

结构上的不变量和 SPIR-V 相同，由 verifier 检查：
- 每个结构化 region 只有一个入口，所有出口都走到它的 merge 块；
- `break` 是跳到循环的 merge，`continue` 是跳到 continue 块；
- region 正确嵌套。

源语言没有 `goto`，所以降低出来的控制流从一开始就满足这些条件。所有 pass 都必须保持这些不变量（0005 §4.1）。

verifier 怎么划分 region（第 6b 步定）：在 CFG 上再加上每个 header 到它的 merge 块的边（loop 再加 header 到 continue 块的边），region 就是 header 支配、merge 块不支配的那些块，并且限制在外层 region 之内；在 loop 里面的 region 到 loop 的 continue 块为止（除非这个 region 本身就在 continue 块那一部分里）。这和 SPIR-V 对 construct 的定义一致。跳出 region 只能去它的 merge 块、最内层 loop 的 merge 或 continue 块（`break`、`continue`），或者外层 switch 的 merge 块。

### 5.2 `discard`

- `discard` 是普通指令，不是终结指令，语义是 **demote-to-helper**：这个 lane 变成 helper lane，仍然参与导数计算，但之后的写出和副作用都被丢弃。
- 对应：SPIR-V 的 `OpDemoteToHelperInvocation`、DXIL 的 `discard`、Metal 的 `discard_fragment`。
- `is_helper` 查询当前 lane 是不是 helper。

### 5.3 convergence

- lane 在结构化 region 的 merge 块汇合，这和 SPIR-V 的 maximal reconvergence 语义一致。
- 下面这些 op 带 convergent 标记：
  - 导数 op；
  - 隐式 LOD 的采样；
  - wave op；
  - barrier；
  - `mesh.set_counts` 和 `mesh.dispatch`。
- 对 convergent 的 op，pass 只能在**控制依赖相同**的位置之间移动它，不能把它移进或移出发散的条件。verifier 会检查这一点；能不能移动由 uniformity 分析判断（0005 §4.3）。这项检查要等 uniformity 分析（0005 §11 第 8 步）才加进 verifier。
- 不用显式的 convergence token。

## 6. 内存与逻辑指针

### 6.1 地址空间

| 地址空间 | 来源 | 读写 |
|---|---|---|
| `function` | `var`（局部变量，包括 ray query） | 读写 |
| `groupshared` | 模块级 `[GroupShared]` 变量 | 读写 |
| `buffer` | 从 `Buffer`/`ByteBuffer` 的 handle 派生；`ReadOnly*` 派生出来的带 `ro` | 读写，或者只读 |
| `uniform` | `Uniform<T>` 的 handle | 只读 |
| `constant` | 模块级的常量数据，比如提升出来的常量数组（0005 §4.4 的 1.7） | 只读 |
| `push` | 入口的 `[Push]` 变量 | 只读 |
| `input`、`output` | 系统值、stage I/O、渲染目标 | 只读 / 只写 |
| `mesh_vertex`、`mesh_primitive`、`mesh_index` | mesh 输出 | 只写 |
| `payload` | amplification 到 mesh 的 payload | amplification 端写，mesh 端读 |

### 6.2 指针操作

```text
%e   = buffer.elem %h, %i            : ptr<buffer ro, @Light>
%pos = access %e, .Position          : ptr<buffer ro, f32x3>
%v   = load %pos                     : f32x3
%t   = access @Tile, [%gi]           : ptr<groupshared, f32x4>
store %t, %v4
%b   = buffer.byte %raw, %offset     : ptr<buffer, i32>    // ByteBuffer，偏移按 4 对齐
```

- 指针只能这样得到：
  - 从变量（`var`、模块级 global）得到；
  - 从 handle 得到（`buffer.elem`、`buffer.byte`、`uniform.ptr`）；
  - 在已有的指针上用 `access` 加字段名或下标派生。
- 指针**不能**：
  - 做算术、和整数互相转换、bitcast；
  - 存进内存；
  - 经过块参数或 select；
  - 作为函数返回值。
- 函数可以接收指针参数：`in`/`out`/`ref` 在 inline 之前会这样表示（0004 §6）。
- `load` 和 `store` 可以带 `aligned(n)`。`volatile` 和 `nontemporal` 先不做。
- 编译期就知道越界的 `access` 是错误。运行时越界的结果由 target 定义（D3D12 和 Vulkan 的 robustness 规则）。

### 6.3 入口 I/O

入口函数没有参数，它的输入输出是一组模块级变量：

```text
global @args : ptr<push, @BlurArgs>
global @tid  : ptr<input, i32x3> builtin(DispatchThreadId)
global @uv   : ptr<input, f32x2> location(0) interp(perspective)
global @col  : ptr<output, f32x4> target(0)
global @dep  : ptr<output, f32> builtin(Depth)

entry compute(8, 8, 1) @Blur uses(@args, @tid) { ... }
```

- 每个入口用 `uses(...)` 列出它用到的变量。
- 系统值用 `builtin(...)` 标注，插值方式用 `interp(...)` 标注。
- SROA、DSE 和 load 去重都可以直接用在这些变量上。

## 7. 资源

```text
%idx = load (access @args, .Src)     : res<Texture2D<f32x4>>
%t   = desc.get %idx                 : handle<Texture2D<f32x4>>
%s   = desc.get (load (access @args, .Linear)) : handle<Sampler>
%c   = tex.sample %t, %s, %uv
```

- `res<K>` 是一个普通的值，实际上就是 u32 下标。它可以经过块参数、select 和内存，struct 里的资源字段就是它。
- `desc.get %idx` 把下标物化成 `handle<K>`，可以带 `nonuniform`（0005 §4.4 的 4.1）。`handle` **不能**经过块参数、select 和内存，也不能作为函数返回值。
  - 前端和 pass 要在下标上做选择，然后再物化。
  - 所以不需要 DXC 那样的 DxilCleanupDynamicResourceHandle。
- 描述符获取的 CSE 和外提（0005 §4.4 的 4.2）针对的就是 `desc.get`。
- 其他资源查询：`buffer.length %h`、`tex.query %t, size(mip)`、`tex.query %t, levels`。

## 8. 指令

### 8.1 算术、比较与转换

| 类别 | op |
|---|---|
| 整数 | `add sub mul sdiv udiv srem urem neg and or xor not shl lshr ashr`，以及 `mulhi.s/u` |
| 浮点 | `fadd fsub fmul fdiv frem fneg` |
| 比较 | `icmp eq/ne/slt/sle/sgt/sge/ult/ule/ugt/uge`、`fcmp oeq/one/olt/ole/ogt/oge/ord/uno/ueq/une…` |
| 选择 | `select %c, %a, %b`（向量条件就是逐分量选择） |
| 转换 | `sext zext trunc fpext fptrunc sitofp uitofp fptosi fptoui bitcast` |
| 打包 | `pack.f16x2`/`unpack.f16x2`、`pack.unorm8x4`/`unpack.unorm8x4`…… |

- 浮点 op 上可以带 `precise` 标记（0004 §9.7）。没有这个标记时，允许重结合、fma 收缩和近似倒数。
- 整数语义见 §10。

### 8.2 数学

| 集合 | op | 说明 |
|---|---|---|
| 核心集 | `fma sqrt rsqrt rcp exp2 log2 sin cos abs smin smax umin umax fmin fmax floor ceil ftrunc round_even fract sign dot cross popcount firstbit_hi.s firstbit_hi.u firstbit_lo bitreverse bitfield_extract.s bitfield_extract.u bitfield_insert isnan isinf` | 各个 target 普遍有原生指令，优化器也要认识它们 |
| 可替换集 | `pow tan asin acos atan atan2 sinh cosh tanh exp log normalize length distance reflect refract smoothstep step faceforward` | 每个 op 都对应一个用 Marlin 写的标准库实现。target 描述声明它有原生实现就直接用原生的，否则在 legalize 的 5.5 inline 标准库的那个实现 |

- 核心集里 target 没有原生实现的 op，也在 5.5 展开。

### 8.3 向量、矩阵与聚合

| 类别 | op |
|---|---|
| 向量 | `splat`、`extract %v, i`、`insert %v, i, %x`、`shuffle %a, %b, [i...]`（swizzle 就是 shuffle），以及用于动态下标的 `extract.dyn`/`insert.dyn` |
| 矩阵 | `matmul`（矩阵乘矩阵、矩阵乘向量）、`transpose`、`mat.column`、`mat.row`、`mat.extract r c`，以及逐分量的算术。legalize 的 5.1 把这些降低成向量运算 |
| 聚合 | `construct T(...)`、`extract.field %s, .F`、`insert.field %s, .F, %x`、`extract.elem %a, i`、`insert.elem %a, i, %x` |

### 8.4 原子操作与 barrier

```text
%old = atomic.add %p, %v scope(device) order(relaxed)
%o2  = atomic.cmpxchg %p, %cmp, %new scope(group) order(acq_rel)
barrier exec(group) mem(group) spaces(groupshared)
barrier exec(none) mem(device) spaces(buffer, image)
```

- 原子操作：`add sub and or xor smin smax umin umax xchg cmpxchg load store`，操作的是 `groupshared` 或 `buffer` 指针，也可以是存储纹理的纹素（`tex.atomic`）。
- 每个原子操作都带：
  - scope：`lane`、`wave`、`group`、`device`；
  - ordering：`relaxed`、`acquire`、`release`、`acq_rel`。
- `barrier` 由三部分组成：执行 scope（`none`、`wave`、`group`）、内存 scope，以及要同步的地址空间集合。
- 源语言里的 `Barrier.Group()` 等函数，只是这些参数的常用组合（0004 §12.2）。

### 8.5 导数

- `deriv.ddx` 和 `deriv.ddy` 都带 `coarse` 或 `fine`，是 convergent 的，只能在 pixel stage 里出现。
- compute 里的导数已经在自动微分阶段（0005 §4.4 的 0.3）变成了普通算术，后端 IR 里不会出现。

### 8.6 纹理

```text
%c = tex.sample %t, %s, %uv                                  // 隐式 LOD，convergent
%c = tex.sample %t, %s, %uv { lod: %l }
%c = tex.sample %t, %s, %uv { grad: (%dx, %dy), offset: const(1, 0) }
%d = tex.sample %t, %cs, %uv { compare: %ref, lod: 0 }
%g = tex.gather %t, %s, %uv { channel: 0 }
%x = tex.load %t, %coord { mip: %m }
tex.store %st, %coord, %v
%a = tex.atomic.add %st, %coord, %v scope(device) order(relaxed)
```

- 操作数包里可以有：`lod`、`bias`、`grad`、`compare`、`offset`、`min_lod`、`sample`（MSAA）、`array`。其中 `lod`、`bias` 和 `grad` 互斥。
- 只有隐式 LOD 的采样是 convergent 的。
- `offset` 在 legalize 的 5.6 之后必须是常量。

### 8.7 wave

```text
%s = wave.reduce add %v cluster(4)
%p = wave.scan add exclusive %v
%m = wave.ballot %c
%b = wave.broadcast %v, %lane
%q = wave.quad swap_x %v
```

- 有这几类：`wave.reduce op`、`wave.scan op inclusive/exclusive`、`wave.ballot`、`wave.broadcast`、`wave.shuffle`、`wave.quad`（`swap_x`、`swap_y`、`swap_diag`、`read_lane`）、`wave.vote`（`any`、`all`、`equal`）、`wave.match`。
- 查询类：`wave.lane_index`、`wave.lane_count`、`wave.is_first`。
- 都可以带 `cluster(n)`。target 不支持 cluster 时，在 legalize 阶段用 shuffle 展开。
- 都是 convergent 的。

### 8.8 调用

- `call @f(args)` 调用已知的函数；接口方法的调用在降低时就解析成具体的 `call`（§2）。
- 默认全部 inline（0005 §4.4 的 1.1），之后只有 target 支持函数调用时才会留下 `call`。
- 后端 IR 里没有间接调用。

### 8.9 ray query 与 mesh

```text
%q = var function : rayquery
rq.init %q, %accel, %ray, flags(cull_back), mask(0xff)
%more = rq.proceed %q
%k = rq.get %q, candidate.kind
rq.commit_triangle %q
%t = rq.get %q, committed.t

global @verts : ptr<mesh_vertex, [@VsOut; 64]>
global @tris  : ptr<mesh_index, [i32x3; 126]>
global @prims : ptr<mesh_primitive, [@PrimOut; 126]>
mesh.set_counts %nv, %np
mesh.dispatch %x, %y, %z          // amplification；payload 是 @payload 变量
```

- `rayquery` 是不透明类型，只能存在于 `function` 地址空间的变量里，`rq.*` op 接收指向它的指针，这和 SPIR-V 一样。DXIL 把它降低成 ray query 句柄整数。
- mesh 输出是 `mesh_*` 地址空间的数组变量，写之前必须先调用 `mesh.set_counts`，由 verifier 检查（0004 §11.6）。

### 8.10 调试

- `debug.print fmt_id, (args)` 和 `debug.assert %c, fmt_id, (args)`，只在 Development profile 里出现（0005 §4.4 的 0.4 会把它们降低成写调试 buffer）。
- 格式字符串存在 module 的格式表里，产物的反射里有这张表。

## 9. 编码与 target op

- 所有 op 是一个**封闭的** Rust enum。pass 可以穷尽匹配，每个 op 的属性（有没有副作用、是不是 convergent、读写哪些地址空间、能不能投机执行）用表来描述。
- `target { backend, opcode, operands, results }` 是唯一的扩展点：
  - 只允许出现在后端自己的 lower 阶段（0005 §4.4 的阶段 6），共享的 pass 永远看不到它；
  - 由后端 crate 定义，包括它的属性和打印格式。主机平台的后端也用它，不需要改共享代码。
- 新增一个核心 op 要求：所有共享的 pass 和 verifier 都能处理它（编译器会因为匹配不穷尽而报错），而且每个后端都有对应的 lower 或展开。

## 10. 语义：没有 poison

- 所有 op 都是**全函数**，结果是确定的：
  - 整数溢出时回绕；
  - 移位次数按位宽取模，这是写在 op 定义里的；
  - 整数除以 0 的结果由 target 定义。源语言要求的"除零结果为 0"由 0005 §4.4 的 0.2 插入检查来保证（0004 §9.6）；
  - 浮点按 IEEE 规则，`precise` 之外的那些宽松规则见 §8.1；
  - `fptosi` 和 `fptoui` 越界时饱和，NaN 转成 0。
- 唯一的未定义值是常量 **`any`**：表示一个任意但固定的值，相当于 LLVM 的 `freeze(poison)`。用在确实无所谓的地方，比如没有写到的向量分量、未使用的 struct 字段。
- 没有 `nsw`、`nuw`、`exact` 之类的标记。代价是少了一些整数优化，换来的是简单的语义：peephole 规则和差分测试都可以直接拿解释器的结果来比较。

## 11. CTFE

- 需要对 const 表达式求值的地方：
  - 数组长度；
  - 静态参数；
  - `const` 字段；
  - `[Export]`（0004 §13.4）。
- 编译器先把用到的 `const` 函数单态化成后端 IR，再用 **IR 解释器**执行。这个解释器也是 0005 §4.6 用来做差分测试的那一个。
- 只有一个解释器，它的语义就是 IR 的语义，所以编译期算出来的结果和 GPU 上跑出来的不会不一致。
- 依赖泛型参数的 const 表达式在降低每个实例时求值（§2）。
- `const` 函数不能访问资源、groupshared、入口 I/O 和 `debug.*`，由 verifier 检查。
- 解释器的执行模型（2026-10-17 定）：
  - 一个 group 的所有 lane 按结构化控制流一起锁步执行，维护活动掩码：分支两边的 lane 先后各跑一遍（条件为真的一边先跑），在 merge 块汇合；只要还有 lane 留在循环里，循环就继续（§5.3）；
  - wave 操作按 wave 分组（相邻的 `wave_size` 个 lane），只算这个 wave 里活动的 lane；wave 大小是参数，测试覆盖多种大小；
  - 这样 barrier 总是由整个 group 同时到达，不需要挂起和切换 wave（第 6c 步定：这同样是 GPU 允许的一种执行顺序，同样确定）；barrier 处如果有 lane 不活动就报错；
  - 同一条指令里各 lane 按序号从小到大执行，所以原子操作和有竞争的写每次结果都一样；越界的访问读到 0、写被丢掉，并且计数；
  - 数学函数（`sin`、`exp2`、`log2`……）在 ir crate 里用软件实现，所有宿主上逐位一致，所以在不同机器上 cook 出的 blob 相同；精度按 GPU 的容差，不保证正确舍入。f16 也用软件实现；
  - 资源由调用方提供：buffer 是字节数组，push 和入口 I/O 是变量的初始值；纹理采样、ray query 和 mesh 第一版不支持（第 10 步再加）；
  - 整数除以 0 在解释器和常量折叠里都得 0（§10 允许 target 定义，两者一致才能做差分测试）。

## 12. 内存中的表示

- 每个函数一个 arena：指令、值和块都存在里面，用 u32 下标引用（和 Cranelift 一样）。
- 块内的指令是双向链表，插入和删除都是 O(1)。
- 每个值有自己的 use 列表，所以替换一个值的所有使用、查找使用者都很快。
- 类型、常量和 struct 布局在 module 级 intern。
- 没有 `Rc`，也没有 unsafe 的指针图。
- source location 和 debug 信息挂在指令上，作为一个额外的下标。

## 13. 文本格式

- 打印和解析能往返。
- 名字：值打印成 `%名字`，名字来自源码，只是提示；重名时加 `.1`、`.2`，没有名字的值打印成 `%序号`。函数、全局变量和 struct 打印成 `@全名`，全名不是标识符时加引号，比如单态化出来的 `@"Game.Grid<4>"`。解析时名字只用来对应定义和使用。
- pass 的测试是 FileCheck 式的：输入一段 IR 文本，跑指定的 pass，然后比对输出。
- 不做二进制序列化：跨进程只缓存最终的 blob（0005 §8）。

```text
struct @BlurArgs { Src: res<Texture2D<f32x4>>, Dst: res<StorageTexture2D<f32x4>>, Linear: res<Sampler>, Radius: f32 }

global @args : ptr<push, @BlurArgs>
global @tid  : ptr<input, i32x3> builtin(DispatchThreadId)

entry compute(8, 8, 1) @Blur uses(@args, @tid) {
block0:
    %id  = load @tid
    %xy  = shuffle %id, %id, [0, 1]
    %src = desc.get (load (access @args, .Src))
    %dst = desc.get (load (access @args, .Dst))
    %c   = tex.load %src, %xy { mip: 0 }
    tex.store %dst, %xy, %c
    return
}
```

为了写起来简洁，文本格式允许嵌套写法，比如 `load (access ...)`。解析时会把它展开成一串独立的指令。

语法（第 6a 步定，2026-10-17）：

```text
module    := item*
item      := 'format' N '=' string
           | 'struct' @name '{' (field ':' type),* '}'
           | 'global' @name ':' ptr-type attr* ('=' lit)?      attr: builtin(X) location(n) target(n) interp(x)
           | 'func' @name '(' (%v ':' type),* ')' ('->' type | '->' '(' type,* ')')? body
           | 'entry' stage @name 'uses' '(' @global,* ')' body  stage: compute(x, y, z) vertex pixel mesh(...) amplification(...)
body      := '{' (label | inst)* '}'
label     := name ('(' (%v ':' type),* ')')? ':'               入口块的参数写在函数头上
inst      := (%v,+ '=')? op-word words* operands suffix*        一行一条
operand   := %v | @global | type lit | '(' inst ')'
lit       := number | true | false | zero | any | '(' lit,* ')'
type      := i1 | i32 | f32x4 | f32x4x4 | '[' type ';' N ']' | @struct
           | ptr<space ro?, type> | res<Kind<type>?> | handle<Kind<type>?> | rayquery
```

- 常量都写成"类型 字面量"：`i32 -1`、`f32 1.5`、`f32x2 (1.0, 2.0)`、`@Light zero`。整数按类型的位宽截断；浮点类型后面跟十六进制整数表示原始位（非有限值这样打印）。
- 结果类型由操作数推出；推不出来的写在后面：转换和 `splat` 写 `: T`，`construct T(...)`、`var function : T` 自带类型。
- 字段用名字：`access %p, .Color, [2]`、`extract.field %s, .Intensity`；`[3]` 是 `i32` 常量下标。
- 块按结构化的顺序打印：跳转目标按顺序，region 的 merge 块在整个 region 之后。值必须在使用之前定义；函数、struct 和全局变量可以先用后定义。
- `//` 到行尾是注释。
- 数学的 `trunc` 叫 `ftrunc`，免得和转换的 `trunc` 重名；`firstbit_hi` 和 `bitfield_extract` 分 `.s`、`.u` 两种，因为整数不带符号。
- 纹理、ray query、mesh 和打包操作随第 10 步加入枚举和文本格式。

## 14. 各 target 的 lower

| IR 概念 | DXIL | SPIR-V | Metal AIR | PS5（只按公开资料） |
|---|---|---|---|---|
| 块参数 | phi | OpPhi | phi | 由工具链的输入格式决定 |
| 结构化终结指令 | 普通的 br，merge 标注丢掉 | OpSelectionMerge、OpLoopMerge | 普通的 br | 同左 |
| `desc.get` | CreateHandleFromHeap + AnnotateHandle | 对描述符数组做 access chain，再 OpLoad | 从 argument buffer 取 | 从描述符表按字节读出描述符 |
| `buffer.elem` + `access` + `load` | rawBufferLoad（下标, 字节偏移） | access chain + OpLoad | 设备指针运算 + load | 由描述符和偏移计算地址 |
| `push` 变量 | root constants（b0） | PushConstant 变量 | buffer 参数 | user data 寄存器 |
| `input`/`output` | loadInput/storeOutput 加签名 | Input/Output 变量 | stage_in 和返回值 | 由工具链决定 |
| 向量 | 标量化（5.8） | 保留 | 保留 | 由 target 描述决定 |
| `barrier` | SM 6.8 的 barrier（scope + flags），或者旧的 barrier op | OpControlBarrier、OpMemoryBarrier | threadgroup_barrier(mem_flags) | 由工具链决定 |
| `discard` | discard | OpDemoteToHelperInvocation | discard_fragment | 由工具链决定 |
| `target` op | DXIL 后端定义 | SPIR-V 后端定义 | Metal 后端定义 | 平台后端定义 |

## 15. 待定

- 每个 op 的完整属性表（副作用、convergent、读写哪些地址空间、能不能投机执行）。
- struct 布局的计算规则在 module 里怎么缓存，以及它和 0004 §16 的对应关系。
- 纹理、ray query、mesh 和打包操作的文本写法（随第 10 步加入）。
- 64 位原子、存储纹理原子在各个 target 上的 capability 对应关系。
