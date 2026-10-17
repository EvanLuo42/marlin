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
| Fuzz | 两层：日常测试里用固定种子的 `Rng` 做有上限的随机/属性测试；长跑用 cargo-fuzz，崩溃用例进 corpus 并转成回归测试 | 7.3 |
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
| `fuzz` | cargo-fuzz 目标，每个目标限时 | |
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
- cargo-fuzz 目标集中在仓库根的 `fuzz/`（不进 workspace），目标名 `<crate>_<what>`，corpus 在 `fuzz/corpus/<target>/`。

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
| `BackgroundServer::spawn(|listener, stop| …)` | 5 处后台服务线程 |
| `MemCap<A: GlobalAlloc>`：带上限的计数全局分配器 | fuzz/鲁棒性/压力测试的内存上限（目前没有） |
| `seed()`：读 `MARLIN_TEST_SEED`，否则用固定默认值；失败时打印 | 各处写死的种子 |
| `scale()`：读 `MARLIN_TEST_SCALE` 放大迭代次数 | 属性测试的固定次数 |
| `skip!(reason)`（GPU 特性用 `marlin_gpu_test_support::require_feature!(device, feature)`） | 运行期静默 `return` |
| `test_uuid(n) -> u128`（调用方包成自己的 `Uuid`） | 5 处 `uuid(n)` |
| `golden::assert_bytes/text(path, actual)`：`MARLIN_BLESS=1` 时重写 | 将来的参考向量文件 |
| `data_path(env!("CARGO_MANIFEST_DIR"), rel)`：`<crate>/tests/data/rel` | 各处 `env!("CARGO_MANIFEST_DIR")` 拼路径 |

### 6.2 `marlin-gpu-test-support`（新 crate，`tools/gpu_test_support`）

依赖 `marlin-rhi`（以后加 PNG 读写），只做 dev-dependency；xtask `check-layers` 本来就不管 dev-dependency。内容从 rhi 的 `tests/support` 搬过来，再补上其他 crate 自己抄的部分：

- `devices()` / `device()`：每个测试二进制一对（enhanced + legacy barrier）共享、永不 drop 的硬件设备，开 validation。**禁止在 `gpu` 二进制里自己建 `Device`**，需要独占设备的测试放 `isolated_*`。
- `run` / `run_on`：录制、提交、等待。
- `buffer` / `upload` / `readback` / `read_bytes` / `read_words` / `bytes_of`。
- `ColorTarget`、`ColumnTarget`、`compute_pipeline`、`solid_pipeline` 等常用目标和管线。
- `read_texture(device, texture) -> Image`：纹理回读（render、cook_texture、framework 都要）。
- 测试 shader 查找：先保留按名字 + stage 查 DXIL；Marlin shader 编译器落地后换成 build.rs 生成的查找表（见 placeholder DXIL 清单）。
- `require_feature!(device, ray_tracing)`：缺特性时 `skip!`。
- `assert_image(name, &image, Tolerance)`：§7.1。

### 6.3 crate 内的 `tests/support/`

只放这个 crate 特有、别处不会用的 helper，例如 asset_db 的 `Project`、cook_mesh 的 glTF 构造器、jobs 的 `system()`、diag 的日志 `Capture`。cook 系列共用的 `Project` 收到 `marlin-cook-core` 的测试 helper 里（或 asset_db 暴露一个测试用构造函数，待定）。

## 7. 新的测试设施

### 7.1 渲染 golden image

- 场景离屏渲染到小分辨率（64×64～256×256）目标，回读成 RGBA8。
- 参考图：`tests/data/golden/<backend>/<name>.png`，用 Git LFS（`.gitattributes` 加 `tests/data/golden/** filter=lfs`）。
- 比较：每像素通道差 ≤ `per_channel`，且超差像素比例 ≤ `max_ratio`；默认 `per_channel = 2`、`max_ratio = 0`，个别测试可放宽。
- 失败时在 `target/golden/<crate>/<name>.{actual,diff}.png` 输出实际图和差异图，错误信息里给出路径（相对路径）。
- `MARLIN_BLESS=1` 或 `xtask test --tier gpu --bless` 重写参考图；更新参考图要单独提交，提交信息写明原因。
- 不同 GPU 厂商有差异时再按 `<backend>-<vendor>` 分目录，先不分。

### 7.2 性能基准：`marlin-bench`（`tools/bench`）

- `benches/<name>.rs`，`[[bench]] harness = false`，release profile。
- API：`Bench::new("name").throughput(bytes).run(|| …)`；预热，然后多轮采样，报告中位数、p10/p90、吞吐。计时用 `marlin-time`。
- 结果写到 `target/bench/<crate>/<name>.json`。`xtask test --tier bench --save-baseline <tag>` 存基线，`--baseline <tag>` 比较，中位数变慢超过阈值（默认 5%）标红。
- 基线只存在本地 `target/` 下（目前只有一台机器）。
- 现在的 `#[ignore]` 基准（compress、ecs、net、asset_db、cook_texture、mem/global_heap、hash/simd、jobs、masset）全部迁过来，删掉各自的计时函数。
- GPU 计时（timestamp query）以后加到 `marlin-gpu-test-support`。

### 7.3 Fuzz 与属性测试

- **日常层**：在 `unit` 二进制里，用 `Rng::new(seed())` 生成输入，迭代次数 `N * scale()`，全局分配器套 `MemCap`（测试二进制可以设全局分配器）。失败输出种子，用 `--seed` 复现。
- **长跑层**：根目录 `fuzz/`，cargo-fuzz 目标覆盖所有解析器和反序列化入口（text、serial、cooked 格式、compress、net 帧、config/TOML、shader 前端、glTF/图片导入的我方处理部分）。`xtask test --tier fuzz --time 60` 每个目标限时。
- 找到的崩溃输入复制到对应 crate 的 `tests/data/regress/`，再加一个 `unit` 测试遍历这个目录。这样修复以后 `quick` 层就能防止回退。

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
| `MARLIN_ISOLATED_CHILD` | `isolated::run` 子进程标记（统一现在的 3 个专用变量） |

## 9. 迁移步骤

1. ✅ 扩充 `marlin-test-support`（§6.1）；新建 `marlin-gpu-test-support`（§6.2），把 rhi 的 `tests/support` 和测试 shader 搬过去；render、framework、cook_texture 的 GPU 测试改用它（2026-10-17）。
2. ✅ `cargo xtask test`（§4），先支持 `quick`/`gpu`/`desktop`/`stress`/`manual`/`all`；`cargo xtask coverage`（§7.4）。还没迁到 §5 目录的扁平测试文件，xtask 归为"未分类"，只在 `all` 里跑并列出来，迁移完成后未分类应为零。
3. 按层（base / core / platform+engine / tools）去重：换成共享 helper，修正门控（§3）。这一步可以几个 session 并行，各管一层。
4. 按层把扁平的 `tests/*.rs` 重组成 §5 的二进制目录，统一 `support/`、`data/`。样板：marlin-sync（全部进 `unit`）和 marlin-rhi（`unit`、`gpu`，以及 `isolated_gpu_device` / `_device_lost` / `_validation` / `_misuse`、`isolated_desktop_present`）已经迁完。注意：几个文件各自声明 `#[global_allocator]` 时（jobs）不能合进同一个二进制，要么统一成一个，要么拆成 `isolated_*`。
5. `marlin-bench` 和 `--tier bench`，迁移现有基准。
6. Git LFS + golden image，先给 render/forward 和 framework 的端到端各加几张。
7. `fuzz/` 和 `--tier fuzz`，先覆盖 text、serial、compress、cooked 格式。
8. ✅ 0001 §11 的"约定"一条改成指向本文档。

## 10. 待定

1. cook 系列共用的 `Project` helper 放哪：cook_core 暴露一个测试用 API，还是新的测试 crate。
2. cargo-fuzz 在 Windows MSVC 上依赖 ASan 支持，可用性要先验证；不行就先在 Linux 上跑长跑层，或自研一个无覆盖引导的随机输入循环。
3. 基线是否入库：多台机器以后，按机器名存在仓库里还是只放本地。
