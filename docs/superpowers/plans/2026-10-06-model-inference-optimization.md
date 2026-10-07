# 模型推理性能优化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按设计文档的四级阶梯，逐个降低 Texo encoder/decoder、layout、tsr_structure、tsr_cells 在 gpuhub 上的单项推理耗时，质量与稳定性不下降。

**Architecture:** 新增按模型生效的 ONNX 调优配置 `OnnxTuning`，由 `OnnxBackend::tuned` 应用到 CUDA/TensorRT EP；FP16 通过离线转换出的模型文件和现有的 `*_path` 配置切换；每一级都用 gpuhub 上的交替块评测决定保留与否。

**Tech Stack:** Rust（ort 2.0.0-rc.13，ORT 1.29 动态库，CUDA 13）、Python（onnx、onnxconverter-common，经 `uv run --with`）、gpuhub HTTP 基准工具。

**Spec:** `docs/superpowers/specs/2026-10-06-model-inference-optimization-design.md`

## Global Constraints

- 不升级 ORT（保持 1.29 / ort 2.0.0-rc.13）。
- 新增配置的默认值必须保持现有行为；只有评测通过后才在单独的任务中修改默认值。
- 保留标准：目标模型 ms/item 在每个相邻块配对中都下降，均值下降 ≥ 5%；其他模型与端到端吞吐 ≥ 基线 97%；质量指标不低于"基线 vs 基线"减去波动幅度；任务失败 0，公式丢失与 CUDA 分配失败不多于基线，p95 ≤ 基线 105%。
- gpuhub 只用临时 worktree `/autodl-fs/data/docparse-bench`、数据库 `docparse_bench`、目录 `/autodl-fs/data/docparse-bench-run`、端口 127.0.0.1:16180；结束后全部清理。
- 生成的模型文件命名为原文件名加 `.fp16.onnx` 后缀，放在原模型目录，不入库。
- AGENTS.md 全部规则适用：英文代码注释与函数注释、typed-builder（>3 字段）、`Arc::clone`、全限定 `tracing::` 宏、Python 需通过 `uvx ruff check .`、未经用户明确许可不 `git commit`。

## Review Focus

1. **调优配置拼写错误或用于不支持的 EP**（例如 CPU 构建写了 `tensorrt = true`）：拼写错误应在配置加载时报错，EP 不可用应在创建 session 时返回 `ExecutionProviderUnavailable`，而不是静默忽略 → Task 2 测试。
2. **TensorRT 引擎缓存目录不可写或缓存过期**：服务应启动失败并给出路径，而不是每次请求都重新构建引擎 → Task 6 测试。
3. **FP16 模型在极端输入下溢出**（超大或全白公式裁图、超宽表格）：公式失败率和表格结构一致率必须在质量对比中被覆盖 → Task 5 质量评测包含全部语料，并单独统计失败数。
4. **TensorRT 动态 batch 超出 profile 范围**（Texo encoder batch > 24）：必须回退或报错，不能崩溃 → Task 6 测试 profile 上限等于配置的 `batch_size`。
5. **调优只作用于目标模型**：给 layout 开启的选项不能影响 TSR/Texo 的 session → Task 2 测试 `tuned` 只改变当前 backend 副本。

---

### Task 1: 质量对比工具与基线噪声下限

**Files:**
- Create: `<scratchpad>/gpuhub/quality.py`（会话临时目录，不入库）
- 运行于 gpuhub `/autodl-fs/data/docparse-bench-run`

**Interfaces:**
- Consumes: `bench.py` 保存的 `results/<tag>/results/<sha16>.json`（规范化结果，已去掉 figures）。
- Produces: `quality.py <reference_tag> <candidate_tag>` 输出 JSON：`layout_block_match`、`formula_exact`、`formula_norm_edit`、`table_structure_match`、`text_block_similarity`；后续每个评测任务都用它。

- [ ] **Step 1: 实现 `quality.py`**：按文档 sha 配对两组结果。layout 区块按页面内（label, bbox）贪心匹配 IoU ≥ 0.9；公式按页面内顺序配对比较 `latex` 字符串；表格按页面内顺序比较行数、列数和单元格 rowspan/colspan 列表；文本块用 `difflib.SequenceMatcher` 比较块文本序列。
- [ ] **Step 2: 自检**：`quality.py base-c2 base-c2` 应输出全部 1.0。
- [ ] **Step 3: 跑两次基线**：用当前 main（`b8f2e39`）构建，`profile.sh base2 config-base.toml` 再跑一轮，得到 `base-*` 与 `base2-*` 两组。
- [ ] **Step 4: 计算噪声下限**：`quality.py base-c2 base2-c2`、`quality.py base-c4 base2-c4`，记录每项指标作为该指标的下限基准，写入报告草稿。

### Task 2: 按模型的 ONNX 调优配置 `OnnxTuning`

**Files:**
- Modify: `crates/config/src/config.rs`（新增 `OnnxTuning`、`ConvAlgorithm`；在 `LayoutConfig`、`TsrConfig`、`TableCellConfig`、`TexoFormulaConfig` 中加字段）
- Modify: `crates/config/src/lib.rs`、`crates/config/src/validate.rs`
- Modify: `crates/layout/src/wasm_compat/backend.rs`（`OnnxBackend::tuned`、CUDA EP 应用调优）
- Modify: `crates/layout/src/wasm_compat/session_pool.rs:153`、`crates/tsr/src/model.rs:454,511`、`crates/formula-texo/src/model.rs:93` 及 `crates/formula-texo/src/wasm_compat.rs:129-130`（encoder/decoder 各自调优）
- Test: `crates/config/tests/loading.rs`、`crates/layout/src/wasm_compat/backend.rs` 中的 `mod tests`

**Interfaces:**
- Produces:
  - `pub enum ConvAlgorithm { Heuristic, Exhaustive, Default }`（serde lowercase，默认 `Heuristic`，即现状）
  - `pub struct OnnxTuning { pub conv_algorithm: ConvAlgorithm, pub prefer_nhwc: bool, pub tf32: bool, pub tensorrt: bool }`，`#[serde(default, deny_unknown_fields)]`，全部默认等于现状（Heuristic、false、false、false）
  - 字段名：`LayoutConfig.onnx`、`TsrConfig.onnx`、`TableCellConfig.onnx`、`TexoFormulaConfig.encoder_onnx`、`TexoFormulaConfig.decoder_onnx`
  - `impl OnnxBackend { pub fn tuned(self, tuning: &OnnxTuning) -> Self }`

- [ ] **Step 1: 写失败测试（config）**：`onnx_tuning_defaults_preserve_current_behavior` 断言不写 `onnx` 时 `layout.onnx == OnnxTuning::default()`，且 `default().conv_algorithm == ConvAlgorithm::Heuristic`；`onnx_tuning_rejects_unknown_keys` 断言 `[layout.onnx] typo = 1` 加载失败。
- [ ] **Step 2: 写失败测试（backend）**：`tensorrt_requires_cuda_provider`：provider 为 `Cpu` 且 `tuning.tensorrt = true` 时，`SessionBuilder::try_from(backend)` 返回 `LayoutError::ExecutionProviderUnavailable { provider: "tensorrt" }`。config crate 不感知构建特性，所以这项检查放在 backend。
- [ ] **Step 3: 写失败测试（backend）**：`tuned_changes_only_this_backend` 断言 `let a = OnnxBackend::compiled(); let b = a.tuned(&t);` 后 `a` 的调优仍为默认；`cuda_options_follow_tuning` 断言一个纯函数 `OnnxTuning::cuda_settings(&self) -> (ConvAlgorithm, bool, bool)` 或等价访问器返回配置值。
- [ ] **Step 4: 运行测试确认失败**：`rtk cargo test -p docparse-config -p docparse-layout`，预期编译失败（类型未定义）。
- [ ] **Step 5: 实现**：config 类型与字段；`OnnxBackend` 新增 `tuning: OnnxTuning`（`#[builder(default)]`）；CUDA 分支按 tuning 调用 `with_conv_algorithm_search`、`with_prefer_nhwc`、`with_tf32`；`tensorrt` 字段本任务只做校验，EP 注册在 Task 6。各加载点调用 `.tuned(...)`，Texo encoder/decoder 分别使用 `encoder_onnx`/`decoder_onnx`。
- [ ] **Step 6: 运行测试与检查**：`rtk cargo test --workspace --exclude docparse-web`，`pre-commit run --files <改动文件>`，全部通过。
- [ ] **Step 7: 请用户批准后提交**：`feat(runtime): add per-model ONNX tuning settings`

### Task 3: 第 1 级评测（CUDA EP 参数）

**Files:** gpuhub 配置变体 `config-t1-<model>-<option>.toml`（由 `make_config.py` 生成，只改目标模型的 `onnx` 段）。

**Interfaces:**
- Consumes: Task 2 的配置键；Task 1 的 `quality.py` 与噪声下限。

- [ ] **Step 1: 构建 Task 2 的二进制并部署到 gpuhub**。
- [ ] **Step 2: 逐模型筛选**：对 Texo encoder、layout、tsr_cells（固定尺寸）分别试 `conv_algorithm = "exhaustive"`、`prefer_nhwc = true`、`tf32 = true`；对 Texo decoder、tsr_structure（动态尺寸）只试 `prefer_nhwc` 与 `tf32`。每个候选先跑 1 个块快速筛选，ms/item 下降 < 3% 的直接淘汰。
- [ ] **Step 3: 确认候选**：入选的组合与基线交替跑 3 个块，按 Global Constraints 判定；质量用 `quality.py`。
- [ ] **Step 4: 记录结果**：每个模型保留的设置及数据写入报告草稿。

### Task 4: 第 2 级——Texo decoder 剖析与优化、layout 跨页 batch

**Files:**
- Modify（视剖析结论）：`crates/formula-texo/src/cuda.rs`、`crates/formula-texo/src/model.rs`
- 配置：`[layout] batch_size`

**Interfaces:**
- Consumes: `CudaIoContext::recognize`（`crates/formula-texo/src/cuda.rs:50`）当前每步重建 `next_ids`/`use_cache` 张量并 `clear_outputs` 重新绑定。

- [ ] **Step 1: 剖析 decoder 单步**：在 gpuhub 临时构建中对 decoder session 开启 ORT profiling（`SessionBuilder::with_profiling`，仅本地实验，不提交），跑一个公式密集文档，统计单步内 `If` 子图、各算子 kernel、Memcpy 的耗时占比。
- [ ] **Step 2: 按剖析结论选定优化**（只做占比 ≥ 10% 的项；设计文档中的"argmax 放到 GPU"不做，因为词表只有 687，每步 logits 仅 batch×687 个 float，回传开销可忽略，除非剖析显示 Memcpy 占比 ≥ 10%）：
  - Memcpy/输出重分配占比高 → 复用每步的 input_ids/use_cache 张量，只更新数据；
  - `If` 调度开销占比高 → 用 `scripts/split_texo_decoder.py` 从合并模型导出 `decoder_with_past.onnx` 与 `decoder_init.onnx`，配置新增 `decoder_init_path`、`decoder_with_past_path`（`Option<PathBuf>`，缺省时沿用合并模型）；
  - 两者都 < 10% → 本步跳过，在报告中记录剖析数据。
- [ ] **Step 3: 为选定的代码改动写失败测试**：在 `crates/formula-texo/src/model.rs` 的 `mod tests` 中，以现有 `Generation` 测试为模板，断言优化前后对同一组 logits 序列产生相同 token 序列（纯 CPU 路径可测部分）。
- [ ] **Step 4: 实现并通过测试**，`rtk cargo test -p docparse-formula-texo`。
- [ ] **Step 5: layout batch**：配置 `batch_size = 2` 与 `4` 各作为候选（不改代码）。
- [ ] **Step 6: 交替 3 块评测**（decoder 改动、layout batch 分别评测），按 Global Constraints 判定并记录。
- [ ] **Step 7: 请用户批准后提交保留的代码改动**：`perf(formula): <具体优化>`。

### Task 5: 第 3 级——FP16 模型文件

**Files:**
- Create: `scripts/convert_fp16.py`
- Modify: `scripts/README.md`（补充用法）

**Interfaces:**
- Produces: `uv run --with onnx --with onnxconverter-common python scripts/convert_fp16.py <input.onnx> [--block-op <Op> ...]` 生成 `<input>.fp16.onnx`；`keep_io_types=True`，默认阻止列表包含 `Softmax`、`LayerNormalization`、`ReduceMean`、`Pow`、`Sqrt`（数值敏感算子保持 FP32）。

- [ ] **Step 1: 写脚本**，并让 `--self-check` 用 onnx 自带的小模型（如单个 `MatMul` 图）转换后断言输出图的初始化器为 FLOAT16、图输入输出仍为 FLOAT。
- [ ] **Step 2: 运行自检并通过 ruff**：`uv run --with onnx --with onnxconverter-common python scripts/convert_fp16.py --self-check`；`uvx ruff check .`。
- [ ] **Step 3: 在 gpuhub 生成五个 FP16 模型**（Texo encoder、Texo decoder、layout、tatr、rtdetr-cell）。
- [ ] **Step 4: 逐模型评测**：配置只把目标模型的 `*_path` 指向 `.fp16.onnx`，叠加 Task 3/4 已保留的设置；交替 3 块；质量重点看公式一致率与表格结构一致率。精度不过关的模型，尝试扩大 `--block-op` 列表后重测一次，仍不过则放弃该模型的 FP16。
- [ ] **Step 5: 请用户批准后提交脚本**：`feat(scripts): add FP16 ONNX conversion`。

### Task 6: 第 4 级——TensorRT EP（layout、tsr_cells、Texo encoder）

**Files:**
- Modify: `Cargo.toml`（根 workspace 的 ort features 不变）、`crates/layout/Cargo.toml`（新增 feature `tensorrt = ["ort/tensorrt", "cuda"]`）、`crates/core/Cargo.toml`、`crates/server/Cargo.toml`（透传 `tensorrt` feature）
- Modify: `crates/layout/src/wasm_compat/backend.rs`（`tuning.tensorrt` 时在 CUDA 之前注册 TensorRT EP）
- Modify: `crates/config/src/config.rs`（`RuntimeConfig` 新增 `tensorrt_cache_dir: Option<PathBuf>`，`#[builder(default)]`）
- Test: `crates/layout/src/wasm_compat/backend.rs` 的 `mod tests`、`crates/config/tests/validation.rs`

**Interfaces:**
- Consumes: Task 2 的 `OnnxTuning.tensorrt`。
- Produces: TensorRT EP 选项——引擎缓存开启且路径为 `tensorrt_cache_dir`；`fp16` 开启；动态 batch profile 的 min=1、opt=max=该模型配置的 `batch_size`。

- [ ] **Step 1: 在 gpuhub 安装与 ORT 1.29 / CUDA 13 匹配的 TensorRT**（按 ORT 1.29 发布说明选择 TensorRT 10.x），确认 `libnvinfer.so` 可被 `LD_LIBRARY_PATH` 找到；不兼容则跳过本任务并记录原因。
- [ ] **Step 2: 写失败测试**：`tensorrt_requires_cache_dir`（`tensorrt = true` 而 `tensorrt_cache_dir` 缺失时校验报错）；`tensorrt_cache_dir_must_be_writable`（不可写目录校验报错，错误信息包含路径）；`tensorrt_profile_matches_batch_size`（纯函数 `tensorrt_profile(batch_size) -> (min, opt, max)` 返回 `(1, n, n)`）。
- [ ] **Step 3: 运行测试确认失败**，然后实现。
- [ ] **Step 4: 运行测试与 pre-commit**，全部通过。
- [ ] **Step 5: gpuhub 评测**：用 `--features tensorrt` 构建；每个目标模型单独开启 `tensorrt = true`，先预热（引擎构建时间单独记录在报告中），再交替 3 块；同时记录首次启动耗时与显存峰值。
- [ ] **Step 6: 请用户批准后提交**：`feat(runtime): add optional TensorRT execution for fixed-shape models`。

### Task 7: 默认值、报告与清理

**Files:**
- Modify: `docparse.toml`（只为通过评测的设置补充注释示例；仅当某设置对所有部署都安全时才修改代码默认值）
- Create: `docs/superpowers/reports/<完成日期>-model-inference-optimization.md`

- [ ] **Step 1: 组合验证**：把所有保留的设置合在一起，与基线交替 3 块，确认组合后没有互相抵消或显存超限。
- [ ] **Step 2: 写报告**：逐模型、逐级的 ms/item、质量指标、稳定性数据；最终推荐的生产配置片段（可直接放入 `docparse.release.toml`）。
- [ ] **Step 3: 清理 gpuhub**：删除 worktree、`tmp/perf-baseline` 分支、`docparse_bench` 数据库、`/autodl-fs/data/docparse-bench-run`、上传的 bundle；保留生成的 `.fp16.onnx` 文件与 TensorRT 安装，并在报告中列出路径。
- [ ] **Step 4: 请用户批准后提交报告**：`docs(superpowers): add model inference optimization report`。
