# 布局融合性能与代码质量修复实施计划

> **执行要求：** 在当前工作区内联执行；仓库禁止 SubAgent 和 worktree。每项行为修复遵循测试先行，所有命令使用 `rtk` 前缀，未经用户明确授权不创建 commit。

**目标：** 修复代码审查确认的 CUDA 无效输出、推理串行化、失败后继续工作、JSON 峰值内存、顺序图复杂度、图像预处理热点、assignment 无效分配、E2E 性能测量失真和 PDFium 安全 API panic。

**架构：** 保持现有 PDFium 单线程 actor、页面级并发和单 CUDA session 的总体架构。将 CPU 预处理移到 session lease 之前形成流水线；通过 ORT 输出选择裁剪未使用 masks；在 core 内用借用型序列化视图和索引化图结构减少复制与树结构开销。所有规范输出和确定性排序结果必须与现有 canonical 结果一致。

**技术栈：** Rust 2024、Tokio、ONNX Runtime `ort`、Serde、ndarray、PDFium、Python 3.11+ E2E 驱动。

**规格：** `docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md`

## 全局约束

- 新增函数必须有英文函数级注释；修改后的非平凡逻辑必须补充英文原因注释。
- 超过 3 个字段的结构体使用 `typed-builder`，`Arc` 字段克隆使用 `Arc::clone`。
- 测试代码仅放在 crate `tests/` 或精确命名的 `#[cfg(test)] mod tests` 中。
- WebUI/E2E 继续使用真实生产 parser、真实模型和 `~/Downloads` 全部顶层 PDF。
- 不改变 JSON schema、阅读顺序决策、模型阈值或文本所有权语义。
- 不创建 commit，不覆盖用户已有修改。

---

### 任务 1：裁剪 ORT 未使用输出并建立 CPU/GPU 流水线

**文件：**

- 修改：`crates/layout/src/pp_doclayout_v3/session.rs`
- 修改：`crates/layout/src/pp_doclayout_v3/mod.rs`
- 测试：`crates/layout/src/pp_doclayout_v3/session.rs` 中的 `#[cfg(test)] mod tests`

**接口：**

- `LayoutSession` 持有 `RunOptions<HasSelectedOutputs>`。
- `LayoutSession::run` 只返回 `fetch_name_0` 与 `fetch_name_1`。
- `PpDocLayoutV3Engine::detect` 先完成 `preprocess`，再获取 `SessionLease` 并推理。

- [x] 添加 ignored 真实模型测试，调用 `LayoutSession::run` 并断言输出包含 `fetch_name_0/1` 且不包含 `fetch_name_2`。
- [x] 运行 `rtk cargo test -p docparse-layout session_requests_only_consumed_outputs -- --ignored --nocapture`，确认旧实现因仍返回 masks 而失败。
- [x] 使用 `RunOptions::new()?.with_outputs(OutputSelector::no_default().with("fetch_name_0").with("fetch_name_1"))` 实现最小修复，并删除每页 masks 提取。
- [x] 将 `preprocess` 放在 lease 获取之前的独立 `spawn_blocking` 中，lease 只包围 `LayoutSession::detect`。
- [x] 重跑目标测试和 layout 全部测试，确认输出契约与 Python parity 不变。

### 任务 2：runtime 首个致命错误立即停止后续页面

**文件：**

- 修改：`crates/core/src/runtime/pipeline.rs`
- 测试：`crates/core/src/runtime/pipeline.rs` 中的 `#[cfg(test)] mod tests`

**接口：**

- `ParseRuntime::collect_page_task` 返回 `Result<PageResult, ParseRuntimeError>`，不再通过可变参数延迟保存错误。
- 主循环遇到 join、分析、render 或缺页错误后停止接收，关闭 channel，取消并回收 `JoinSet`，等待 producer 归还 executor 后关闭 PDFium actor。

- [x] 增加计数型失败 layout engine，使用多页 PDF、`page_concurrency = 1`，断言解析失败时只调用一次 layout。
- [x] 运行目标测试，确认旧实现会继续调用后续页面而失败。
- [x] 重构任务收集结果和 loop 退出路径，保证首个业务错误优先于清理错误返回。
- [x] 重跑 runtime 目标测试及 `docparse-core` 全部测试。

### 任务 3：零克隆 JSON 可见性视图与原子流式输出

**文件：**

- 修改：`crates/core/src/render/json.rs`
- 修改：`crates/core/tests/render.rs`
- 修改：`crates/cli/src/lib.rs`
- 测试：`crates/cli/tests/cli_with_fake.rs`

**接口：**

- 新增 `JsonRenderer::write_with_config<W: Write>(document, config, writer) -> Result<(), RenderError>`。
- `render_with_config` 复用借用型 `Serialize` 视图，不克隆 `DocumentResult`。
- CLI 的 JSON 文件路径直接写临时文件；stdout 与文本/Markdown 路径保持现有返回值。

- [x] 添加 writer 输出与 canonical JSON 在全部字段可见时结构相等的测试，并覆盖 page、block、relation evidence 和 diagnostics 隐藏。
- [x] 运行 render 目标测试，确认旧代码因 writer API 不存在而失败。
- [x] 实现 `DocumentView`、page/block/relation 序列化包装器，逐字段借用原结构并动态写出空 evidence/diagnostics。
- [x] 将 `atomic_write` 改为接收写入闭包，让 JSON 直接使用 `serde_json::to_writer_pretty`。
- [x] 运行 core render 与 CLI fake integration 测试，检查原子替换和 schema 反序列化。

### 任务 4：优化 OpenCV-compatible cubic resize

**文件：**

- 修改：`crates/layout/src/pp_doclayout_v3/preprocess.rs`
- 测试：现有 `#[cfg(test)] mod tests` 与 Python parity fixtures

**接口：**

- `resize_inter_cubic` 保持输入输出及 ties-to-even 行为不变。
- 水平阶段产生定点中间行，垂直阶段复用结果，避免每个目标通道重复 4×4 源索引计算。

- [x] 在修改前重复运行 release preprocessing parity 测试并记录稳定基线。
- [x] 将源缓冲区长度、row stride 和中间缓冲区长度在循环外一次性校验。
- [x] 实现不进行中间舍入的水平/垂直两阶段计算，最终仍只执行一次 `round_shift_ties_even(OUTPUT_SHIFT)`。
- [x] 运行全部固定样本 parity，要求 tensor 逐值完全相等；随后重复 release 测量并比较。

### 任务 5：索引化阅读顺序图并复用邻接关系

**文件：**

- 修改：`crates/core/src/fusion/order.rs`
- 测试：同文件 `#[cfg(test)] mod tests`

**接口：**

- 节点在 `resolve` 开始时稳定映射为整数索引。
- forward/reverse adjacency 保存 edge index 并只构建一次；删除边由 active bitmap 表示。
- SCC 每轮仅遍历 active edge；候选删除边单次扫描确定。
- Kahn 排序使用按现有 `compare_nodes` 语义排序的 ready heap 和 outgoing adjacency。

- [x] 增加多 SCC、重叠周期和多个同端点来源的回归用例，手工断言删除边顺序与最终节点顺序。
- [x] 运行目标测试，建立旧策略输出基线。
- [x] 实现索引邻接、active edge、整数 DFS 和 ready heap；不得改变弱边权重与 tie-break。
- [x] 运行 order 全测试，并通过真实 E2E canonical hash 与修改前结果比较。

### 任务 6：消除 assignment 每 fragment 分配与死字段

**文件：**

- 修改：`crates/core/src/fusion/assign.rs`
- 修改：`crates/core/src/semantic/mod.rs`
- 测试：`crates/core/src/fusion/assign.rs` 中的现有测试模块

**接口：**

- `AssignEvidence` 只保留 `selected` 与 `alternative_count`。
- `AssignmentEngine::assign` 单遍计算最佳候选与候选总数，不构造 candidates `Vec`。

- [x] 增加多个合格 region 的 assignment 用例，断言稳定 owner 及 `alternatives` 诊断数量。
- [x] 运行目标测试，记录现有语义输出。
- [x] 用单遍 `Option<(usize, AssignmentScore)>` 替代候选集合，删除未消费的 text item/region ID 克隆。
- [x] 重跑 assignment、semantic 与 render 测试。

### 任务 7：修复 PDFium 颜色转换 panic

**文件：**

- 修改：`crates/pdfium/src/bitmap.rs`
- 修改：`crates/pdfium/src/page.rs`
- 测试：`crates/pdfium/src/bitmap.rs` 中的 `#[cfg(test)] mod tests`

**接口：**

- `Bitmap::fill_rect` 返回 `Result<(), PdfiumError>` 并在所有平台先验证 `u64 -> u32`。
- 内部再将合法 ARGB 转为平台相关 `FPDF_DWORD`，不得使用 `unwrap`。

- [x] 添加 `u32::MAX + 1` 被拒绝、`u32::MAX` 被接受的纯转换测试。
- [x] 运行目标测试，确认旧实现缺少验证路径而失败。
- [x] 实现 checked conversion，更新 render 调用点用 `?` 传播。
- [x] 运行 pdfium 全测试和 Clippy，确认 unsafe 块继续有完整 SAFETY 依据。

### 任务 8：让真实 E2E 测量 release 测试二进制

**文件：**

- 修改：`scripts/run_real_pdf_e2e.py`
- 修改：`crates/core/tests/python/run_real_pdf_e2e_test.py`

**接口：**

- 新增 `--cargo-profile {dev,release}`，默认 `release`。
- 构建步骤使用 Cargo JSON message 找到 `real_pdfs` executable，并在计时区间外完成。
- `run_harness` 直接执行测试二进制；summary 记录 `cargo_profile`。

- [x] 先修改 Python 行为测试，断言 build command 含 `--no-run --message-format=json --release`，direct command 不含 cargo。
- [x] 运行 `rtk uv run crates/core/tests/python/run_real_pdf_e2e_test.py`，确认旧接口失败。
- [x] 实现构建产物发现和 direct harness 执行，保持参数数组调用且不使用 shell。
- [x] 重跑 Python 测试，并用单 PDF smoke 验证 summary profile 与 executable 启动。

### 任务 9：全量验证与真实 PDF 性能回归

**文件：**

- 验证：全部修改文件与 `target/docparse-e2e/` 非 canonical 结果

- [x] 运行 `rtk cargo fmt --all -- --check`。
- [x] 运行相关 crate 测试及 `rtk cargo test --workspace`。
- [x] 运行 `rtk cargo clippy --workspace --all-targets -- -D warnings`。
- [x] 运行 `rtk cargo check -p docparse-layout --features cuda` 与 CUDA Clippy。
- [x] 使用 `~/Downloads` 全部顶层 PDF 分别运行 release CUDA 并发 1、并发 4 E2E。
- [x] 使用 compare 脚本确认修改前后及串并行 canonical hash 零差异。
- [x] 汇报 release 吞吐、峰值 RSS、输出规模、解环统计以及任何仍需 profiler 才能确认的瓶颈；不创建 commit。
