# ONNX Runtime 全局线程池方案

日期：2026-10-04
状态：待评审（实现前需确认第 6 节的待定事项）

## 1. 背景与问题

目前每个 ORT session 都会创建自己的 intra-op 线程池。线程数取 ORT 的默认值（物理核数），并且默认开启自旋。原生代码从未调用 `ort::init()`，所有 session 都使用 ORT 的默认环境。

## 2. 现状数据（gpuhub，2026-10-04 采集）

| 项目 | 数值 |
| --- | --- |
| CPU | 2 × Xeon Platinum 8352V，容器可见 128 个逻辑核（64 个物理核） |
| cgroup CPU 配额 | `cpu.max = 1500000 100000`，即 **15 核** |
| 内存上限 | 约 62 GiB |
| GPU | RTX 4080 SUPER 32 GiB，驱动 595.71.05，CUDA 13.2 |
| `available_parallelism()` | 受 cgroup 配额约束，为 15（CPU 池实际使用 14 个线程） |

正在运行的 `docparse-server`（production.toml，共约 38 个 ORT session）：

| 线程名 | 数量 | 来源 |
| --- | --- | --- |
| `docparse-sessio*` | 1028 | SessionManager 会话线程及其继承名称的 ORT 计算线程 |
| `docparse-onnx` | 1016 | Texo 共 16 个 session × 每个 63 个 ORT 计算线程 + 拥有线程 |
| 其他 | 44 | tokio、PDFium、日志等 |
| **合计** | **2088** | |

结论：

1. ORT 按物理核数（64）创建计算线程，忽略了 cgroup 配额（15）。线程数超出配额约 136 倍。
2. 从 Sep 16 起累计：`nr_throttled = 87998`，`throttled_usec ≈ 2.0e11`（约 56 小时被节流）。这个数字包含所有历史负载，只能说明节流确实存在；它与线程超额之间的因果关系要由第 4 节的实验确认。
3. 历史基准 `baseline-main-c1-a`（并发 1，11 个文档）：CPU 峰值 765%，没有打满 15 核的配额。所以并发 1 下的收益可能有限，并发 2 和 4 才是重点观察对象。
4. 主要推理负载跑在 CUDA 上。ORT 的 CPU 线程池只用于回退到 CPU 的算子和少量宿主侧工作。全局线程池在 GPU 部署下的主要收益，预计是减少线程数、上下文切换和自旋带来的 CPU 消耗，而不是推理本身变快。这个判断需要数据确认。

## 3. 设计

### 3.1 配置（默认全自动，允许手动覆盖）

在 `[runtime]` 下新增：

```toml
[runtime]
# ORT 计算线程池：global（所有 session 共享一个池）或 session（每个 session 一个池，即旧行为）。
onnx_thread_pool = "global"
# 0 表示自动：取 std::thread::available_parallelism()，遵守 cgroup 配额。
onnx_intra_threads = 0
# 关闭自旋；空闲时不消耗 CPU。
onnx_spinning = false
```

- 三项都有默认值，现有配置文件不用修改。
- 保留 `session` 模式，有两个用途：一是作为实验里的对照组，二是在线上作为应急回退开关。这两种模式只能通过配置切换，因此所有实验变体可以使用**同一个二进制**，变体之间只差配置。
- `onnx_intra_threads = 0` 时的自动取值由实验数据决定，候选值见第 4 节。初始实现先取 `available_parallelism()`。

### 3.2 初始化

- 在 `docparse-layout` 的 `OnnxBackend` 上增加线程配置字段，从 `ValidatedConfig` 中读取。
- `cpu_builder()` 被调用前，先执行一次进程级初始化（`OnceLock` 包裹）：

  ```rust
  ort::init()
      .with_global_thread_pool(
          GlobalThreadPoolOptions::default()
              .with_intra_threads(threads)?
              .with_inter_threads(1)?
              .with_spin_control(spinning)?,
      )
      .commit()
  ```

- 这样 layout、OCR、TSR 和 Texo 都经过 `SessionBuilder::try_from(OnnxBackend)`，同一个入口就能覆盖所有原生模型，不用改每个模型的加载代码。
- `commit()` 返回 `false`，表示环境已经被设置过（同一进程里第一次设置生效）。这种情况下，如果参数和第一次不一致，就记录一条 `tracing::warn!`；一致则不记录。
- 环境生效后，ort 会自动给每个 session 调用 `DisablePerSessionThreads`，不需要再对每个 session 单独设置。
- `session` 模式下不启用全局池，改为对每个 session 调用 `with_intra_threads(threads)` 和 `with_intra_op_spinning(spinning)`。
- 启动时记录一条 `INFO` 日志，内容包括模式、线程数，以及线程数取值的来源（自动或手动）。
- 新增 gauge `docparse_onnx_intra_threads{mode}`，便于在监控中确认实际生效的配置。
- wasm 构建不受影响，`crates/web` 维持现状。

### 3.3 测试

- 单元测试：自动线程数的解析（0 映射到 `available_parallelism`，手动值原样使用），以及配置校验（拒绝非法枚举值）。
- 被 ignore 的集成测试（需要模型）：在 global 模式下加载两个 layout session，验证进程线程数（`/proc/self/task`）不会随 session 数线性增长。
- 现有的推理测试和 HTTP 端到端测试必须全部通过。

## 4. 实验方案（数据支撑）

### 4.1 环境

- 机器：gpuhub（15 核配额，RTX 4080 SUPER）。
- 复用现有的 `bench.py`、`run-http-scenario.py` 和 `validity.py`。这套工具已经会记录进程 CPU、RSS、GPU 利用率和 `cpu.stat`，并且会校验输出没有变化。
- 新建独立的实验目录，使用独立端口和数据库，**不干扰**目前正在运行的 `main-port-20261004-133823` 实验。
- 所有变体使用同一个 commit 构建出的同一个二进制，配置文件除第 4.2 节列出的三项外完全相同。

### 4.2 变体矩阵

| 变体 | onnx_thread_pool | onnx_intra_threads | onnx_spinning | 目的 |
| --- | --- | --- | --- | --- |
| V0 | session | 0（物理核数，复现现状） | true | 基线 |
| V1 | session | 1 | false | 区分"减少线程"和"共享线程池"各自的贡献 |
| V2 | global | 15（自动） | false | 推荐默认候选 |
| V3 | global | 15 | true | 判断是否应开启自旋 |
| V4 | global | 8 | false | 判断全局池大小是否应低于配额（给 CPU 池留余量） |

V0 在 `session` 模式下需要还原 ORT 的默认值（线程数为物理核数），实现上 `session` 模式的 0 直接交给 ORT 处理。

### 4.3 负载

- 语料：`corpus-healthy8.json`（8 个文档）和 `corpus-production.json`。
- 并发：1、2、4，每档先预热 1 轮，再正式跑 2 轮（a、b）。
- 每个变体跑完都重启服务，避免缓存和显存状态影响下一个变体。

### 4.4 指标

| 类别 | 指标 |
| --- | --- |
| 吞吐 | docs/s、pages/s |
| 延迟 | p50、p95（从上传到取得完整结果） |
| CPU | 进程 CPU 平均值和峰值，`cpu.stat` 中 `nr_throttled`、`throttled_usec` 在压测期间的增量 |
| 线程 | 稳态时的进程线程数 |
| 内存与 GPU | RSS 峰值、GPU 利用率平均值、显存峰值 |
| 正确性 | `validity.py` 的输出哈希一致，模型的错误数和取消数都为 0 |

### 4.5 决策标准

把 V2 设为默认值，需要同时满足以下条件：

1. 输出与 V0 一致（validity 通过），没有新增模型错误。
2. 每一档并发下，吞吐都不低于 V0 的 97%。
3. 并发 2 和 4 中至少有一档满足以下任意一条：吞吐提升 ≥ 5%；或节流时间减少 ≥ 50% 且 p95 不变差超过 5%。
4. 稳态线程数少于 200。

另外两项取舍：

- 自旋：V3 只有在吞吐比 V2 提升 ≥ 3%，并且空闲 CPU 没有明显升高时，才改为默认开启。
- 自动线程数：如果 V4 在吞吐上不劣于 V2（差距在 3% 以内），并且 CPU 池的节流更少，那么自动值改为 `max(1, available_parallelism / 2)`，否则保持 `available_parallelism`。

如果 V1 明显优于 V2（吞吐高 5% 以上），说明共享线程池在多个调用方同时运行时有排队代价。这种情况下暂停推进，把数据带回来重新讨论。

### 4.6 产出

`docs/superpowers/reports/2026-10-xx-onnx-thread-pool-benchmark.md`（中文），内容包括原始数据路径、汇总表和最终默认值的结论。

## 5. 实施步骤

1. 配置：在 `RuntimeConfig` 中新增三个字段，补充校验，在 `docparse.toml` 中补充说明。
2. 初始化：`OnnxBackend` 携带线程配置，实现进程级 `OnceLock` 初始化，以及 `session` 模式下逐个 session 的设置；记录日志和 gauge。
3. 测试：第 3.3 节列出的单元测试和被 ignore 的集成测试。
4. 在 gpuhub 构建同一个二进制，按第 4 节跑完 V0–V4。
5. 根据第 4.5 节的标准确定默认值，更新代码默认值和 `docparse.toml`，写出实验报告。

## 6. 待确认事项

1. 在 gpuhub 上跑实验时，是等目前的 `main-port-20261004-133823` 实验结束，还是可以并行跑？并行时两个服务会抢同一份 15 核配额，数据不可用，所以建议等它结束。
2. 生产部署是否也运行在有 cgroup 配额的容器里？如果部署机器的核数和配额与 gpuhub 不同，自动取值的结论需要在那台机器上抽查确认。
3. 当前的 CPU 池（N-1 线程）与 ORT 全局池（N 线程）会叠加。本方案先通过 V4 观察两者的竞争；如果需要，再单独讨论如何统一划分核数，不在本方案范围内。
