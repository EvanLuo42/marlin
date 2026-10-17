# 0015 — C# 系语言的共享前端

状态：草案 · 2026-10-17

前置：
- [0004](0004-shader-language.md)：shader 语言
- [0005](0005-shader-compiler.md)：shader 编译器

## 0. 决定摘要

Marlin shader 的语法照着 C# 走（0004），以后的 VM 语言大概率也照着 C# 走，只是比 shader 多出 `new`、`class` 这些东西。所以前端按"一族语言"来做，两种语言共用尽量多的代码。

| 维度 | 决定 | § |
|---|---|---|
| 复用范围 | 基础设施、语法、名字解析共享；类型检查（`hir_ty`）及之后各语言自己实现 | 1 |
| 语法差异 | 一份**超集文法**，每种语言提供一个 **`Dialect`**；parser 不看 dialect，解析之后由 validation 按 dialect 报错 | 2 |
| 目录 | `compiler/common`（语言无关）、`compiler/front`（C# 系共享前端）、`compiler/shader`、`compiler/vm` | 1 |
| VM 专有语法 | 结构现在搭好；shader 为了给出好提示本来就要解析的构造现在就建成正式节点，其余等 VM 语言设计时再加 | 2.4 |
| query 引擎 | 每个 query 一个类型、单一动态 `Db`；红绿增量 + backdate + durability；协作式取消（`get` 返回 `Result<_, Cancelled>`）；可恢复的循环；诊断是返回值的一部分 | 4 |

## 1. 分层

```
compiler/
  common/   语言无关，不知道 C#                          包名 marlin-lang-*
    span      源码位置、文件 id、行列映射
    diag      结构化诊断与文本渲染器
    arena     arena、interning、类型化下标
    query     增量 query 引擎
  front/    C# 系语言共用的前端                          包名 marlin-front-*
    syntax    lexer、无损 CST、超集文法的 parser、validation、有类型的 AST 视图
    hir_def   包与 namespace、using、item tree、def map、路径解析、函数体降低成 HIR
  shader/   shader 专有                                  包名 marlin-shaderc-*
    syntax    shader 的 Dialect 和解析入口
    hir_ty mir mono ir opt target backend_* reflect，facade marlin-shaderc
  vm/       VM 语言专有（以后）                          包名 marlin-vmc-*
```

依赖规则：
- `common` 不依赖 `compiler/` 里的任何东西；
- `front` 只依赖 `common`；
- `shader/*` 和 `vm/*` 可以依赖 `front` 和 `common`，两者之间不能互相依赖；
- `common` 和 `front` 里不允许出现针对某种语言的分支（`if shader`、`match language`）。语言之间的差异写进 `Dialect`（§2）或者语言描述（§4）里，这和后端的"共享代码里不写 `match target`"是同一条原则（0005 §1）。

为什么类型检查不共享：shader 只有值类型、跑在 GPU 上，有 capability、uniformity、自动微分；VM 语言有 class、引用、GC、继承和可空性。两边的类型系统差别太大，硬共享 `hir_ty` 会让两边都背上对方的复杂度。共享到名字解析为止，两边重复的只是"按类型查成员"这类较小的部分。

## 2. 语法：超集文法 + Dialect

### 2.1 流程

```
parse(text, &dialect)
  ├─ lex       C# 的保留字 + dialect 额外保留的关键字
  ├─ parse     超集文法：任何一种语言用到的构造都建成正式节点，不看 dialect
  └─ validate  遍历 CST，dialect 不允许的构造报错并给出修改建议
```

例如 shader 里写 `new Light(c, 2)`：parser 建出一个正常的 `NEW_EXPR` 节点，validation 报"Marlin shader has no `new`"，并建议改成 `Light(c, 2)`。VM 语言里写 `const if`：parser 照样建出 `IF_STMT`，VM 的 validation 报错。

- 树的结构对所有语言都一样：同一段源码在两种语言里解析出同一棵 CST，只是诊断不同。所以 AST 视图、格式化、语法高亮、IDE 的大部分功能都只写一份。
- parser 里没有语言判断，加一种语言不需要改文法。
- 所有语言都不支持的 C# 构造（目前有 `try`/`catch`、`goto`、`lock`、`unsafe`、`fixed` 等）由 parser 报通用的"不支持"错误；等哪种语言需要了，再把它加进文法和那种语言的 dialect。

### 2.2 `Dialect`

```rust
pub struct Dialect {
    /// 诊断里用的语言名，比如 "Marlin shader"
    pub name: &'static str,
    /// 在 C# 保留字之外额外保留的关键字，比如 shader 的 `half`
    pub extra_keywords: &'static [SyntaxKind],
    /// 允许的构造
    pub allowed: ConstructSet,
    /// 不允许时给出的修改建议
    pub hint: fn(Construct) -> Option<&'static str>,
}
```

- `SyntaxKind` 是所有语言的超集，`HALF_KW` 也在里面；只是 VM 的 dialect 不保留 `half`，它的 lexer 就把 `half` 当成普通标识符。
- `Construct` 列出所有"有的语言有、有的语言没有"的构造，分两类：
  - C# 有而 shader 没有：`new`、`class`、`record`、`null`、`as`、`typeof`、`sizeof`、`base`、`throw`、char 字面量、`decimal` 字面量……
  - shader 新增而 VM 不一定要：`const if`、关联类型、const 泛型参数、带接口实现的 `extension`、`half` 字面量后缀、namespace 级函数和字段……
- 字面量后缀、关键字这类词法上的差异，lexer 只负责识别，是否允许也交给 validation。
- 每种语言的 dialect 定义在它自己的 `syntax` crate 里（shader 的在 `compiler/shader/syntax`）。`front/syntax` 自己的测试用一个允许所有构造的 dialect。

### 2.3 诊断码

| 段 | 来源 |
|---|---|
| `E00xx` | 宿主和驱动（找不到入口、读不到源文件……） |
| `E01xx` | lexer |
| `E02xx` | parser |
| `E03xx` | validation（dialect 不允许的构造） |
| `E04xx`–`E09xx` | `front` 的名字解析等共享阶段 |
| `E1xxx` | shader 专有阶段 |
| `E2xxx` | VM 语言专有阶段 |

同一个语法错误在两种语言里是同一个码。

### 2.4 现在实现哪些构造

- 超集结构、`Dialect` 和 validation 现在就做。
- shader 为了给出好的错误提示本来就要解析的那些 C# 构造，现在就建成正式节点：`new`、`class`、`record`、`null`、`as`、`typeof`、`sizeof`、`base`、`throw`、char 字面量。
- 其余 VM 专有的构造（继承列表里的基类、`virtual`/`override` 的语义、`try`/`catch`、`?.`/`??`、可空类型、事件、委托……）等 VM 语言设计时再加进文法和 VM 的 dialect，只改 `front/syntax` 的文法和 VM 自己的 crate。

## 3. 名字解析：`front/hir_def`

共享的部分：
- 包、`SourceProvider`、文件到包的映射（0005 §5.2）；
- file-scoped `namespace`、跨文件合并 namespace；
- `using`、`using static`、`using` 别名、`global using`；
- 可见性 `public`/`internal`/`private`；
- item tree：item 的种类是超集（namespace、using、struct、class、interface、enum、extension、函数、字段、属性、索引器、运算符、转换、关联类型），每种语言只会遇到它的 dialect 允许的那些；
- 泛型参数表（包括 const 参数）和 `where` 约束的语法形式；
- attribute 作为数据：路径加实参的语法树，含义由各语言解释；
- 类型路径和 namespace 路径的解析。按类型查成员依赖类型，放在各语言的 `hir_ty` 里；
- 函数体降低成 HIR：HIR 的表达式和语句也是超集。

### 3.1 input

宿主把这些设进 `Db`（§4）；编译器不读文件，也不解析包清单：

| input | key | value |
|---|---|---|
| `LanguageInput` | `()` | `&'static Language`：这个 `Db` 编的是哪种语言 |
| `PackageInput` | `PackageId` | 包名、文件列表、直接依赖。由宿主解析 `ShaderPackage` 这类清单得到 |
| `FilePackage` | `FileId` | 文件属于哪个包 |
| `FileText` | `FileId` | 源文件内容 |

`Language` 是每种语言的描述，定义在语言自己的 crate 里（shader 的在 `compiler/shader/syntax`）：
- `dialect`：解析用的 `Dialect`（§2.2）；
- `implicit_usings`：每个文件隐式 `using` 的 namespace（shader 是 `Marlin`，0004 §7.1）；
- `require_namespace`：文件是否必须有 file-scoped `namespace`（shader 是）。

### 3.2 query 与数据

| query | key | 内容 |
|---|---|---|
| `ParseFile` | `FileId` | 语法树和语法错误 |
| `AstIdMap` | `FileId` | 文件里每个声明和 `using` 的语法位置，按先序编号 |
| `ItemTreeQuery` | `FileId` | 文件的声明结构：namespace、`using`、各个声明的名字、种类、可见性、泛型参数个数和嵌套成员。**不含函数体，也不含位置**，只用 `AstId` 指回语法树，所以只改函数体或空白时它的值不变，依赖它的 query 都不重算 |
| `PackageDefMap` | `PackageId` | 包里所有文件合并出的 namespace 树：每个 namespace 里的子 namespace、类型（按名字和泛型参数个数区分，`Func<T>` 和 `Func<T, U>` 是两个类型）、函数（按名字的重载集合）、字段、extension 块；加上重复定义、缺少 `namespace` 声明这类诊断 |
| `FileImports` | `FileId` | 文件的 `using` 解析结果（namespace、`using static` 的类型、别名）和解析不了的诊断 |

- 声明的 id 是 `ItemId { file, index }`，`index` 是 item tree 里的下标。
- namespace 用全名（`Marlin.Post`）标识，所有包共享：同名 namespace 里的成员合并，这和 C# 一样。
- 一个包能看到自己和**依赖的传递闭包**里的包；别的包里只有 `public` 的声明可见。

### 3.3 名字查找

照 C# 的 namespace-or-type-name 规则（0004 §7.1）。`Resolver` 记录查找位置（包、文件、namespace、外层类型链、泛型参数），对一条路径：
- 第一段按 局部（4b）→ 泛型参数 → 外层类型的成员 → 本 namespace 和外层 namespace（每层先成员、后这一层的 `using`）的顺序找；文件里的 `using` 属于文件的 namespace 这一层，`global using` 和语言的隐式 `using` 属于最外层；
- 后面的段在前一段的 namespace 或类型里找；
- 结果是 namespace、类型、函数重载集合、字段，或者"前缀解析到某个类型、剩下的段要等类型检查"（比如泛型参数的关联类型 `M.Params`）；
- 找不到、不可访问（只找到了看不见的声明）、有歧义，各给一种结果，由调用方报诊断。

按类型查成员（字段、方法、属性）依赖类型，放在各语言的 `hir_ty` 里。

### 3.4 函数体与签名（HIR）

- **一种存储**：`ExprStore` 里有表达式、语句、模式、类型引用和局部绑定，函数体（`Body`）和声明的签名（`Signature`）都用它。常量泛型实参、数组长度、参数默认值、attribute 实参都是这里面的普通表达式。
- **贴近语法**：HIR 基本保持 C# 的写法，只去掉纯语法的东西（括号、表达式体）。需要类型才能决定的展开（复合赋值、运算符重载、`foreach` 的迭代方式）留到 MIR 降低时再做。HIR 是超集，VM 语言的 `new`、`throw`、`as` 也在里面。
- **名字不解析**：名字和点号连起来的名字（`Marlin.Math.Dot`、`v.x`）都是一条路径，`hir_ty` 用 `Resolver::resolve_in_body` 解析：第一段先找局部，找到了后面的段就是它的成员。
- **按 body 做 query**：`BodyWithSourceMap` 和 `SignatureWithSourceMap` 带着源码位置；`BodyQuery` 和 `ItemSignature` 去掉位置。改一个函数体时，同一个文件里其他函数的 `BodyQuery` 值不变，依赖它们的 query 都不重算。
- **局部作用域**照 C#（`ExprScopesQuery`）：
  - 局部从声明处起到块尾可见，局部函数在整个块里可见；
  - 表达式里声明的变量（`out var x`、`is var y`、解构）在表达式语句、声明和 `if` 条件里声明时，`if` 之后的语句也能看到；在循环头、`switch` 分支和 lambda 里声明时只在里面可见；
  - 两个同名局部，只要一个的声明空间包含另一个（不管先后），就报错 E0405；
  - 查找从使用处向外走，所以在声明之前用到的名字找不到那个局部。`ExprScopes::declared_later` 告诉 `hir_ty` 这个名字其实是块里后面声明的局部，由它报"声明前使用"（C# 的 CS0841）。
- **遍历**：`ExprStore::children` 按源码顺序给出一个表达式、语句或模式的直接子节点，`descendants` 给出整棵子树。检查和以后的降低都用它，不用各写一遍 match。

## 4. query 引擎：`common/query`

### 4.1 决定

| 维度 | 决定 |
|---|---|
| 定义 | 每个 query 是一个实现了 `Query` 的类型（`Key`、`Value`、`execute`，可选的循环回退）；input 是实现了 `Input` 的类型。只有一个具体的 `Db`，按 `TypeId` 存表。任何 crate 加 query 都只是加一个类型，不用宏，也不用拼数据库 trait |
| 组合 | `front` 的 query 和各语言的 query 都放进同一个 `Db`。语言专有的上下文（`Dialect`、target 描述、`SourceProvider` 读到的内容）都是普通 input |
| 增量 | 红绿算法：每个 memo 记录读过的依赖、`verified_at` 和 `changed_at`。input 变了以后，用到时才从依赖往上验证；依赖都没变就直接把 memo 标绿，不重算 |
| backdate | 重算出的值和旧值相等（`Value: Eq`）时，保留旧的 `changed_at`，依赖它的 query 不用重算 |
| durability | input 分 `Low` 和 `High`（标准库、核心库这类很少改的是 `High`）。只改了 `Low` 的 input 时，只依赖 `High` input 的 memo 不必逐个验证依赖 |
| 并发 | `Db` 是 `Sync`。同一个 key 只算一次，别的线程等它的结果。编译器内部不开线程（0005 §5.5） |
| 取消 | **协作式**：`set` 先举起取消标志，等正在读的 snapshot 都结束后再改 input 并进入新 revision。`get` 返回 `Result<V, Cancelled>`，query 里用 `?` 往上传；长循环里调 `cx.check_cancelled()?`。工程是 `panic = "abort"`，不能靠 unwinding 取消 |
| 循环 | 同一线程上的环和跨线程的等待环都检测。query 可以声明 `cycle_fallback`：环上请求它的那一次拿到回退值（比如"类型未知"，再由 query 自己的返回值带上一条列出整个环的诊断）。没声明回退的 query 出现在环上是编译器 bug，直接 panic，信息里列出环上的所有 query |
| 诊断 | 作为 query 返回值的一部分，由入口按固定顺序收集；不做旁路收集器 |
| 确定性 | query 必须是纯函数，只能通过 `cx` 读 input 和别的 query，结果不能依赖线程调度 |
| 持久化 | 不做，只在内存里（0005 §8） |

### 4.2 API

```rust
struct ItemTree;
impl Query for ItemTree {
    type Key = FileId;
    type Value = Arc<item_tree::ItemTree>;
    fn execute(cx: &Cx<'_>, file: &FileId) -> Result<Self::Value, Cancelled> {
        let parse = cx.get::<ParseFile>(file)?;
        ...
    }
}

// 宿主
let db = Arc::new(Db::new());
db.set::<SourceText>(file, text, Durability::Low);          // 也可以 db.update(|w| { ... }) 批量改
let snap = db.snapshot();                                   // 每个线程一个
let tree = snap.get::<ItemTree>(&file)?;                    // Err(Cancelled)：input 正在改，稍后重试
```

- 读要通过 `Snapshot`：它在存在期间挡住 `set`。`set` 会先让正在跑的 query 尽快返回 `Cancelled`，再等所有 snapshot 释放。
- 持有 snapshot 的线程不能调 `set`（会自己等自己），debug 构建里检测并 panic。
- 每个 `Snapshot` 只给一个线程用；多线程编译时每个线程各取一个。

## 5. 待定

- 包依赖的可见范围现在是传递闭包；如果要像 C# 的 `PrivateAssets` 那样限制传递，再加。
- 在局部声明之前用到它的名字（C# 的 CS0841）：现在作用域查不到这个局部，由 `hir_ty` 报错。
- 局部函数自己的类型参数还没进名字查找。
- query 的内存回收（LRU、按 revision 清理不再用到的 memo）：等内存成为问题时再做。
- VM 语言的 dialect 和它的专有构造：VM 语言设计时定。
- VM 语言是否需要 `protected`，以及它的默认可见性。
