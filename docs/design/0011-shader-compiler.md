# 0011 — Marlin shader 编译器：架构与 API

状态：草案 · 2026-10-16

前置：
- [0002](0002-shader-interface.md)：shader ABI
- [0003](0003-asset-pipeline.md)：cook 键、DDC、marlin-reflect
- [0007](0007-material-system.md) §2：材质系统对编译器的需求
- [0010](0010-shader-language.md)：语言

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 形态 | 纯 Rust 库，没有 CLI；编译器 crate 放在 `compiler/shader/`，主机平台的后端放在 `platforms/<p>/shader/` | 1 |
| 前端 | 由 query 驱动，增量编译；query 引擎自己写一个精简版 | 2 |
| IR | 两层：前端产出泛型的 MIR，单态化之后是后端的 SSA IR | 3 |
| 后端 IR 控制流 | CFG + SSA；分支和循环的终结指令带 merge 块和 continue 块，结构和 SPIR-V 同构，pass 必须保持 | 3.2 |
| 后端 | pass 流水线（LLVM 新 pass manager 式），所有后端共用一条流水线；后端只负责最后的 lower 和写出 | 4 |
| 调用模型 | 长生命周期的增量 `Session`；一次请求对应一个单态化入口和一个 target，产出一个 blob | 5 |
| 源码输入 | 宿主实现 `SourceProvider`；编译器自己不访问文件系统 | 5.2 |
| 诊断 | 以结构化的 `Diagnostic` 返回，编译器从不自己打印；库里自带 rustc 风格的文本渲染器 | 5.3 |
| 编译选项 | 只有 `profile`：Development 或 Shipping，没有细粒度 flag | 5.4 |
| 并行 | `Session` 是 `Sync` 的；编译器内部不开线程，由宿主负责并行 | 5.5 |
| 反射 | 普通 Rust 数据类型，注册到 marlin-reflect，按 0003 的通用格式写进 cooked 单元；后端专用的信息放在扩展字段里 | 6 |
| 编译期导出 | `const` 函数在编译期求值，`[Export]` 常量的值写进反射 | 6.2 |
| 调试信息 | Development 构建输出源码级调试信息（PDB、NonSemantic.Shader.DebugInfo），每个产物都带统计 | 7 |
| cook 指纹 | 自动指纹：编译器源码哈希 + target 描述版本 + 官方工具链版本；另附用到的源文件内容哈希 | 8 |
| Rust 端类型 | **不生成 Rust 代码**。Rust 端手写 `#[repr(C)]` 的 struct；Development 构建创建 pipeline 时，用反射比对布局 | 9 |

## 1. 参照与原则

| 参照 | 借鉴的部分 | 避开的部分 |
|---|---|---|
| rustc | query 驱动的前端；泛型 MIR 上的 CTFE；单态化时收集实例；与后端无关的 codegen trait；span、errors、session 等基础设施拆成独立的 crate | 体量（宏展开、借用检查这些我们都不需要） |
| rust-analyzer | 无损 CST 加上能容错的 parser；同一份前端同时服务 IDE 和编译 | — |
| 参考实现 | 在 IR 上做自动微分；capability 推断；带 merge 标注的结构化 CFG；按 target 参数化的 legalize | 预处理器；泛型直接留在 IR 上（让 pass 变复杂）；以文本 emitter 为主的后端 |
| DXC | LLVM 风格的 pass 流水线；DXIL 的容器和验证哈希 | SPIR-V 是另一条从 AST 出发的路，两条路的功能长期对不齐 |

原则：
- 前端用 query，后端用 pass 流水线。
- 所有 target 共用同一条 IR 流水线。
- 加一个后端只需要一个 crate：target 描述、`Backend` 实现和格式写出器。共享代码里不允许出现 `match target`，target 之间的差异放进 target 描述里。
- target 描述也是前端 query 的输入，capability 检查和 `const if` 依赖它。

## 2. crate 划分

```
compiler/shader/
  span          源码位置、文件 id、行列映射
  diag          结构化诊断与文本渲染器
  arena         arena、interning、类型化下标
  query         增量 query 引擎（红绿算法、依赖记录、取消、确定性）
  syntax        lexer、无损 CST、能容错的 parser、有类型的 AST 视图
  hir_def       item tree、包与 namespace 的 def map、名字解析、把函数体降低成 HIR
  hir_ty        类型检查与推断、泛型与约束求解、capability 推断、const 求值的入口
  mir           泛型的、结构化的中层 IR；从 HIR 降低；CTFE 解释器
  mono          从 EntryRequest 收集实例，单态化，产出后端 IR
  ir            后端 SSA IR、builder、verifier、打印器
  opt           pass manager 和各个 pass
  target        target 描述（capability、布局规则、资源模型、支持的操作）
  backend_dxil  DXIL：LLVM 3.7 bitcode 写出器、DXIL 元数据、容器和验证哈希
  backend_spirv SPIR-V 写出器
  reflect       反射数据类型（运行时也能依赖）
  marlin-shader facade：Session、target 注册表、EntryRequest
platforms/<p>/shader/  主机平台的后端（在 cook 平台 cfg 下通过 #[path] 引入）
```

依赖方向：
- `syntax` → `hir_def` → `hir_ty` → `mir` → `mono` → `ir`，每一层都只依赖它前面的层；`opt` 只依赖 `ir` 和 `target`；
- 每个后端只依赖 `ir`、`target` 和 `reflect`；
- `marlin-shader` 把上面这些组装起来。

## 3. IR

### 3.1 两层

| 层 | 特点 | 上面做的事 |
|---|---|---|
| **MIR** | 前端的产物：泛型的、按函数做 query 的；控制流是结构化的；还保留着源码级的类型 | CTFE（`const` 函数、`[Export]`、静态参数）；热重载时只重算改动了的函数 |
| **IR** | 单态化之后的 SSA；类型都是具体的；按入口组织成 module | 自动微分、uniformity 和 NonUniform 分析、legalize、所有优化 |

- 单态化（`mono`）从 `EntryRequest` 出发，收集所有用到的实例，包括泛型函数、`Func` 参数和 lambda（0010 §9.2）。
- 编译期递归在单态化时展开，运行时递归在这里报错（0010 §6）。

### 3.2 后端 IR 的控制流

- 基本块组成的 CFG 加上 SSA，与 LLVM 相同。
- 分支和循环的终结指令带 merge 块，循环还带 continue 块。这和 SPIR-V 的结构化控制流同构，参考实现也是这样做的。
- pass 必须保持这些结构，verifier 负责检查。
- 源语言没有 `goto`，所以降低出来的控制流从一开始就是结构化的。
- 输出 SPIR-V 时不需要 structurizer。DXIL 和 Metal AIR 这类 LLVM 系的格式直接忽略这些标注。

## 4. 后端流水线

- pass manager 参照 LLVM 新 pass manager 的模式：
  - analysis 结果有缓存，每个 pass 声明自己保留了哪些 analysis；
  - pipeline 由 profile 和 target 描述组装出来。
- 大致顺序：
  1. 自动微分：前向模式，以后加反向模式（0010 §15）；
  2. inline、SROA、mem2reg；
  3. 一轮通用优化：GVN、DCE、LICM、常量传播、CFG 化简（保持结构）；
  4. uniformity 分析，自动加上 NonUniform 标记（0010 §10.3）；
  5. 按 target 参数化的 legalize：资源变成 bindless 堆访问，处理 16 位类型、矩阵布局和 scalar 布局，展开 target 不支持的操作；
  6. 后端各自的 lower，然后写出。
- 浮点：默认允许重结合、fma 收缩和近似倒数；带 `[Precise]` 的值和函数不做这些变换（0010 §9.7）。

## 5. API

### 5.1 Session 与请求

```rust
let mut s = Session::new(SessionDesc { sources: &provider, targets: &registry });
s.add_package("shaders/substrate")?;

let out = s.compile(&EntryRequest {
    entry: "Marlin.Material.Entries.ResolvePixel",
    type_args: &["Game.Rock"],
    consts: &[("Game.Rock.UseMoss", Const::Bool(true))],
    target: "d3d12-sm6.6",
    profile: Profile::Development,
})?;
// out.code：单入口的字节码；out.reflection；out.diagnostics；out.used_sources

s.invalidate(&path);   // 源文件变了，相关的 query 会重算
```

- `Session` 的生命周期很长：
  - cookd 进程里一直保持一个，热重载时只重算受影响的 query（0007 §7.3）；
  - build.rs 创建一个，一次编完所有入口；
  - 以后的语言服务器也用同一套前端。
- 一个请求对应一个入口、一组类型实参和常量、一个 target 和一个 profile，产出**一个单入口 blob**。这和 0002 的"每个 stage 一个 module"一致，也是 cook 键和 DDC 最细的粒度。
- 可以把多个请求作为一批提交。
- target 用名字从注册表里查，比如 `"d3d12-sm6.6"`、`"vulkan-1.3"`、`"metal-3"`。名字写在 cook 平台的设置里。

### 5.2 源码输入

```rust
pub trait SourceProvider: Send + Sync {
    fn list(&self, package: &str) -> Result<Vec<SourcePath>>;
    fn read(&self, path: &SourcePath) -> Result<Arc<str>>;
}
```

- 编译器不自己访问文件系统，只通过宿主给的 `SourceProvider` 按包和路径读内容，所以编译器里不会出现机器路径。
- 宿主各自的实现：
  - build.rs：从磁盘读，顺带输出 `rerun-if-changed`；
  - cook：从磁盘读，同时记下内容哈希；
  - 编辑器：读的内容包括未保存的缓冲区。
- `SourcePath` 是"包名 + 包内的相对路径"。

### 5.3 诊断

- 编译器从不自己打印，也不自己写日志，而是返回 `Diagnostic` 值，里面有：
  - 错误码和严重级别；
  - 主 span 和附加标签；
  - note；
  - 可以自动应用的修改建议。
- 库里自带一个 rustc 风格的文本渲染器，给 build.rs 和测试用。
- cook 和引擎拿到诊断后，转给 marlin-diag 输出。

### 5.4 编译选项

- 只有一个选项 `profile`：

| profile | 优化 | `Debug.*`（0010 §12.5） | 调试信息 |
|---|---|---|---|
| Development | 完整优化 | 生效 | 有 |
| Shipping | 完整优化 | 删除 | 没有 |

- 没有细粒度的 flag，这样 cook 键的维度很少，也不会出现"某个 flag 只在某个地方开了"的问题。
- 平台之间的差异全部由 target 描述决定。

### 5.5 并行

- `Session` 是 `Sync` 的，可以跨线程共享，共享的前端 query 只算一次。
- 编译器内部不开线程，也不依赖任何线程池。
  - cook 用引擎的 job 系统并行提交请求；
  - build.rs 用 `std::thread::scope`。

## 6. 反射

### 6.1 内容与格式

- 反射是普通的 Rust 数据类型，放在 `reflect` crate 里。运行时也能依赖这个 crate，它的类型都注册到 marlin-reflect。
- cook 时按 0003 的通用格式写进 cooked 单元。调试时可以直接导出成 `.masset` 文本。
- 每个产物的反射包括：
  - 入口：stage、线程组大小、mesh 的容量和拓扑；
  - push 布局：字段名、类型、偏移、大小，以及字段上的 attribute；
  - 静态参数和它们的值；
  - 材质参数块的布局（0007 §8.2）：字段名、类型、偏移、默认值、用户 attribute；
  - 实现了哪些可选方法（0007 §2.4）；
  - 用到的 capability，以及引用了哪些 `Target.*` 常量（用来推导 permutation 维度）；
  - `[Export]` 常量的值（§6.2）；
  - 用到的源文件和它们的内容哈希。
- 后端专用的信息（比如主机平台的资源表）放在扩展字段里，由后端 crate 定义。

### 6.2 编译期求值与导出

0007 要求反射给出 Substrate 树展平后的 closure 数量和特性位。这类"库自己定义的编译期信息"，用编译期求值来导出，编译器不需要知道 Substrate 是什么：

```csharp
public const ClosureInfo Flatten<M>() where M : IMaterial { ... }

// 入口模板里
[Export] const ClosureInfo Topology = Flatten<M>();
// 反射：exports["Topology"] = { Count = 2, Features = Aniso | Fuzz }
```

- 语言层面的规则见 0010 §13.4。
- CTFE 解释器在 MIR 上运行。

## 7. 调试信息与统计

- Development 构建输出源码级的调试信息：DXIL 用 PDB，SPIR-V 用 NonSemantic.Shader.DebugInfo。这样在 PIX 和 RenderDoc 里能单步调试 `.mshader`。
- Shipping 构建不输出调试信息。
- 调试信息不影响代码部分的字节。
- 每个产物都带统计：指令数、寄存器估计、groupshared 大小。统计写进 cook 报告。

## 8. cook 指纹

- `Session::fingerprint(target)` 由下面三部分合成：
  - 编译器自己源码的哈希，在编译器构建时算好；
  - target 描述的版本；
  - 主机平台官方工具链的版本（如果这个后端要调用它）。
- 改了编译器代码，缓存就自动失效，不会因为忘了加版本号而读到过期的缓存。
- 每个产物还带上它用到的源文件的内容哈希，cook 键（0003 §4.1）把这些都算进去。

## 9. Rust 端的参数结构

**不生成 Rust 代码。**

- Rust 端手写 `#[repr(C)]` 的 struct，字段名按 0003 §1.2 的规则用 snake_case。
- 向量字段要用 4 字节对齐的类型，比如 `[f32; 4]` 或者专门的 POD 类型。marlin-math 的 `Vec4` 是 16 字节对齐的，和 scalar 布局对不上（0010 §16）。
- **布局校验**：Development 构建创建 pipeline 时，引擎会做一次比对：
  - 一边是反射里的 push 布局；
  - 另一边是 Rust 类型的 marlin-reflect 布局：字段名（经过 0003 §1.2 的转换）、偏移和大小。
  - 不一致就报错，并指出是哪个字段。
- buffer 元素类型也可以用同一个函数做比对。

## 10. 待定

- query 引擎的具体设计：持久化缓存（跨进程复用 query 结果），以及取消的语义。
- MIR 和 IR 的指令集。
- pass 的具体清单与顺序；自动微分 pass 在流水线里的确切位置。
- DXIL 写出器：LLVM 3.7 bitcode 的子集，以及验证哈希的算法。
- 语言服务器的范围。
