# 0005 — Marlin shader 编译器：架构与 API

状态：草案 · 2026-10-16

前置：
- [0002](0002-shader-interface.md)：shader ABI
- [0003](0003-asset-pipeline.md)：cook 键、DDC、marlin-reflect
- [0004](0004-shader-language.md)：语言

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 形态 | 纯 Rust 库，没有 CLI；shader 专有的 crate 放在 `compiler/shader/`，和 VM 语言共用的前端放在 `compiler/common/`、`compiler/front/`（0015）；主机平台的后端放在 `platforms/<p>/shader/` | 2 |
| 前端 | 由 query 驱动，增量编译；query 引擎自己写一个精简版 | 2 |
| IR | 按实例降低：每个实例（函数加具体实参）直接从 HIR 降低成具体的 SSA IR，IR 里没有泛型（0006 §2，2026-10-17 改） | 3 |
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
| rustc | query 驱动的前端；单态化时收集实例；与后端无关的 codegen trait；span、errors、session 等基础设施拆成独立的 crate | 体量（宏展开、借用检查这些我们都不需要） |
| rust-analyzer | 无损 CST 加上能容错的 parser；同一份前端同时服务 IDE 和编译 | — |
| slang | 在 IR 上做自动微分；capability 推断；带 merge 标注的结构化 CFG；按 target 参数化的 legalize | 预处理器；泛型直接留在 IR 上（让 pass 变复杂）；以文本 emitter 为主的后端 |
| DXC | LLVM 风格的 pass 流水线；DXIL 的容器和验证哈希 | SPIR-V 是另一条从 AST 出发的路，两条路的功能长期对不齐 |

原则：
- 前端用 query，后端用 pass 流水线。
- 所有 target 共用同一条 IR 流水线。
- 加一个后端只需要一个 crate：target 描述、`Backend` 实现和格式写出器。共享代码里不允许出现 `match target`，target 之间的差异放进 target 描述里。
- target 描述也是前端 query 的输入，capability 检查和 `const if` 依赖它。

## 2. crate 划分

前端的通用部分和以后的 VM 语言共用（0015）：语言无关的基础设施在 `compiler/common`，C# 系语言共用的语法和名字解析在 `compiler/front`，shader 专有的部分在 `compiler/shader`。

```
compiler/common/          （0015 §1，包名 marlin-lang-*）
  span          源码位置、文件 id、行列映射
  diag          结构化诊断与文本渲染器
  arena         arena、interning、类型化下标
  query         增量 query 引擎（红绿算法、依赖记录、取消、确定性）
compiler/front/           （0015 §1，包名 marlin-front-*）
  syntax        lexer、无损 CST、超集文法的 parser、按 dialect 的 validation、有类型的 AST 视图
  hir_def       item tree、包与 namespace 的 def map、名字解析、把函数体降低成 HIR
compiler/shader/          （包名 marlin-shaderc-*）
  syntax        shader 的 Dialect（0015 §2.2）和解析入口
  hir_ty        类型检查与推断、泛型与约束求解、capability 推断、const 求值的入口
  lower         从 EntryRequest 收集实例，把每个实例从 HIR 直接降低成 IR（直接构造 SSA）；CTFE（0006 §2、§11）
  ir            指令集、类型、builder、verifier、打印器和解析器；IR 解释器（CTFE 和差分测试都用它）
  opt           pass manager 和各个 pass
  target        target 描述（capability、布局规则、资源模型、支持的操作）
  backend_dxil  DXIL：LLVM 3.7 bitcode 写出器、DXIL 元数据、容器和验证哈希
  backend_spirv SPIR-V 写出器
  reflect       从 IR 构造反射；反射的数据类型在 runtime 的 `marlin-shader`（`runtime/engine/shader` 的 `reflect` 模块）里（运行时读它，运行时不能依赖 compiler/，2026-10-17 定）
  shaderc       facade marlin-shaderc：Session、target 注册表、EntryRequest
  core          核心库（`Marlin` 包，0004 §17）的 `.mshader` 源码，用 include_str! 嵌进编译器；`Session::new` 自己加上这个包，所以编译器和核心库永远配套，cook 指纹也覆盖它（2026-10-17 定）
  testgen       只给测试用：随机生成的结构化 IR 程序，以及包住它的 compute 入口（pass 和后端的差分测试共用）
platforms/<p>/shader/  主机平台的后端（在 cook 平台 cfg 下通过 #[path] 引入）
```

- facade 叫 `marlin-shaderc`：`marlin-shader` 这个名字已经给了运行时的 shader 容器（`runtime/engine/shader`）。
- `compiler/` 在 0001 的分层里位于 runtime 之上、tools 之下：可以依赖 runtime，tools 可以依赖它，runtime 不能依赖它。

依赖方向：
- `common` ← `front` ← `shader`（0015 §1）；
- `syntax` → `hir_def` → `hir_ty` → `lower`，每一层都只依赖它前面的层；`lower` 还依赖 `ir`；`opt` 只依赖 `ir` 和 `target`；
- 每个后端只依赖 `ir`、`target` 和 `reflect`；
- `marlin-shaderc` 把上面这些组装起来。

## 3. IR

### 3.1 按实例降低

- `lower` 从 `EntryRequest` 出发，收集所有用到的实例，包括泛型函数、`Func` 参数和 lambda（0004 §9.2）。每个实例直接降低成具体的 SSA IR，按入口组织成 module（0006 §2；2026-10-17 改，原来是"泛型 MIR + 单态化"两层）。
- 每个实例是一个 query，热重载时只重算改动了的函数的实例。
- IR 上做的事：自动微分、uniformity 和 NonUniform 分析、legalize、所有优化。
- 编译期递归在单态化时展开，运行时递归在这里报错（0004 §6）。

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
  - 不对带 `[Precise]` 的计算做重结合、fma 收缩和近似倒数（0004 §9.7）。
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
| **uniformity（发散）分析** | 新增：按 cycle 处理的发散分析，分 wave 和 group 两级（第 8c 步）。值：参数、每个 lane 不同的输入、原子操作、经过 `function` 变量的 load、reduce/broadcast/vote/ballot 以外的 wave op（对 group 是所有 wave op）不一致，`assume.uniform` 一致，其他的跟着操作数走；控制：按后支配算控制依赖，依赖于条件不一致的分支的块只有部分 lane 执行（`return` 也算在内）；汇合：条件不一致的 region 的各个出口处，块参数不一致；lane 离开时间不同的 loop，它定义的值在 loop 之后不一致。DXC 只把它用在 WaveSensitivity 验证上 | 自动加 NonUniform 标记；判断 convergent 操作能不能移动；unswitch、if-conversion 和 LICM 的决策；标出 wave 内一致的 load |
| **convergence 信息** | 新增：显式记录哪些操作是 convergent 的（导数、隐式 LOD 采样、wave 操作、barrier） | 替代 DXC 的 DxilConvergentMark/Clear：不靠临时插桩来阻止优化，而是让每个 pass 都去查询它 |
| **资源别名分析** | 新增：基于 bindless 类型的别名分析。`ReadOnly*` 在一次 dispatch 内不变；资源种类不同就不会别名；同一 heap 下标、同一偏移就一定别名 | 跨 buffer 访问的 load CSE、DSE、LICM。DXC 只有一个全局开关 `HLSLResMayAlias` |
| 整数值域分析 | 新增，取代 CorrelatedValuePropagation 依赖的 LVI | 删掉多余的除零检查（0004 §9.6）和多余的边界检查；`Float2Int`；判断移位次数的范围 |
| SCEV | 标准 | IndVarSimplify、unroll 次数、LICM |
| 寄存器压力估计 | 新增：在 SSA 上按 live 区间数估计，阈值来自 target 描述 | LICM、sinking、rematerialization、unroll 的成本 |
| demanded bits | 标准 | BitTrackingDCE、窄化 |

### 4.4 pass 清单与顺序

按阶段列出。"来源"一栏写的是 DXC 有没有这个 pass。

**阶段 0：单态化之后（还没开始优化）**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 0.1 | 按实例降低 | — | 每个实例从 HIR 直接降低成 SSA：用 Braun 等人的"简单高效的 SSA 构造"，局部变量不经过 alloca + mem2reg 绕一圈；降低时代入类型实参、解析接口调用、求出 const 表达式的值（0006 §2、§11） |
| 0.2 | 除零检查插入 | 新增 | 0004 §9.6：除数不是常量时，插入 `d == 0 ? 0 : n / d`。放在优化之前，值域分析可以把多余的检查消掉 |
| 0.3 | 自动微分 | 新增（参考实现） | 前向模式：`ForwardDiff` 的每个调用点是一个切向通道；`Derivatives.Seed` 出发的数据流有 x、y 两个通道（0004 §15.3）。只为确实用到的导数生成代码。反向模式以后在这里加。2026-10-18 改：降低时只留下标记（0006 §8.5），这个 pass 实际放在 1.3 mem2reg 之后、阶段 2 之前，在 inline 过的单个 SSA 函数里传播切向量，生成的导数代码仍然和原代码一起被优化；原来的设计是在 inline 之前为每个调用点生成导数函数 |
| 0.4 | `Debug.*` 删除或降低 | 新增 | Shipping 构建删除；Development 构建保留 IR 里的 `debug.print`/`debug.assert`，由各后端降低（2026-10-17 定）：SPIR-V 直接用 `NonSemantic.DebugPrintf`（由验证层输出，第 10f 步实现），DXIL 和 Metal 没有对应的东西，写调试 buffer（0004 §12.5），buffer 的格式和绑定随 RHI 一起定 |

**阶段 1：规范化**

| # | pass | 来源 | 说明 |
|---|---|---|---|
| 1.1 | 全量 inline | DXC Inliner | 默认全部内联。只有 target 支持函数调用、并且体积超过阈值时，才保留调用。被调函数放进一个只跑一次的 loop 里，`return` 变成跳出这个 loop；`return` 在被调函数的 loop 或 switch 里时，先存返回值、置一个标志，跳到最内层那一层的 merge，在每一层 merge 处检查标志、继续往外跳，所以 break 永远只出最内层（第 8b 步） |
| 1.2 | SROA | DXC SROA、ScalarReplAggregatesHLSL、SROA_Parameter_HLSL | 拆开 struct、元组和下标为常量的数组 |
| 1.3 | mem2reg | DXC DxilConditionalMem2Reg | 处理 SROA 拆出来的局部变量 |
| 1.4 | 动态下标的向量 → 数组 | DXC DynamicIndexingVectorToArray | 只有 DXIL 需要（SPIR-V 有动态下标的向量操作），随 DXIL 后端一起做 |
| 1.5 | 多维数组 → 一维数组 | DXC MultiDimArrayToOneDimArray | 同上 |
| 1.6 | precise 传播 | DXC DxilPrecisePropagate | 把 `[Precise]` 从值传播到产生它的整条计算链 |
| 1.7 | 局部常量数组提升 | DXC HoistConstantArray、DxilFixConstArrayInitializer | 只读的局部常量数组变成模块级常量 |
| 1.8 | 死函数和死全局变量删除 | DXC GlobalDCE、HLDeadFunctionElimination | 有入口的 module 删掉入口调用不到的函数；没有入口的 module（函数库，测试里这样用）保留所有函数；没被用到的模块级变量都删 |

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
| 4.1 | NonUniform 标记 | 新增 | 基于 uniformity 分析，给发散的资源下标加标记（0004 §10.3），并遵守作者的 `[Uniform]` 和 `NonUniform()` |
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
- SPIR-V 的布局类型（2026-10-18）：API 布局的内存（storage buffer、uniform、push constant）里，struct 和数组有一份带 `Offset`/`ArrayStride` 的布局类型，别处用不带装饰的普通类型，整体读写用 `OpCopyLogical` 转换。NVIDIA 驱动经 descriptor heap 的 buffer 指针读的时候，会忽略类型是 struct 或 struct 数组的成员的 `Offset`（0016 §4.2），所以布局类型里的 struct 不含这两种成员，写出器这样处理：
  - **嵌套 struct 展开**：内层 struct 的成员直接成为外层 struct 的成员，各自带算好的偏移，名字是 `Now.R0` 这样的路径。字段到成员的对应由 `Writer::flat_start` 算。指向嵌套 struct 的指针没有对应的 SPIR-V 类型，函数体里记成"外层 struct 的指针 + 起始成员"（`body.rs` 的 `Place::Flat`），继续往下 access 时换算成平的成员下标；整体读写逐成员 load/store，再用 `OpCompositeConstruct`/`OpCompositeExtract` 拼回或拆开。
  - **struct 数组成员按字存**：成员如果是 struct 的数组（可以多维），写成同样大小的 32 位字数组（`ArrayStride` 4），因为下标可以是动态的，没法展开。里面的位置记成"字数组指针 + 字下标"（`Place::Words`），下标按 scalar 布局的偏移和步长计算；读写按叶子一个字一个字地做，float 用 `OpBitcast`，64 位是两个字，bool 是 0/1 的字。这种成员里的原子操作只支持 32 位整数，16 位成员还不支持（会报错）。
  - 其余成员（标量、向量、矩阵、元素不是 struct 的数组）照旧。整体读写一个含这些成员的布局值时，逐成员转换，不再用 `OpCopyLogical`。
  - buffer 的运行时数组、uniform 和 push 的 Block 里，被包着的 struct 在偏移 0，不受影响。

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
- **后端验证**：DXIL 写出之后跑我们自己的验证器（规则对齐 DxilValidation），SPIR-V 写出之后检查 SPIR-V 的合法性规则。都不依赖外部工具。SPIR-V 的验证器（`backend_spirv` 的 `validate`，第 9b 步）查后端用到的那部分规则：模块结构、id、类型、Vulkan 的布局规则、入口的 interface、能力和扩展、函数里的类型和支配关系、SPIR-V 2.11 的结构化控制流；它不认识的指令会明说。测试把 fixture 故意改坏，验证器必须报出来；随机改坏的模块上它和 `spirv-val` 的判定比较（可选，0014 §6.1），它可以比 `spirv-val` 宽，但不能拒绝 `spirv-val` 接受的模块（Vulkan 要求而 `spirv-val` 不查的规则除外）；fuzz target `shaderc_backend_spirv_validate` 保证它在任何输入上都不 panic。

## 5. API

### 5.1 Session 与请求

```rust
let mut s = Session::new(SessionDesc { sources: &provider, targets: &registry });
s.add_package(&PackageDesc { name: "Marlin.Substrate", dependencies: &["Marlin"] })?;

let out = s.compile(&EntryRequest {
    entry: "Marlin.Material.Entries.ResolvePixel",
    type_args: &["Game.Rock"],
    consts: &[("Game.Rock.UseMoss", ConstValue::Bool(true))],
    target: "d3d12-sm6.6",
    profile: Profile::Development,
})?;
// out.code：单入口的字节码；out.reflection；out.diagnostics；out.used_sources

s.invalidate(&path);   // 源文件变了，相关的 query 会重算
```

- 包由宿主描述：名字和直接依赖。宿主自己解析 `ShaderPackage` 清单（0004 §7.2），文件由 `SourceProvider::list` 按包名列出；编译器不读清单（0015 §3.1，2026-10-17 定）。
- `Session::entries(package)` 列出包里能直接编译的入口：public、带 stage 属性、不带类型参数的 namespace 函数，按全名排序。cook 用它把整个包编成库（0003 §4.4，2026-10-18）。
- `Session` 的生命周期很长：
  - cookd 进程里一直保持一个，热重载时只重算受影响的 query；
  - build.rs 创建一个，一次编完所有入口；
  - 以后的语言服务器也用同一套前端。
- 一个请求对应一个入口、一组类型实参和常量、一个 target 和一个 profile，产出**一个单入口 blob**。这和 0002 的"每个 stage 一个 module"一致，也是 cook 键和 DDC 最细的粒度。
- 可以把多个请求作为一批提交。
- target 用名字从注册表里查，比如 `"d3d12-sm6.6"`、`"vulkan-1.4"`、`"metal-3"`。名字写在 cook 平台的设置里。
- 静态参数的值用反射的 `ConstValue`（`marlin_shader::reflect`），请求和反射里的写法一样。
- 一个请求的流程（第 9c 步）：入口能看到的所有包的前端诊断（有错误就停）→ 降低 → 入口需要的 capability 和 target 对比 → 优化 → 后端写出 → 后端的验证器 → 反射。Session 自己的错误码是 E15xx：E1501 没有这个 target，E1502 路径不指向任何声明，E1503 不是静态参数或者值的类型不对，E1504 target 缺入口需要的 capability，E1505 后端写不出这个入口，E1506 编译器 bug（模块验证不过、验证器拒绝后端写出的代码）。

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

| profile | 优化 | `Debug.*`（0004 §12.5） | 调试信息 |
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

- 反射是普通的 Rust 数据类型，放在运行时的 `marlin-shader`（`reflect` 模块）里，由编译器的 `reflect` crate 构造；类型都注册到 marlin-reflect。
- cook 时按 0003 的通用格式写进 cooked 单元。调试时可以直接导出成 `.masset` 文本。
- 每个产物的反射包括：
  - 入口：stage、线程组大小、mesh 的容量和拓扑；
  - push 布局：字段名、类型、偏移、大小，以及字段上的 attribute；
  - 静态参数和它们的值；
  - 材质参数块的布局：字段名、类型、偏移、默认值、用户 attribute；
  - 实现了哪些可选方法；
  - 用到的 capability，以及引用了哪些 `Target.*` 常量（用来推导 permutation 维度）；
  - `[Export]` 常量的值（§6.2）；
  - 用到的源文件和它们的内容哈希。
- 后端专用的信息（比如主机平台的资源表）放在扩展字段里，由后端 crate 定义。
- 第 9c 步先做 compute 需要的部分：入口、push 布局、静态参数、用到的 capability、`[Export]` 常量、用到的源文件和哈希、统计。材质参数块和可选方法跟着图形 stage 在第 10 步做。`Target.*` 常量还不存在，引用列表等它有了再填（2026-10-17 定）。
- 第一版"用到的源文件"是入口所在包及其依赖的传递闭包里的所有文件。名字解析读了包里每个文件的 item tree，所以这个集合不会漏，只是偏大。

### 6.2 编译期求值与导出

材质库需要反射给出 Substrate 树展平后的 closure 数量和特性位。这类"库自己定义的编译期信息"，用编译期求值来导出，编译器不需要知道 Substrate 是什么：

```csharp
public const ClosureInfo Flatten<M>() where M : IMaterial { ... }

// 入口模板里
[Export] const ClosureInfo Topology = Flatten<M>();
// 反射：exports["Topology"] = { Count = 2, Features = Aniso | Fuzz }
```

- 语言层面的规则见 0004 §13.4。
- CTFE 的执行方式：先把用到的 `const` 函数单态化，再用 IR 解释器执行（0006 §11）。

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
- 编译器源码的哈希由 `marlin-shaderc` 的 build.rs 算：`compiler/` 下每个 crate 的 `Cargo.toml` 和 `src/` 里的文件，按相对路径排序，换行统一成 `\n`，取 xxh3-128。测试和测试数据不算在内（第 9c 步）。
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
- 向量字段要用 4 字节对齐的类型，比如 `[f32; 4]` 或者专门的 POD 类型。marlin-math 的 `Vec4` 是 16 字节对齐的，和 scalar 布局对不上（0004 §16）。
- **布局校验**：Development 构建创建 pipeline 时，引擎会做一次比对：
  - 一边是反射里的 push 布局；
  - 另一边是 Rust 类型的 marlin-reflect 布局：字段名（经过 0003 §1.2 的转换）、偏移和大小。
  - 不一致就报错，并指出是哪个字段。
- buffer 元素类型也可以用同一个函数做比对。

## 10. 待定

- query 引擎的设计已定（0015 §4）。第一版不做持久化，以后如果实测发现标准库的前端开销明显，再考虑（§8）。
- peephole 规则表的格式；寄存器压力模型的参数（按 target）。
- DXIL 写出器：LLVM 3.7 bitcode 的子集，以及验证哈希的算法。
- 语言服务器的范围。

## 11. 实现顺序

桌面平台（Windows、Linux）以后只用 Vulkan，Windows 上的 D3D12 支持会移除（2026-10-17）。所以第一个后端是 SPIR-V；DXIL 留给仍然需要 D3D12 的平台（Xbox 等），放到第 11 步。

按 §2 的依赖方向自下而上，每一步都带测试、单独提交：

| 步 | 内容 | 完成的标志 |
|---|---|---|
| 1 | `common`：`span`、`diag`（含文本渲染器）、`arena` | 诊断能渲染成 rustc 风格的文本 |
| 2 | `front/syntax`：lexer、无损 CST、超集文法的 parser、validation、有类型的 AST 视图；`shader/syntax`：shader 的 dialect | 覆盖 0004 的全部语法；任意输入都不 panic，并且能原样往返 |
| 3 | `common/query`：红绿增量引擎 | 改一个文件只重算依赖它的 query |
| 4 | `front/hir_def`：4a 是 item tree、def map、名字解析；4b 是函数体降低成 HIR 和局部作用域 | 包内跨文件、跨 namespace 的名字都能解析 |
| 5 | `shader/hir_ty`：5a 是类型表示、内建类型（0004 §17）、类型降低和签名；5b 是表达式的类型推断、转换、运算符、成员查找和重载；5c 是其余语义检查（确定赋值、readonly、穷尽、接口实现、入口和系统值、push 大小、scalar 布局、capability 推断和 `[Requires]` 契约） | 0004 的类型规则都有诊断 |
| 6 | `ir`：6a 是类型、指令集、函数和 module 的表示、builder、文本打印和解析；6b 是 verifier；6c 是解释器 | 文本能往返；解释器能跑 compute 语义 |
| 7 | `lower`：7a 是非泛型代码降低成 SSA IR（表达式、语句、结构化控制流、调用、内建函数）；7b 是实例收集、泛型、接口调用、lambda；7c 是入口（系统值、push、资源）和 CTFE（const 字段、静态参数、数组长度、const 函数） | 泛型入口能单态化，`const` 函数能在编译期求值 |
| 8 | `opt`：8a 是 pass manager、控制流分析、结构保持的 CFG 化简、DCE、SCCP 和差分测试（随机生成的结构化函数，用解释器比较 pass 前后）；8b 是阶段 0–1 的 pass（1.4、1.5 只有 DXIL 需要，随 DXIL 后端一起做）；8c 是 uniformity 分析和 verifier 的 convergence 检查（0006 §5.3），并核对 verifier 的 break 规则和 SPIR-V 一致（break 只能去最内层 loop 或 switch 的 merge；现在的 verifier 接受任何一层外面的 loop） | 第一个 compute 入口能走完整条流水线 |
| 9 | 9a 是 `target`（target 描述、`Backend` 接口、注册表）和 `backend_spirv` 的 compute 部分（SPIR-V 1.6、原生 descriptor heap，0002 §6）；9b 是我们自己的 SPIR-V 验证器；9c 是 `reflect` 和 facade `Session`；9d 是真正的核心库；9e 是第一个 compute shader 在真实 GPU 上跑通，和解释器比较 | 第一个 compute shader 生成的 SPIR-V 通过我们自己的验证器；Vulkan RHI 就绪后在真实 GPU 上跑通 |
| 10 | 补齐其余 stage、资源、wave、纹理，替换占位 shader。10a 是 vertex 和 pixel 入口及其 stage I/O；10b 是纹理和采样器；10c 是 mesh 和 amplification；10d 是 ray query；10e 是测试运行时编译 shader 的工具、把全部占位 HLSL 换成 Marlin shader（2026-10-17 定：先补齐功能，再一次换完），并在真实 GPU 上跑通；10f 补上调试输出、有符号纹理、比较采样、gather、偏移和尺寸查询；10g 是 visbuffer 渲染器要的导数、64 位整数和原子操作、纹理原子操作 | RHI 和其他 crate 的 GPU 测试全部用 Marlin shader 通过；仓库里没有 HLSL、DXIL 和 dxc 编出的 SPIR-V |
| 11 | 阶段 2–5 的其余 pass、自动微分、调试信息（NonSemantic.Shader.DebugInfo）、其他后端：DXIL（Xbox 等需要 D3D12 的平台）、Metal、主机 | |

第 9d 步的核心库（`compiler/shader/core`）覆盖 compute 用得到的部分：
- 标量、向量、矩阵和全部短名；
- 数值接口；数学函数；`Select`、`All`、`Any`；
- `Uniform`、`Buffer`、`ReadOnlyBuffer`、`ByteBuffer`、`ReadOnlyByteBuffer` 和原子操作，`Func`/`Action`；
- `Barrier`、`Wave`、`Debug`、`NonUniform`；
- 编译器认识的所有 attribute，以及 `Capability`。

纹理、采样器、导数、ray query 和 mesh 跟着图形 stage 在第 10 步做。为此前端和后端也补了一些：
- 类型检查：成员和运算符查找沿接口的继承走；`uint3.One` 这类别名上的静态成员；类型已经确定的参数按隐式转换接收字面量（`b.AtomicAdd(0, 1)`）。
- 降低：通过类型读的静态属性（`Wave.LaneIndex`、`T.Zero`）；按种类选 op 的 intrinsic；`Uniform.Value` 和字节 buffer 的读写。
- SPIR-V 后端：uniform 和字节 buffer。

Development 下的 `Debug.Print` 第 10f 步在 SPIR-V 上实现了，用 `NonSemantic.DebugPrintf`。

第 10b 步加了纹理和采样器：
- 核心库：`Textures.mshader` 里的十种纹理和两种采样器；`AsUInt`、`AsInt`、`AsFloat`；字节 buffer 的 `Load2`–`Load4`、`Store2`–`Store4`。
- 降低：可选参数按形参名传，调用没写的就不传；`lod`、`bias`、`grad` 最多写一个；没有 `lod`/`grad` 的采样只在 pixel 入口里允许（E1309；第 11 步取消了这条限制，导数由自动微分给出）。整数纹素按无符号读，有符号整数的纹理先报 E1301。
- SPIR-V 后端：纹理和采样器从 heap 里 `OpLoad` 出来；sampler heap 的槽宽是 `OpConstantSizeOfEXT(sampler)`；采样和 fetch 都取四个分量，再按纹素类型截短；storage 纹理用无格式的读写，也都按四个分量；`desc.get nonuniform` 加 `NonUniform` 装饰。
- 比较采样、gather、偏移、尺寸查询和纹理原子操作，用到时再加（前四个第 10f 步加了，纹理原子操作第 10g 步）。

第 10c 步加了 mesh 和 amplification：
- 核心库：`MeshTopology`，`MeshOutput<TVertex>`、`MeshOutput<TVertex, TPrimitive>`（`SetCounts`、`Vertices`、`Primitives`，以及 `Triangles`、`Lines`、`Points`），只能写的 `MeshElements<T>`，`Mesh.Dispatch`，`[Mesh]` 的 `MaxVertices`、`MaxPrimitives`、`Topology` 字段，`[Payload]`。
- 前端：`Name = value` 写法的 attribute 实参不算位置实参。
- 降低：`MeshOutput` 参数变成 mesh 输出变量，它本身和它的列表只是常量标记，写入时找到对应的变量；amplification 入口传给 `Mesh.Dispatch` 的 `[GroupShared]` 字段放在 `payload` 空间里；第一版的 `Mesh.Dispatch` 规则（必须是最后一条语句，只调用一次，前面不能有 `return`）、写错下标列表、在 `SetCounts` 之前写输出都报 E1308。
- SPIR-V 后端：`SPV_EXT_mesh_shader` 的 `MeshEXT`、`TaskEXT`；输出是 `Output` 数组，图元输出加 `PerPrimitiveEXT`，下标列表是 `Primitive*IndicesEXT`；`OutputVertices`、`OutputPrimitivesEXT` 和拓扑从输出数组推出来；`OpSetMeshOutputsEXT`；`OpEmitMeshTasksEXT` 带上 payload 变量，结束这个块。

第 10d 步加了 inline ray query：
- 核心库：`RayTracing.mshader` 里有 `Ray`、`[Flags] RayFlags`、`HitKind`、`RayHit`（`InstanceIndex` 和 `InstanceId` 都有）、`RayQuery`（构造函数、`Proceed`、`CommitTriangle`、`Committed`、`Candidate`），以及 `AccelerationStructure` 的 `TraceAny`、`TraceClosest`。后两个用 Marlin 写，循环调用 `Proceed`，遇到三角形候选就提交。它们需要 `Capability.RayQuery`。
- 类型检查：任何 enum 都有 `==`、`!=`；`[Flags]` enum 还有 `&`、`|`、`^`，结果还是这个 enum。声明了构造函数的内建类型可以构造。
- 降低：`RayQuery` 是指向它自己那个 `rayquery` 变量的指针；只有一个值的块参数被去掉（trivial phi），所以循环里的 query 还是同一个指针；intrinsic 的结果类型取成员声明的类型；常量路径也可以当默认实参；构造函数、静态函数和命名空间函数不带接收者。
- SPIR-V 后端：`SPV_KHR_ray_query`；`rq.get` 对应 `OpRayQueryGet*KHR`。candidate 的种类加 1，和 `HitKind` 对齐。`vulkan-1.4` target 把 `RayQuery` 当作可选 capability，设备没有它就跳过，反射里会列出来。加速结构的取法见第 10e 步。
- ray tracing pipeline（raygen、hit、miss）不在第 10 步里。

第 10e 步把占位 shader 全部换掉，并在真实 GPU 上跑通（RTX 5060，GPU tier 132 个测试，2026-10-18）：
- 测试 shader：`marlin-gpu-test-support` 在运行时用 `Session` 编译，见 0002 §5。共用包 `Test` 有 22 个 entry，cook_texture 自己的包里有 `TexelLoad`。
- 引擎 shader 交给 cooker：`RendererDesc` 的 shader 由调用方传入，`framework::run` 在 cooker 能编 shader 之前只清屏。
  - 后续（2026-10-18）：`cook_shader` 能编 shader 包了。forward shader 移到 `content/engine/shaders/`，`Test` 包剩 20 个 entry；`framework::run` 用 cooked 的库画 mesh（0003 §4.4）。
- 硬件上发现的问题：
  - RHI 要启用 `VK_KHR_shader_untyped_pointers`；
  - RHI 的槽宽改成正好等于 shader 算出的值，不对齐的 device 不支持；
  - 加速结构改经 set 0 binding 0 读取，因为从 `ResourceHeapEXT` 直接 load 出来的，在 NVIDIA 驱动上做 ray query 什么也找不到（0016 §4.2）。

第 10f 步补上了第 10 步漏下的部分（2026-10-18，决定记在 0004 §10.4、§12.5）：
- `Debug.Print` 和 `Debug.Assert`：
  - 格式里的插值位置写明类型（0006 §8.10）；
  - Assert 在降低时变成 `if` 加 print；
  - SPIR-V 用 `NonSemantic.DebugPrintf`；
  - RHI 打开验证层的 `printf_enable`，把输出以 `shader: ...` 记到日志。
  - 在 RTX 5060 上实测，descriptor heap 下也能用。
- 有符号整数纹理：图像类型是有符号的，读写时用 bitcast 转换。
- 纹理查询：`Size`、`MipSize`、`MipCount`、`LayerCount`、`SampleCount`，对应 `OpImageQuery*`，需要 `ImageQuery` capability。
- `SampleCompare`、`Gather`、`GatherCompare`，以及常量 `offset:`（−8 到 7，SPIR-V 用 `ConstOffset`）。这一版的 gather 只用于浮点纹理。
- 测试：
  - GPU 测试覆盖打印和断言、有符号纹理、gather、偏移、比较采样和尺寸查询；
  - `bad_requests` 里加了错误用例；
  - verifier 的错误清单里加了新规则。

第 10g 步加了 visbuffer 渲染器（0009 §4.3、§11）要的东西（2026-10-18，决定记在 0004 §3.1、§12.2、§12.3、§15.3）：
- 导数：
  - 核心库有 `Ddx`、`Ddy`、`Fwidth` 和它们的 `Coarse`、`Fine` 版本，不带后缀的是 coarse；`Fwidth*` 用 Marlin 写成 `Abs(Ddx) + Abs(Ddy)`。
  - 降低成 `deriv.ddx`/`deriv.ddy`。第 10g 步时，非 pixel 入口走到的代码里求导报 E1309；第 11 步之后改由自动微分 pass 求出。
  - SPIR-V 用 `OpDPdxCoarse` 这一类，需要 `DerivativeControl`。
- 64 位整数：
  - `vulkan-1.4` target 加了可选的 `Int64`、`Int64Atomics`、`Int64TextureAtomics`（target 版本升到 2）。和 `RayQuery` 一样，设备没有就跳过，RHI 在 `Features` 里报告（0016 §3）。
  - capability 推断：64 位整数上的原子操作，在 buffer 和 groupshared 上要 `Int64Atomics`，在存储纹理上要 `Int64TextureAtomics`。泛型代码实例化后才知道的类型，由 shaderc 在降低后的 IR 上补查（0004 §12.3）。
  - SPIR-V：64 位原子操作声明 `Int64Atomics`。64 位存储纹理的图像类型写明格式 `R64ui`/`R64i`（不能无格式读写），需要 `Int64ImageEXT` 和 `SPV_EXT_shader_image_int64`。
- 纹理原子操作：
  - 核心库：存储纹理上的 `Atomic*` 方法，对应 IR 的 `tex.atomic.<op>`（0006 §8.6）。
  - SPIR-V：用 `OpUntypedImageTexelPointerEXT` 取纹素指针，指向纹理所在的 heap 槽。图像类型操作数写明格式（`R32ui`、`R32i`、`R64ui`、`R64i`），因为 Vulkan 要求原子操作的图像有格式；之后是普通的 `OpAtomic*`。有符号纹理的值在两边用 bitcast 转换。
  - 我们的验证器检查这几个新指令和它们的 capability。另外，只有图像无格式时，`OpImageRead`/`OpImageWrite` 才要求 `StorageImage*WithoutFormat`。
- 测试：
  - 编译器：`Game/Visibility.mshader` 走完整条流水线，并过 spirv-val；`bad_requests` 里加了两个用例；IR fixture 和 verifier 清单；验证器的破坏用例；capability 推断。
  - GPU（RTX 5060）：quad 导数的值；64 位算术和每一种 buffer 原子操作；`R64Uint` visbuffer 上的 `AtomicMax` 和带进位的 `AtomicAdd`；32 位纹理原子操作。

第 11 步的自动微分（前向模式，2026-10-18；决定记在 0004 §15）：
- 核心库：`Differentiation.mshader` 里的 `DiffPair<T>` 和同名的构造函数、`ForwardDiff`、`Detach`、`Derivatives.Seed`；`Attributes.mshader` 里的 `[Differentiable]`、`[NoDiff]`；数学函数标了 `[Differentiable]`。`Ddx`、`Ddy`、`Fwidth`（带 `Coarse`、`Fine`）和 derivatives 那一步的声明一致。
- 前端：
  - `ForwardDiff(f)(args)` 的类型规则（E1137）；
  - `[Differentiable]` 函数里有导数流过的调用（E1138）；
  - `[NoDiff]` 的位置（E1139）；
  - 共享前端的 `Language::type_and_function_names`：shader 语言只列了 `DiffPair`，让泛型类型和同名函数并存。
- 降低：`ForwardDiff` 的调用点变成 `diff.channel`、对每个参与微分的实参 `diff.seed`、照常调用、对结果 `diff.tangent`；`[NoDiff]` 的实参和 `Detach` 变成 `diff.detach`；`Derivatives.Seed` 是 `deriv.seed`。隐式 LOD 的采样和 `deriv.*` 在任何 stage 都能降低，verifier 也不再把它们限制在 pixel 入口。
- `opt` 的 `Autodiff` pass（0.3，放在 mem2reg 之后）：
  - 每个通道先算活跃（从 seed 出发、经过可微操作）和有用（从查询往回、经过要用的切向量），两者都满足的值才生成切向量；
  - 块参数加切向量参数，剩下的 `function` 变量配一个影子变量；
  - `ForwardDiff` 的通道先处理，内层先于外层，这样外层和屏幕空间的通道会对内层生成的代码再求导（二阶导数）；
  - 没有导数又被需要的操作给出 E1402 警告。`PassManager` 的 `Problem` 加了 `warning`，facade 把它们放进输出的诊断。
- 测试：
  - `opt` 用解释器把每个可微操作和带循环、分支、内存的程序跟中心差分比较；
  - 屏幕空间导数和嵌套通道有 golden；
  - 核心库的测试从 Marlin 源码走完整条流水线；
  - facade 有错误和警告的 golden，compute 入口里的 seed、采样和 `ForwardDiff` 能写出 SPIR-V。
- 留到后面的：用户 struct 的 `IDifferentiable` 和合成、自定义导数（0004 §15.4）、反向模式、`ForwardDiff(f)` 存进变量、`ref`/`out` 参数。

第 5c 步留到后面的：`[StructLayout]`/`[FieldOffset]`（0004 §16）；mesh 的 `MeshOutput`、`[Payload]` 和 `Mesh.Dispatch` 的检查（§11.6）；长度由 `const` 字段给出的数组，布局要等第 7 步的编译期求值；capability 和具体 target 的对照、`const if` 的裁剪，等第 9 步的 target 描述。

第 7c 步留到后面的：vertex、pixel、mesh 和 amplification 入口以及 stage I/O（第 10 步；location 按声明顺序分配，0004 §11.2）；`[Export]` 和局部 `const` 的编译期求值（第 9 步，和反射一起）；`const` 函数不能访问资源、groupshared 和系统值的检查（verifier）；auto-property。
