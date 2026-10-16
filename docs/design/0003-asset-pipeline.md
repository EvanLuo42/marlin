# 0003 — 资产管线：源格式、索引、cook、缓存、加载与 framework

状态：草案 · 2026-10-16

参考：David Marcelis, *Scaling Tools for Millions of Assets*（GDC 2023，Guerrilla / Decima）。从这个演讲里取了四个核心思想：

1. 全部内容都存成人类可读的文本对象，链接语法所有类型共用。
2. 有一个只依赖共有结构的本地索引。
3. 资产只能通过内容里的链接加载，不允许在代码里拼路径。
4. cook 结果按内容寻址缓存，开发期按需串流。

## 0. 决定摘要

| 维度 | 决定 |
|---|---|
| 源格式 | `.masset` 文本，CoreText 式结构，**类型化字面量** |
| 命名 | 文本中的类型名即 Rust 类型名；字段在 Rust snake_case 与文本 PascalCase 之间自动转换；缩写当普通单词（`hdr_texture` ↔ `HdrTexture`） |
| 对象链接 | `<路径:名字 #UUID>`，UUID 为准，路径和名字由工具自动修正 |
| 二进制源文件 | 与文本放在同一个资产树里，用 `` `source:…` `` 文件链接引用；DCC 工程文件放在管线外的 work 目录 |
| 索引 | 嵌入式库，内存列式表加字符串池，持久化为可 mmap 的快照，增量更新；**每台机器各有一份，不共享**（Decima 也是这样） |
| 共享 | 共享缓存服务器（GlobalStore 式）存 cook 结果和索引 extract |
| cook 单元 | 一个 `.masset` 文件就是一个 cook 与加载单元 |
| 缓存 | 本地内容寻址缓存加共享服务器，两者都现在实现 |
| 开发期加载 | 游戏按 ID 向 cook 服务请求，缺失的按需 cook，结果串流过来；发行时用包 |
| 压缩 | 每个 chunk 可选 codec；zstd、LZ4、GDeflate **全部自研**，先把 API 定死并实现功能，之后再优化；主机硬件解压走 ext |
| 包与挂载 | 分层挂载并按优先级覆盖（base < patch < DLC < dev）；包布局对平台的二进制差分补丁友好 |
| 主循环 | 模拟与渲染重叠：渲染数据抽取成快照，两侧都以 job 图运行 |
| 世界数据 | 用同一种文本格式：entity 是对象，component 是嵌套块，prefab、关卡、流式 cell 都通过链接组织 |
| 第三方 | 核心算法自研：压缩、BCn 编码、网格优化、meshlet；外部格式的解析用成熟 crate（png/jpeg/exr/glTF） |
| 首批导入格式 | glTF 2.0，PNG/TGA/JPEG，EXR/HDR |
| VCS | 抽象层（trait）。先做 Git，再做 Perforce，并为 Epic Lore 留好位置 |
| 共享服务器实现 | 自研完成式网络 IO（IOCP/RIO、io_uring），与游戏的 marlin-net 共用一套网络层 |

## 1. 源格式 `.masset`

### 1.1 语法

```text
!Version 1

EnvironmentProbe
{
    !Uuid = "deb8916d-3350-48d5-bf61-c2afbe805f67"
    !Name = "Probe1"

    Kind = DynamicLighting
    DepthTexture = <level/probe1_depth:Probe1Depth #a1c3…>
    AlbedoTexture = <:Probe1Albedo #0183…>          // 同一文件内的对象，路径省略
    Transform = { Translation = (252.0, 242.08, 2.5), Rotation = (0.0, 0.0, 0.0, 1.0) }
    Tags = ["outdoor", "baked"]
}

Texture
{
    !Uuid = "01832501-7918-3ee4-b177-2f200aab91b3"
    !Name = "Probe1Albedo"
    Format = Bc1
    MipMaps = false
    Source = `source:level/probe1_albedo.png`
}
```

| 元素 | 写法 |
|---|---|
| 文件头 | `!Version N`，表示格式版本 |
| 对象 | `TypeName { … }`。必须有 `!Uuid`（UUIDv7）和 `!Name`（文件内唯一） |
| 布尔值 | `true` / `false` |
| 数字 | 整数与浮点；浮点按最短往返形式写出 |
| 字符串 | `"…"`，转义写法同 Rust |
| 枚举 | `Variant`、`Variant(…)` 或 `Variant { … }` |
| `Option` | `None` 或直接写值 |
| 元组、定长数组 | `(a, b, c)` |
| 列表 | `[a, b]` |
| 结构体 | `{ Field = … }` |
| 对象链接 | `<相对资产根的路径不含扩展名:对象名 #uuid>` |
| 文件链接 | `` `source:路径` `` |
| 注释 | `//` 和 `/* */` |

### 1.2 与 Rust 的绑定

- 绑定完全通过 marlin-reflect 完成。文本中的类型名就是类型注册时的名字（默认是 Rust 标识符，可以用 `#[reflect(name = …)]` 覆盖）。字段名、枚举变体名在 snake_case 与 PascalCase 之间按固定规则互转，Rust 代码里从不出现 PascalCase 的字段名。
- 链接字段的 Rust 类型是 `Link<T>`（对象链接）和 `SourceFile`（文件链接）。工具层通用代码不需要知道具体类型，就能找出所有链接。
- **Schema 演进**：
  - 字段按名字匹配。
  - 缺失的字段取默认值。
  - 未知字段报警告，**并在编辑器保存时原样保留**，不能丢数据。
  - 类型可以声明 `version`，并注册升级函数。
- **写回**：由 writer 产出规范格式。字段按声明顺序输出；只写出与默认值不同的字段（UE 的 delta 方式）；未知字段排在后面；文件里对象的顺序保持不变。同一份数据写出的字节总是相同，diff 稳定。

### 1.3 两条解析路径

- **完整解析**：先把文本解析成通用语法树（保留位置信息），再绑定到 reflect 类型。cook 和编辑器走这一条。
- **快速 extract**：只扫描出每个对象的类型、UUID、名字、链接和文件链接，不做类型绑定。索引走这一条，它要能扫几百万个文件。

## 2. 资产索引（`tools/asset_db`）

每台机器本地一份，嵌入 cooker、cook 服务和编辑器使用，不跨人共享。

- **表**：
  - files：目录、文件名、扩展名分别放在不同的字符串池里；另存大小、mtime、内容哈希，以及该文件对象在 objects 表中的区间。
  - objects：UUID、名字、类型、所属文件，以及链接区间、文件链接区间。
  - links：目标 UUID，以及用于诊断的 path:name。
  - file_links
  - 反向引用加速表
- **持久化**：
  - 快照是可 mmap 的文件，加载时不需要反序列化。
  - 增量更新的事件来源按平台不同：Windows 用 NTFS USN journal，Linux 用 fanotify/inotify，macOS 用 FSEvents。拿不到时退化为按 size+mtime 扫描。
  - 列的位压缩先留出 API，之后再做优化。
- **Extract 缓存**：每个文件的 extract 以 (路径, 大小, mtime, 内容哈希) 为键存进共享缓存。新机器建立索引时只需列出文件并下载 extract，不用解析每一个文件。
- **查询**（都是批量、多线程的）：
  - 按类型、名字子串、UUID 查找。
  - 正向和反向引用。
  - 依赖闭包。
  - 悬空链接。
  - 一个对象属于哪些关卡/cell（沿链接走到关卡）。
- **校验**：
  - 悬空链接。
  - 链接指向的文件未提交。
  - UUID 冲突、文件内 `!Name` 重名。
  - 提交前检查经由 VCS 抽象执行。

## 3. VCS 抽象（`tools/vcs`）

`trait Vcs` 负责以下几件事：
- 查询工作区状态（新增、修改、删除、未跟踪）
- 查询待提交集合
- 文件锁（用于二进制文件）
- 当前修订号
- 提交前检查的挂钩点

后端计划：先做 Git（本仓库用的就是 Git），然后做 Perforce，Epic Lore 的位置也预留好。

## 4. Cook

### 4.1 单元与键

- 一个 `.masset` 文件对应一个 **cook 单元**，单元 ID 是资产路径（不含扩展名）的 64 位 xxh3 哈希。对象按 UUID 寻址，构建清单记录每个对象所在的单元，所以文件改名只换单元 ID，不影响链接。它的输入包括：文件文本、它引用的源文件内容，以及**构建依赖**（cooker 需要读取其 cook 结果的其他单元，例如材质依赖 shader）。
- 跨文件链接分两种：
  - **运行时引用**：只在运行时加载目标，不影响本单元的 cook 结果。
  - **构建依赖**：会影响 cook 结果。
  - 由 cooker 插件声明属于哪一种。
- **cook 键** = H(各类型 cooker 的版本号, 目标平台, 平台 cook 设置, 单元源文本哈希, 文件链接内容哈希, 各构建依赖的 cook 键)，128 位。同样的输入必须产出逐字节相同的结果，测试里会校验确定性。

### 4.2 cook 插件

```rust
pub trait Cooker {
    const TYPE: &'static str;      // reflect 名
    const VERSION: u32;            // 改了输出就加一
    fn build_dependencies(&self, obj: &ObjectRef, ctx: &DepContext) -> Vec<UnitId>;
    fn cook(&self, obj: &ObjectRef, ctx: &mut CookContext) -> Result<CookedObject, CookError>;
}
```

- `cook_texture`：解码源文件、生成 mip、编码 BC1/3/4/5/6H/7，主机上还要做 swizzle（`platforms/<p>/cook/`）。
- `cook_mesh`：处理 glTF，包括顶点缓存优化、overdraw 优化、LOD、meshlet，以及骨骼、蒙皮与动画。
- 没有专用 cooker 的类型走通用 cooker，把 reflect 值写成 cooked 二进制。

### 4.3 cooked 单元格式

```text
header      magic "MCU1"  version  unit_id  platform  sizes
objects     [type_id, uuid_hash, name_hash, offset, size]   按 uuid_hash 排序
imports     [uuid_hash, unit_id]                              外部链接，加载时解析
payload     marlin-serial cooked 布局（相对指针，就地使用）
bulk        GPU 数据（顶点、纹理 mip），对齐到 4 KiB，直接上传，不经过 CPU 拷贝
```

单元内部的链接直接写成相对指针。跨单元的链接写成 import 下标，加载后再修正。

## 5. 缓存

- **本地 DDC**：内容寻址存储（键为 128 位），数据放在分片的 pack 文件里，带索引；按 LRU 和容量上限淘汰。
- **共享服务器 `marlin-store`**（`tools/store`）：
  - 内存中保存索引，磁盘上是追加写的数据文件。
  - 二进制分帧协议，支持流水线：`GET`、`PUT`、`HAS`、`BATCH`、`REF_GET`、`REF_SET`。其中 ref 是带名字的可变指针，用于构建清单。
  - 网络层是自研的完成式 IO：Windows 上用 IOCP（之后加 RIO），Linux 上用 io_uring，与 runtime 的 marlin-net 共用。
- **构建清单**：一个构建 = 一张从单元 ID 到 cook 键的表，这张表本身也存进 store，只用一个 ID 就能指代整个构建。有了它，就能实现"加载任意一个构建"、把自己的本地改动分享给同事，以及分布式烘焙。

## 6. 开发期加载：cook 服务

- `marlin-cookd`（`tools/cookd`）在每台工作站上常驻。它持有索引、监听文件改动、按需 cook，并为游戏提供数据。
- 游戏的 asset 系统有一个 **dev source** 后端，用 marlin-net TCP 连接 cookd（本机或开发机）。按单元 ID 请求时，cookd 用索引做依赖检查，cook 缺失的部分（命中缓存就不用再 cook），然后把结果串流给游戏。
- **热重载**：cookd 推送失效通知；runtime 重新加载对应单元，并原地修正引用（句柄保持不变）。

## 7. 包与 VFS（runtime `marlin-asset`）

- **包格式 `.mpak`**：
  - header 与单元索引表（单元 ID → chunk 区间）。
  - chunk 大小在 64 KiB 到 1 MiB 之间，按扇区对齐，每个 chunk 记录自己用的 codec。
  - bulk 区按 DirectStorage 的要求对齐。
  - 单元按稳定顺序排列，以减小补丁的差分。
- **挂载**：分层挂载，后挂载的层覆盖同 ID 的单元。查找时按单元 ID 查。
- **Codec**（`marlin-compress`，L2，no_std）：zstd、LZ4、GDeflate 都自研。解码器在 runtime 中使用，编码器在工具中使用（通过 feature 开关）。主机的硬件解压通过 L1 的 ext 提供；L2 只做纯软件实现。

## 8. Runtime asset 系统

- **句柄**：`Handle<T>` 带类型和引用计数，由下标加世代号组成；另有弱引用。状态有 `Loading`、`Loaded`、`Failed` 三种。
- **链接不持有对象**：loader 把链接解析成 `AssetRef<T>`，它不计引用。单元被加载着时，它 import 的单元也一直留着，所以链接目标总比持有链接的对象活得久；互相链接的对象也就不会形成引用环、永远卸载不掉。
- **加载流水线**：
  1. 用 marlin-io `IoQueue` 读取（打包时读包，开发时从 dev source 读）。
  2. 在 job 里解压。
  3. 修正 import。
  4. 调用按类型注册的 loader，构造运行时对象。
  5. bulk 数据交给渲染的上传队列（以后改用 copy queue）。
- **只能通过链接加载**：根集合来自启动配置（入口关卡），没有按路径加载的 API。
- 流式加载的优先级与内存预算在之后的阶段实现。

## 9. Framework（`marlin-framework`）

- 负责启动：
  - 读取启动配置（marlin-config 的 cvar 加一个启动用 `.masset`）。
  - 注册各个子系统。
  - 挂载包或连接 cookd。
  - 加载入口关卡。
- **帧流水线**：
  - 模拟第 N+1 帧的同时，提交第 N 帧的渲染。
  - 每帧结束时，从 world 抽取不可变的渲染快照。（已由 0006 §1 修订：渲染侧持有持久的 GPU Scene，每帧只抽取不可变的**变化量**，即 `RenderDelta`。流水线扩展为三级：Game N+1 ∥ Render N ∥ GPU N-1，见 0004 §3。）
  - 两侧都以 fiber job 图运行，不设固定线程。
  - 每帧调用 profile 的 `frame_mark`。
- `apps/game` 成为真正的"读包和关卡"的可执行文件，`apps/demo_cubes` 删除。示例工程放在 `samples/`。

## 10. Crate 布局

| crate | 层 | 内容 |
|---|---|---|
| `runtime/core/compress` (`marlin-compress`) | L2 | zstd、LZ4、GDeflate 的解码器；编码器放在 `encode` feature 后面 |
| `runtime/platform/net` (`marlin-net`) | L1 | 完成式 TCP（IOCP、io_uring），PAL socket 层 |
| `runtime/engine/asset` (`marlin-asset`) | L3 | 句柄、包、VFS、加载流水线、dev source |
| `runtime/engine/framework` | L3 | 主循环与帧流水线 |
| `tools/masset` | std | `.masset` 的词法、语法、writer、reflect 绑定和快速 extract |
| `tools/asset_db` | std | 索引 |
| `tools/vcs` | std | VCS trait 与 Git 后端 |
| `tools/cook_core` | std | 依赖图、cook 键、DDC、cooked 单元写出、打包 |
| `tools/cook_texture`、`tools/cook_mesh` | std | 导入与编码 |
| `tools/store` | std | 共享缓存服务器（库加二进制） |
| `tools/cookd` | std | cook 服务 |
| `tools/cooker` | std | CLI：cook、打包、校验、查询 |

## 11. 实施阶段

| 阶段 | 内容 | 可并行 |
|---|---|---|
| P1 | `.masset` 格式；`marlin-compress`（三种 codec）；`marlin-net` TCP；BCn 编码器与 mip | 四项互相独立 |
| P2 | `asset_db` 索引、`vcs`（Git）、校验 | |
| P3 | `cook_core`（cook 键、本地 DDC、cooked 单元）、`cook_texture`、`cook_mesh`（glTF） | |
| P4 | runtime `marlin-asset`（包、VFS、句柄、加载）、打包 | |
| P5 | `marlin-store` 服务器、`cookd`、dev 串流、热重载 | |
| P6 | `marlin-framework`、`apps/game`、`samples/`；删除 `demo_cubes` | |

测试：每个 crate 都在 `tests/` 下写测试。端到端测试是：samples 工程 → cook → 打包或 cookd 串流 → game 加载 → 在真实硬件上渲染，并读回像素校验。

## 12. 实施状态

- **P1 · `.masset` 格式（`tools/masset`）完成**：
  - 词法、语法、规范化 writer；
  - reflect 绑定：PascalCase 自动转换、默认值省略、未知属性原样保留、schema 版本升级、按 UUID 解析链接；
  - 快速 extract，以及 UUIDv7 生成。
  - 测试覆盖：语法与错误位置、随机损坏输入不 panic、round trip 后为不动点。
  - 吞吐（release，单线程）：extract 257 MiB/s，完整 parse 98 MiB/s，write 140 MiB/s。
- **身份类型**：`Uuid`、`Link<T>`、`SourceFile` 放在 `marlin-asset`（no_std），reflect 名字固定。
- **`marlin-cook check <路径>`**：检查语法错误、跨文件的 UUID 冲突和悬空链接。
- **并行进行中**：`marlin-compress`、`marlin-net` TCP、BCn 编码器与 mip。
- **P2 · 索引 `tools/asset_db` 完成**：
  - 列式表、字符串池、开放寻址的 UUID/路径表。
  - 快照 64 字节对齐，用 mmap 打开、按列写时复制；版本化文件加 `CURRENT`，绕开 Windows 上"被映射的文件不能替换"的限制。
  - 增量更新：
    - Windows 上读 NTFS 变更日志。不需要管理员权限：经目录句柄读取，记录里没有文件名，按 FRN 解析，也就是说文件变更会重列它所在的目录，目录变更按 FRN 求出新路径后重扫子树。
    - 否则退化为全量 stat 扫描。
  - 查询：类型、名字子串、反向引用、依赖闭包、源文件使用者，并实现了 `marlin_masset::Links`。
  - 校验：解析错误、UUID 冲突、悬空链接、过期的 path:name、缺 UUID 的链接、缺失的源文件。
  - 规模（release，5 万文件 / 20 万对象）：完整构建 0.62 s，打开加无变化更新 30 ms，搜索 10 ms，反向引用热查询 2 µs，校验 135 ms。
- **`marlin-cook index|validate <root>`** 已接上索引。
- **P2 · `tools/vcs` 与提交前校验完成**：
  - `Vcs` trait 带 Git 后端（porcelain v2 -z、批量查询、LFS 锁、pre-commit 挂钩），能识别 Perforce 和 Lore 工作区（目前报 Unsupported）。
  - `AssetDb::validate_submit` 检查三件事：提交内容里的错误；链接或源文件指向既未跟踪、也不在本次提交里的文件；剩下的文件链接到本次提交删除的文件。
  - `marlin-cook validate --pre-submit` 与 `install-hook` 已在真实 Git 仓库里验证过：能挡住"只提交了链接"的那种提交。
- **P3 · cook 管线完成**：
  - `marlin-asset::cooked`：MCU1 单元格式，零拷贝读取 `UnitView`，所有偏移都检查范围，随机损坏输入不 panic。
  - `tools/cook_core`：
    - cook 键（xxh3-128）覆盖 cook 格式版本、平台、单元路径、源文本、每个对象类型的 reflect 结构指纹与 cooker 版本、每个 `source:` 文件的内容哈希、每个链接目标所在的单元。
    - cooker 只能读取对象用 `source:` 声明过的文件，杜绝"字符串魔法"依赖。
    - 没有专用 cooker 的类型走 marlin-serial 通用 cooker。
    - `LocalDdc`：追加写、带校验和的 pack 文件，崩溃时截断不完整的尾部，读出校验失败的条目视为未命中。
    - 单元并行 cook，结果逐字节确定（与线程数无关）。
    - 构建清单（MMAN）同时写入 DDC 与 `.marlin/build/<platform>.manifest`。
  - 资产类型与 cooked 布局都是 marlin-serial cooked 布局（相对指针、就地读取），带校验的 view：
    - `marlin-render::asset::Texture`：用法、格式、立方体、mip、`MaxSize`、alpha 覆盖率。cooked 为 RHI 拷贝布局（行 256、子资源 512 对齐）；mip 尾以上每级一个 bulk（流送页），64 KiB 以下的级别合成常驻的 mip 尾。
    - `marlin-render::asset::Mesh`：0006 §2 的格式。16 位量化位置；法线、切线和副切线符号打包进 32 位（八面体法线 2×10 位，切线角 11 位）；UV 为 f16；蒙皮为 4×u16 加 unorm8 权重，和为 255；每个 LOD 一页，含 meshlet、meshlet 顶点与三角形，以及给 BLAS 和无 mesh shader 路径用的索引。
    - `marlin-anim::asset::{Skeleton, AnimationClip}`：关节按父先子后排列；动画按固定采样率重采样，常量通道只存一个键，旋转用 48 位 smallest-three 量化（误差 < 0.01°）。网格的蒙皮关节和动画轨道都**按名字**绑定骨骼，所以网格、骨骼和动画可以来自不同文件。
  - `tools/cook_texture`：解码 PNG/TGA/JPEG/EXR/HDR（解析用 `image`），经纬图转立方体（线性空间超采样），然后生成 mip 并做 BC 编码（这两步是 P1 的成果）。
  - `tools/cook_mesh`（glTF 2.0 用 `gltf` 解析；以下算法全部自研）：
    - 导入：GLB、外部 buffer（必须写进 `ExtraSources`）、data URI；三角形、strip 和 fan；缺法线时按 glTF 规定用平面法线；缺切线时生成切线，镜像 UV 处按手性拆分顶点。
    - 二次误差简化器：半边折叠，带属性代价；UV 和法线接缝只沿接缝折叠；开放边界、材质边界和非流形边锁定；每级 LOD 都从 LOD 0 简化，误差不会累积。
    - Forsyth 顶点缓存优化与顶点取用顺序优化。
    - meshlet 生成：64 顶点、124 三角形；优先不增加顶点的三角形和悬空三角形，再按距离和法线锥选；遇到孤岛就吸收附近的三角形。在规则网格上平均每个 meshlet 82 个三角形（理论上限约 98）。包围球用 Ritter 算法；法线锥量化保守，测试验证了被剔除的 meshlet 没有正面三角形。
  - `marlin-cook cook <root> [--platform] [--threads]`：更新索引、cook、发布清单，并报告 cook 数、命中缓存数和错误。
- **P4 · runtime 加载与打包完成**（`marlin-asset`，no_std）：
  - `.mpak` 包格式：
    - 结构：文件头、chunk 数据、末尾的索引（单元、blob、chunk、对象四张表，加 xxh3 校验）。
    - 单元按 cooked 字节切成 chunk，每个 chunk 不超过 256 KiB，各自记录 codec、两种大小和校验和。
    - 每个 bulk blob 从新的 chunk 开始，起点在文件里 4 KiB 对齐，以后可以单独流送，也满足 DirectStorage 的要求；单元头之间按 16 字节紧凑排列。
    - 单元按资产路径排序，打补丁时其余字节位置不变。
    - 不值得压缩的 chunk（节省不到 3%）原样存储。
    - 索引和 chunk 的损坏都会报成错误。
  - `Vfs`：分层挂载，后挂载的层覆盖同 ID 的单元，按 UUID 查对象所在的单元。`UnitSource` 有两种交付方式：给出文件区间，异步读完后再解码；或者直接给出字节。实现有 `Package` 和 `MemorySource`，P5 的 cookd dev source 也从这里接入。
  - `AssetServer`：
    - 句柄：`Handle<T>`（引用计数加世代号槽位）、`WeakHandle<T>`、`AssetRef<T>`。
    - 只能按 UUID 加载。
    - 流水线：`IoQueue` 异步读取，在 `JobSystem` 上并行解压，然后解析，补齐 import 闭包（互相引用的单元一起加载），再调用按 cooked 类型 id 注册的 loader。`register_serial` 处理通用类型。
    - 没有句柄、也没有常驻单元 import 它的单元会被卸载。
    - 失败（找不到、没有 loader、类型不对、读取失败、单元损坏、loader 拒绝）都是状态，不会 panic。
    - `update_blocking` 给工具和加载画面用。
  - 单元组装（`cooked::write::unit`）移到了 `marlin-asset`，cook_core、测试和以后的 cookd 共用。
  - 打包器 `cook_core::write_package`：多线程压缩，输出逐字节确定。对应命令 `marlin-cook package <root> [--platform] [--out] [--head-codec] [--bulk-codec] [--level]`。
  - 测试：
    - 四种 codec 都能逐单元、逐 blob 还原；
    - 用真实 `IoQueue` 加上 job 解压加载，bulk 用 GDeflate；
    - 补丁层覆盖；
    - chunk 或索引损坏被检测出来，其余单元照常加载；
    - 链接环能加载，也能一起卸载；
    - 全工作区 532 个测试通过。
- **P5 · 共享缓存、cook 服务与热重载完成**：
  - `marlin_asset::wire`：工具链 TCP 协议的分帧。16 字节帧头，含 magic、kind、id、长度和校验，后接负载。请求可以流水线发送，响应按 id 对上，id 0 用于推送。损坏或超长（> 1 GiB）的帧会断开该连接；负载读取都有越界检查。
  - `tools/store`（`marlin-store`）：
    - `Server`：单线程、多连接的分帧服务器，跑在 marlin-net 的完成式队列上（Windows 上是 IOCP）。每个连接始终挂着一个接收，发送按顺序排队。cookd 也用它。
    - `Store`：数据复用 `LocalDdc` 的 pack 文件；ref 是追加写、带校验的日志，打开时丢弃不完整的尾记录。
    - 协议：`GET`、`HAS`、`PUT`、`BATCH`、`REF_GET`、`REF_SET`（可选 CAS）、`STATS`。
    - `StoreClient`（支持流水线），以及实现了 `Ddc` 的 `RemoteDdc`（失败时重连一次，连不上就当成空缓存）。
  - `cook_core::LayeredDdc`：本地加共享两级缓存。读时先查本地，共享层命中后写回本地；写时两边都写，共享层只是尽力而为。`fetch_unit` 取单个单元：命中直接读，否则现场 cook。
  - `marlin-cooker` 拆成库（`engine_cookers`、`open_ddc`）和 CLI；`cook` 与 `package` 都支持 `--store <addr>`。
  - `tools/cookd`（`marlin-cookd <root> [--listen] [--store] [--workers]`）：
    - 持有索引，用工作线程按需 cook，同一单元的并发请求合并成一次。
    - 每 250 ms 用 NTFS 变更日志更新一次索引。对每个客户端，比较它拿到过的单元的当前 cook 键，只推送真正变了的（文本、源文件、类型或链接目标所在单元变化）。对象表变化时一并推送新表。
    - 记住告诉过客户端的键（包括失败时的键），所以修好一个坏单元也会通知到它。
  - `marlin_asset::DevSource`（no_std，基于 marlin-net）：连接时拿到全部对象表，之后查对象不用等网络。单元请求不阻塞（新增的 `Read::Pending` / `UnitSource::poll`），失效通知走 `UnitSource::take_invalidated`。
  - `AssetServer` 热重载：
    - 失效单元重新加载，句柄和槽位不变，值原地替换，`version` 加一；`take_reloaded` 列出替换过的对象。
    - 重载失败时保留上一个好版本，错误进入 `take_reload_errors`。
    - 对象从单元里删掉会进入失败状态；挪到别的单元的对象会跟过去。
    - 加载途中再次失效，会在这次完成后再载一次。
    - `update_blocking` 会等待远程单元。
  - 修了一个既有问题：Windows 的 `WaitOnAddress` 超时可能比期限早一个时钟节拍触发。现在早到的超时算作伪唤醒，`futex::wait` 返回 false 一定表示整个超时已过，原来偶发失败的 `marlin-pal` futex 测试因此稳定。
  - 测试：
    - store：流水线、8 个并发客户端、5 MB 条目、损坏帧和超长帧、ref 的 CAS、持久化与尾记录恢复、`RemoteDdc` 的重连；
    - 两台工作站共用一个 store 时，第二台什么都不用 cook；
    - cookd 端到端：经 `DevSource` 加载；改文本和改源文件都原地重载；坏的修改保留旧版本，修好后恢复；失败以状态返回；
    - 纯运行时的重载用例：删除、迁移、失败保留、加载途中失效；
    - 帧的分片到达与随机损坏；
    - 全工作区 548 个测试通过。
- **待做**：
  - Linux 和 macOS 的变更源（fanotify、FSEvents）。
  - 共享缓存中的 extract；store 的 LRU 与容量上限；store 的鉴权与 TLS（目前只适合可信局域网）。
  - cook：MikkTSpace 逐位一致的切线（现在是 Lengyel 加按角度加权，与 MikkTSpace 很接近，但拆分处不完全一致）、morph target、主机纹理 tiling 单元、cluster DAG（格式里已经预留了字段）、DDC 的 LRU 淘汰。
  - 加载：
    - 按 blob 流送，带优先级和内存预算（包格式与 `Package::read_blob` 已经支持）；
    - unbuffered IO 与 DirectStorage；
    - 渲染侧 Texture/Mesh 的 loader，以及 GPU 上传（随 framework 在 P6 做）；
    - 热重载之后通知渲染侧重建 GPU 资源（`version` 已经提供），随 framework 在 P6 做。
