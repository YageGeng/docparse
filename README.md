# DocParse

DocParse 是一个 Rust PDF 解析流水线：PDFium 提供原生文字事实和页面渲染，固定的 PP-DocLayoutV3 ONNX 模型提供版面区域，`docparse-core` 将两者融合为稳定、可校验的 `DocumentResult`。模型缺失或单页 layout 失败时，文字仍会通过 residual XY-cut 保留。

每个通过校验的模型 Region 对应一个最终 Block；归属判断以 `TextItem` 为最小边界，避免部分重叠的视觉行把模型框外文字带入模型 Block。没有任何模型 owner 的文字会先重新组行，再通过 residual XY-cut 形成 `label_source = "Fallback"` 的 Block，因此模型漏检不会删除文字。

## 准备模型

模型不随 crate 或仓库分发。使用带锁定依赖的 `uv` 脚本下载并验证固定 revision：

```bash
uv run scripts/download_models.py --output models/pp-doclayout-v3
uv run scripts/download_models.py --output models/pp-doclayout-v3 --verify-only
```

固定来源为 `PaddlePaddle/PP-DocLayoutV3_onnx` revision `46bbdf188bb0a772c08aed74882ce7e51a8f1ea6`。manifest 会校验 ONNX 与 YAML 的 SHA-256、模型 schema 和预处理合同。

## 配置

仓库根目录的 `docparse.toml` 是可直接使用的默认配置。相对模型路径以主配置文件目录为基准。覆盖顺序为：代码默认值 → 主 TOML → 显式/`DOCPARSE_PROFILE` profile 文件 → `DOCPARSE_...` 环境变量 → 调用方显式覆盖。

CLI 省略 `--config` 时只读取当前目录的 `./docparse.toml`，不会向父目录搜索。库 API 不隐式读取配置文件。

## 构建与 CUDA

CPU：

```bash
cargo build -p docparse-cli
```

NVIDIA CUDA：

```bash
cargo build -p docparse-cli --features layout-cuda
docparse parse input.pdf --config docparse.cuda.toml --format json
```

CUDA 配置使用 `execution_provider = "cuda"`。请求的 accelerator 注册失败时会明确报错，不会静默回退 CPU。`layout-cuda`、`layout-coreml` 和 `layout-openvino` 互斥；不要用 `--all-features` 构建 provider 矩阵。大型模型可能为每个 CUDA session 保留数 GB 显存，应按设备容量设置 `session_pool_size`。

## Rust API

```rust,no_run
use docparse_config::{ConfigLoader, ValidatedConfig};
use docparse_core::DocParser;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let raw = ConfigLoader::new("docparse.toml").load_raw()?;
let parser = DocParser::from_config(ValidatedConfig::try_from(raw)?).await?;
let document = parser.parse_path("input.pdf").await?;
# Ok(())
# }
```

`DocParserBuilder` 可注入 `Arc<dyn LayoutEngine>` 和可选 `Arc<dyn OcrEngine>`。第一版只定义 OCR trait，不内置 OCR 模型。同步代码可用 `parse_path_blocking`；Tokio runtime 内必须使用 async API。

## CLI

```bash
docparse parse INPUT.pdf --config docparse.toml --format json
docparse parse INPUT.pdf --format text --view semantic
docparse parse INPUT.pdf --format markdown --output result.md --force
docparse parse INPUT.pdf --overlay-dir diagnostics
docparse inspect-model --config docparse.toml
```

JSON 保留完整 schema、evidence、warnings 和 relations。Text/Markdown 的 semantic view 仅在展示层隐藏重复 chrome；不会改写规范结果。overlay 会在解析完成后串行重开 PDF 生成 PNG/SVG，不重跑 ONNX。

## 测试

默认门禁离线运行：

```bash
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

模型 parity：

```bash
uv run scripts/reference_layout.py \
  --model-dir models/pp-doclayout-v3 \
  --input-dir crates/layout/tests/fixtures/model \
  --output crates/layout/tests/fixtures/model/python_outputs.json
cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
```

真实 E2E 会扫描 `~/Downloads` 顶层所有大小写不敏感的常规 PDF，并要求发现集合与 `tests/e2e-corpus.toml` 的 basename、size、SHA-256、page count 完全相等。新增、删除或替换任一 PDF 都会使预检失败；更新语料时必须显式重算并审查 manifest。完整门禁禁止 `--only`，该参数只用于 smoke。

```bash
uv run scripts/run_real_pdf_e2e.py \
  --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 \
  --execution-provider cuda --page-concurrency 1 --run-id serial
uv run scripts/run_real_pdf_e2e.py \
  --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 \
  --execution-provider cuda --page-concurrency 4 --run-id parallel \
  --write-overlays
python scripts/compare_e2e_runs.py \
  target/docparse-e2e/serial/canonical-hashes.json \
  target/docparse-e2e/parallel/canonical-hashes.json
uv run scripts/build_visual_review.py \
  --run-dir target/docparse-e2e/parallel
```

E2E 默认先构建 release 测试产物，再在计时区间内直接执行测试二进制；仅排查调试行为时使用 `--cargo-profile dev`。性能数据和实际 Cargo profile 只记录在 `summary.json`，不参与跨机器阈值。规范 hash 不含耗时、绝对路径或本机信息。

## 首版边界

- 不识别公式 LaTeX，仅保留 inline/display formula 的位置和内容状态。
- table 第一版提供视觉阅读顺序，不恢复完整单元格结构。
- OCR 需要调用方注入实现。
- 真实 PDF、模型、渲染图片和 E2E 报告均不进入 Git 或 crate 包。

## 来源与许可证

DocParse 采用 Apache-2.0。PDFium、PP-DocLayoutV3、ONNX Runtime、Rust/Python 开发依赖及 LiteParse 派生来源有各自条款；分发前请阅读 [NOTICE](NOTICE) 与 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
