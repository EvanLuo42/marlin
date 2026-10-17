# 0014 — 测试体系：分类、分层、目录与共享 helper

状态：草案 · 2026-10-17

前置：
- [0001](0001-repo-layout.md) §9 / §11 "约定"：测试只写在 `tests/`，随机数用 `marlin_math::Rng`，通用 helper 用 `marlin-test-support`

## 0. 决定摘要

| 问题 | 决定 | § |
|---|---|---|
| 测试分几类 | 两条轴：**能在哪跑**（平台无关 / OS 契约 / 平台专属 / 硬件）× **测什么**（14 种，见 §2） | 1, 2 |
| 平台门控 | 编译期一律用 `cfg(marlin_platform)` / `cfg(marlin_ext)`；不用 `cfg(windows)`、`target_os` | 3 |
| 怎么跑 | `cargo test` 照旧全跑；`cargo xtask test --tier <quick\|gpu\|desktop\|stress\|manual\|bench\|fuzz\|all>` 按层挑选 | 4 |
| 目录 | 每个 crate 按层分测试二进制：`tests/unit/`、`tests/gpu/`、`tests/golden/`、`tests/desktop/`、`tests/stress/`；必须独占进程的用 `tests/isolated_*.rs` | 5 |
| 共享 helper | `marlin-test-support`（std、零依赖，扩充）＋ 新的 `marlin-gpu-test-support`（依赖 marlin-rhi，只做 dev-dependency）＋ 自研 bench harness `marlin-bench` | 6 |
| 渲染回归 | golden image：离屏渲染回读，与 Git LFS 中的参考 PNG 按容差比较，`MARLIN_BLESS=1` 重写 | 7.1 |
| 性能 | `benches/*.rs` + `harness = false`，自研 harness，结果 JSON 写到 `target/`，xtask 与基线比较 | 7.2 |
| Fuzz | 两层：日常测试里用固定种子的 `Rng` 做有上限的随机/属性测试；长跑用 libFuzzer + ASan（cargo-fuzz 的目录布局，由 xtask 构建和运行），崩溃用例进 corpus 并转成回归测试 | 7.3 |
| 覆盖率 | `cargo xtask coverage`：xtask 直接调 llvm-tools（`-C instrument-coverage` → llvm-profdata → llvm-cov），统计 quick + gpu + desktop 层；按 crate 出行/区域覆盖率和 HTML 报告；不设绝对门槛，和本地基线比较、下降就提示 | 7.4 |
| 运行环境 | 暂时只在本机（Windows + NVIDIA）跑，不接 CI；层级设计要能直接映射到以后的 CI 矩阵 | 4 |

## 1. 现状（2026-10-17 盘点）

- 40 个 crate 有 `tests/`，共 223 个文件、约 4.3 万行、约 1200 个 `#[test]`。没有 `benches/`、`fuzz/`、CI、nextest 配置，xtask 也没有 test 命令。
- 约定（0001 §11）说"不要在测试里自己再写一套"，实际违反很多。典型重复：

| 模式 | 自己写了一份的地方 | 已有的共享实现 |
|---|---|---|
| 临时目录/文件 | io/fs、pal/fs、pal/watch 各一份完整 `TempDir`；diag/file_sink、serial/stream 的 `TempFile`；pal/cancel、pal/ext、rt/process、diag/crash_dump、io/file 的手写 `temp_dir().join(pid)`；cook_text 整个重写 | `marlin_test_support::TempDir`（没有 TempFile） |
| 轮询等待 | io/fs、pal/watch、thread/spawn、sync/event、input/windows、framework/end_to_end（同一文件既用 `wait_until` 又手写两次）、cookd/dev、cook_core/package | `wait_until` |
| 计时断言 `assert_waited` | sync 的 channel/event/reentrant/semaphore/timed 五份一模一样；condvar、pal/futex、net/blocking、pal/watch 内联 | 无 |
| 后台服务线程（停止标志 + 回传地址 + drop 时 join） | store/store、cookd/dev、framework/end_to_end、cooker/shared、net/common | 无 |
| 子进程重入（`current_exe --exact --ignored` + 环境变量） | diag/crash_dump、pal/ext、rt/process | 无 |
| drop 计数器 | containers/common、thread/scope、thread/spawn、sync/channel、ecs/world、ecs/dynamic、mem/pool、jobs/cancel、reflect、thread/local | 无（containers 有一份但只在本 crate） |
| panic 信息提取 | diag/assert、diag/report、rhi/support；ecs/non_send、cookd/dev、cook_core/cook 内联 | 无 |
| 随机数 | ecs/storage、text/message 手写 xorshift；hash/stream 抄 stable.rs 的生成器（不在冻结豁免内） | `marlin_math::Rng` |
| 浮点近似、向量运算 | input/hid `close`、rhi/raster `near`、cook_mesh/cook_texture/render 内联 `abs() < eps`；anim、render 的测试在数组上重写 normalize/dot/cross | `marlin_math` 的 `abs_diff_eq`、`Vec3` |
| 测试用 UUID | asset/reload、asset/server、cookd/dev、asset_db/index、cook_core/cook 各写一个 `uuid(n)` | 无 |
| `Project`（临时目录 + 写文件 + 建 AssetDb） | asset_db/common、cook_core/cook、cook_core/package、cook_mesh、cook_text、cook_texture | 无 |
| **GPU 设备** | render/forward、framework/end_to_end、cook_texture/gpu_conformance 各抄一份 `OnceLock<Device>`（注释都写着"见 rhi 的 support::devices"） | rhi `tests/support`，但别的 crate 用不到 |
| **GPU 提交并等待、回读** | render/forward、cook_texture/gpu_conformance、rhi 的 clears/device/device_lost/services | rhi `support::run`、`readback`、`read_words` |
| 性能计时 | compress/bench、cook_texture/bench、hash/simd、profile/stats 各写 `time`/`throughput`/`gbps` | 无 |

- 门控方式有 7 种并存：文件级 `#![cfg(marlin_platform)]`、`cfg(windows)`/`target_os`、`cfg(marlin_ext)`、cargo feature/`marlin_build`、`#[ignore = "…"]`、环境变量跳过、运行期 `if !features.x { return }`（静默通过，看不出被跳过）。
- 基准写成 `#[ignore]` 测试，各自在 `//!` 里写运行命令。
- 目录名不统一：helper 有的叫 `tests/common/`，rhi 叫 `tests/support/`；数据目录有 `fixtures/`、`data/`、`shaders/` 三种。

## 2. 测试分类

### 2.1 轴一：能在哪跑

| 级别 | 含义 | 门控 | 例子 |
|---|---|---|---|
| **P0 平台无关** | 纯逻辑，不碰 OS 和硬件 | 无 | math、containers、hash、serial、ecs 存储、cook 的纯算法、shader 编译器前端 |
| **P1 OS 契约** | 通过 PAL 等抽象用 OS 服务；**同一套测试**在每个实现了该 backend 的平台上都要过 | 只在某平台还没实现时用 `cfg(marlin_platform = …)` 排除 | pal 的 vmem/fs/watch/futex、thread、io、net 的 loopback、time |
| **P2 平台专属** | 只有某个平台才有的行为或扩展 | `cfg(marlin_platform = "x")` 或 `cfg(marlin_ext = "y")`，放进 `platform/<x>.rs` 模块 | Win64 fiber 的 TEB 栈边界、EcoQoS、XInput、crash dump |
| **P3 硬件** | 需要真实 GPU、显示器/桌面会话、手柄、音频设备 | 按 backend 所在平台 `cfg` ＋ 独立测试二进制（§5）；运行期缺特性用 `skip!`（§6.1） | RHI、渲染、GPU 一致性、present、窗口、输入 |

硬件测试一律在真实硬件上跑，不用 WARP、lavapipe 之类的软件设备。

### 2.2 轴二：测什么

| # | 种类 | 目的 | 规范 |
|---|---|---|---|
| 1 | **行为测试**（unit） | 公共 API 的正常行为 | 只经公共 API；快（单个 < 100 ms）；确定 |
| 2 | **契约测试**（contract） | 同一个 trait/抽象的所有实现行为一致 | 写成对实现泛型的函数，每个实现实例化一次：PAL 各 backend、RHI 各 backend、`Allocator` 的各实现、shader 的各 `Backend` |
| 3 | **模型/属性测试**（property） | 随机操作序列与参考模型（std 容器、朴素实现）对照 | `Rng` + 固定种子；失败时打印种子（§6.1 `seed()`）；迭代次数乘 `scale()` |
| 4 | **回归测试**（regression） | 修过的 bug 不再出现 | 修 bug 必须带测试；按行为命名，不叫 `regression_*`；上方注释写一句是什么 bug |
| 5 | **鲁棒性/损坏输入** | 任何输入只返回错误，不 panic、不 UB、不爆内存 | 有内存上限（`MemCap`）和时间上限；覆盖所有解析器、cooked 格式、网络帧、压缩流 |
| 6 | **参考向量/互操作** | 冻结的格式与算法输出不变 | 参考数据放 `tests/data/`；cooked 布局哈希、稳定哈希、压缩互操作、math 跨平台逐位一致 |
| 7 | **误用测试**（misuse） | 错误用法在到达 OS/GPU 之前就 panic 或返回错误 | 用 `panic_message` 断言信息内容，而不是裸 `#[should_panic]` |
| 8 | **确定性测试** | 同一输入多次、多线程运行结果逐位相同 | ECS Commands 应用顺序、jobs 结果、cook 产物重复 cook 字节一致、物理步进 |
| 9 | **GPU 一致性**（conformance） | GPU 结果与 CPU 参考实现一致 | 两条 barrier 路径都跑；例：BC 解码、计算着色器、采样 |
| 10 | **渲染 golden image** | 渲染输出不回退 | §7.1 |
| 11 | **端到端**（e2e） | 跨 crate 的完整流程 | cook → package → game 离屏启动；放在最上层用到它的 crate |
| 12 | **压力/并发**（stress） | 长时间、高并发下无死锁、泄漏、竞争 | 只在 `stress` 层跑；必须有超时（`with_timeout`）和内存上限 |
| 13 | **编译期测试** | 不该编译的代码确实编不过；层级、no_std 不被破坏 | derive 宏和 ECS API 误用写 doctest `compile_fail`；`xtask check-layers`、no_std 检查算这一类 |
| 14 | **性能基准**（bench） | 量化并防止性能回退 | §7.2；不再写成 `#[ignore]` 测试 |

另有 **手动测试**（manual）：需要人在场的（插手柄、看剪贴板）。保留 `#[ignore = "原因"]`，由 `--tier manual` 跑。

## 3. 门控规则

1. 平台用 `cfg(marlin_platform = "…")`，扩展用 `cfg(marlin_ext = "…")`，构建配置用 `cfg(marlin_build = "…")`。不再用 `cfg(windows)`、`target_os`（`asset_db/tests/journal.rs` 要改）。`target_arch` 只用于和架构本身有关的测试（fiber ABI、SIMD）。
2. P2 平台专属测试放进对应二进制的 `platform/<name>.rs`，在 `main.rs` 里 `#[cfg(marlin_platform = "<name>")] mod …;`，不要在文件里散落 cfg。
3. 运行期才知道的条件（GPU 特性、ext 运行期探测失败）用 `skip!("原因")`：打印 `SKIP <test>: 原因` 后返回。xtask 汇总跳过的数量，跳过不再无声无息。
4. `#[ignore]` 只给手动测试和"被子进程重入的那一半"，而且必须写原因。
5. 环境变量只用 `MARLIN_` 前缀，并在 §8 登记。

## 4. 分层运行：`cargo xtask test`

`cargo test --workspace` 保持能跑全部非 ignore 的测试。xtask 按测试二进制名挑选：

| tier | 包含 | 预期时长（本机） |
|---|---|---|
| `quick` | 所有 crate 的 `unit` 二进制 + doctest | < 1 分钟，日常改代码就跑 |
| `gpu` | `gpu`、`golden` 二进制和 `isolated_*` 里的 GPU 测试；带卡死看门狗 | 几分钟 |
| `desktop` | `desktop` 二进制（会弹窗口、要桌面会话） | |
| `stress` | `stress` 二进制，`MARLIN_TEST_SCALE` 默认放大到 10 | 可很长，必须有上限 |
| `manual` | 加 `--ignored`，只跑手动测试 | |
| `bench` | `benches/`，release，和基线比较 | |
| `fuzz` | `fuzz/` 的 libFuzzer 目标，每个目标限时、限内存 | 默认每个目标 60 秒 |
| `all` | quick + gpu + desktop + stress | 提交前 |

其他选项：`-p <crate>` 限定 crate；`--seed N` 设 `MARLIN_TEST_SEED`；`--bless` 设 `MARLIN_BLESS=1`。

GPU 层的看门狗：xtask 监控测试进程，超时没有输出就报告卡住的测试名并结束进程。驱动级死锁时进程可能杀不掉（2026-10-17 遇到过），这时提示需要重启，而不是继续跑下一个。

以后接 CI 时，层级直接对应 runner：无 GPU 的 runner 跑 `quick`，自托管 GPU 机器跑 `gpu`/`desktop`。

## 5. 目录结构

```
<crate>/
├── tests/
│   ├── support/mod.rs        # 本 crate 测试共用的 helper（统一叫 support，不再叫 common）
│   ├── data/                 # 所有测试数据：fixtures、参考向量、测试 shader
│   │   └── golden/<backend>/ # golden image 参考图（Git LFS）
│   ├── unit/                 # P0 + P1 (+P2)：行为、契约、属性、回归、损坏输入、误用、确定性
│   │   ├── main.rs           # //! 说明本二进制的层级和运行要求；mod 声明
│   │   ├── <feature>.rs      # 按功能分模块，和现在的文件名一致
│   │   └── platform/windows.rs
│   ├── gpu/                  # P3 GPU：共享一对 Device
│   ├── golden/               # 渲染 golden image
│   ├── desktop/              # 窗口、present、输入
│   ├── stress/               # 压力、长跑属性测试
│   └── isolated_<what>.rs    # 必须独占进程：crash dump 子进程等；GPU 的叫 isolated_gpu_<what>.rs，要桌面的叫 isolated_desktop_<what>.rs
├── benches/<name>.rs         # harness = false
└── src/
```

- 不是每个 crate 都要有全部目录，只建用得到的。
- xtask 只按测试目标名分层：`unit` 和其余 `isolated_*` → quick；`gpu`、`golden`、`isolated_gpu_*` → gpu；`desktop`、`isolated_desktop_*` → desktop；`stress` → stress。别的名字算未分类。
- 每个二进制在 `main.rs` 里用 `#[path = "../support/mod.rs"] mod support;` 引入本 crate 的 helper。
- 一个 crate 只剩一个测试文件时也放进 `tests/unit/`，保持统一。
- 二进制按层分，所以 GPU 测试天然只在 `gpu` 二进制里共享一个设备，不再有"每个文件各建一个 `OnceLock<Device>`"。
- 链接次数从 223 个二进制降到约 60 个，`cargo test` 明显变快。
- fuzz 目标集中在仓库根的 `fuzz/`（自成一个 workspace，`cargo test --workspace` 不会构建它），目标名 `<crate>_<what>`，种子 corpus 在 `fuzz/corpus/<target>/`。

## 6. 共享 helper

原则（沿用 0001）：引擎本身也会用的东西进 runtime（如 `marlin_math::Rng`、`abs_diff_eq`、`Vec3`），测试直接用；只给测试用的才进测试 crate。

### 6.1 `marlin-test-support`（std、零 workspace 依赖，任何层都能用）

已有：`TempDir`、`wait_until`、`git*`。新增：

| helper | 取代 |
|---|---|
| `TempFile` | diag/file_sink、serial/stream、io/common 的各一份 |
| `assert_waited(start, timeout)`、`assert_elapsed(min, max, f)`、`assert_duration_in` | sync 的 5 份 `assert_waited` 和各处内联计时断言 |
| `with_timeout(dur, f)`：在线程里跑，超时就带名字 panic | 各处 `is_finished` / `recv_timeout` 轮询 |
| `DropCounter` / `Tracked<T>` | 10 处 drop 计数器 |
| `panic_message(f)`、`assert_panics_with(text, f)` | 6 处 panic 信息提取 |
| `isolated::run(test, env)` / `isolated::is_child()` | 3 处子进程重入 |
| `BackgroundServer::spawn(|listener, stop| …)`：helper 先在回环临时端口上 bind 一个 std `TcpListener` 交给服务 | 用 std socket 的后台服务线程 |
| `BackgroundService::spawn(|ready, stop| …)`：服务自己 bind，`ready.listening(addr)` 报告地址 | 自己建监听的服务（cookd、store、cooker 的共享 store、framework 端到端） |
| `one_at_a_time()`：进程内互斥的 guard | 会激活窗口或注入输入的 desktop 测试轮流跑（共享前台窗口、焦点、光标裁剪；见 §9） |
| `MemCap<A: GlobalAlloc>`：带上限的计数全局分配器 | fuzz/鲁棒性/压力测试的内存上限（目前没有） |
| `seed()`：读 `MARLIN_TEST_SEED`，否则用固定默认值；失败时打印 | 各处写死的种子 |
| `scale()`：读 `MARLIN_TEST_SCALE` 放大迭代次数 | 属性测试的固定次数 |
| `skip!(reason)`（GPU 特性用 `marlin_gpu_test_support::require_feature!(device, feature)`） | 运行期静默 `return` |
| `test_uuid(n) -> u128`（调用方包成自己的 `Uuid`） | 5 处 `uuid(n)` |
| `golden::assert_bytes/text(path, actual)`：`MARLIN_BLESS=1` 时重写 | 将来的参考向量文件 |
| `data_path(env!("CARGO_MANIFEST_DIR"), rel)`：`<crate>/tests/data/rel` | 各处 `env!("CARGO_MANIFEST_DIR")` 拼路径 |
| `regress::for_each(manifest_dir, target, |bytes| …)`：回放 `tests/data/regress/<target>/*`，全部跑完再报出失败的输入 | fuzz 找到的崩溃用例（§7.3） |

### 6.2 `marlin-gpu-test-support`（新 crate，`tools/gpu_test_support`）

依赖 `marlin-rhi` 和只读写 PNG 的 `png` crate（不用 `image`：`png` 不和任何 runtime crate 共享依赖，不会经 feature 统一漏进 runtime 构建），只做 dev-dependency；xtask `check-layers` 本来就不管 dev-dependency。内容从 rhi 的 `tests/support` 搬过来，再补上其他 crate 自己抄的部分：

- `devices()` / `hardware_device()`：每个测试二进制共享、永不 drop 的硬件设备，开 validation，backend 的每条屏障路径一个（Vulkan 一个；D3D12 是 enhanced + legacy 两个，0016 §9）；测试遍历切片，不写死下标。**禁止在 `gpu` 二进制里自己建 `Device`**，需要独占设备的测试放 `isolated_*`。
- `run` / `run_on`：录制、提交、等待。
- `buffer` / `upload` / `readback` / `read_bytes` / `read_words` / `bytes_of`。
- `ColorTarget`、`ColumnTarget`、`compute_pipeline`、`solid_pipeline` 等常用目标和管线。
- `read_texture(device, texture) -> Image`：纹理回读（render、cook_texture、framework 都要）。
- 测试 shader 查找：先保留按名字 + stage 查 DXIL；Marlin shader 编译器落地后换成 build.rs 生成的查找表（见 placeholder DXIL 清单）。
- `require_feature!(device, ray_tracing)`：缺特性时 `skip!`。
- `assert_image!(name, &image)` / `assert_image!(name, &image, Tolerance { .. })`：§7.1。是宏，自动取调用 crate 的目录和包名；`Image::from_words` 从回读的 u32 构造。

### 6.3 crate 内的 `tests/support/`

只放这个 crate 特有、别处不会用的 helper，例如 cook_mesh 的 glTF 构造器、jobs 的 `system()` 和 `tree_sum`、diag 的日志 `Capture` 和 `setup()`。

几个 crate 共用、又依赖 workspace crate 的测试 helper，留在"拥有者" crate 的 `tests/support/` 里，别的 crate 用 `#[path]` 相对路径引入，不新建 crate、不进任何库：`Project`（临时目录 + 写文件 + 建 AssetDb）在 `tools/asset_db/tests/support/project.rs`，cook 相关的在 `tools/cook_core/tests/support/cook.rs`，cook_* 都引这两个文件。

## 7. 新的测试设施

### 7.1 渲染 golden image

- 场景离屏渲染到小分辨率（64×64～256×256）目标，回读成 RGBA8。
- 参考图：`tests/data/golden/<backend>/<name>.png`，用 Git LFS（`.gitattributes` 的 `**/tests/data/golden/** filter=lfs diff=lfs merge=lfs -text`；不带 `**/` 只匹配仓库根）。`<backend>` 是 `marlin_gpu_test_support::image::backend()`，即 `marlin_rhi::GRAPHICS_API.name()`（`vulkan`、`d3d12`）。
- 比较：每像素通道差 ≤ `per_channel`，且超差像素比例 ≤ `max_ratio`；默认 `per_channel = 2`、`max_ratio = 0`，个别测试可放宽。
- 失败时在 `target/golden/<crate>/<name>.{actual,diff}.png` 输出实际图和差异图，错误信息里给出路径（相对路径）。
- `MARLIN_BLESS=1` 或 `xtask test --tier gpu --bless` 只重写**比较失败**的参考图（已匹配的 LFS 文件不动）；更新参考图要单独提交，提交信息写明原因。
- 场景必须确定：渲染不依赖时间、随机数或线程完成顺序（framework 的首帧图成立，是因为这些帧里时间不走）。
- 现有：render 的 `golden` 二进制（正面受光的立方体、旋转立方体、地面上前后遮挡的几个立方体），framework 的 `gpu/end_to_end`（样例首帧）。
- 不同 GPU 厂商有差异时再按 `<backend>-<vendor>` 分目录，先不分。

### 7.2 性能基准：`marlin-bench`（`tools/bench`）

- `benches/<name>.rs`，`[[bench]] harness = false`，release profile。
- API：`Bench::new("name").throughput_bytes(n)`（或 `.throughput_items(n, "unit")`）`.metric("ratio", r).run(|| …)`，另有 `run_with_setup`、`run_custom`（自计时）；`samples` / `warmup` / `sample_time` / `max_time` 可调。文件里用 `marlin_bench::main!(a, b, …)` 生成 `main`。预热后自动定每个样本的调用次数，多轮采样，报告中位数、p10/p90、吞吐；每次调用过 `black_box`。计时用 `marlin_time::Instant`（因此 marlin-pal、marlin-time 自己不能用它测）。
- 命令行：位置参数是名字过滤；`--quick` 少量短样本；`--list` 只列名字。`cargo test --benches`（不带 `--bench`）时每个基准只跑一次当冒烟测试，不写结果。
- 结果写到 `target/bench/<crate>/<name>.json`。`xtask test --tier bench --save-baseline <tag>` 存到 `target/bench/baselines/<tag>/`，`--baseline <tag>` 比较，中位数变慢超过 `--threshold`（默认 5%）标出来，不改退出码；每个基准进程超过 `--stall`（默认 1800 秒）就结束。
- 基线只存在本地 `target/` 下（目前只有一台机器）。
- 已迁：compress（`codecs`）、ecs（`world`）、net（`loopback`）、asset_db（`index`）、cook_texture（`encode`）、mem（`global_heap`，文件里声明 `MarlinHeap` 为全局分配器）、hash（`simd`）、jobs（`jobs`，同样跑在 `MarlinHeap` 上）、masset（`extract`）。测试文件里不再有 `#[ignore]` 基准；profile/stats 的 `spin` 是 stats 测试用来造固定时长 zone 的，不是基准，留在原处。
- GPU 计时（timestamp query）以后加到 `marlin-gpu-test-support`。

### 7.3 Fuzz 与属性测试

- **日常层**：在 `unit` 二进制里，用 `Rng::new(seed())` 生成输入，迭代次数 `N * scale()`，全局分配器套 `MemCap`（测试二进制可以设全局分配器）。失败输出种子，用 `--seed` 复现。
- **长跑层**：根目录 `fuzz/`，libFuzzer 目标最终要覆盖所有解析器和反序列化入口（text、serial、cooked 格式、compress、net 帧、config/TOML、shader 前端、glTF/图片导入的我方处理部分）。
  - 现有 12 个：`asset_package`、`asset_unit`、`asset_wire`、`compress_gdeflate`、`compress_lz4_block`、`compress_lz4_frame`、`compress_zstd`、`serial_cooked`、`serial_read`、`text_locale`、`text_message`、`text_string_table`。2026-10-17 各跑 60 秒，没有发现问题。
  - `cargo xtask test --tier fuzz [--target <name>] [--time <secs>] [--seed <n>] [-- <libFuzzer flags>]`，每次运行限 2048 MB 内存、单个输入 10 秒。
  - 不用 cargo-fuzz 本身：它的 `RUSTFLAGS` 会替换掉 gen-cfg 的 `marlin_platform` 等 flag。xtask 用和 cargo-fuzz 相同的 flag 加上 gen-cfg 自己构建。Windows MSVC 上要额外处理：链接加 `/include:main`；用 vswhere 找到 MSVC 的 `clang_rt.asan_dynamic-x86_64.dll` 放进 PATH；`marlin_fuzz::init()` 装一个调 C 运行时 `abort()` 的 panic hook（Rust 自己的 abort 会在 libFuzzer 保存输入之前结束进程）；检出路径太深时 `cl.exe` 会因 260 字符限制报 C1083。
  - 新发现的 corpus 留在 `target/fuzz/corpus/`，还没有合回 `fuzz/corpus/` 的步骤。
- 找到的崩溃输入经最小化后复制到对应 crate 的 `tests/data/regress/<target>/`，xtask 打印要加的回放测试：在 `unit` 里用 `marlin_test_support::regress::for_each` 遍历这个目录。这样修复以后 `quick` 层就能防止回退。

### 7.4 覆盖率

- 工具：`rust-toolchain.toml` 加 `llvm-tools` 组件；不装第三方 cargo 插件。
- `cargo xtask coverage [--tier quick|gpu|desktop|all] [-p <crate>] [--html] [--save-baseline]`，默认 `all`（= quick + gpu + desktop，不含 stress、fuzz、bench、manual）。
- 过程：
  1. 用单独的 target 目录 `target/coverage/`，`CARGO_ENCODED_RUSTFLAGS` = 本机平台的 gen-cfg flags + `-Cinstrument-coverage`（环境变量会整体替换 `.cargo/config.toml` 的 rustflags，所以要把平台 flags 一起带上）。
  2. `LLVM_PROFILE_FILE=target/coverage/profraw/%p-%m.profraw` 跑选中层的测试二进制。
  3. `llvm-profdata merge -sparse` 合并，`llvm-cov export -summary-only` 取每个文件的统计，`--html` 时再 `llvm-cov show -format=html` 输出到 `target/coverage/html/`。
- 只统计 workspace 的 `src/`：排除 `tests/`、`benches/`、xtask、两个测试 support crate、标准库和第三方依赖。
- 报告：按 crate 列出行覆盖率和区域（region）覆盖率，按层（L0–L3、tools、editor）汇总。
- 防下降：`--save-baseline` 把每个 crate 的数字存到 `target/coverage/baseline.json`；之后每次运行和基线比较，某个 crate 的行覆盖率下降超过 0.5 个百分点就标出来（只提示，不让命令失败）。
- 平台专属和硬件代码只有在对应机器上跑才会被统计到；报告标明本次统计的平台和层。

## 8. 命名与写法

- 测试函数名是描述行为的句子：`damage_is_an_error_never_a_panic`、`waiter_wakes_after_timeout`。不加 `test_` 前缀。
- 模块文件按被测功能命名（`spawn.rs`、`query_state.rs`）；平台专属的放在 `platform/<name>.rs`，不以平台名命名功能模块（`input/tests/windows.rs` 要拆）。
- 每个二进制的 `main.rs` 用 `//!` 写明：层级、硬件要求、怎么单独运行。
- 断言带上下文：`assert_eq!(a, b, "frame {i}")`。浮点用 `abs_diff_eq` 或 ulp 比较，不手写 `abs() < eps`。
- 不写任何本机绝对路径；临时输出放 `env!("CARGO_TARGET_TMPDIR")` 或 `TempDir`。
- 长时间测试必须有时间和内存上限。

环境变量登记：

| 变量 | 作用 |
|---|---|
| `MARLIN_TEST_SEED` | 随机测试的种子 |
| `MARLIN_TEST_SCALE` | 迭代次数倍数（stress 层默认 10） |
| `MARLIN_BLESS` | 重写 golden 文件和图 |
| `MARLIN_UCD_DIR` | Unicode 一致性测试的数据目录 |
| `MARLIN_SKIP_LOG` | xtask 设置后，`skip!` 把跳过记录追加到这个文件，供汇总 |
| `MARLIN_ISOLATED_CHILD` | `isolated::run` 子进程标记（统一了原来的 3 个专用变量） |
| `MARLIN_TEST_DUMP_DIR` | diag、pal 的 crash dump 子进程把 dump 写到这里 |

## 9. 迁移步骤

1. ✅ 扩充 `marlin-test-support`（§6.1）；新建 `marlin-gpu-test-support`（§6.2），把 rhi 的 `tests/support` 和测试 shader 搬过去；render、framework、cook_texture 的 GPU 测试改用它（2026-10-17）。
2. ✅ `cargo xtask test`（§4），先支持 `quick`/`gpu`/`desktop`/`stress`/`manual`/`all`；`cargo xtask coverage`（§7.4）。还没迁到 §5 目录的扁平测试文件，xtask 归为"未分类"，只在 `all` 里跑并列出来，迁移完成后未分类应为零。
3. ✅ 按层（base / core / platform+engine / tools）去重：换成共享 helper，修正门控（§3）（2026-10-17，四个 session 并行）。
4. ✅ 按层把扁平的 `tests/*.rs` 重组成 §5 的二进制目录，统一 `support/`、`data/`（2026-10-17）。值得记下的特例：
   - 进程级全局状态迫使拆成 `isolated_*`：diag 每个日志测试文件一个 `isolated_log_*`（日志过滤器全局；致命报告按线程 id 存，Windows 会复用线程 id）；config（全局 cvar 注册表）和 profile（全局 profiler）各有几个；mem 的 `isolated_global_heap` / `_purge` / `_debug` / `_debug_global`。
   - jobs、text 在 `main.rs` 里统一声明一次 `#[global_allocator]`。
   - 只有 clipboard（会覆盖用户剪贴板）和 `connected_gamepads`（要插手柄）还是手动测试；其余 12 个"需要桌面会话"的测试进了 desktop 层。
   - 顺带修掉一个真 bug：config 的注册测试按大小写敏感过滤 cvar，链接顺序不同时会失败。
5. ✅ `marlin-bench` 和 `--tier bench`；compress、ecs、net、asset_db、cook_texture 的基准已迁到 `benches/`；mem、hash、jobs、masset 嵌在测试文件里的 `#[ignore]` 基准随后也迁了（2026-10-17）。
6. ✅ Git LFS + golden image：render 3 张、framework 1 张（2026-10-17）。
7. ✅ `fuzz/` 和 `--tier fuzz`，先覆盖 text、serial、compress、cooked 格式（2026-10-17，12 个目标）。
8. ✅ 0001 §11 的"约定"一条改成指向本文档。

迁移后留下的后续：
- diag 里几个有界的自旋等待可以换成 `wait_until`。
- pal 的 `windows_command_line_quoting` 需要精确的原始命令行，`isolated::run` 传不了，仍自己重入。
- ✅ app 窗口测试和 input 注入测试的并行失败已查清（2026-10-17）。修了三个真 bug，各有一个去掉修复就失败的测试：
  - 进全屏的 `SetWindowPos` 没带 `SWP_NOACTIVATE`，隐藏窗口会被激活、拿走焦点和会话前台。
  - 退全屏时 `SetWindowPlacement` 重放 `SW_SHOWNORMAL`，会把隐藏窗口显示出来。
  - 原始输入的注册是整个进程共用的，而 `Input::drop` 会无条件 `RIDEV_REMOVE`，丢一个 `Input` 会关掉别的 `Input` 的原始输入。现在按引用计数，最后一个才注销。

  剩下的确实是会话状态：另一个线程同时改前台窗口时，Windows 会跳过正在显示的窗口的 `WM_WINDOWPOSCHANGED`（`WH_CALLWNDPROC` 钩子也收不到）。`set_visible` 之后现在会按 `IsWindowVisible` 对账，但最小化和还原的事件仍会丢。注入的触摸和按键发给屏幕和前台窗口。所以 `one_at_a_time()` 只留在会激活窗口的 3 个 app 测试和 2 个注入测试上，每处都写了原因。
- 每个 fuzz 目标在所属 crate 的 `unit` 里加一个 `regress::for_each` 回放测试（text 3 个、serial 2 个、compress 4 个、asset 3 个；serial 的要把 `fuzz/src/lib.rs` 里的测试类型也复制过去），这样 `tests/data/regress/` 一有用例就会被跑到。
- 读代码发现、fuzz 还没打到的两个疑似问题：`PackageIndex::check` 里 `c.unit_offset + c.raw_size as u64` 可能溢出；`Package::finish` 按 `record.size` 分配，没有上限，一个很小的恶意包就能要求分配巨量内存。
- `asset_package`、`asset_unit` 基本过不了包的校验和：加一个 `cfg(fuzzing)` 下跳过校验和的开关，或写一个会重算校验和的目标，才能测得更深。
- 补齐 §7.3 剩下的目标：net 帧、config/TOML、shader 前端、glTF/图片导入；再加一步把 `target/fuzz/corpus/` 的新发现合回 `fuzz/corpus/`。

## 10. 待定

1. ~~cook 系列共用的 `Project` helper 放哪~~ **已定**：留在 asset_db / cook_core 的 `tests/support/`，用 `#[path]` 共享（§6.3）。
2. ~~cargo-fuzz 在 Windows MSVC 上是否可用~~ **已定**：libFuzzer + `-Zsanitizer=address` 在 x86_64-pc-windows-msvc 上可用，由 xtask 驱动（§7.3）。
3. 基线是否入库：多台机器以后，按机器名存在仓库里还是只放本地。
