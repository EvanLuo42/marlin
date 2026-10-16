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
│   ├── engine/                #   L3  引擎子系统：render/asset/world/vm/physics…
│   └── marlin/                #   umbrella crate `marlin`：game 与 editor 共同链接的 runtime
│
├── editor/                    # editor-only 库（std 可用，仅桌面）
├── tools/                     # 离线工具（std）：cooker、vm 编译器、shader 编译器…
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
├── heap/            # 通用堆（mimalloc 式）：segment / page / size class / 线程本地 heap / 跨线程 free list
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
├── math/          SIMD 向量/矩阵/四元数、几何
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
├── vm_bytecode/   VM 字节码与模块格式（no_std；tools/vm_compiler 也依赖它）
├── vm/            VM 运行时：解释器、GC、模块加载、调试协议（主机禁止 JIT → 解释器 + 可选 cook 期 AOT）
├── script/        引擎 API 绑定到 VM（world/asset/input/audio…，基于 reflect 自动生成为主）
├── shader/        运行时 shader 容器、permutation 查找
├── render_graph/  render graph：资源生命周期、barrier 推导、异步计算调度
├── render/        场景渲染：材质、光照、阴影、后处理、可见性
├── font/          字形栅格化/SDF 图集、shaping
├── ui_render/     立即模式 UI 渲染：rect/圆角/线/图像/文字 → 批处理 → rhi
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
├── asset_tools/   资源浏览器、导入（直接调用 tools/cook_* 的库）、热重新 cook
└── script_tools/  VM 脚本集成：热重载、调试器（断点/单步）、错误面板
```

建议 `ui_core` 只用 `core + alloc` 编写。虽然 editor 允许 std，但这样以后游戏内 UI 也能复用同一套保留模式框架，成本几乎为零。

### 6.2 `tools/`（std）

```
tools/
├── cook_core/       cook 流水线：依赖图、增量缓存（DDC）、平台目标、打包成资源包
├── cook_texture/    BCn/ASTC 压缩；主机 swizzle 通过 #[path] 引入 platforms/<p>/cook/
├── cook_mesh/       网格优化、LOD、meshlet
├── cook_shader/     shader 编译（Slang/DXC → DXIL/SPIR-V/MSL/主机格式）、permutation
├── cook_audio/
├── cook_level/      关卡/世界数据 → 流式 cell
├── cook_script/     调用 vm_compiler，产出字节码模块
├── cooker/          CLI：marlin-cook --platform ps5 --project samples/xxx
├── vm_compiler/     VM 语言前端：lexer/parser/类型检查/codegen → vm_bytecode
└── vm_lsp/          VM 语言的 language server
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
2. fiber 在 Windows 上用 `CreateFiber` 还是统一用 `arch/` 里的手写上下文切换。
3. 物理引擎自研，还是绑定 Jolt（需要 C++ 运行时，主机 SDK 提供）。
4. PAL 的 TLS：用 `#[thread_local]`（依赖 target 支持），还是 PAL 显式 TLS slot API。
5. 资源包格式与 VFS 挂载优先级（补丁/DLC）。

---

## 11. 骨架现状（2026-10-15）

已实现：

- `marlin-pal`：契约 trait `VMem` + `Process`，以及 ext `LargePages`；Windows backend 完整实现，Linux/macOS 共享 `backend/unix`。
- `marlin-mem::MarlinHeap`：**占位实现**，每次分配都是一次独立的 OS reservation（Windows 上为 64 KiB 粒度），后续由 mimalloc 式堆替换。
- `marlin-rt`：`start` / `game_entry!` / `panic::report` / `hosted::init`。
- xtask：`platforms`、`gen-cfg`、`build`、`check-layers`。

实现时确定的细节：

- 公共 vmem API 对 `size == 0` 一律返回 `InvalidInput`，因为 Windows `VirtualFree(MEM_DECOMMIT, 0)` 会把整个 reservation 一起 decommit。
- `gen-cfg` 同时写 `rustflags` 和 `rustdocflags`；否则 doctest 看不到 `marlin_platform`。
- 空壳 crate 暂不声明依赖：nightly cargo 的 `unused_dependencies` lint 会报警，等有代码时再加。层级规则由 `check-layers` 保证，不依赖这些边是否存在。
- `apps/game` 的 bin 设 `test = false`：no_std 二进制配上 std 测试 harness 会出现两个 panic handler。
- Linux 的 `commit` 受 overcommit 影响，物理内存在页面首次被访问时才真正分配。
