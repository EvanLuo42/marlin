# 0001 — Marlin 仓库与目录结构设计

状态：草案 · 2026-10-15

## 0. 需求摘要（来自需求访谈）

| 维度 | 决定 |
|---|---|
| 定位 | AAA 级通用引擎 |
| 首批平台 | Windows / Linux / macOS + PS5 / Xbox Series / Switch 2 |
| no_std 边界 | runtime 全部 `core + alloc`，自己实现 `#[global_allocator]`；editor-only 代码与 tools 可用 `std` |
| 工具链 | 锁定 nightly（`allocator_api`、`build-std`、naked fn、自定义 target JSON） |
| PAL 分发 | 编译期 `cfg` 选单一 backend + **契约 trait** 校验签名 |
| Extension | **能力型**：编译期 `cfg(marlin_ext = "...")` 表示"该平台有这个 API"，运行期 `get() -> Option<Token>` 表示"这台机器/这个进程可用" |
| PAL API 风格 | 薄层 `unsafe` + 少量 RAII；安全封装交给上层 wrapper crate |
| 错误 | 统一 `pal::Error { kind: ErrorKind, raw: i32 }`，`Copy`、无分配 |
| 内存能力 | reserve/commit/decommit/release/protect + 大页、GPU 可见内存、虚拟内存别名/重映射、内存统计 |
| 并发 | 线程（亲和性/优先级/拓扑）+ fiber，上层做 fiber job system |
| 文件 IO | 完成式异步 IO 为核心，同步只是封装 |
| PAL 范围 | 只管 OS 基础；窗口/输入/音频设备/网络/平台服务/RHI 各自独立，但沿用同一套 backend + ext 模式 |
| 封装层 | 独立基础 crate（mem/sync/thread/io…）+ 薄 facade `marlin-std` |
| NDA 隔离 | 主机代码在独立私有仓库，clone 到 gitignore 的 `platforms/<name>/` |
| 入口 | 引擎接管入口、`panic = "abort"`；game 与 editor 是两个独立可执行文件，共用 runtime |
| Gameplay | 自研 VM 语言；**所有游戏共用同一个 game 可执行文件**，读取资源包与关卡 |
| 世界模型 | 底层 archetype ECS + 面向 VM 的对象化 entity/component API |
| 资源管线 | 离线 cooker → 平台专属二进制；runtime 不含任何导入器 |
| RHI | 自研现代 RHI（显式 barrier、bindless），backend：D3D12 / Vulkan / Metal / 主机 API |
| Editor UI | runtime 提供立即模式 `ui_render`（只画 primitive 与文字）；editor 在其上自建保留模式 + MVVM 框架 |
| 测试 | 桌面 host 上开 std 跑 `cargo test` |
| 构建 | cargo + xtask |

---

## 1. 顶层目录

```
marlin/
├── Cargo.toml                 # workspace：成员、[workspace.dependencies]、[workspace.lints]、profiles
├── rust-toolchain.toml        # 锁定 nightly，components = ["rust-src", ...]（build-std 需要）
├── .cargo/
│   └── config.toml            # alias xtask；桌面平台的 --cfg（由 xtask gen-cfg 生成）
├── deny.toml                  # （待建）cargo-deny：runtime 只允许 no_std 兼容的第三方依赖
├── .gitignore                 # 含 /platforms/* 与对公开平台的 ! 例外
│
├── runtime/                   # ★ 全部 no_std + alloc（测试时除外）
│   ├── base/                  #   L0  OS 基础：PAL 与它的直接封装（std 的角色）
│   ├── platform/              #   L1  其他平台层：窗口/输入/音频设备/网络/平台服务/RHI
│   ├── core/                  #   L2  与平台无关的基础设施：math/容器/job/ECS/反射…
│   ├── engine/                #   L3  引擎子系统：render/asset/world/physics…
│   └── marlin/                #   umbrella crate `marlin`：game 与 editor 共同链接的 runtime
│
├── compiler/                  # 语言工具链（待建）：VM 语言的编译器与运行时放在 compiler/vm
├── editor/                    # editor-only 库（std 可用，仅桌面）
├── tools/                     # 离线工具（std）：cooker 与各资源类型的 cook…
├── apps/                      # 最终可执行文件：game、editor（全局符号只允许在这里定义）
├── xtask/                     # 构建编排：cargo xtask build/cook/run/package/check-layers
│
├── platforms/                 # 平台清单 + 主机私有 backend（主机目录 gitignore）
├── third_party/               # vendored C/C++ 库与 Rust fork
├── samples/                   # 示例游戏工程（源资源 + VM 脚本），用于测试整条管线
└── docs/
    ├── design/                # 设计文档（本文件）
    └── guides/
```

### 1.1 分层与依赖规则

```
 apps/game ─────────────┐        apps/editor ──┐
                        ▼                       ▼
                 runtime/marlin  ◄────────  editor/*  (std)
                        │                       │
            ┌───────────┴───────────┐           ▼
            ▼                       │        tools/* 的库部分（cook_*）
      L3 runtime/engine/*           │
            │                       │
            ▼                       │
      L2 runtime/core/*             │
            │                       │
            ▼                       ▼
      L1 runtime/platform/*  ───►  L0 runtime/base/*  ───►  pal
```

由 `cargo xtask check-layers` 强制检查（解析 `cargo metadata`）：

1. **只有 L0 与 L1 的 crate 可以依赖 `marlin-pal`**。L2/L3 只能通过 wrapper（`marlin-mem`、`marlin-io`……）访问 OS 能力，包括 ext。
2. **只有 L0/L1 包含平台 backend 代码**。L2/L3 完全平台无关，唯一允许的平台相关写法是 `#[cfg(marlin_ext = "...")]`。
3. 下层不得依赖上层；同层依赖需在 `docs/design/` 中登记。
4. `runtime/**` 不得依赖 `std`，第三方依赖必须在 `deny.toml` 的 no_std 白名单里。
5. `#[global_allocator]`、`#[panic_handler]`、入口符号只能出现在 `apps/*`（库 crate 只提供宏/函数）。这样 editor（std）和 game（no_std）才能链接同一份 runtime。

---

## 2. `runtime/base/` — L0：PAL 与它的封装

```
runtime/base/
├── pal/        marlin-pal      core only（不依赖 alloc，因为分配器建在它上面）
├── mem/        marlin-mem      core only + pal；实现 alloc：页分配、arena、通用堆、GlobalAlloc
├── sync/       marlin-sync     基于 wait-on-address 的 Mutex/RwLock/Condvar/Once/Event
├── thread/     marlin-thread   Thread/Builder/Fiber/CPU 拓扑/TLS
├── time/       marlin-time     Instant/高精度计时/sleep/高精度等待
├── io/         marlin-io       完成式异步文件 IO、Path、mmap、目录遍历（开发版）
├── diag/       marlin-diag     log 宏与 sink、assert、debug break、栈回溯、crash dump
├── rt/         marlin-rt       启动序列、入口宏、panic 处理逻辑
└── std/        marlin-std      facade：pub use core/alloc + 以上全部 + prelude
```

### 2.1 `pal/` 内部结构

```
runtime/base/pal/
├── Cargo.toml
└── src/
    ├── lib.rs                 # 选择 backend：mod backend; use backend::imp;
    ├── error.rs               # Error { kind, raw }、ErrorKind
    ├── types.rs               # 共享 POD：Protection、PageSizes、CpuSet、ThreadPriority…
    │
    ├── contract/              # ★ 契约：每个 backend 都必须实现的 trait
    │   ├── mod.rs             #   pub trait Platform: VMem + Thread + Fiber + Futex + Time + File + Debug + SysInfo + Process {}
    │   ├── vmem.rs
    │   ├── thread.rs
    │   ├── fiber.rs
    │   ├── futex.rs           #   wait_on_address / wake_one / wake_all
    │   ├── time.rs
    │   ├── file.rs            #   IoQueue：submit / poll completions
    │   ├── debug.rs
    │   ├── sysinfo.rs         #   内存总量/可用/进程 commit、CPU 拓扑、页大小
    │   ├── process.rs         #   args、env、exit、abort
    │   └── ext/               #   能力型 extension 的契约（每个能力一个 trait）
    │       ├── large_pages.rs
    │       ├── physical_memory.rs   # 显式物理页分配 + 映射（主机）
    │       ├── vmem_alias.rs        # 同一物理页多重映射、MEM_RESET/MADV_FREE
    │       ├── gpu_memory.rs        # GPU 可见/统一内存类型（主机）
    │       ├── hybrid_cores.rs      # P/E 核调度提示（QoS/EcoQoS）
    │       ├── hw_decompression.rs  # 硬件解压
    │       ├── direct_storage.rs    # 存储 → GPU 直通
    │       ├── crash_dump.rs
    │       ├── cpu_markers.rs       # PIX/Razor/Instruments 等 CPU 标记
    │       └── dylib.rs             # 动态库（仅桌面；editor 插件等）
    │
    ├── vmem.rs  thread.rs  fiber.rs  futex.rs  time.rs
    ├── file.rs  debug.rs   sysinfo.rs process.rs
    │                          # ↑ 公共 API：薄 #[inline(always)] 函数，转发到 <imp::Backend as Contract>
    ├── ext/                   # 公共 ext API，每个文件 #[cfg(marlin_ext = "...")]
    │   ├── mod.rs
    │   └── large_pages.rs …   #   get() -> Option<LargePages>（零大小 token）+ 方法
    ├── os/                    # 平台专属逃生口（类似 std::os）
    │   ├── windows.rs         #   原始 HANDLE 访问等
    │   ├── linux.rs
    │   └── macos.rs           #   主机的 os 模块随私有 backend 一起 #[path] 引入
    │
    ├── arch/                  # 与 OS 正交的 CPU 架构代码
    │   ├── x86_64.rs          #   fiber 上下文切换（naked fn）、cpuid、rdtsc
    │   └── aarch64.rs         #   上下文切换、cntvct
    │
    └── backend/
        ├── mod.rs             # 唯一一处 cfg 分发（含主机 #[path] 引入，见 §7）
        ├── windows/           # Win32；Xbox backend 可 `use crate::backend::windows::*` 复用
        ├── unix/              # Linux 与 macOS 共享的 POSIX 部分
        ├── linux/             # io_uring、futex、hugetlb
        └── macos/             # __ulock_wait、mach_vm、dispatch_io
```

契约与转发的形态：

```rust
// contract/vmem.rs
pub unsafe trait VMem {
    unsafe fn reserve(size: usize, hint: Option<NonNull<u8>>) -> Result<NonNull<u8>>;
    unsafe fn commit(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()>;
    unsafe fn decommit(ptr: NonNull<u8>, size: usize) -> Result<()>;
    unsafe fn release(ptr: NonNull<u8>, size: usize) -> Result<()>;
    unsafe fn protect(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()>;
}

// vmem.rs（公共 API）
#[inline(always)]
pub unsafe fn reserve(size: usize, hint: Option<NonNull<u8>>) -> Result<NonNull<u8>> {
    unsafe { <imp::Backend as VMem>::reserve(size, hint) }
}

// lib.rs：编译期断言 backend 实现了完整契约
const _: () = { const fn assert<P: contract::Platform>() {} assert::<imp::Backend>() };
```

ext 的形态（编译期"有没有这个 API"，运行期"能不能用"）：

```rust
// ext/large_pages.rs
#![cfg(marlin_ext = "large_pages")]
#[derive(Clone, Copy)]
pub struct LargePages(());          // 只能通过 get() 拿到，证明运行期可用

impl LargePages {
    pub fn get() -> Option<Self> { <imp::Backend as ext::LargePages>::available().then_some(Self(())) }
    pub fn size(self) -> usize { … }
    pub unsafe fn reserve_commit(self, size: usize) -> Result<NonNull<u8>> { … }
}
```

`marlin_ext` 的取值由平台清单 `platforms/<name>/platform.toml` 声明，xtask 转成全局 `--cfg`。
如果清单声明了某个能力但 backend 没实现对应 trait，转发函数会直接编译失败，所以清单和实现不可能不同步。

### 2.2 wrapper crate 如何暴露 ext

上层不能碰 pal，因此每个 wrapper 都有自己的 `ext` 模块，用同样的 cfg 把能力**安全地**重新暴露：

| 能力 | 暴露位置 |
|---|---|
| `large_pages` / `vmem_alias` / `physical_memory` | `marlin_mem::ext::*`（如 `LargePageArena`、`MirroredRing`） |
| `gpu_memory` | `marlin_mem::gpu`，主要给 `rhi` 的主机 backend 用 |
| `hybrid_cores` | `marlin_thread::ext::qos` |
| `hw_decompression` / `direct_storage` | `marlin_io::ext::*` |
| `crash_dump` / `cpu_markers` | `marlin_diag::ext::*` |

### 2.3 `mem/` 内部结构

```
runtime/base/mem/src/
├── lib.rs
├── vrange.rs        # Reservation RAII、commit 跟踪
├── page/            # 页级分配器、大页池
├── heap.rs          # 通用堆（mimalloc 式）：线程本地 heap、alloc/free 快慢路径、跨线程 free、线程退出 abandon/reclaim
├── segment.rs       #   4 MiB 对齐 segment 与 page、huge 分配、segment 缓存
├── layout.rs        #   segment 几何与 48 个 size class
├── lock.rs / list.rs#   futex 锁、侵入式链表（mem 不能依赖 sync，后者可能分配）
├── arena/           # 线性/帧/栈分配器
├── pool/            # 定长池
├── tag.rs           # 内存标签（Render/Asset/VM/…）
├── budget.rs        # 预算与统计，数据来自 pal::sysinfo
├── global.rs        # pub struct MarlinHeap; unsafe impl GlobalAlloc（apps/* 里注册）
├── alloc_api.rs     # core::alloc::Allocator 实现（nightly allocator_api）
├── gpu.rs           # #[cfg(marlin_ext = "gpu_memory")]
├── ext/
└── debug/           # guard page、泄漏追踪（feature = "mem-debug"）
```

### 2.4 `rt/` 与入口

`marlin-rt` **不定义**任何全局符号，只提供：

- `rt::start(main: fn() -> i32) -> i32`：`pal::init` → 堆 → diag → jobs → 回调，返回进程退出码。
- `rt::panic::report(&PanicInfo) -> !`：写日志、crash dump、abort。
- `rt::game_entry!(main)` 宏：展开为 `#[global_allocator]`、`#[panic_handler]` 和平台入口（`main` / `wWinMain` / 主机入口）。
- `rt::hosted::init()`：给 editor 用（std 已提供入口与 panic；这里装 panic hook，并使用同一个 `MarlinHeap`）。

---

## 3. `runtime/platform/` — L1：其他平台层

结构和 pal 一样：契约 trait + 公共 API + `ext/` + `os/` + `backend/`（主机通过 `#[path]` 引入）。

```
runtime/platform/
├── app/           marlin-app          应用生命周期（suspend/resume/constrained）、窗口/显示、Surface 句柄
├── input/         marlin-input        键鼠、手柄、触控；ext：adaptive_triggers、haptics、gyro…
├── audio_device/  marlin-audio-device 输出端点（WASAPI/PipeWire/CoreAudio/主机）；ext：3d_audio_hw、controller_speaker
├── net/           marlin-net          socket（Winsock/BSD/主机）、DNS
├── services/      marlin-services     用户/账号、成就/奖杯、存档、权限/DLC、活动卡（主机/Steam/桌面空实现）
└── rhi/           marlin-rhi          GPU 硬件抽象
    └── src/
        ├── contract/              # Device/Queue/CommandList/Buffer/Texture/Pipeline/Descriptor…
        ├── ext/                   # mesh_shader、ray_tracing、vrs、work_graphs、sampler_feedback…
        └── backend/
            ├── d3d12/             # Windows（Xbox 私有 backend 复用其中大部分）
            ├── vulkan/            # Linux（Windows 可选）
            └── metal/             # macOS
            # 主机：#[path] → platforms/{ps5,xbox,switch2}/rhi/
```

RHI 放在 L1 而不是 L3：它和 pal 一样含平台 backend，放在这里才能让"只有 L0/L1 有平台代码"这条规则成立。

---

## 4. `runtime/core/` — L2：平台无关的基础设施

```
runtime/core/
├── math/          向量/矩阵/四元数/TRS/几何（自研，portable_simd；右手 Y 向上、列向量、reverse-Z）
├── containers/    allocator-aware 容器：Vec 别名、HashMap（hashbrown）、SmallVec、SlotMap、Handle、侵入式链表
├── text/          Name（驻留字符串，类似 FName）、UTF-8 工具、格式化
├── hash/          稳定哈希（资源 ID）、快速哈希
├── jobs/          fiber job system：counter、优先级、main-thread 亲和、fiber-aware 同步原语
├── profile/       CPU/GPU 标记、计数器；backend 借助 marlin_diag::ext::cpu_markers
├── reflect/       类型反射（VM 绑定、序列化、editor 属性面板共用）
├── serial/        二进制序列化；cooked 数据的零拷贝布局（相对指针、可直接 mmap）
├── ecs/           archetype ECS：存储、查询、调度（运行在 jobs 上）
└── config/        cvar / 启动配置
```

---

## 5. `runtime/engine/` — L3：引擎子系统

```
runtime/engine/
├── asset/         资源句柄、包挂载/VFS、流式加载调度（建在 marlin_io 异步队列上）
├── world/         Level/流式 cell、Transform 层级、面向 VM 的对象化 Entity/Component API（底层 ECS）
├── shader/        运行时 shader 容器、permutation 查找
├── render_graph/  render graph：资源生命周期、barrier 推导、异步计算调度
├── render/        场景渲染：材质、光照、阴影、后处理、可见性
├── material/      材质类型/实例/动态实例/MPC/参数表
├── terrain/       地形 cell 运行时数据、RVT 合成、多层查询
├── font/          字形栅格化/SDF 图集、shaping
├── ui_render/     立即模式 UI 渲染：rect/圆角/线/图像/文字 → GPU tile binning → rhi
├── anim/          骨骼动画、混合、IK
├── physics/       物理（自研或 third_party 绑定）
├── audio/         混音、DSP、空间化（输出走 audio_device）
├── nav/           寻路
├── vfx/           粒子
└── framework/     游戏框架（marlin-framework，避免与 apps/game 的 marlin-game 重名）：主循环、帧流水线、子系统注册、启动时读取 boot 配置 → 加载 VM 模块与初始关卡
```

`runtime/marlin/`（package 名 `marlin`）是 umbrella：依赖并重导出 L0–L3，对外提供 `marlin::run(BootConfig)`。
`apps/game` 和 `editor/*` 都只依赖它（以及需要时的具体子 crate）。

**editor-only 数据**：runtime crate 用 cargo feature `editor` 打开 editor-only 字段与功能。
由于 resolver 2 会在一次构建中统一 feature，**game 与 editor 必须分开构建**（xtask 保证），否则 game 会把 editor feature 带进来。

---

## 6. `editor/`、`tools/`、`apps/`

### 6.1 `editor/`（std，仅桌面）

```
editor/
├── ui_core/       保留模式 UI 框架核心
│                  · 细粒度响应式 Signal / Memo / Effect 作为 MVVM 绑定机制
│                  · 保留的 widget 树、flexbox 布局（taffy）、hit-test、焦点、输入路由
│                  · 每帧把脏区域转成 ui_render 的立即模式绘制命令
├── ui_widgets/    控件：树、虚拟化列表/表格、属性面板（由 reflect 自动生成）、docking、节点图画布、代码编辑
├── ui_theme/      主题、图标、字体资源
├── core/          项目模型、undo/redo 事务、选择集、命令系统、源资源数据库（GUID/依赖）、editor 插件注册
├── viewport/      场景视口、gizmo、相机、PIE（进程内运行 world）
├── world_tools/   关卡编辑：放置、地形、流式 cell 编辑
└── asset_tools/   资源浏览器、导入（直接调用 tools/cook_* 的库）、热重新 cook
```

建议 `ui_core` 只用 `core + alloc` 编写。虽然 editor 允许 std，但这样以后游戏内 UI 也能复用同一套保留模式框架，成本几乎为零。

### 6.2 `tools/`（std）

```
tools/
├── cook_core/       cook 流水线：依赖图、增量缓存（DDC）、平台目标、打包成资源包
├── cook_texture/    BCn/ASTC 压缩；主机 swizzle 通过 #[path] 引入 platforms/<p>/cook/
├── cook_mesh/       网格优化、LOD、meshlet
├── cook_shader/     shader 编译（Marlin shader → DXIL/SPIR-V/MSL/主机格式）、permutation
├── cook_audio/
├── cook_level/      关卡/世界数据 → 流式 cell
└── cooker/          CLI：marlin-cook --platform ps5 --project samples/xxx
```

### 6.3 `apps/`

```
apps/
├── game/      #![no_std] #![no_main]；只做一件事：marlin_rt::game_entry!(game_main)，
│              main 读取包内 boot.toml → marlin::run()。所有游戏共用这一份源码，
│              发行时每个游戏/平台各构建一次（产物相同，只是打包的资源不同）。
└── editor/    std 的 fn main()；#[global_allocator] = MarlinHeap；marlin_rt::hosted::init() → editor 启动
```

---

## 7. `platforms/` 与主机私有代码

```
platforms/
├── README.md              # 如何获取私有 backend、需要的 SDK 环境变量
├── windows/platform.toml  # 已提交
├── linux/platform.toml    # 已提交
├── macos/platform.toml    # 已提交
├── ps5/                   # gitignore，私有仓库 marlin-platform-ps5 clone 到这里
├── xbox/                  # gitignore
└── switch2/               # gitignore
```

每个主机私有仓库的布局与公开 crate 一一对应：

```
platforms/ps5/
├── platform.toml          # target、ext 列表、SDK 路径、链接库
├── target/*.json          # 自定义 target spec（build-std 用）
├── pal/mod.rs             # → 被 runtime/base/pal/src/backend/mod.rs #[path] 引入
│   └── sys/               #   SDK FFI 绑定，经 pal::os::ps5::sys 给其他 L1 crate 复用
├── app/  input/  audio_device/  net/  services/  rhi/
├── cook/                  # cooker 的平台插件（纹理 swizzle 等）
└── package/               # 打包脚本/元数据，被 xtask 调用
```

接入方式：用 `#[path]` 引入模块，而不是 Cargo 依赖。

```rust
// runtime/base/pal/src/backend/mod.rs
#[cfg(marlin_platform = "windows")] mod windows;  #[cfg(marlin_platform = "windows")] pub use windows as imp;
#[cfg(marlin_platform = "linux")]   mod linux;    #[cfg(marlin_platform = "linux")]   pub use linux as imp;
#[cfg(marlin_platform = "ps5")]
#[path = "../../../../../platforms/ps5/pal/mod.rs"]
mod ps5;
#[cfg(marlin_platform = "ps5")] pub use ps5 as imp;
```

原因：

- Cargo 生成 lockfile 时会解析**所有**可选的 path/git 依赖，私有目录不存在就会报错；而被 cfg 掉的 `mod` 根本不会去读文件。
- 主机 backend 和公开 backend 处在同一个 crate 里，受同一份契约 trait 约束；Xbox 可以直接 `use crate::backend::windows::…` 复用 Win32 代码。
- 公开仓库里只有平台名称，不含任何 NDA 内容。

`marlin_platform` 是全局 cfg，它不等同于 `target_os`：Xbox 的 `target_os` 也是 `windows`。
它和 `marlin_ext`、`marlin_build` 都在 `[workspace.lints.rust] unexpected_cfgs` 中声明 check-cfg。

---

## 8. 构建配置与 xtask

| 配置 | cargo profile | `marlin_build` cfg | 说明 |
|---|---|---|---|
| Debug | `dev` | `debug` | 无优化，全部检查 |
| Development | `development`（opt-level 2） | `development` | 日常开发：assert、日志、cvar、调试工具 |
| Profile | `profile` | `profile` | 接近 shipping，保留 profiler 标记 |
| Shipping | `shipping`（fat LTO, codegen-units=1） | `shipping` | 去掉日志/断言/开发工具 |

所有 profile 都设 `panic = "abort"`。

```
xtask/src/
├── main.rs
├── platform.rs     # 读取 platforms/*/platform.toml
├── cfg.rs          # gen-cfg：生成 --cfg marlin_platform/marlin_ext/marlin_build 与 .cargo/config.toml 桌面段
├── build.rs        # build --platform <p> --config <c> --app game|editor（build-std、target JSON、target/<p>/<c>/）
├── cook.rs         # cook --platform <p> --project <path>
├── run.rs          # run / deploy 到 devkit
├── package.rs
└── check.rs        # check-layers、no_std 检查、所有可用平台的 cargo check 矩阵
```

---

## 9. 单个 runtime crate 的约定

```
runtime/<layer>/<name>/
├── Cargo.toml       # package = "marlin-<name>"；[lints] workspace = true
├── src/lib.rs       # #![cfg_attr(not(test), no_std)]  extern crate alloc;（pal/mem 除外）
├── tests/           # 桌面 host 上用 std 跑的集成测试
└── benches/         # 可选
```

- 命名：目录 `snake_case`，package `marlin-<name>`，代码中 `marlin_<name>`。
- 公共第三方依赖全部写在 `[workspace.dependencies]`，并带 `default-features = false`。

---

## 10. 待定问题（后续设计文档）

1. 自研 VM 的执行策略：纯解释器，还是 cook 期 AOT（字节码 → Rust/C → 静态链接进 game）。主机禁止 JIT，后者会影响"所有游戏一个可执行文件"的假设。
2. ~~fiber 实现方式~~ **已定**：统一用 `arch/` 里的手写上下文切换，共 4 种 ABI：x86_64 SysV/Win64、AArch64 AAPCS/Windows。Windows 的两个变体会同时切换 TEB 中的栈边界。
3. 物理引擎自研，还是绑定 Jolt（需要 C++ 运行时，主机 SDK 提供）。
4. ~~PAL 的 TLS~~ **已定**：PAL 提供带退出析构的 key（Windows FLS / POSIX `pthread_key`），热路径直接用 `#[thread_local]` static。
5. 资源包格式与 VFS 挂载优先级（补丁/DLC）。

---

## 11. 骨架现状（2026-10-15）

已实现：

- `marlin-pal`：契约 `VMem`、`Process`、`Debug`、`Time`、`Thread`（含亲和性与优先级）、`Futex`、`Tls`、`SysInfo`（目前只有 CPU 拓扑），以及 ext `LargePages`、`HybridCores`（仅 Windows）。Windows、Linux、macOS 都有实现（Linux/macOS 共享 `backend/unix`，futex 和调试器检测各自实现）。
- `marlin-mem::MarlinHeap`：mimalloc 式通用堆（见 §2.3 与 `MarlinHeap` 文档）。单线程 alloc+free 约 3.5 ns，同一测试下 Windows 系统分配器约 52 ns（release）。另提供 `stats()` 和 `collect_abandoned()`。
- `marlin-rt`：`start` / `game_entry!` / `panic::report` / `hosted::init`。
- xtask：`platforms`、`gen-cfg`、`build`、`check-layers`。

实现时确定的细节：

- 公共 vmem API 对 `size == 0` 一律返回 `InvalidInput`，因为 Windows `VirtualFree(MEM_DECOMMIT, 0)` 会把整个 reservation 一起 decommit。
- `gen-cfg` 同时写 `rustflags` 和 `rustdocflags`；否则 doctest 看不到 `marlin_platform`。
- 空壳 crate 暂不声明依赖：nightly cargo 的 `unused_dependencies` lint 会报警，等有代码时再加。层级规则由 `check-layers` 保证，不依赖这些边是否存在。
- `apps/game` 的 bin 设 `test = false`：no_std 二进制配上 std 测试 harness 会出现两个 panic handler。
- Linux 的 `commit` 受 overcommit 影响，物理内存在页面首次被访问时才真正分配。
- 线程入口不分配：调用方的结构体以 `#[repr(C)] ThreadStart` 作为第一个字段，新线程拿到指向它的指针后回调 `entry`。闭包式 API 由 `marlin-thread` 提供。
- `rt::panic::report` 经 `pal::debug::OutputBuffer` 输出 panic 信息；有调试器时先断下来，再走 `__fastfail` / `abort`。重入的 panic 直接 abort。
- 主机平台的 `#[path]` 模块都标了 `#[rustfmt::skip]`：rustfmt 不看 cfg，没有私有仓库时会找不到文件。
- `sysinfo::cpu_topology(out: &mut [LogicalCpu]) -> Result<usize>` 不分配：返回总数，最多写 `out.len()` 项（先传空 slice 取数量），按 id 排序。`LogicalCpu` 含 `id`、`core`（同一物理核的 SMT 兄弟共享，全系统唯一但不连续）和 `efficiency_class`（0 = 最省电；同构 CPU 全为 0）。
- `CpuSet` 是 1024 位的位图，每 64 位一个 word：在 Windows 上 word 下标就是 processor group，id = `group * 64 + number`；在 Linux 上与 `cpu_set_t` 布局一致，id 就是内核 CPU 编号。
- Windows 拓扑用 `GetLogicalProcessorInformationEx(RelationProcessorCore)`。结果是变长的，PAL 不能用堆，所以先查大小，再用 `vmem` 申请临时页读取。
- Windows 线程只能属于一个 processor group，所以 `set_current_affinity` 要求集合落在同一个 group 内，否则返回 `InvalidInput`；`current_affinity` 也只反映当前 group。
- Linux 拓扑来自 sysfs：`core` 取 `core_cpus_list`（旧内核为 `thread_siblings_list`）中最小的 CPU，因为 `core_id` 在每个 package 内会重新从 0 编号。Intel 混合架构用 `/sys/devices/cpu_atom/cpus` 区分 E 核；ARM 按 `cpu_capacity` 的不同取值排出等级。
- Linux 优先级通过对 tid 调 `setpriority` 设置 nice。降低 nice 值需要 `CAP_SYS_NICE` 或足够的 `RLIMIT_NICE`，否则返回 `PermissionDenied`，包括从 `Background` 改回 `Normal`。
- macOS 只提供各性能等级的数量（`hw.perflevelN.*`），无法知道每个 CPU 属于哪个等级。因此拓扑中的数量和各等级的 CPU 数是准确的，但 id 是合成的：从最省电的等级开始编号；Intel Mac 的 SMT 兄弟假定相邻。macOS 没有硬亲和性，亲和性 API 返回 `Unsupported`；优先级映射到 QoS class，QoS 本身决定 P/E 核（`Background` 只跑在 E 核上）。
- `hybrid_cores` ext 只在 Windows 实现（EcoQoS，`SetThreadInformation(ThreadPowerThrottling)`），运行期用无副作用的 `GetThreadInformation` 探测是否可用。macOS 不声明这个 ext：它的 QoS 已经由 `ThreadPriority` 使用，再暴露一次会让两个 API 互相覆盖。
- `vmem::reserve_aligned(size, align)`：Windows 用 `VirtualAlloc2` 的 `MEM_ADDRESS_REQUIREMENTS`（Windows 10 1803+），POSIX 多 reserve 一段再 unmap 头尾。分配器靠它让 segment 按 4 MiB 对齐。
- 堆的所有权模型：small/medium segment 归某个线程的 heap 所有，只有它能动 owner 侧字段。其他线程释放时 CAS 压入 page 的 `thread_free`，由 owner 稍后收集。page 进入 full 队列与外线程释放之间用 SeqCst 的 Dekker 配对防止丢唤醒：要么 owner 看到被释放的块，要么释放方看到 `in_full` 并留下 hint。
- 线程退出时，TLS 析构把 heap 的 segment 交给全局 abandoned 列表；其他线程需要新 segment 时最多接手 8 个。块全被外线程释放的 abandoned segment 只在这时，或调用 `collect_abandoned()` 时才释放。
- heap 结构体从 OS chunk 里切出、循环复用、从不 unmap，因为正在和 abandon 竞争的外线程可能还会往旧 heap 写 hint。
- 空 segment 进全局缓存（16 个），除 header 外全部 decommit；huge 分配因此天然拿到清零的内存。
- 对齐 > 16 的小分配：在 page 内多分配一些，返回块内对齐后的指针，并给该 page 打 `has_aligned` 标记，`free` 时回推到块起点。对齐 > 2 MiB 不支持，返回 null。
- mem 用了 nightly 的 `#[thread_local]`；没有原生 TLS 的主机平台需要换成 PAL TLS key 的慢路径。
- **Arena**（`marlin_mem::arena`）：`Arena` 先 reserve 一段地址空间，按 64 KiB 粒度随用随 commit。分配就是一次原子 bump（CAS），多线程并发分配无锁；只有 commit 走锁。reset 后已 commit 的内存保留，`trim` 才归还。`&Arena` 实现了 `Allocator`，`Vec::new_in(&arena)` 在自己是最后一块分配时原地增长或收缩。debug 构建会在 reset/rewind 时把释放的内存填成 0xCD。`Arena::scope` 提供栈式用法（用高阶生命周期保证分配不会逃出闭包）。`FrameArenas<N>` 是帧流水线用的环：`begin(frame)` 重置第 `frame % N` 个 arena，是 unsafe 的，由调用方（job handle）保证上一轮的读者已经结束。arena 里的值不会被 drop。
- 尚未实现：purge（延迟 decommit 空闲 page）、内存标签与预算、通用堆的 `Allocator` trait、定长 pool、mem-debug（guard page、泄漏追踪）、free list 指针编码（防止 heap 被利用）。
- **Fiber**（`pal::fiber`，经 `marlin_thread::fiber` 原样提供给上层）：`Stack` 底部有一个不可访问的 guard page，其余部分预先 commit；`make_context` 在栈顶手工构造初始帧，返回地址指向 trampoline，trampoline 再调用 `entry(arg)`；`switch` 把 callee-saved 寄存器和浮点控制字压栈，然后交换栈指针。Win64 还保存 xmm6–15，以及 TEB 的 StackBase / StackLimit / DeallocationStack，否则 `__chkstk` 和异常分发会出错（有 800 KiB 深递归的测试覆盖）。5 个目标都完整 build 过（汇编通过汇编器），但只有 x86_64 Windows 实际运行过。
- **TLS 与 fiber 迁移**：LLVM 可能在同一个函数里把 TLS 地址缓存下来，跨过不透明调用继续使用；fiber 在别的线程上恢复后，就会读到旧线程的值。因此所有可能在 fiber 上执行的 `#[thread_local]` 读取都包在 `#[inline(never)]` 访问函数里（mem 的 `thread_heap()`、jobs 的 `current_worker()`）。新写的代码要遵守同样的规则。
- **marlin-jobs**：每个 worker 线程在 fiber 上跑调度循环。job 在 counter 上等待时，fiber 被 park，worker 换一个空闲 fiber 继续；counter 归零后，被 park 的 fiber 进入 ready 队列，优先于新 job 执行。凡是"切走之后才能做"的动作（把 fiber 还回池子、登记为等待者）都放进 `Worker::pending`，由切换到的那个 fiber 去完成，避免另一个线程恢复一个还在运行的 fiber。
- 调度顺序：ready fiber → High 全局队列 → 本 worker 的本地队列（取最新的，深度优先）→ Normal 全局队列 → 从其他 worker 偷最旧的 → Low 全局队列。worker 内部 spawn 的 Normal job 进本地队列。深度优先把嵌套等待占用的 fiber 数限制在大约 worker 数 × 递归深度。只用全局 FIFO 时，4095 个嵌套等待就能耗尽 512 个 fiber。
- 空闲 worker 用 eventcount（`epoch` + `sleepers`，SeqCst 配对）睡在 Condvar 上，不会丢唤醒。外部线程等待 counter 时直接阻塞（不帮忙执行 job）。counter 由 `Arc` 共享，最后一次 decrement 发生在 scope 返回之后也是安全的。
- 性能（release，11 个 worker）：空 job（spawn + 执行）约 224 ns；4095 次嵌套等待的 fork-join 约 2.8 ms。
- **跨帧 handle**：`JobSystem::spawn` / `spawn_after(deps, priority, f)` / `batch(|b| …)` 生成 `'static` 的 detached job，返回可克隆的 `JobHandle`（`wait` 在 job 里会 park fiber，`is_complete`，`JobHandle::complete()`）。`spawn_after` 不占 fiber：continuation 挂在每个依赖的 counter 上，最后一个依赖完成时才把 job 推进队列（创建者自己也持有一份计数，所以登记依赖的过程中 job 不会提前启动）。依赖可以来自别的 JobSystem。`Drop for JobSystem` 会先等完所有 detached 工作，包括还在等依赖的 continuation。
- 尚未实现：无锁的 Chase-Lev 双端队列（现在是 Mutex<VecDeque>）、fiber 感知的锁（job 不要在等待时持有 `marlin_sync` 的锁）、fiber 池耗尽时的降级处理（现在直接 panic）、主线程作为 worker 参与执行、frame 级的 job 图与 profiler 标记。
- **约定**：测试一律写在各 crate 中与 `src/` 同级的 `tests/` 目录（集成测试），`src/` 里不放 `#[cfg(test)]` 模块。需要验证内部逻辑时，通过有实际用途的公共 API 来测，例如 mem 的 `good_size()`。
- **marlin-math**（自研，不用 glam）：右手系、Y 向上、相机朝 -Z；列向量（`M * v`），列主序存储；投影统一是 reverse-Z，深度范围 [0, 1]（`perspective_rh`、`perspective_infinite_rh`、`orthographic_rh`）。`Vec4`、`Quat`、`Mat4` 的列都基于 `core::simd::f32x4`（portable_simd，16 字节对齐，通过 `Deref` 用 `.x/.y/.z/.w` 访问字段）；`Vec2`/`Vec3` 是 `repr(C)` 标量（8/12 字节），方便直接作为 GPU 顶点数据。四元数乘法用 SIMD swizzle 实现；`Mat4` 求逆用 MESA 的余子式公式，另提供更快的 `inverse_affine`。
- **超越函数**：`core::f32::math` 只有 floor/ceil/round/trunc/fract/sqrt/mul_add 等，没有 sin/exp/ln，所以自研：在 f64 里做区间规约和多项式求值（sin/cos 用 FreeBSD k_sinf/k_cosf 的系数，atan/exp/ln 用级数，阶数按误差界选定）。实测和 std libm 相比：sin/cos/atan/asin/acos/exp/exp2/ln/log2/log10 ≤ 1 ulp，tan/pow ≤ 2 ulp。只用基本 IEEE 运算、不用 FMA，所以各平台结果逐位一致；三角函数在 |x| < 约 1.6e6 时保持精度。
- `core::simd` 没有向量 sqrt（它在 std 的 `std_float` 里），所以需要开方的地方都先归约成标量再用 `core::f32::math::sqrt`。
- 几何：`Aabb`（Arvo 变换）、`Sphere`、`Plane`、`Ray`（slab / 平面 / 球求交）、`Frustum`（按 reverse-Z 的 Gribb/Hartmann 提取平面；无限远投影的远平面退化为"总在内侧"）。
- **PC minimum CPU: x86-64-v3** (AVX2/FMA/BMI1/2, i.e. Intel Haswell 2013 / AMD Zen 2017 and later). Set in `platforms/*/platform.toml` with `target-cpu = { x86_64 = "x86-64-v3" }`; xtask passes it to both `.cargo/config.toml` and `xtask build` as `-Ctarget-cpu`. aarch64 is unaffected. In the Steam hardware survey for September 2026, about 96% of users support AVX2. Consoles (PS5/Xbox Series, Zen 2) all support it. Rust doesn't contract floating-point ops, so FMA isn't generated implicitly; the math library's cross-platform bit-for-bit consistency is unaffected. `marlin_rt::cpu::check()` checks at startup every instruction set the build enabled. The game returns exit code 1 when one is missing, and the editor prints a message and exits. This check is best-effort: it is itself compiled with AVX2. A guaranteed message would need a launcher compiled for baseline x86-64.
- **marlin-ecs**（archetype ECS）：
  - **存储**：component 类型集合相同的实体共用一个 `Archetype`，每个 component 一列，列是类型擦除的 `Column`（只记布局和 drop 函数，SoA）。增删 component 时把实体搬到对应的 archetype，原位置用 swap-remove 填补，并修正被换过来的那个实体的位置；add/remove 的迁移结果缓存成 archetype 之间的边。
  - **实体与资源**：实体用带世代号的 `Entity`，旧 id 不会和复用的槽位混淆。资源是按类型存的单例，放在 `UnsafeCell` 里，通过指针 cast 访问，不额外创建引用。
  - **Query**：`QueryData` 由 `&T`/`&mut T`/`Option<…>`/`Entity`/元组组合，过滤器是 `With`/`Without`。构造 query 时会检查自身是否有冲突，比如同时出现 `&mut A` 和 `&A`。`par_for_each` 把 archetype 的行切成批，交给 job 系统执行。
  - **System**：每个 system 显式声明读写的类型，`.reads::<T>()` / `.writes::<T>()` 对 component 和资源都适用。`Schedule` 把每个 system 放进"晚于所有与它冲突的已加入 system"的最早一个 wave，同一个 wave 内的 system 在 job 系统上并行；冲突的 system 保持加入顺序；exclusive system 拿到 `&mut World`，独占一个 wave。运行时，system 的每次 query、资源访问和 `get`/`get_mut` 都会和它的声明比对，越权就 panic，并行访问共享 World 的正确性就靠这个保证。
  - **Commands**：结构性修改都要经过 `Commands`。每个 system 有自己的缓冲，wave 结束后按 system 的加入顺序应用，结果是确定的，和完成顺序无关。
  - 注册 component 时可以附带反射信息（`register_reflected`），供 VM 和 editor 使用。
  - **Change detection**：每个 component 行旁边存 `added`/`changed` 两个 tick（`Column` 里的 `Vec<TickCell>`，随行一起 swap-remove 和迁移）。`&mut T` 查询产出 `Mut<T>`，只有 `DerefMut` 才记录写入（另有 `bypass_change_detection`、`set_if_neq`），`Ref<T>` 只读但带变化信息。过滤器 `Changed<T>`/`Added<T>` 是行级过滤（`QueryFilter::ROW_FILTER`），并声明对 `T` 的读；同一个 query 可以写它自己过滤的类型。每个 system 有自己的 `last_run`，看到的是"自己上次运行以来"的变化；直接通过 World 的查询以 `clear_trackers()` 为界（framework 每帧 extract 之后调用一次）。删除和 despawn 按类型记入 `World::removed`，保留两个周期。tick 是会回绕的 u32，每隔 `CHECK_TICK_THRESHOLD` 把过旧的 tick 钳到 `MAX_CHANGE_AGE`（与 Bevy 相同的方案）。exclusive system 用的是 world 的 tick，没有自己的 `last_run`。
  - **尚未实现**：query 缓存（目前每次 query 都要重新匹配所有 archetype）、bundle 信息缓存、Commands 里 spawn 时预留实体 id、函数式 system 参数（类似 Bevy 的 `fn(Query, Res)`）、性能基准。另外，system 内部的 job 一旦 panic 会直接 abort；越权访问的 panic 只有在 system 单独占一个 wave 时才能被测试捕获。
- **Registered same-layer dependencies** (§1.1 rule 3): `marlin-input → marlin-app` (L1: keyboard and mouse input arrives through the app's message pump); `marlin-rt → marlin-diag` (L0: the panic handler logs through the diag sinks).
- **L1 first slice**: the Win32 window and message pump (no_std), input (keyboard/mouse/raw input/XInput), and a D3D12 RHI (hand-written COM vtables, enhanced barriers with a legacy fallback, flip-model swapchain, frames in flight). `apps/demo_clear` animates a clear color. The Linux/macOS backends compile but return `Unsupported` at runtime.
- **Error convention** (`marlin-error`, `#[derive(Error)]` / `#[derive(ErrorKind)]`): OS/API errors are a `Copy` `{ kind, raw }`, and each subsystem has its own kind enum. Wrapper errors only describe their own layer and carry the cause through `source`. An error is logged once, where it is handled (`marlin_diag::chain`). Fatal errors go through `fatal!`/`or_fatal`, and the panic handler doesn't log them a second time. Sinks must not panic: the panic handler logs through the sinks, so a panic inside a sink can hang it.
- **RHI bindless slice**:
  - **Resources:** buffers in three memory locations (GPU-only, upload, readback; the latter two stay mapped) and 2D/array textures, including D32 depth and BC formats.
  - **Views:** RTV/DSV come from CPU-side heaps. Buffer and texture views (`ShaderView`) and samplers occupy slots in device-wide shader-visible tables, and shaders index them through SM 6.6 `ResourceDescriptorHeap[i]` / `SamplerDescriptorHeap[i]`.
  - **Pipelines:** all of them share one root signature (32 words of push constants plus directly indexed heaps). There are no input layouts; vertices are fetched from buffers (vertex pulling).
  - **Commands:** buffer barriers, copies (buffer↔buffer and buffer↔texture), draw / draw_indexed / dispatch, and render passes with depth.
  - **Baseline:** D3D12 rejects adapters below FL 12_0, SM 6.6, resource binding tier 3, mesh shader tier 1 or wave intrinsics (R0).
  - **Shaders:** the RHI accepts only bytecode, and `SHADER_FORMAT` tells engine code which format this build uses. The shader interface contract is in `docs/design/0002-shader-interface.md`. Until the in-house shader compiler exists, test and renderer shaders are **placeholders** (HLSL compiled once by hand with the SDK's dxc; only `.dxil` is committed, and nothing in the build calls dxc).
- **Testing convention**: tests come in two kinds, platform-independent ones and platform-dependent ones (`cfg(marlin_platform)`). GPU tests run on real hardware (`AdapterPreference::HighPerformance` with validation on), never on WARP or other software rasterizers. They read results back to the CPU and assert on pixels exactly, and validation must report 0 errors. The legacy barrier path is tested on the same hardware through `disable_enhanced_barriers`.
- **marlin-render (temporary minimal version)**:
  - **What it does:** extracts entities with `Transform` + `MeshRenderer` from the ECS and renders them from the first `Camera`. It is a reverse-Z forward pass with one directional light plus ambient, and instances of the same mesh are merged into one instanced draw.
  - **Why it exists:** to validate the whole RHI → shader interface → ECS chain end to end. Its tests render offscreen and assert on the pixels read back.
  - **What replaces it:** the real renderer will be rewritten on top of `render_graph`, as designed in docs 0008–0013: a cluster DAG with a visibility buffer, Substrate materials with open BSDFs, unified ReSTIR lighting, TSR, and a GPU tile-based UIRenderer. Its migration is stage R1 in 0008 §8.
  - **`apps/demo_cubes`** was removed in asset phase P6: `apps/game` now boots `samples/basic` (cooked and packaged, or streamed from `marlin-cookd`), whose level holds the same kind of scene.
