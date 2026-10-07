# 模型推理性能优化设计

日期：2026-10-06
状态：待评审

## 1. 目标与约束

- **目标**：在 gpuhub（RTX 4080 SUPER 32 GiB、cgroup 配额 15 核、`docparse.release.toml` 生产配置）上逐个降低模型的单项推理耗时。主指标是每个模型的 ms/item。
- **质量**：质量指标不下降，即落在基线自身两次运行之间的波动范围内。允许使用改变数值精度的优化（FP16、TF32、TensorRT）。
- **稳定性**：任务失败数、公式丢失数和 CUDA 分配失败数不增加，显存峰值不超过 32 GiB，p95 不变差。
- **允许的变更**：生成新的模型文件，与原文件并存；在 gpuhub 安装 TensorRT。**不升级 ORT**（保持 1.29）。

## 2. 基线（2026-10-05，生产配置，18 篇文档 / 350 页）

| 模型 | c2 耗时占比 | ms/call | 平均 batch | ms/item |
| --- | --- | --- | --- | --- |
| Texo encoder | 39.9% | 323 | 15.1 | 21.4 |
| Texo decoder（单步） | 34.8% | 2.8 | 18.3 | 0.15 |
| layout | 14.6% | 89 | 1.0 | 89 |
| tsr_structure（TATR） | 5.7% | 165 | 1.0 | 165 |
| tsr_cells（RT-DETR） | 5.1% | 155 | 1.06 | 147 |

生产配置下 OCR 是 `disabled`，三个 OCR 模型没有调用，因此只纳入通用优化（第 1、3 级），不单独深挖。

## 3. 方案：逐模型优化阶梯

按耗时占比确定顺序：Texo encoder → Texo decoder → layout → tsr_structure → tsr_cells。每个模型依次尝试以下四级。每一级都是独立的配置开关，单独评测，满足第 4 节的标准才保留，否则回退：

1. **CUDA EP 参数**：卷积算法搜索策略（固定输入尺寸的模型用 Exhaustive）、`prefer_nhwc`、TF32、`cudnn_conv_use_max_workspace`。
2. **模型专属代码优化**：
   - Texo decoder：把 argmax 放到 GPU 上，只回传 token id，不再每步拷回 logits；用不含 `If` 的 with-past 子图替代合并图；预分配并复用每步的输出绑定。
   - layout：启用跨页 batch。基线中 `batch_size = 1`，平均 batch 也是 1。
3. **FP16 模型文件**：离线转换（保持输入输出为 FP32，对数值敏感的算子保留 FP32），生成 `*.fp16.onnx` 与原文件并存，在配置中按模型选择。
4. **TensorRT EP**：只用于输入尺寸固定的模型（layout 800×800、tsr_cells 640×640、Texo encoder 固定分辨率），开启引擎缓存并预热，未覆盖的算子回退到 CUDA EP。TATR 和 decoder 是动态形状，不做。

配置原则：新增开关的默认值保持现状，评测通过后再按数据修改默认值（与 `onnx_thread_pool` 的做法一致）。

## 4. 评测方法

### 4.1 性能

- 走生产 HTTP 服务、真实语料，并发 c2 与 c4。从 `/metrics` 中 `docparse_onnx_run_seconds` 和 `docparse_onnx_batch_items` 的前后差值计算每个模型的 ms/call、ms/item 和耗时占比。
- 每个候选与基线**交替**运行至少 3 个块；每块重启服务，预热 1 轮后 c2、c4 各跑 1 轮。比较时以相邻块配对为准，以抵消机器状态漂移。
- 通过标准：目标模型的 ms/item 在配对比较中稳定下降（每对都下降，且均值下降 ≥ 5%），其他模型和端到端吞吐不变差（不低于基线的 97%）。

### 4.2 质量

先跑两次基线，确定波动下限，再按模型对比：

| 模型 | 指标 |
| --- | --- |
| layout | 区块匹配率：标签相同且 bbox IoU ≥ 0.9 的区块占比 |
| Texo | 公式 LaTeX 完全一致率，以及归一化编辑距离的均值 |
| TSR | 表格结构一致率：行数、列数和单元格跨度全部相同的表格占比 |
| 端到端 | 块级文本一致率（沿用上次报告中的度量） |

候选的每项指标都不得低于"基线 vs 基线"的对应值减去其波动幅度。

### 4.3 稳定性

任务失败数为 0，公式丢失和 CUDA 分配失败不多于基线，显存峰值低于上限并留出余量，c2/c4 的 p95 不超过基线的 105%。

## 5. 实施与环境

- gpuhub 上使用临时 worktree、独立数据库 `docparse_bench`、独立目录 `/autodl-fs/data/docparse-bench-run`，只走内网 127.0.0.1:16180，结束后全部清理。原有仓库和实验不动。
- 评测工具：`bench.py`（吞吐、延迟、资源、公式丢失、保存规范化结果）、`metrics_delta.py`（逐模型耗时）、新增 `quality.py`（第 4.2 节的指标）。工具放在会话临时目录，不进入仓库。
- 代码改动走 TDD；每个保留的优化单独提交。FP16 转换脚本和生成步骤写入文档，生成的模型文件不入库（`models/` 已在 gitignore 中）。
- 安装 TensorRT 前先确认其版本与 ORT 1.29 及 CUDA 13 兼容。如果不兼容，跳过第 4 级并在报告中说明。

## 6. 产出

- 每个保留的优化：代码、配置开关和测试。
- 中文报告 `docs/superpowers/reports/<完成日期>-model-inference-optimization.md`（文件名日期取报告完成当天）：逐模型、逐级的性能和质量数据，以及最终推荐的默认配置。

## 7. 不做

- 不升级 ORT，不替换模型结构或权重来源（FP16 由原权重转换而来）。
- 不为 OCR 单独做第 2、4 级优化。
- 不对 TATR 和 Texo decoder 使用 TensorRT（动态形状）。
