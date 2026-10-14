# DocParse

[English](README.md) | [简体中文](README.zh-CN.md)

DocParse 是 Rust 实现的 PDF 解析流水线，提供原生 CLI、持久化 HTTP 服务和浏览器 WASM SDK。它结合 PDFium 文本提取与渲染、版面检测、表格重建、OCR 和公式识别，输出经过校验的 JSON 和 Markdown。

从[中文部署指南](DEPLOYMENT.zh-CN.md)（[English deployment guide](DEPLOYMENT.md)）开始，了解环境准备、模型下载、编译命令、运行文件和验证步骤。

## 目录

- [部署](#部署)：[CLI](DEPLOYMENT.zh-CN.md#cli) · [Web Server](DEPLOYMENT.zh-CN.md#web-server-与-http-工作台) · [WASM SDK](DEPLOYMENT.zh-CN.md#wasm-浏览器-sdk)
- [流水线与输出](#流水线与输出)
- [配置](#配置)
- [Rust 库集成](#rust-库集成)
- [仓库结构](#仓库结构)
- [开发与验证](#开发与验证)
- [许可](#许可)

## 部署

先准备[工具](DEPLOYMENT.zh-CN.md#准备工具)、[模型](DEPLOYMENT.zh-CN.md#下载并校验模型)和[原生推理后端](DEPLOYMENT.zh-CN.md#原生推理后端)，再从下表直接进入对应的编译与运行指南。

### 选择集成方式

| 集成方式 | 解析位置 | 交付产物 | 运行依赖 | 适用场景 |
| --- | --- | --- | --- | --- |
| [CLI 部署](DEPLOYMENT.zh-CN.md#cli) | 本地原生进程 | `docparse` 可执行文件 | 模型、PDFium、所选推理后端 | 本地文件、脚本、批处理 |
| [Web Server 部署](DEPLOYMENT.zh-CN.md#web-server-与-http-工作台) | 原生服务与 PDFium 子进程 | `docparse-server`、`docparse-pdfium-worker`、可选 HTTP 工作台 | 模型、PostgreSQL、共享存储、原生库 | 持久化任务、HTTP 客户端、共享 GPU 服务 |
| [WASM SDK 部署](DEPLOYMENT.zh-CN.md#wasm-浏览器-sdk) | 浏览器模块 Worker | 静态 ES 模块、WASM、ORT Web 资源 | 静态托管、独立托管的模型、兼容浏览器 | 无解析服务器的浏览器本地解析 |

[HTTP 工作台](packages/web/README.md)将 PDF 上传到原生服务并读取持久化结果；WASM 示例在浏览器本地解析。任一方式选择外部 HTTP 公式引擎后，都会向该服务发送公式裁剪图。

部署运行相关内容可直接跳转：[配置与资源限制](DEPLOYMENT.zh-CN.md#配置与资源限制)、[原生产物打包](DEPLOYMENT.zh-CN.md#原生产物打包)、[部署验证与排障](DEPLOYMENT.zh-CN.md#部署验证与排障)。

### 快速开始：原生 CLI

安装 stable Rust 和 uv，在仓库根目录执行以下命令。模型命令会下载完整模型目录，包括可选替代模型；部署指南另有仅下载当前配置所需模型的步骤。

macOS 需要先从 Linux 准备并复制已校验的 TATR 安装目录：当前 exporter 依赖没有 macOS wheel。执行下方模型命令前，先完成 [TATR 平台准备](DEPLOYMENT.zh-CN.md#macos-上的-tatr)。

```sh
uv run --locked scripts/download_models.py
uv run --locked scripts/download_models.py --verify-only
cargo build --locked --release -p docparse-cli
./target/release/docparse inspect-model --config docparse.toml
./target/release/docparse parse input.pdf --config docparse.toml \
  --format json --output result.json
./target/release/docparse parse input.pdf --config docparse.toml \
  --format markdown --view semantic --output result.md
```

将 `input.pdf` 换成实际 PDF。输出文件采用原子写入，已有文件只有指定 `--force` 才能覆盖。`cargo build` 把可执行文件放在 `target/release/`，不会自动将 `docparse` 安装到 `PATH`。

默认使用 CPU。原生 CLI 和服务支持统一的 `cuda`、`tensorrt`、`coreml`、`metal`、`openvino` Cargo feature。选择加速器前阅读[原生推理后端](DEPLOYMENT.zh-CN.md#原生推理后端)，不要使用 `--all-features`。

## 流水线与输出

![DocParse 架构：集成入口、持久化任务、页面与模型的有界队列](docs/architecture/docparse-architecture.png)

PDFium 提供原生文本和页面图像，PP-DocLayoutV3 检出版面候选区域。仓库的 [docparse.toml](docparse.toml) 使用 TATR 与 wireless RT-DETR 单元格检测重建表格，使用 PP-OCRv6 恢复缺失文本，使用 Texo 识别行内与独立公式。也支持本地表格规则、SLANet+/SLANeXt、PP-FormulaNet 和外部 HTTP 公式服务。

`docparse-core` 确保每个原生文本事实只有一个归属，保留原始几何信息与证据，并计算阅读顺序。模型区域之外的文本、或可恢复的版面推理失败，由 residual XY-cut 保留。表格单元格和公式锚点引用原始文本，不重复占有它。

JSON 保留文档结构、文本、表格、公式、图像、告警和关系。Markdown 提供 raw 与 semantic 两种视图；semantic 视图仅在展示时隐藏重复页眉页脚等内容。公式识别增加 LaTeX 和 Markdown，同时保留源文本。可恢复失败会记录在页面告警和文档错误中。

`reference` 是视觉标注，参考文献正文使用 `reference_content`。部分相交的内容区域保持独立并报告 `ContentLayoutOverlap`。水印保留为独立块；旋转内容在可获得时保留精确多边形。

架构图标签为简体中文。[SVG](docs/architecture/docparse-architecture.svg) 与 [Archify 源文件](docs/architecture/docparse-architecture.archify.json) 和图片一起维护。算法与 schema 细节见 [docparse-core](crates/core/README.md) 和[设计文档](docs/superpowers/specs/)。

## 配置

[docparse.toml](docparse.toml) 是完整的原生配置示例，不会自动根据硬件选择后端。相对模型路径以主配置文件所在目录为基准。CLI 配置优先级依次为：代码默认值、主 TOML、选定的 profile、`DOCPARSE_...` 环境变量、显式调用覆盖。

不传 `--config` 时，CLI 只读取当前目录的 `./docparse.toml`，不向父目录查找。Rust 库只在调用方要求时加载配置。原生后端由编译 feature 固定，TOML 的 `execution_provider` 覆盖会被拒绝。WASM 使用模型 URL/字节和 JavaScript 配置对象，不读取原生配置文件或环境变量。

渲染与模型队列容量必须显式提供，即使对应模型已禁用也不能省略。仓库配置启用 OCR 并使用 `tsr_only`；原生库默认关闭 OCR，表格先走规则再回退到模型。WASM SDK 默认使用 TATR 和规则优先回退，OCR 需要显式启用；浏览器示例启用了自动 OCR。

路径、并发、日志和必填队列见[部署指南](DEPLOYMENT.zh-CN.md#配置与资源限制)。模型专项设置见 [TSR](crates/tsr/README.md)、[OCR](crates/ocr/README.md)、[公式](crates/formula/README.md)、[Texo](crates/formula-texo/README.md) 和 [HTTP 公式](crates/formula-http/README.md)说明。

## Rust 库集成

原生应用也可以直接使用 `docparse-core`：

```rust,no_run
use docparse_config::{ConfigLoader, ValidatedConfig};
use docparse_core::DocParser;

# /// Demonstrates explicit native configuration loading and parsing.
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let raw = ConfigLoader::new("docparse.toml").load_raw()?;
let parser = DocParser::from_config(ValidatedConfig::try_from(raw)?).await?;
let document = parser.parse_path("input.pdf").await?;
# Ok(())
# }
```

复用 parser 可保持模型已加载。原生异步 API 需要 Tokio；`parse_path_blocking` 只适用于现有 Tokio runtime 之外的调用。`DocParserBuilder` 支持注入版面、OCR、表格和字形引擎。浏览器应用应使用 SDK 生命周期 API，它在同一个 Worker 中初始化 PDFium、WASI 和 ORT Web。

## 仓库结构

| 路径 | 职责 |
| --- | --- |
| [crates/cli](crates/cli/README.md) | CLI 参数与输出 |
| [crates/core](crates/core/README.md) | PDF 提取、融合、parser、schema、渲染器 |
| [crates/common](crates/common/README.md)、[crates/config](crates/config/README.md) | 队列、资源生命周期、配置 |
| [crates/layout](crates/layout/README.md)、[crates/tsr](crates/tsr/README.md)、[crates/ocr](crates/ocr/README.md) | 版面、表格、OCR 模型 |
| [crates/formula](crates/formula/README.md)、[crates/formula-texo](crates/formula-texo/README.md)、[crates/formula-http](crates/formula-http/README.md) | 本地与外部公式引擎 |
| [crates/server](crates/server/README.md)、[crates/database](crates/database/README.md)、[crates/migration](crates/migration/README.md) | HTTP 任务、持久化、迁移 |
| [packages/web](packages/web/README.md) | HTTP 工作台 |
| [crates/web](crates/web/README.md)、[packages/wasm-web](packages/wasm-web/README.md) | 浏览器 ABI、SDK、本地解析示例 |
| [scripts](scripts/README.md) | 模型准备、兼容性检查、真实 PDF E2E 工具 |
| [optimum-texo](optimum-texo/README.md) | 可选外部公式服务 |

## 开发与验证

默认 workspace 命令选择原生 crate，不包含浏览器专用 crate：

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
uv run --locked scripts/check_wasm_compat.py
```

显式执行原生 `--workspace` 检查时需要 `--exclude docparse-web`。浏览器 SDK 按 [WASM 部署步骤](DEPLOYMENT.zh-CN.md#wasm-浏览器-sdk)单独构建。前端构建与浏览器验收命令见对应 package 指南。

模型对照与真实 PDF 验收独立于 fixture 测试。[Python 工具指南](scripts/README.md)说明了依赖组和命令。原生 corpus runner 会按 [tests/e2e-corpus.toml](tests/e2e-corpus.toml) 校验 PDF 身份；带 `--only` 的冒烟运行不代表完整验收。历史[性能报告](docs/reports/)仅说明记录时的环境与结果，不保证新部署的性能。

## 许可

DocParse 源码使用 Apache-2.0。PDFium、ONNX Runtime、模型及其他随包资源保留各自许可。模型下载器将 Texo 记录为 AGPL-3.0、TATR 记录为 MIT；模型许可与 Rust 源码许可独立。分发二进制、模型或浏览器资源前参阅 [LICENSE](LICENSE)、[NOTICE](NOTICE) 和 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

用户 PDF、下载模型、构建输出和生成的验收报告不进入 Git。小型回归 fixture 及其许可随仓库维护。
