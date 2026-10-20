# 0024 — 代码文档与注释

状态：草案 · 2026-10-20

前置：
- [0001](0001-repo-layout.md) §9：单个 crate 的约定
- [0023](0023-errors-and-logging.md)：`# Errors`、`# Panics` 描述的就是那里定义的失败
- 参照：Rust 标准库源码（`rust-src` 组件，`library/core`、`alloc`、`std`）、[RFC 1574](https://rust-lang.github.io/rfcs/1574-more-api-documentation-conventions.html)、[Rust API Guidelines · Documentation](https://rust-lang.github.io/api-guidelines/documentation.html)

本文定 Marlin 的 rustdoc 和代码注释怎么写。**标准就是 Rust 标准库**：拿不准时，打开 `rustc --print sysroot` 下的 `lib/rustlib/src/rust/library`，找一个相似的 std 项照着写。

## 0. 决定摘要

| 维度 | 决定 | § |
|---|---|---|
| 标准 | 照 std 写；本文只记录 std 的做法和我们的少量补充 | — |
| 摘要行 | 每个文档的第一段是**一句话**，以句号结尾，能单独出现在模块索引里；函数用第三人称动词开头（`Returns`、`Creates`），类型用名词短语 | 1 |
| 章节 | 固定的几个标题、固定顺序：`# Panics`、`# Errors`、`# Safety`、`# Platform-specific behavior`、`# Examples` | 2 |
| 必须写的 | 公开项都有文档；返回 `Result` 的写 `# Errors`；可能 panic 的写 `# Panics`；`unsafe fn`/`unsafe trait` 写 `# Safety` | 2、6 |
| 示例 | crate 根、公开类型的主要用法、宏、用法看签名不明显的函数必须有 `# Examples`，而且是能编译的 doctest；简单的访问器不强求 | 3 |
| 设计文档引用 | **只在 crate/模块文档里**链接设计文档；条目文档不写 `(0010 §2.8)` 这类引用 | 4 |
| 废话 | 不复述签名和名字，不写 `# Arguments`，不写 “This function…”，不写历史和计划 | 5 |
| 行内注释 | 只解释**为什么**；不写叙述步骤的注释、分节横幅、注释掉的代码；每个 `unsafe` 块前写 `// SAFETY:` | 7 |
| 语言 | 代码里的注释和文档一律英文，美式拼写（与 std 一致） | 1.4 |
| 强制 | workspace lint + `cargo doc -D warnings` + `xtask check` 的几条文本规则 | 8 |

## 1. 文档的结构

一个文档注释由三部分组成，依次是：

1. **摘要行**：一句话，单独成段。
2. **正文**：零到几段，补充摘要没说的行为、约束和与其他项的关系。
3. **章节**：§2 的固定标题。

```rust
/// Divides one slice into two at an index.
///
/// The first will contain all indices from `[0, mid)` (excluding
/// the index `mid` itself) and the second will contain all
/// indices from `[mid, len)` (excluding the index `len` itself).
///
/// # Panics
///
/// Panics if `mid > len`.
///
/// # Examples
///
/// ```
/// let v = ['a', 'b', 'c'];
/// let (left, right) = v.split_at(2);
/// assert_eq!(left, ['a', 'b']);
/// assert_eq!(right, ['c']);
/// ```
```

（`core::slice::split_at`，略有删减）

### 1.1 摘要行

- **一句话，以句号结尾**，单独成段。rustdoc 在模块索引和搜索结果里只显示这一段，所以它必须独立成立。
- 函数和方法用**第三人称现在时动词**开头：

  | 种类 | 写法 | std 例子 |
  |---|---|---|
  | 构造 | `Creates …` / `Constructs …` | `Creates a new, empty Vec<T>.` |
  | 取值 | `Returns …` | `Returns the number of elements in the vector.` |
  | 判断 | ``Returns `true` if …`` | ``Returns `true` if the vector contains no elements.`` |
  | 修改 | `Sets …`、`Appends …`、`Removes …`、`Clears …` | `Appends an element to the back of a collection.` |
  | 转换 | `Converts …` | `Converts a vector of bytes to a String.` |
  | 尝试 | `Attempts to …` | `Attempts to open a file in read-only mode.` |
  | 迭代器 | `Returns an iterator over …` | `Returns an iterator over the slice.` |

- 类型、trait、模块、常量、字段用**名词短语**：`A contiguous growable array type.`、`A mutual exclusion primitive useful for protecting shared data.`、`The number of frames the CPU may record ahead of the GPU.`
- 不以 `This function`、`This struct`、`Helper to`、`Used to` 开头。

### 1.2 正文

- 写摘要没覆盖的内容：边界情况、复杂度、内存行为、线程安全、与相关项的区别（“For a non-panicking alternative see [`split_at_checked`].”）。
- 一段一个意思。用完整的句子，不用冒号和分号串起好几层意思。
- 第一次提到其他项时用 intra-doc link（`[`Vec::new`]`），后面再提到就只用反引号。
- 代码、标识符、字面量（`true`、`None`、`0`）都放进反引号。
- 正文可以为空。摘要已经说完的，就不要再写。

### 1.3 各类条目

| 条目 | 要写什么 |
|---|---|
| crate 根（`lib.rs` 的 `//!`） | 一句话说明 crate 是什么；它在哪一层、依赖谁、谁用它；主要入口类型的列表；一个完整示例；cargo feature 和 cfg 的说明；相关设计文档（§4） |
| 模块（`//!`） | 一句话；模块里的主要类型以及它们怎么配合；需要时给示例 |
| struct / enum | 名词短语；不变量；所有权和线程语义（`Send`/`Sync` 不显然时）；主要用法示例 |
| 字段、变体 | 名词短语或一句话；单位、取值范围、`None` 代表什么。名字已经说清楚的可以只写一个短语 |
| trait | 它抽象的是什么；实现者必须保证什么；调用方可以依赖什么 |
| trait 方法 | 写在 trait 上。impl 上**不重复**写，除非这个实现有额外行为（如更强的保证、不同的复杂度） |
| 函数、方法 | §1.1 的动词摘要 + §2 的章节 |
| 常量、static | 名词短语；单位 |
| 宏 | 和函数一样，而且**必须**有示例 |
| 私有项 | 不强制。约束不明显的私有项写 `///`，格式要求同上 |

### 1.4 语言与格式

- 英文，**美式拼写**（`behavior`、`color`、`initialize`、`neighbor`），与 std 一致。仓库里已经基本是美式，个别英式拼写（`behaviour`、`honour`、`neighbour`）在改到时顺手修正。
- 按 rustfmt 的 100 列换行。
- 单位写在数字后面（`in bytes`、`in milliseconds`）；与 std 一样用 *O*(1) 表示复杂度。
- 不用感叹号，不用第一人称（`we`）。描述代码时用 `the vector`、`this function` 都可以，但不要用来开头摘要行。

## 2. 章节

标题用一级 `#`（rustdoc 会自动降级），**只用下面几个，按这个顺序**：

| 章节 | 什么时候必须写 | 怎么写（std 的句式） |
|---|---|---|
| `# Panics` | 函数会因为**调用方的输入**而 panic（包括 `marlin_assert!`） | `Panics if `mid > len`.` |
| `# Errors` | 返回 `Result` | `Returns an error if `path` does not exist.` 列出调用方应该区分的情况；错误类型的变体用链接 |
| `# Safety` | `unsafe fn`、`unsafe trait`、有安全前提的 `unsafe impl` 的 trait 方法 | `Behavior is undefined if any of the following conditions are violated:` 加列表，每条前提一项 |
| `# Platform-specific behavior` | 不同平台的行为不同 | 说清楚每个平台做什么、哪里不支持 |
| `# Examples` | 见 §3 | 一个或几个 doctest，示例之间用一句话说明它演示什么 |

- 标题一律用复数 `# Examples`，即使只有一个示例（std 的约定）。
- 不写 `# Arguments`、`# Returns`、`# Parameters`：参数的意义写进正文的句子里（RFC 1574 明确反对参数列表）。
- 其他少见的标题（`# Implementation notes`、`# Memory layout`）只在内容确实多到需要单独一节时用，std 里也有，但很少见。

`# Safety` 示例（`core::ptr::read` 的写法）：

```rust
/// Reads the value from `src` without moving it.
///
/// # Safety
///
/// Behavior is undefined if any of the following conditions are violated:
///
/// * `src` must be valid for reads.
/// * `src` must be properly aligned.
/// * `src` must point to a properly initialized value of type `T`.
```

## 3. 示例

- **必须**有 `# Examples` 的：crate 根、每个公开类型的主要用法（通常放在类型上，而不是每个方法上）、每个宏、用法从签名看不出来的公开函数（需要先调用别的函数、需要特定顺序、有返回值必须处理的协议）。
- 简单的访问器、`From`/`Default` 之类的 trait 实现不强求。std 几乎每个项都有示例，这是我们与 std 唯一有意的差别：引擎 API 的数量和装配成本使逐项示例不划算。
- 示例是**能编译的 doctest**：
  - 默认能运行，用 `assert!`/`assert_eq!` 展示结果，而不是 `println!`。
  - 需要 GPU、窗口、网络、文件的用 ` ```no_run `：编译但不运行。
  - ` ```ignore ` 只用于无法在 doctest 环境里编译的情况（如平台私有类型），而且要在上一行写明原因。现有的 37 处 `ignore` 逐步改成 `no_run`。
  - 需要 `?` 时用隐藏行 `# fn main() -> Result<(), Box<dyn std::error::Error>> {` … `# }`；与主题无关的样板代码用 `#` 隐藏。
- 示例里的注释和 std 一样，解释“为什么会这样”（`// The vector contains no items, even though it has capacity for more`），不复述代码。

## 4. 设计文档的引用

- 设计文档（`docs/design/`）讲**为什么这么设计**；rustdoc 讲**这个项做什么、怎么用**。
- **只在 crate 和模块的 `//!` 文档里**引用设计文档，而且每个文档引用一次，放在末尾：`Design: docs/design/0003-asset-pipeline.md, §7–8.`
- 条目文档（`///`）里**不写** `(0010 §2.8)` 这类引用。现在有 280 处，迁移时把该说清楚的行为写进句子里；只是解释动机的，删掉或移到模块文档。
- 例外：常量或算法直接实现某个规范（Vulkan、SPIR-V、Unicode、某篇论文）时，写出规范的名称和章节，这和 std 引用 IEEE 754 是一样的。

## 5. 不写的东西

| 不写 | 例子 | 改成 |
|---|---|---|
| 复述名字或签名 | `/// The meshes.` 放在 `meshes: Meshes` 上 | 写名字没表达的东西（`/// The meshes resident on the GPU.`），否则只留短语 |
| 参数列表 | `/// * `path` - the path` | 正文里的一句话 |
| 开场白 | `/// This function returns…`、`/// Helper that…`、`/// Simply…` | 直接用动词：`/// Returns…` |
| 历史和计划 | `/// Previously…`、`/// Now uses…`、`/// Will support…` | 历史放 git，计划放设计文档的待办 |
| 实现细节 | 在公开文档里描述内部字段、私有算法步骤 | 移到实现里的注释，或者根本不写 |
| 重复 | 同样的说明写在 trait 和每个 impl 上 | 只写在 trait 上 |
| 冗长的括号 | 一句话里嵌两三层括号和冒号 | 拆成几个短句 |

对照一个现有的文档（`WorldRenderer::new`）：

```rust
// 现在
/// A renderer for the world whose scene rows come from `ids` ([`crate::scene_ids`]), made with
/// the render module's [`RenderShaders`] (an engine resource); its post chain writes `output`.

// 照 std 写
/// Creates a world renderer that draws into `output`.
///
/// Scene rows are allocated from `ids`, which must come from [`scene_ids`].
///
/// # Errors
///
/// Returns an error if a pipeline or GPU resource cannot be created.
```

（第二段和 `# Errors` 的具体内容是示意，以代码的实际行为为准。）

## 6. 必须有文档的范围

- 所有 crate 的**公开项**（`pub` 且从 crate 根可达）都要有文档，由 `missing_docs` 检查。
- 返回 `Result` 的公开函数要有 `# Errors`，可能 panic 的要有 `# Panics`，`unsafe` 的要有 `# Safety`，都由 clippy 检查（§8）。
- `apps/*` 和 binary crate 的 `main.rs` 只需要 crate 文档。测试代码不要求文档。

## 7. 行内注释

**默认不写。** 代码应该靠命名、类型和小函数表达自己在做什么。只有读者凭代码本身**无法知道**的事情才写注释，而且写的是**为什么**：

| 值得写 | 例子（仓库里的好注释） |
|---|---|
| 不显然的原因 | `// An append-only handle may not truncate (Windows): drop the torn tail first.` |
| 依赖的外部事实 | `` // `--git-path` honors core.hooksPath and worktrees. `` |
| 不变量及其来源 | `` // Checked in `new`. `` |
| 规避驱动/平台 bug | 写清楚是哪个驱动、哪个版本、链接到问题 |
| 性能取舍 | 说明测过的数字（“halves the decode time on 4 KiB blocks”） |
| 看起来不对但其实对的代码 | `// Start at the earliest-added system, so the report does not depend on where the walk began.` |

不写的：

- **叙述步骤**：`// Parse decoded units; request their imports.`、`// Changed units: load them again.`、`// Lifetimes.`。一个长函数需要靠这种注释分段，说明它应该拆成几个有名字的函数（`parse_units`、`reload_changed`）。
- **分节横幅**：`// --- Resources ---`（现在有 223 处）。一个文件需要分节，说明应该拆成模块；同一个 impl 里的分组用多个 `impl` 块。
- **注释掉的代码**：删掉，git 里有。
- **复述代码**：`// increment the counter`。
- **行尾注释**：一般不写；常量表、match 分支里的简短说明除外。
- **TODO**：只允许 `// TODO(<设计文档或 issue>): <要做什么>`，例如 `// TODO(0022 §5.1): retire through GpuLifetime.`；不允许不带出处的 TODO。

### 7.1 `// SAFETY:`

- 每个 `unsafe` 块前**必须**有 `// SAFETY:` 注释（`clippy::undocumented_unsafe_blocks`，改为 deny），说明**为什么这里满足被调用函数 `# Safety` 里的每一条前提**，而不是描述 unsafe 操作做了什么。
- std 的写法：`// SAFETY: A `Vec` always has a non-null pointer.`、`// SAFETY: Upheld by caller.`（在 `unsafe fn` 内部转发调用方的保证时）。
- 一个 `unsafe` 块只包一个需要论证的操作；多个操作的前提不同，就分成多个块，各写各的 `SAFETY`。
- `unsafe impl Send/Sync` 前同样写 `// SAFETY:`，说明为什么跨线程是安全的。

## 8. 强制检查

加进 workspace 的 `[workspace.lints]`：

| lint | 级别 | 作用 |
|---|---|---|
| `missing_docs` | warn（runtime 和 compiler 改为 deny，等存量清完） | 公开项必须有文档 |
| `rustdoc::broken_intra_doc_links` | deny | 链接必须能解析 |
| `rustdoc::private_intra_doc_links` | warn | 公开文档不链接私有项 |
| `clippy::missing_errors_doc` | warn | 返回 `Result` 的写 `# Errors` |
| `clippy::missing_panics_doc` | warn | 会 panic 的写 `# Panics` |
| `clippy::missing_safety_doc` | deny | `unsafe fn` 写 `# Safety` |
| `clippy::undocumented_unsafe_blocks` | deny（现在是 warn） | `unsafe` 块写 `// SAFETY:` |
| `clippy::doc_markdown` | warn | 标识符放进反引号 |
| `clippy::too_long_first_doc_paragraph` | warn | 摘要行是一句话 |
| `clippy::empty_docs`、`clippy::doc_lazy_continuation` | warn | 空文档、列表续行 |

CI 跑 `cargo doc --workspace --no-deps` 并加 `-D warnings`，并且跑 doctest。

`xtask check` 增加文本规则（与 0023 §6 的 `check-diagnostics` 放在一起）：

1. `///` 条目文档里出现设计文档引用（`§` 或 `docs/design`）→ 警告（§4）。
2. 分节横幅 `// ---`、`// ===` → 失败（§7）。
3. 不带出处的 `TODO`、`FIXME`、`XXX` → 失败（§7）。
4. ` ```ignore ` 的上一行没有说明原因 → 失败（§3）。
5. 文档摘要以 `This function`、`This method`、`This struct`、`Helper` 开头 → 失败（§5）。

## 9. 迁移

存量不需要一次改完，但**新写和改动的代码立即按本文执行**。

1. 先加 lint 和 `xtask check` 规则，存量用 `#[expect(...)]` 或基线文件挂起，保证不再新增。
2. 按 crate 逐个清理，每个 crate 一个提交：摘要行、`# Errors`/`# Panics`、删除条目文档里的设计文档引用、删除横幅和叙述步骤的注释（必要时拆函数）、crate 根补示例。先清公开 API 最多的 crate：`marlin-asset`、`marlin-render-graph`、`marlin-rhi`、`marlin-ecs`、`marlin-world`。
3. 每清完一个 crate，把它的 `missing_docs` 升为 deny，并移除它的基线条目。
