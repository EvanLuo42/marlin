# 0023 — 错误与日志

状态：草案 · 2026-10-20

前置：
- [0001](0001-repo-layout.md) §11 的 **Error convention**（`marlin-error` derive、`{kind, raw}`、记录一次、`fatal!`）：本文保留它，并补全为完整的规范
- [0022](0022-architecture-review.md) §4：现状证据（约 70 条日志、零个 sink、插件边界是 `String`）
- [0020](0020-editor-ux.md) §10.5 Output Log（级别、类别、可点击的资产/entity/源码位置）、§9 状态栏的错误/警告计数
- [0003](0003-asset-pipeline.md) §4.6 配置与启动、§13 用户目录（`Saved`）

本文定 Marlin 的**失败模型和可观测性**：失败分几类、各用什么表示；错误类型怎么写；日志的级别、内容、频率；哪些地方**必须**有日志；每种程序装哪些 sink；运行时怎么调；测试和 `xtask check` 怎么强制。决定按 3A 引擎（UE、Frostbite、Decima）的通行做法加上 Rust 社区惯例来定。

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 失败分类 | **bug**（panic/assert）、**可恢复错误**（`Error` 值）、**内容问题**（`Issue`）、**致命环境失败**（`fatal!`）四类，各有唯一表示 | 1 |
| 错误类型 | 每个 crate 的公开 API 返回该 crate 的类型化错误，**一律 `#[derive(marlin_error::Error)]`**；禁止手写 `impl Error`、禁止 `Result<_, String>`、禁止 `format!("{e:?}")` 压扁错误 | 2.1 |
| 插件边界 | `Cooker`、资产 loader 等扩展点用类型化错误（`CookError`、`LoadError`），内容问题走 `Issue`，不再塞进字符串 | 2.3 |
| 内容问题 | **`marlin_diag::Issue`**：严重度、代码、主体（资产/entity/源码位置/cvar）、字段路径、消息；cook、config、shader 编译、关卡加载共用；editor 的 Output Log 和状态栏计数、cook 报告都消费它 | 2.4 |
| 记录时机 | 沿用“**在处理处记录一次**”；补充：失败**变成状态**的地方（异步加载、后台任务、Drop）就是处理处，必须在那里记录 | 2.6 |
| unwrap | 只用于本地已建立的不变量；**来自磁盘、网络、用户、GPU 的数据绝不 unwrap/panic** | 2.5 |
| 级别 | `error` = 用户可见的结果错了或缺了；`warn` = 降级或回退后继续；`info` = 生命周期里程碑，**绝不每帧**；`debug` = 每次操作；`trace` = 每帧/每项 | 3.1 |
| 类别 | 默认 target = `module_path!()`，按 crate 前缀过滤；跨 crate 的通道（如 `gpu.validation`）用 `target:` 覆盖 | 3.2 |
| 消息格式 | `<主体>: <发生了什么>[; <后果或改做什么>]`；小写开头、无句号；错误用 `chain` 而不是 `{:?}` | 3.3 |
| 结构化 | `Record` 增加可选 **`Subject`**（资产 UUID、entity、源码位置、cvar）和**帧号**；Output Log 据此做跳转 | 3.4 |
| 必记点 | §3.5 的清单是**强制**的：启动环境、适配器、挂载、关卡、资产失败、PSO、回退、热重载、cook 汇总、关闭等 | 3.5 |
| 频率 | 每帧可能重复的日志必须用 `warn_once!` / `log_every!` / 聚合；`info` 及以上稳态下 ≤ 约 1 行/秒 | 3.6 |
| sink | 唯一的安装入口 `marlin_rt::log::install(LogSetup)`，由 `framework::launch()`（0022 §7.3）和工具的 CLI 初始化调用；游戏、editor、工具、测试各有固定组合 | 3.7 |
| 日志文件 | 开发期 `<工程>/.marlin/saved/logs/`，shipping 在系统用户目录；`<程序>.log` 是最新一次，保留最近 10 份带时间戳的备份；异步写 | 3.7 |
| 运行时控制 | cvar `log.level`、`log.filter`（`marlin_asset=debug,marlin_rhi::vulkan=trace`），命令行 `-log=`，控制台可改 | 3.8 |
| CLI 输出 | 工具的**结果**（汇总表、JSON）写 stdout；**诊断**一律走日志（console sink → stderr）。`eprintln!` 只允许在日志系统装好之前 | 3.9 |
| 测试 | 测试装 capture sink；**出现未声明的 `error`/`warn` 日志就让测试失败**（`expect_log!` 声明预期） | 5 |
| 强制 | `xtask check` 检查 §6 的规则；新代码不满足就不能合入 | 6 |

## 1. 失败的四种类型

| 类型 | 含义 | 表示 | 谁处理 | 例子 |
|---|---|---|---|---|
| **Bug** | 程序自己的不变量被破坏；继续运行没有意义 | `marlin_assert!` / `panic!` / `unreachable!` | panic handler：记录、回溯、crash dump、终止 | 索引越界、状态机进入不可能状态、handle 代际不匹配 |
| **可恢复错误** | 环境或输入导致操作失败，调用方可以选择重试、回退、放弃 | `Result<T, CrateError>`，`#[derive(marlin_error::Error)]` | 决定怎么办的那一层，记录一次 | 文件不存在、socket 断开、cooked 数据损坏、shader 入口找不到 |
| **内容问题** | 用户编写的数据有问题；可能一次产生多条，带位置和严重度；常常仍能产出（降级）结果 | `Issue`（§2.4），通过 `IssueSink` 收集 | editor Output Log、cook 报告、启动时的 config 检查 | `.masset` 第 12 行字段类型不对、mesh 有退化三角形、cvar 值越界被夹紧、shader 编译错误 |
| **致命环境失败** | 环境让程序无法继续，但这不是 bug | `fatal!` / `OrFatal::or_fatal` | 记录 + crash 报告 + 退出；不要再用 panic 消息重复记录 | GPU device lost、显存/内存耗尽、CPU 缺少构建要求的指令集、安装文件损坏 |

判断顺序：**是我们的错吗？**是 → bug。不是 → **调用方能做点什么吗？**能 → 错误值；不能 → fatal。**是用户数据的问题？** → Issue（可以同时让操作以错误结束，见 §2.3）。

## 2. 错误

### 2.1 错误类型的规则

1. **每个 crate 一个（或少数几个）公开错误类型**，`#[derive(Debug, marlin_error::Error)]`。不手写 `impl Display`/`impl Error`（0022 §4.2 列出的 16 个运行时类型和 8 个编译器类型都要改）。
2. **OS/API 错误**沿用 code error：`Copy` 的 `{ kind, raw }`，kind 是 `#[derive(ErrorKind)]` 枚举。
3. **包装错误只描述自己这一层**，原因放在 `source`（`#[source]`/`#[from]`/字段名 `source`）；消息里**不**插入 source，`chain` 会打印 `外层: 内层: 根`。
4. **消息要带上识别信息**：路径、资产名、入口名、cvar 名……让人只看这一行就知道是哪个对象。格式同 §3.3：小写开头、无句号。
5. **调用方需要分支的，就给出可匹配的结构**：枚举变体，或 `fn kind(&self) -> XxxKind`。调用方永远不应该去匹配字符串。
6. **禁止**：
   - `Result<_, String>`、`Result<_, &'static str>`、`Vec<String>` 形式的警告；
   - `map_err(|e| format!("{e:?}"))`、`e.to_string()` 之类把类型化错误压扁成字符串（目前 46 处）；
   - 用 `{:?}` 打印错误（`Debug` 不是给人看的；用 `chain(&e)`）；
   - 在**库**里用 `Box<dyn Error>` 作为公开错误类型。唯一的例外是 §2.3 的插件边界里的 `Other` 变体，以及 binary 的 `main`（§2.7）。
7. **错误是值**：创建和 `?` 传播的地方不记录日志（沿用 `marlin_diag::report` 的约定）。

### 2.2 在哪一层定义什么

| 层 | 错误 |
|---|---|
| L0 pal / io / mem | code error（`pal::Error { kind, raw }`、`io::Error`） |
| L1 rhi / net / app | code error + 少量包装（`rhi::Error` 带 `DeviceLost`、`OutOfMemory` 等 kind） |
| L2 serial / reflect / config / compress | 结构化枚举（`serial::Error::Truncated { at, need }`……） |
| L3 asset / world / render / shader | 包装下层的枚举；**带上资产 / 入口 / 关卡的身份** |
| tools | `CookError` 等插件边界类型（§2.3） |
| 编译器 | 阶段边界返回 `Diagnostic`（0022 §9.2）；进入引擎时转换成 `Issue` |

### 2.3 插件边界：cooker 和 loader

两个最重要的扩展点现在返回 `String`（`tools/cook_core/src/cooker.rs:279`、`runtime/engine/asset/src/server.rs:143,160`），改为：

```rust
// tools/cook_core
#[derive(Debug, marlin_error::Error)]
pub enum CookError {
    /// 读源文件或依赖失败。`ContentPath` 是 0022 §8 提出的“包 + 相对路径”类型。
    #[error("reading `{path}`")]
    Source { path: ContentPath, source: marlin_io::Error },
    /// 内容有 Error 级 Issue；细节已经通过 `ctx.issue(..)` 报告，这里不重复。
    #[error("the content has errors")]
    Invalid,
    /// 依赖的对象 cook 失败。
    #[error("dependency {0} failed to cook")]
    Dependency(ObjectId),
    /// cooker 内部用到的第三方库或子系统失败。
    #[error("{context}")]
    Other { context: &'static str, source: Box<dyn Error + Send + Sync> },
}

pub trait Cooker {
    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, CookError>;
}

impl CookContext<'_> {
    /// 报告一个内容问题；主体默认是当前对象。Error 级的 Issue 让这次 cook 失败。
    pub fn issue(&mut self, issue: Issue);
}
```

- 内容问题（字段不对、几何退化、纹理尺寸不合规）一律 `ctx.issue(..)`：可以有多条、带字段路径或源码位置；**有 Error 级 Issue 时，cook 结果不进 DDC**。
- 警告不再是 `Vec<String>`，而是 Warning 级的 `Issue`。
- 编译器的 `Diagnostic` 在 `cook_shader` 里**按值**转换成 `Issue`（保留 span），不再渲染成字符串。
- loader 同理：`LoadFn -> Result<Loaded, LoadError>`；`LoadError` 有 `Corrupt { source: serial::Error }`、`Version { found, expected }`、`Dependency(..)`、`Other { .. }`；`AssetError::Corrupt(String)` 改为携带 source。

### 2.4 `Issue`：内容问题的统一表示

放在 `marlin-diag`（L0，已有 `alloc`），所以 config（L2）、asset、cook、编译器转换层、editor 都能用：

```rust
pub struct Issue {
    pub severity: Severity,          // Error | Warning | Note
    pub code: Option<&'static str>,  // 稳定代码，如 "cook.mesh.degenerate"、编译器的 Code
    pub subject: Subject,            // 见 §3.4：资产、entity、源码位置、cvar……
    pub field: Option<String>,       // 反射字段路径，如 "materials[2].base_color"
    pub message: String,             // 小写开头、无句号
    pub notes: Vec<String>,          // 建议和补充
}

pub trait IssueSink { fn issue(&mut self, issue: Issue); }
```

- **Issue 也会进入日志**：`IssueSink` 的默认实现把每条 Issue 以对应级别（Error → `error!`，Warning → `warn!`，Note → `info!`）、带 `Subject` 记录下来。所以 cook 时在控制台、editor 里在 Output Log 都能看到，Output Log 可以跳转到资产、字段或源码行。
- **cook 报告**：cooker 汇总每个单元的 Issue，结束时打印数量汇总（§3.5），并且可以输出 JSON 给 CI。
- **config 文件**：未知 cvar、类型不对、值越界被夹紧，都是带 `Subject::Source { path, line }` 的 Issue（现在是 `warn!("config: {warning}")`）。
- **状态栏计数**（0020 §9）：按 Error/Warning 计数，数据来自日志记录的级别，不需要单独的通道。

### 2.5 panic、assert 和 unwrap

- `unwrap()` 只能用于**本地已建立的不变量**（刚检查过长度、刚插入过的 key）；不明显时用 `marlin_assert!` 先写明条件，或在旁边一行注释说明为什么不会失败。
- **来自外部的数据绝不 unwrap 或 panic**：磁盘文件（包括 cooked 数据——被截断、版本不对、被别的工具写坏都会发生）、网络、用户输入、cvar 值、GPU 回读、OS 回调。它们产生错误值或 Issue。示例：`asset/src/le.rs:17` 的 `try_into().unwrap()` 依赖调用方事先检查长度，应该换成 §8 的带边界检查的 `ByteReader`。
- `panic!` 的消息要说明哪个不变量被破坏，带上相关的值。
- `expect` 打印的是 `Debug`，不要对 `Result` 用 `expect`；不可恢复时用 `or_fatal("context")`。
- 非 `shipping` 构建里 `marlin_assert!` 都生效；**昂贵的检查**用新增的 `marlin_debug_assert!`（只在 `debug` 构建生效，失败时与 `marlin_assert!` 一样记录并中断），不要因为怕慢而不写检查。

### 2.6 失败变成状态的地方必须记录

“在处理处记录一次”的推论：**如果调用栈上没有人会收到这个错误，那么把它变成状态的地方就是处理处，必须在那里记录。**

| 场景 | 记录点 | 级别 |
|---|---|---|
| 异步资产加载失败（`AssetServer::fail_unit`） | 失败的那一刻，每个单元一次，带资产 `Subject` 和 `chain` | `error`（热重载失败保留旧版本：`warn`） |
| job / 后台任务（cook 守护进程、shader 编译、存档）失败 | 任务的完成回调 | `error`，或者变成 Issue |
| `Drop` 里的失败（`FrameContext::drop` 的 fence 等待、swapchain/frames 的 `wait_idle`） | 就在 `Drop` 里 | `warn`；device lost 时 `error` |
| 安装可选设施失败（`rt/src/crash.rs` 的 `let _ = crash_dump::install(..)`、sink 注册表满） | 安装处 | `warn`：“crash dumps are off: …” |
| 回退到降级路径（没有光追、没有 HDR、没有某扩展） | 做出决定的地方，一次 | `warn`（预期中的平台差异用 `info`） |

**`let _ = <Result>` 一律禁止**，除非旁边注释说明为什么可以忽略（例如写进固定缓冲的 `write!` 只会因截断而失败）。0022 §4.2 列出的 Drop 中的忽略要改成记录。

### 2.7 binary 的顶层

- `main` 和工具的顶层命令处理可以用 `Result<(), Box<dyn Error + Send + Sync>>`（或一个小的 `Report` 类型）来汇总，在退出前**用 `chain` 记录一次 `error!`**，然后返回非零退出码。
- 退出码：0 成功；1 操作失败（cook 有错误、命令失败）；2 用法错误；其他保留。致命失败走 panic/crash 路径。
- cooker 的 `marlin_error_chain`（`tools/cooker/src/main.rs:166`）删掉，用 `marlin_diag::chain`。

## 3. 日志

**原则：日志是产品的黑匣子。** 拿到一份用户、QA 或 CI 的日志文件，不连调试器、不复现，就应该能知道：在什么机器和配置上运行、加载了什么、哪里失败、失败后做了什么。今天的日志做不到这一点。

### 3.1 级别

| 级别 | 含义 | 频率预算 | shipping | 例子 |
|---|---|---|---|---|
| `error` | 用户可见的结果**错了或缺了**，且没有恢复。每条都应该能操作：说清楚是什么、在哪里 | 少；同一原因只出现一次（§3.6） | 保留 | 资产加载失败、shader 入口缺失、存档写入失败、device lost |
| `warn` | 发生了问题，但**降级或回退后继续**；或者很可能是配置/内容错误 | 少；重复的要限流 | 保留 | 没有光追扩展，关闭 RT 阴影；cvar 值被夹紧；热重载失败，保留旧版本；帧耗时超预算 |
| `info` | **生命周期里程碑**：启动环境、挂载、关卡开始/完成、PIE 开始/停止、连接、关闭、cook 汇总 | 稳态（游戏中每帧）**零条**；平均 ≤ 约 1 行/秒 | 剔除 | “level `Main` loaded in 1.84 s (12 sublevels, 31 204 entities)” |
| `debug` | **每次操作**的细节，给开发者定位问题：每个资产单元加载、每个 PSO 编译、graph plan 重建、swapchain 重建参数 | 可以多，但不能每帧 | 剔除 | “pipeline `post::tonemap` compiled in 3.1 ms” |
| `trace` | **每帧或每项**：调度决策、每个 pass、每个请求 | 不限，默认关闭 | 剔除 | “io request 912 completed (64 KiB)” |

静态剔除沿用 `level.rs` 的表：`debug`/`development` 构建保留到 `trace`，`profile` 到 `info`，`shipping` 到 `warn`。运行时默认过滤器 `info`。

### 3.2 类别（target）

- 默认 target 是 `module_path!()`，例如 `marlin_asset::server`。过滤按 `::` 前缀（`marlin_asset` 覆盖整个 crate），已经实现。crate 名就是稳定的类别名，**不再另建一套 `LogXxx` 类别**。
- 宏增加 `target:` 参数，只用于**不对应某个模块**的跨 crate 通道：

  | target | 内容 |
  |---|---|
  | `gpu.validation` | Vulkan validation / D3D12 debug layer 的消息 |
  | `gpu.driver` | 驱动回调、device fault 信息 |
  | `perf` | 帧超预算、卡顿、预算溢出（§3.6 限流） |
  | `script` | VM 脚本的日志（0018），主体是脚本源码位置 |

- 游戏代码（VM 脚本）的日志走 `script`，Output Log 里可以单独筛选。

### 3.3 消息格式

- 形式：**`<主体>: <发生了什么>[; <后果或改做什么>]`**。现有好的例子：`input context \`{name}\`: {e}; its commands are off`。
- 小写开头（专有名词除外），无句号，英文（日志是给开发者的，不走 i18n）。
- 主体放在最前，并且**同时**放进结构化 `Subject`（§3.4）：资产用内容路径，必要时加短 UUID；entity 用名字 + GUID；pass 和 pipeline 用名字。
- 错误一律 `{}` + `marlin_diag::chain(&e)`，**禁止 `{e:?}`**。
- 数字带单位：`ms`、`MiB`、`KiB`、像素尺寸 `1920x1080`；耗时统一用 `ms` 或 `s`，大小用二进制单位。
- 一行就是一条完整的信息，不拆成多条；多行内容（回溯、validation 对象列表）只在 `error`/`debug` 中用缩进续行。
- 不记录密钥、令牌、账户凭据；shipping 的用户目录路径可以记录，但不要记录用户名之外的个人信息。

### 3.4 结构化：Subject 和帧号

`Record` 目前只有 level、target、文件行号、线程、时间戳和 `fmt::Arguments`。增加两项（都不分配内存）：

```rust
#[derive(Clone, Copy)]
pub enum Subject<'a> {
    None,
    Asset { id: u128, path: &'a str },     // 资产 UUID + 内容路径
    Entity { guid: u128, world: u32 },
    Source { path: &'a str, line: u32, col: u32 },
    Cvar(&'a str),
}

// 宏的用法
marlin_diag::error!(subject: Subject::Asset { id, path }, "{path}: failed to load: {}", chain(&e));
```

- **帧号**：`Record.frame: u64`，由 host 每帧写一个全局原子量；没有帧循环的程序（工具）是 0。日志行显示为 `[f 18342]`，可以和 profiler 抓帧、render graph dump 对上。
- Output Log 用 `Subject` 做跳转（0020 §10.5），用 `target` 做类别筛选；文件 sink 把 Subject 以 `{asset=… }` 后缀写出，便于 grep。

### 3.5 必须记录的日志点

这一节是**强制**清单：所列事件没有日志就算缺陷。级别写在括号里。

**启动（每个程序，`info`）**
- 程序名、版本、构建配置（`marlin_build`）、git commit、编译时间。
- OS 名称和版本、CPU 型号 / 核数 / 构建要求和检测到的指令集、物理内存总量、大页是否可用。
- 命令行、工作目录、**日志文件路径**、crash dump 目录（或“crash dumps are off: 原因”，`warn`）。
- 工程（`.mproject`）、每个挂载点（名称、根路径、只读与否）。
- 应用了哪些 config 层（文件路径）；与默认值不同的 cvar 列表（`debug`，每个一行）。
- job system 工作线程数、IO 队列深度；模块列表和顺序（`debug`）。

**RHI / GPU**
- instance 和 API 版本、启用的 layer（`info`）。
- **所有候选适配器**及被拒原因（`debug`，已有）；**选中的适配器**、驱动版本、显存大小、启用的特性和扩展（`info`）。
- 显存堆和预算（`info`）；预算超出（`warn`，限流）；分配失败（`error`）。
- swapchain 创建/重建：尺寸、格式、present mode、HDR（`debug`）；HDR/VRR 不可用而回退（`warn`，一次）。
- pipeline 创建失败（`error`，带入口名和 `chain`）；descriptor 槽用尽（`error`，已有）。
- validation 消息（`gpu.validation`，已有）；device lost / fault（`error`，已有）；关闭时仍存活的 GPU 对象（`warn`，带数量和类型）。

**渲染**
- 生效的画质档位和渲染设置（`info`，切换时）；分辨率或缩放变化（`debug`）。
- 能力不足导致的 feature 回退，例如关闭光追或改用其他降噪路径（`warn`，一次）。
- PSO 编译：每个入口一行，带耗时（`debug`）；**帧内同步编译超过 2 ms**（`perf` 的 `warn`，限流），这是卡顿的头号来源。
- render graph plan 重建（`debug`，带原因）；graph 校验失败（`error`）。
- shader 库加载（`debug`）；入口缺失或反射不匹配（`error`）；shader 热重载成功（`info`）/ 失败（`warn`，保留旧版本）。

**资产与 IO**
- 每个资产单元加载失败：**一次**，`error`，带 `Subject::Asset` 和 `chain`（目前完全没有）。
- 类型不匹配、缺少依赖、版本不对（`error`）。
- 热重载：成功（`info`），失败保留旧版本（`warn`）。
- 每个单元加载完成，带大小和耗时（`debug`）；超过阈值的慢加载（`perf` 的 `debug`）。
- 流式预算溢出、请求被丢弃（`warn`，限流）；IO 队列满（`warn`，限流）。
- cookd 连接、断开、重连（`info`/`warn`）。

**World 与 framework**
- 关卡加载开始（`info`），完成时带耗时、子关卡数和 entity 数（`info`）；加载失败（`error`）。
- 子关卡流入/流出（`debug`）；World 创建/销毁及其种类（`debug`）。
- 模块 init / shutdown（`debug`）；模块 init 失败（`error` 或 fatal）。
- PIE 开始/停止（`info`）；保存成功（`info`）/失败（`error`）。
- 固定步长追帧被截断（“spiral of death” 保护触发，`perf` 的 `warn`，限流）。
- 帧耗时超过预算的卡顿（`perf` 的 `warn`，限流，带最慢的几个阶段）。
- 组件或实体数据不一致（`warn`，已有部分）。

**配置与输入**
- config 文件问题以 `Issue` 报告（§2.4）；运行时通过控制台修改 cvar（`info`：旧值 → 新值）；修改只读 cvar（`warn`）。
- 输入设备连接/断开（`info`）；mapping context 或 action 无效（`warn`，已有）。

**网络**
- 连接建立、断开及原因（`info`）；连接失败、超时（`warn`）；协议错误（`error`）。

**Cook 与工具**
- 开始：目标平台、profile、DDC 层（本地/远程地址）、并行度（`info`）。
- 每个单元：命中/未命中和耗时（`debug`）；失败（`error`，附带它的 Issue）。
- 远程 DDC 失败、回退到本地（`warn`，一次加计数）；确定性检查失败（`error`）。
- **结束汇总**（`info`）：单元总数、cook 数、缓存命中数、失败数、Error/Warning Issue 数、总耗时、最慢的 5 个单元。

**Editor**
- 打开工程（`info`）；资产保存（`info`）/失败（`error`）；外部工具启动（`debug`）；撤销事务（`trace`）。

**关闭（`info`）**
- 正常退出及退出码；内存标签泄漏报告（`warn`，依赖 `MemTag` 真正被使用，0022 §8）；日志 sink 丢弃的条数（`warn`，`AsyncSink::dropped()`）。

### 3.6 频率控制

- **每帧可能重复的日志不允许裸写。** 新增：
  - `warn_once!` / `error_once!`：每个调用点一次（调用点的静态 `AtomicBool`）。
  - `log_every!(Duration, level, ...)`：每个调用点按时间间隔限流，再次输出时附上“(N suppressed)”。
- **按对象去重**：同一资产的失败只记录一次（`AssetServer` 用每个单元的标志实现），重试成功后再允许记录。
- **批量操作先聚合**：例如 1000 个 mesh 都缺切线，记成“1000 meshes have no tangents; generated (first: `…`)”，加上 `debug` 级的逐项明细。
- `AsyncSink` 队列溢出时丢弃并计数，不阻塞调用方；丢弃数在下一条日志之前以 `warn` 报告一次。

### 3.7 Sink 与安装

**唯一入口** `marlin_rt::log::install(&LogSetup)`，在 `rt::startup` 之后、模块注册之前调用：游戏和 editor 通过 `framework::launch()`（0022 §7.3），工具通过 `marlin_rt::hosted::init_cli()`。以后不允许各程序自己 `register_sink`。

| 程序 | sink |
|---|---|
| 游戏（开发构建） | `DebugOutput`；`AsyncSink(FileSink)`；`RingSink`（crash 报告附带最近日志）；有控制台时 `ConsoleSink`；in-game 控制台 sink |
| 游戏（shipping） | `AsyncSink(FileSink)`（用户目录，`warn` 及以上）；`RingSink` |
| editor | `DebugOutput`；`AsyncSink(FileSink)`；`RingSink`；**Output Log sink**（把记录加 `Subject` 推进 editor 的虚拟化列表） |
| cooker、cookd、store、bench 等工具 | `ConsoleSink`（stderr，TTY 时带颜色）；`AsyncSink(FileSink)`；`RingSink` |
| 测试 | capture sink（§5）；失败时把捕获的记录打印到测试输出 |

- **文件位置**：开发期 `<工程>/.marlin/saved/logs/`；没有工程的工具用 `<可执行文件目录>/logs/`；shipping 用系统用户目录（与 0003 §13 的 `Saved` 待定项一起定）。`MARLIN_LOG_DIR` 环境变量可以覆盖。
- **轮转**：启动时把上次的 `<程序>.log` 改名为 `<程序>-<YYYYMMDD-HHMMSS>.log`，保留最近 10 份。
- **文件行格式**：`<相对启动的时间> [f <帧号>] <级别> <线程名> <target>: <消息> {subject}`。文件里永远包含文件名和行号（`debug` 及以上的构建），控制台只在 `debug`/`trace` 级显示。
- crash 时 panic handler 先 `flush()` 所有 sink，再写 dump；dump 旁边附上 `RingSink` 的内容。

### 3.8 运行时控制

- cvar `log.level`（全局级别）、`log.filter`（逗号分隔的 `前缀=级别`，例如 `marlin_asset=debug,marlin_rhi::vulkan=trace,gpu.validation=warn`）。启动时来自 config 和命令行 `-log=…`，运行时可以在控制台修改；修改时记录一条 `info`。
- 工具的 `-v`/`-vv`/`-q` 映射为 `debug`/`trace`/`warn`。
- editor 的 Output Log 设置级别和类别时，只影响显示，不改变全局过滤；要真正改变采集范围，改 `log.filter`。

### 3.9 CLI 输出不是日志

- 工具的**结果**（`cooker` 的汇总表、`--json` 输出、`store` 的查询结果）写 **stdout**，可以被管道处理。
- **诊断**（进度、警告、错误）一律走日志；console sink 写 **stderr**。
- `println!`/`eprintln!` 只允许出现在 binary crate 里：前者只用于结果；后者只用于日志装好之前的失败（例如参数解析失败）。库 crate 里两者都禁止。

## 4. 错误与日志的配合

一条失败从发生到被看见，一个完整的例子：

1. `marlin_io` 读文件失败，返回 `io::Error { kind: NotFound, raw: 2 }`，不记录。
2. `marlin_asset` 包装为 `LoadError::Read { path, source }`，不记录。
3. `AssetServer::fail_unit` 把单元标为 Failed：**这里是处理处**（调用栈上没有人等着它）→ `error!(subject: Asset{..}, "{path}: failed to load: {}", chain(&e))`，同一单元只记一次。
4. 引用它的组件看到 handle 是 Failed，使用默认资产，**不再记录**（已经记过了）。
5. Output Log 显示红色一行，点击跳到资产；状态栏错误数 +1；日志文件里有同一行，带帧号。

反例（现在的状态）：第 3 步没有日志，第 4 步静默地使用默认资产，用户看到一块空白而没有任何线索。

## 5. 测试

- 测试 harness（`marlin-test-support`、`gpu-test-support`）为每个测试安装 **capture sink**。
- **测试结束时如果捕获到未声明的 `error` 或 `warn` 记录，测试失败**；把 GPU 测试“validation 必须报告 0 个错误”的规则推广到所有日志。
- 预期中的失败用 `expect_log!(Level::Warn, "marlin_asset", "failed to load")` 声明；声明了却没出现，同样算失败。这样“必须记录”的日志点（§3.5）本身也能被测试：为失败路径写测试时，顺带断言它记录了日志。
- 错误路径的测试断言**错误的变体或 kind**，不断言消息字符串（消息可以改）。

## 6. 强制检查（`xtask check`）

新增一个 `check-diagnostics` 步骤，与 `check-layers` 并列，CI 必跑：

1. 非 binary crate 中出现 `println!`、`eprintln!`、`print!`、`dbg!` → 失败。
2. 非测试代码中，公开函数签名出现 `Result<_, String>` 或 `Result<_, &str>` → 失败。
3. 手写 `impl … Error for`（`core::error::Error` 或 `std::error::Error`）→ 失败（`marlin-error` 自身除外）。
4. 日志宏的参数里出现 `{e:?}` / `{err:?}` / `{error:?}` → 失败。
5. `let _ = ` 后面接返回 `Result` 的调用、且上一行没有注释 → 警告（基于语法的启发式，列出位置）。
6. binary crate 的 `main` 里没有经过 `framework::launch` 或 `hosted::init_cli` → 失败。
7. 第三方日志/错误库（`log`、`tracing`、`anyhow`、`thiserror`、`eyre`）出现在依赖里 → 失败（现在没有，保持）。

## 7. 迁移计划

按依赖顺序，每一步都带上对应的检查规则，免得退化：

1. **基础设施**（diag/rt）：`Subject`、帧号、`target:` 参数、`warn_once!`/`log_every!`、`marlin_debug_assert!`、`Issue`/`IssueSink`、`marlin_rt::log::install` 和 `hosted::init_cli`、文件轮转、capture sink 和 `expect_log!`。
2. **装上 sink**：game、editor、demo_clear 走 `install`；cooker、cookd、store、bench 走 `init_cli`，把 `eprintln!` 改成日志，stdout 只留结果；删掉 `marlin_error_chain`。加上检查 1、6。
3. **补齐必记点**（§3.5）：先做启动环境、资产失败、关卡、RHI 适配器/预算、PSO、回退、cook 汇总、关闭，再做其他。顺手把 `{e:?}` 改成 `chain`（检查 4），Drop 和可选设施安装里的 `let _ =` 改成记录（检查 5）。
4. **类型化错误**：所有手写 `impl Error` 改为 derive（检查 3）；`Cooker`、loader 改为 `CookError`/`LoadError` + `Issue`；`AssetError::Corrupt(String)` 带 source；cook_font、cook_mesh、cook_shader、cook_texture、editor/core 去掉 `Result<_, String>`（检查 2）；config 警告改为 `Issue`；cook_shader 按值转换编译器的 `Diagnostic`。
5. **测试收紧**：打开“未声明的 warn/error 让测试失败”，修复由此暴露的问题。
6. **editor**：Output Log sink 和状态栏计数（0020 §10.5、§9）。

## 8. 对其他文档的修改

- 0001 §11 的 **Error convention** 仍然有效，本文是它的展开；以本文为准。
- 0020 §10.5 “依赖统一的错误/报告设计”，指的就是本文的 `Subject`（§3.4）和 `Issue`（§2.4）。
- 0022 §4 是本文的现状证据；0022 §10 的第 1 步就是本文 §7。
