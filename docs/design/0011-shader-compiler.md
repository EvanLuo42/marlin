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
| IR | 两层：前端产出泛型的 MIR，单态化之后是后端的 SSA IR；两层共用同一套指令（0012） | 3 |
| 后端 IR 控制流 | CFG + SSA；分支和循环的终结指令带 merge 块和 continue 块，结构和 SPIR-V 同构，pass 必须保持 | 3.2 |
| 后端 | pass 流水线（LLVM 新 pass manager 式），所有后端共用一条流水线；后端只负责最后的 lower 和写出 | 4 |
| pass 清单 | 保留 DXC 有用的 pass；DXC 因为缺 GPU 语义而关掉的 LICM、unswitch、sinking，补上相应的 analysis 后重新打开；再加上 uniformity、资源别名、值域分析，以及 barrier、groupshared、原子聚合、描述符外提这些 GPU 专用的优化 | 4.4 |
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
  mir           从 HIR 降低到泛型 MIR（与 ir 共用指令集，0012 §2）；直接构造 SSA
  mono          从 EntryRequest 收集实例，单态化，产出后端 IR
  ir            指令集、类型、builder、verifier、打印器和解析器；IR 解释器（CTFE 和差分测试都用它）
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
| **MIR** | 前端的产物：泛型的、按函数做 query 的。指令集和后端 IR 相同，但类型里可以有类型参数、关联类型投影和还没求值的 const 表达式，接口调用也还没解析（0012 §2） | 热重载时只重算改动了的函数；作为单态化的输入 |
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

### 4.1 pass manager

- 参照 LLVM 新 pass manager 的模式：
  - analysis 结果有缓存，每个 pass 声明自己保留了哪些 analysis；
  - pipeline 由 profile 和 target 描述组装出来。
- 两个 profile 都做完整优化，区别只在 `Debug.*` 和调试信息（§5.4）。
- 所有 pass 都必须：
  - 保持结构化 CFG（§3.2）；
  - 保持 debug location；
  - 不对带 `[Precise]` 的计算做重结合、fma 收缩和近似倒数（0010 §9.7）。
- 共享的 pass 只读 target 描述，里面不写 `match target`。

### 4.2 参照：DXC 的流水线

下面是 DXC 的 `PassManagerBuilder::populateModulePassManager` 和 `addHLSLPasses`，以及 `lib/HLSL`、`lib/Transforms/Scalar` 里的 HLSL 专用 pass：

| 阶段 | DXC 的 pass |
|---|---|
| HL 降低 | HLPreprocess、HLDeadFunctionElimination、LowerStaticGlobalIntoAlloca、HLExpandStoreIntrinsics、SROA_Parameter_HLSL、HLMatrixLower、DynamicIndexingVectorToArray、DxilConditionalMem2Reg（跳过 precise）、CleanupDxBreak、DxilConvergentMark、DxilPromoteLocal/StaticResources、DxilGeneration、DxilPrecisePropagate、Scalarizer、DxilEliminateVector、DxilLoopUnroll（`[unroll]`）、DxilFixConstArrayInitializer |
| 标量优化 | IPSCCP、GlobalOpt、DeadArgElim、InstCombine（关掉 sinking）、SimplifyCFG、Inliner、FunctionAttrs、SROA/ScalarRepl、CorrelatedValuePropagation、Reassociate（可选 aggressive）、LoopRotate、IndVarSimplify、LoopDeletion、SimpleLoopUnroll、MergedLoadStoreMotion、GVN、DxilSimpleGVNHoist、DxilSimpleGVNEliminateRegion、SCCP、BitTrackingDCE、DSE、HoistConstantArray、ADCE、Float2Int、LoopUnroll、GlobalDCE、ConstantMerge |
| DXIL 收尾 | DxilEraseDeadRegion、DxilConvergentClear、MultiDimArrayToOneDimArray、DxilRemoveDeadBlocks、DxilMutateResourceToHandle、DxilCleanupDynamicResourceHandle、DxilLowerCreateHandleForLib、DxilTranslateRawBuffer、DxilLegalizeSampleOffset、DxilFinalizeModule、ComputeViewIdState、DxilValidateWaveSensitivity、DxilEmitMetadata |

DXC **故意关掉**了以下几项：
- LICM：不考虑寄存器压力；
- LoopUnswitch：可能把 barrier 移进发散的 if；
- JumpThreading 和 EarlyCSE；
- 指令下沉（HLSLNoSink）；
- 向量化。

另外，SPIR-V 那条路用的是 SPIRV-Tools 的 legalization 和 performance 两套 recipe（这台机器上 DXC 仓库里的这个 submodule 没有检出，内容是按 SPIRV-Tools 上游整理的）。和 LLVM 那套相比，它多出这些 pass：
- vector DCE；
- dead insert elimination；
- if-conversion；
- copy propagate arrays；
- reduce load size；
- combine access chains；
- private-to-local。

我们的思路：DXC 里有用的 pass 都保留；它因为缺少 GPU 语义而关掉的那几项，补上相应的 analysis 后重新打开；再加上 GPU 特有的优化。

### 4.3 关键 analysis

| analysis | 来源 | 用途 |
|---|---|---|
| 支配树、后支配树、loop info、控制依赖 | 标准 | 几乎所有 pass |
| **uniformity（发散）分析** | 新增：按 cycle 处理的发散分析。DXC 只把它用在 WaveSensitivity 验证上 | 自动加 NonUniform 标记；判断 convergent 操作能不能移动；unswitch、if-conversion 和 LICM 的决策；标出 wave 内一致的 load |
| **convergence 信息** | 新增：显式记录哪些操作是 convergent 的（导数、隐式 LOD 采样、wave 操作、barrier） | 替代 DXC 的 DxilConvergentMark/Clear：不靠临时插桩来阻止优化，而是让每个 pass 都去查询它 |
| **资源别名分析** | 新增：基于 bindless 类型的别名分析。`ReadOnly*` 在一次 dispatch 内不变；资源种类不同就不会别名；同一 heap 下标、同一偏移就一定别名 | 跨 buffer 访问的 load CSE、DSE、LICM。DXC 只有一个全局开关 `HLSLResMayAlias` |
| 整数值域分析 | 新增，取代 CorrelatedValuePropagation 依赖的 LVI | 删掉多余的除零检查（0010 §9.6）和多余的边界检查；`Float2Int`；判断移位次数的范围 |
| SCEV | 标准 | IndVarSimplify、unroll 次数、LICM |
| 寄存器压力估计 | 新增：在 SSA 上按 live 区间数估计，阈值来自 target 描述 | LICM、sinking、rematerialization、unroll 的成本 |
| demanded bits | 标准 | BitTrackingDCE、窄化 |

### 4.4 pass 清单与顺序

按阶段列出。"来源"一栏写的是 DXC 有没有这个 pass。

**阶段 0：单态化之后（还没开始优化）**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 0.1 | 单态化 | — | MIR 本身就是 SSA：HIR 降低到 MIR 时，用 Braun 等人的"简单高效的 SSA 构造"直接构造，局部变量不经过 alloca + mem2reg 绕一圈。这一步把 MIR 拷贝一份、代入类型实参、解析接口调用、求出 const 表达式的值（0012 §2、§11） |
| 0.2 | 除零检查插入 | 新增 | 0010 §9.6：除数不是常量时，插入 `d == 0 ? 0 : n / d`。放在优化之前，值域分析可以把多余的检查消掉 |
| 0.3 | 自动微分 | 新增（参考实现） | 前向模式：对 `ForwardDiff` 的调用点生成导数函数；对 `Derivatives.Seed` 出发的数据流，生成 x、y 两个方向的切向量（0010 §15.3）。只为确实用到的导数生成代码。反向模式以后在这里加。放在优化之前，生成的导数代码可以和原代码一起被优化 |
| 0.4 | `Debug.*` 删除或降低 | 新增 | Shipping 构建删除；Development 构建降低成写调试 buffer（0010 §12.5） |

**阶段 1：规范化**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 1.1 | 全量 inline | DXC Inliner | 默认全部内联。只有 target 支持函数调用、并且体积超过阈值时，才保留调用 |
| 1.2 | SROA | DXC SROA、ScalarReplAggregatesHLSL、SROA_Parameter_HLSL | 拆开 struct、元组和下标为常量的数组 |
| 1.3 | mem2reg | DXC DxilConditionalMem2Reg | 处理 SROA 拆出来的局部变量 |
| 1.4 | 动态下标的向量 → 数组 | DXC DynamicIndexingVectorToArray | |
| 1.5 | 多维数组 → 一维数组 | DXC MultiDimArrayToOneDimArray | |
| 1.6 | precise 传播 | DXC DxilPrecisePropagate | 把 `[Precise]` 从值传播到产生它的整条计算链 |
| 1.7 | 局部常量数组提升 | DXC HoistConstantArray、DxilFixConstArrayInitializer | 只读的局部常量数组变成模块级常量 |
| 1.8 | 死函数和死全局变量删除 | DXC GlobalDCE、HLDeadFunctionElimination | |

**阶段 2：标量优化（这一组跑两轮）**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 2.1 | SCCP | DXC SCCP | 稀疏条件常量传播，同时删掉不可达的 region |
| 2.2 | peephole 合并 | DXC InstCombine | 代数化简和窥孔优化。规则用一张声明式的表来写，不像 InstCombine 那样写成手工代码；每条规则都在测试里用 IR 解释器做随机验证（§4.6） |
| 2.3 | 重结合 | DXC Reassociate | 默认就是 aggressive 的（浮点默认宽松），跳过 precise 链 |
| 2.4 | GVN + load PRE | DXC GVN | 用资源别名分析，跨 buffer 访问也能做 load 去重和 PRE |
| 2.5 | GVN 外提 | DXC DxilSimpleGVNHoist | if 的两边都有的相同计算，提到分支之前 |
| 2.6 | 等价 region 消除 | DXC DxilSimpleGVNEliminateRegion | if 的两边计算结果相同时，去掉这个分支 |
| 2.7 | 值域传播 | DXC CorrelatedValuePropagation（替代） | 基于整数值域分析：化简比较，删掉多余的除零检查和边界检查 |
| 2.8 | DSE | DXC DSE | 局部变量、groupshared 和 buffer 上的死写删除，用资源别名分析 |
| 2.9 | 位级 DCE | DXC BitTrackingDCE | |
| 2.10 | ADCE | DXC ADCE | 基于控制依赖，会连同删掉的代码一起删掉空的结构化 region |
| 2.11 | 向量 DCE、死 insert 删除 | SPIRV-Tools | 只对保留向量的 target 生效 |
| 2.12 | 结构保持的 CFG 化简 | DXC SimplifyCFG、DxilEraseDeadRegion、DxilRemoveDeadBlocks | 合并块、折叠常量分支、删除死 region，始终保持 merge 结构 |
| 2.13 | Float2Int | DXC Float2Int | 值域允许时，把浮点运算改成整数运算 |

**阶段 3：循环**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 3.1 | 循环旋转 | DXC LoopRotate | |
| 3.2 | 归纳变量化简 | DXC IndVarSimplify | 基于 SCEV |
| 3.3 | 死循环删除 | DXC LoopDeletion、DxilLoopDeletion | |
| 3.4 | 强制展开 | DXC DxilLoopUnroll | `[Unroll]`；还有为了合法性必须展开的循环，比如采样偏移和 `const` 数组的下标依赖归纳变量时。展开失败就报错 |
| 3.5 | 启发式展开 | DXC LoopUnroll、SimpleLoopUnroll | 成本模型里算上寄存器压力 |
| 3.6 | **按压力做 LICM** | 新增（DXC 关掉了） | 外提不变量，但外提后的估计压力不能超过 target 的预算；convergent 操作只在循环条件一致时外提 |
| 3.7 | **按 convergence 做 unswitch** | 新增（DXC 关掉了） | 只对一致的条件做 unswitch，循环里有 convergent 操作时更要求这一点，所以不会把 barrier 移进发散的分支 |
| 3.8 | 合并 if 两边的 load/store | DXC MergedLoadStoreMotion | |

**阶段 4：GPU 专用**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 4.1 | NonUniform 标记 | 新增 | 基于 uniformity 分析，给发散的资源下标加标记（0010 §10.3），并遵守作者的 `[Uniform]` 和 `NonUniform()` |
| 4.2 | 描述符获取的 CSE 和外提 | 新增（DXC 只有 DxilCleanupDynamicResourceHandle） | 同一个 heap 下标的描述符只取一次，一致的下标提到入口附近 |
| 4.3 | 冗余 barrier 删除 | 新增 | 两个 barrier 之间没有 groupshared 或 buffer 访问时，删掉其中一个；入口开头和结尾多余的 barrier 也删掉 |
| 4.4 | groupshared 的死数据删除与紧凑化 | 新增 | 删掉没有被读过的 groupshared 写入和字段；各个变量的生命周期在 barrier 区间上不重叠时，让它们共用一段空间 |
| 4.5 | wave 级原子聚合 | 新增（target 描述打开时才做） | 同一个 wave 对一致地址做的 `AtomicAdd`/`Min`/`Max`/`Or`/`And`，改成 `Wave.Sum`（或相应的规约）加上一个 lane 执行一次原子操作 |
| 4.6 | 小菱形 if-conversion | SPIRV-Tools | 发散的、两边都很小又没有副作用的 if，改成 select；一致的 if 保留分支 |
| 4.7 | **按压力做 sinking 和 rematerialization** | 新增（DXC 关掉了 sinking） | 把计算下沉到使用它的分支里；地址计算、描述符获取这类廉价的值在使用处重算，降低 live 范围；阈值来自 target 描述 |

**阶段 5：按 target 参数化的 legalize**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 5.1 | 矩阵降低 | DXC HLMatrixLower | 矩阵变成向量和标量，按存储布局读写 |
| 5.2 | 资源降低 | DXC DxilPromote*Resources、DxilMutateResourceToHandle、DxilLowerCreateHandleForLib | 资源值（下标）→ heap 访问；target 描述决定怎么取描述符 |
| 5.3 | 内存访问降低 | DXC DxilTranslateRawBuffer、SPIRV-Tools reduce load size | scalar 布局的 load/store → target 的内存操作：按对齐拆分或合并，`Uniform<T>` 按 16 字节行读取 |
| 5.4 | 类型 legalize | 新增 | 16 位、64 位、bool 的寄存器表示；target 不支持的类型直接报错（已经由 capability 检查过） |
| 5.5 | 内建函数展开 | DXC DxilExpandTrigIntrinsics | target 没有原生指令的函数（三角函数、`Fwidth` 等）展开成多条指令 |
| 5.6 | 采样偏移 legalize | DXC DxilLegalizeSampleOffset | 偏移必须是立即数；不是就报错，并给出原因 |
| 5.7 | 插值和 eval legalize | DXC DxilLegalizeEvalOperations | |
| 5.8 | 向量化策略 | DXC Scalarizer、DxilEliminateVector | 标量 target（DXIL）全部标量化；向量 target（SPIR-V、Metal）保留向量 |
| 5.9 | stage I/O 打包 | DXC DxilPackSignatureElement | 按 target 规则把 stage I/O 字段打包进寄存器 |
| 5.10 | 收尾清理 | DXC DxilFinalizeModule、DxilDeleteRedundantDebugValues | 跑一轮 DCE 和 CFG 化简，清理多余的 debug value |

**阶段 6：后端**

- 后端自己的 lower 和写出（DXIL 元数据、SPIR-V 装饰等），然后由后端做验证（§4.6）。

### 4.5 不做的

| pass | 原因 |
|---|---|
| JumpThreading | 会破坏结构化 CFG；能带来的收益大部分由 SCCP、值域传播和 if-conversion 覆盖 |
| 自动向量化（Loop/SLP） | 标量 target 不需要；向量 target 本来就保留源码里的向量 |
| 尾调用消除、异常相关 | 语言里没有对应的概念 |
| memcpy 和 memset 优化（LoopIdiom、MemCpyOpt） | IR 里没有 memcpy |
| 跨过程优化（IPSCCP、DeadArgElim、ArgumentPromotion） | 默认全量 inline 之后就用不上了 |

### 4.6 正确性保证

- **IR verifier**：
  - 检查 SSA、类型、结构化 CFG 的不变量，以及 convergent 操作没有被移进发散控制流；
  - Development 构建的编译器在每个 pass 后都跑一遍；release 构建只在流水线的开头和结尾跑。
- **IR 解释器**：
  - 后端 IR 有一个参考解释器，按 wave 语义执行，也支持 groupshared 和 barrier；
  - 测试里对每个 pass 做差分测试：用随机生成的 IR 和随机输入，比较 pass 前后的执行结果；
  - peephole 规则表里的每条规则也用它验证。
- **后端验证**：DXIL 写出之后跑我们自己的验证器（规则对齐 DxilValidation），SPIR-V 写出之后检查 SPIR-V 的合法性规则。都不依赖外部工具。

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
- CTFE 的执行方式：先把用到的 `const` 函数单态化，再用 IR 解释器执行（0012 §11）。

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
- **跨进程缓存**用的是 manifest 模式（ccache 和 Bazel 也是这么做的）：
  - 不编译，就不知道一个入口用到了哪些文件。所以 DDC 里额外存一份"请求 → 上次用到的源文件及其内容哈希"。
  - 查缓存时先核对这些哈希。全部没变就直接取 blob，不进编译器。
  - 只有依赖了改动文件的入口才会重编。
- 第一版只缓存最终的 blob，前端 query 的结果不落盘。
  - 在 cookd 里，增量靠内存中的 `Session`。
  - 在新进程里，需要重编的入口，它们的前端从头再走一遍。
  - 以后如果实测发现标准库的前端开销明显，再加 rustc 式的持久化 query 缓存。

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

- query 引擎的具体设计：取消的语义。第一版不做持久化，以后如果实测发现标准库的前端开销明显，再考虑（§8）。
- peephole 规则表的格式；寄存器压力模型的参数（按 target）。
- DXIL 写出器：LLVM 3.7 bitcode 的子集，以及验证哈希的算法。
- 语言服务器的范围。
