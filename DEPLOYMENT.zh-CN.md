# 部署指南

[English](DEPLOYMENT.md) | [简体中文](DEPLOYMENT.zh-CN.md) | [项目概览](README.zh-CN.md)

本指南以仓库中的 Cargo/npm manifest、构建脚本和 `docparse.toml` 为准。除特别说明外，命令均在仓库根目录执行。原生产物应在目标操作系统/架构上构建；原生服务面向 Linux/macOS。模型独立下载，不会打进二进制或 WASM SDK。

## 选择部署方式

| | CLI | Web Server + HTTP 工作台 | WASM 浏览器 SDK |
| --- | --- | --- | --- |
| Rust package | `docparse-cli` | `docparse-server` + `docparse-core` worker | `docparse-web` |
| 前端 package | 无 | `packages/web` | `packages/wasm-web` |
| 解析位置 | 原生进程 | 原生 worker；HTTP 客户端上传 PDF | 浏览器模块 Worker |
| 输出 | 本地 JSON/Markdown，可选 overlay | 持久化任务、JSON/Markdown、SSE、网页检查 | 内存文档、JSON/Markdown、浏览器预览 |
| 模型交付 | 配置文件对应的磁盘文件 | 每个解析 worker 可访问的磁盘文件 | URL 或调用方提供的字节 |
| 后端选择 | Cargo feature | Cargo feature | `executionProvider: "webgpu"` 或 `"wasm"` |
| PostgreSQL / 共享存储 | 均不需要 | 所有服务角色都需要 | 均不需要 |
| 部署产物 | 可执行文件、原生运行文件、模型/配置 | 两个可执行文件、运行文件、模型/配置、可选 UI | 完整 SDK `dist/`、启用的模型资源、宿主页面 |

HTTP 工作台是服务端客户端，会上传 PDF。WASM 示例只托管静态资源，在浏览器本地解析。两种方式选择 HTTP 公式引擎后，都会向外部服务发送公式图像。

按顺序阅读：[工具](#准备工具)、[模型](#下载并校验模型)、[原生后端](#原生推理后端)，然后选择 [CLI](#cli)、[Web Server](#web-server-与-http-工作台) 或 [WASM](#wasm-浏览器-sdk)。

## 准备工具

| 工具/资源 | 使用场景 | 要求 |
| --- | --- | --- |
| Rust + Cargo | 所有 Rust 构建 | 使用 `rust-toolchain.toml` 的 stable 工具链和已提交的 `Cargo.lock`；crate 使用 edition 2024 |
| 原生链接器 / 系统运行库 | 原生构建 | Linux 编译工具链与 C/C++ 运行库，或 macOS Xcode Command Line Tools |
| uv / Python | 模型准备 | `.python-version` 选择 Python 3.12；uv 管理根目录的 `pyproject.toml` 和 `uv.lock` |
| Node.js + npm | 工作台与 WASM SDK 构建 | Node.js 22.13+；按各 package 已提交的 npm lockfile 安装 |
| PostgreSQL | Web Server | 启动前准备可访问的数据库，应用自动执行迁移 |
| `wasm32-unknown-unknown` target | WASM SDK | 通过 rustup 安装 |
| `wasm-bindgen-cli` | WASM SDK | 必须为 `0.2.125`，与 Rust 依赖一致 |
| 原生加速器运行库 | CUDA/TensorRT/OpenVINO | 必须与构建使用的 ONNX Runtime 发行包匹配 |

新环境先获取仓库：

```sh
git clone https://github.com/YageGeng/docparse.git
cd docparse
rustup show
uv sync --locked
```

默认 Python 环境没有第三方依赖。首次准备 TATR 会通过 uv 在独立、版本固定的 CPU 环境中导出 ONNX，需要联网获取其 Python 依赖。模型准备不要求在根环境中安装 PaddleOCR、Python ONNX Runtime 或 CUDA。

未指定本地覆盖时，原生构建脚本会获取固定版本的 PDFium `chromium/8028` 和 ONNX Runtime。Cargo 锁定的 `ort 2.0.0-rc.13` 发行包表使用原生 ONNX Runtime 1.28；浏览器打包独立固定为 `onnxruntime-web 1.27.0`。模型下载不会安装这两个 runtime。首次构建需要访问依赖 registry 和 runtime 下载地址；准备离线构建时应保留相应缓存与产物。

PDFium 默认使用预生成 bindings，仅启用可选 `bindgen` feature 时需要 libclang。构建时若提供已解包的 PDFium SDK，需要同时设置 `PDFIUM_LIB_PATH` 和 `PDFIUM_INCLUDE_PATH`，指向其 `lib` 和 `include` 目录。WASM 构建会校验固定静态库的 checksum，拒绝不兼容的 SDK。

## 下载并校验模型

### macOS 上的 TATR

当前 `scripts/export_tatr.py` 固定使用 PyTorch CPU index 的
`torch==2.9.1+cpu` 和 `torchvision==0.24.1+cpu`。这些固定版本没有 macOS
wheel，因此 macOS 首次准备 TATR 时无法解析 exporter 环境。已经导出并校验的
TATR 模型仍可使用。

在 Linux 导出机的 checkout 中执行：

```sh
uv run --locked scripts/download_models.py --model tatr-v1.1-all --models-dir /srv/docparse/models
uv run --locked scripts/download_models.py --model tatr-v1.1-all --models-dir /srv/docparse/models --verify-only
```

再将完整安装目录复制到 macOS checkout，`export-host` 换成自己的 SSH 主机：

```sh
mkdir -p models
scp -r export-host:/srv/docparse/models/tatr-v1.1-all models/
uv run --locked scripts/download_models.py --model tatr-v1.1-all --verify-only
```

后续模型准备会跳过已校验的 TATR 文件，不会调用 exporter。也可以按
[TSR 配置示例](crates/tsr/README.md)显式选择 SLANet+ 及其匹配路径/资源；
否则仓库原生配置和浏览器示例仍会请求 TATR。

### 仓库原生配置所需模型

将以下七组模型安装到 `models/`：

```sh
for model in pp-doclayout-v3 tatr-v1.1-all rtdetr-table-cell-wireless \
  pp-ocrv6-medium-det pp-ocrv6-medium-rec pp-lcnet-textline-ori texo
do
  uv run --locked scripts/download_models.py --model "$model" || exit 1
done
```

| 阶段 | 模型目录 | 安装的运行文件 |
| --- | --- | --- |
| 版面 | `pp-doclayout-v3` | `inference.onnx`、`inference.yml`、`model-manifest.json` |
| 表格结构 | `tatr-v1.1-all` | 同上三个文件 |
| 表格单元格 | `rtdetr-table-cell-wireless` | 同上三个文件 |
| OCR 检测 | `pp-ocrv6-medium-det` | 同上三个文件 |
| OCR 识别 | `pp-ocrv6-medium-rec` | 同上三个文件；YAML 包含识别字典 |
| OCR 方向 | `pp-lcnet-textline-ori` | 同上三个文件 |
| 公式 | `texo` | `encoder_model.onnx`、`decoder_model_merged.onnx`、`tokenizer.json`、`model-manifest.json` |

保留每组完整安装文件，只有 ONNX 文件不够。下载器固定上游 revision 与 SHA-256，跳过已校验文件，修复缺失或损坏的文件，最后发布 manifest。TATR 从已校验 checkpoint 在 CPU 上导出；模型准备完成后不再需要 exporter。

仅校验这七组模型，不发起下载：

```sh
for model in pp-doclayout-v3 tatr-v1.1-all rtdetr-table-cell-wireless \
  pp-ocrv6-medium-det pp-ocrv6-medium-rec pp-lcnet-textline-ori texo
do
  uv run --locked scripts/download_models.py --model "$model" --verify-only || exit 1
done
```

### 完整模型目录、替代路径与可选模型

需要全部 14 个已注册模型，包括 SLANet+/SLANeXt、wired 单元格和 PP-FormulaNet Plus S/M/L 时：

```sh
uv run --locked scripts/download_models.py
uv run --locked scripts/download_models.py --verify-only
```

不指定 `--model` 的 `--verify-only` 会校验完整目录。只安装七组模型时，它会因缺少主动省略的替代模型而失败。

```sh
uv run --locked scripts/download_models.py --model texo --models-dir /srv/docparse/models
uv run --locked scripts/download_models.py --model pp-doclayout-v3 --output /srv/docparse/layout
uv run --locked scripts/download_models.py --model pp-formulanet-plus-s
```

`--models-dir` 指定模型根目录；`--output` 仅能和一个显式 `--model` 配合，指定该模型的安装目录。二者都不会改写 TOML 路径。`--force` 会重新下载所选模型的所有文件。统一下载器已经包含 Texo，不需要再执行一次独立下载。

版面模型始终必需。`tsr.mode = "rules_only"` 跳过表格模型；`ocr.policy = "disabled"` 跳过 OCR。同时把 `formula.inline_enabled` 和 `formula.display_enabled` 设为 `false` 可跳过公式模型。HTTP 公式引擎需要外部服务，无需本地公式权重。禁用阶段时仍需保留必填队列设置。

仓库原生配置选择 TATR、`tsr_only`、自动 OCR 和 Texo。原生库默认选择 SLANet+、规则优先回退，关闭 OCR；WASM SDK 默认选择 TATR、规则优先回退，关闭 OCR。浏览器示例启用自动 OCR。切换模型时参阅 [TSR](crates/tsr/README.md) 和[公式引擎](crates/formula/README.md)说明。

## 原生推理后端

CLI 和服务端都应选择一个加速器系列。`embed-web` 独立于后端，可与其组合。

| 后端 | 编译 feature | 部署要求 |
| --- | --- | --- |
| CPU | 不传加速器 feature | 不需要 GPU |
| NVIDIA CUDA | `cuda` | 匹配的 NVIDIA 驱动；锁定的 x86_64 Linux 自动下载 GPU 包使用 CUDA 13、cuDNN 9（[要求](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider#requirements)） |
| NVIDIA TensorRT | `tensorrt` | 包含 CUDA；额外需要兼容的 TensorRT 运行库与可写的 `runtime.tensorrt_cache_dir` |
| Apple CoreML | `coreml` | 兼容 macOS；为保持模型兼容，公式推理明确使用 CPU |
| Apple Metal | `metal` | CoreML 的 CPU/GPU compute units，不使用 ANE；公式使用 CPU |
| Intel OpenVINO | `openvino` | 需提供包含 OpenVINO 的兼容 ONNX Runtime；锁定的自动下载发行包表没有该后端 |

版面、OCR、TSR 和公式共享编译选择的后端，CoreML 公式与 TensorRT 图兼容策略如上述说明。原生 TOML/环境变量的 `execution_provider` 覆盖及模型前缀后端 feature 会被拒绝。CUDA、CoreML/Metal、OpenVINO 系列互斥，不要使用 `--all-features`。显式选择的加速器无法初始化时，启动失败。

不同原生后端使用独立的 `CARGO_TARGET_DIR`。输出目录中复制或链接的 ONNX Runtime 运行库必须属于当前构建。

例如，构建 CUDA CLI：

```sh
CARGO_TARGET_DIR=target/cuda cargo build --locked --release -p docparse-cli --features cuda
./target/cuda/release/docparse parse input.pdf --config docparse.toml --output result.json
```

OpenVINO 或自定义 CUDA 发行包需要在编译时提供匹配的 runtime，例如：

```sh
ORT_LIB_LOCATION=/opt/onnxruntime/lib ORT_PREFER_DYNAMIC_LINK=1 \
  CARGO_TARGET_DIR=target/openvino cargo build --locked --release \
  -p docparse-cli --features openvino
```

runtime 必须提供这些 crate 要求的原生 API（API 28），并包含所选 provider。需要安装其 provider 依赖，将动态库随部署交付。只安装 Python `onnxruntime` wheel 不会配置这个 Rust 构建。

TensorRT 处理支持的版面、OCR、表格和 Texo encoder 图，不支持的部分回退到 CUDA；Texo merged decoder、PP-FormulaNet、SLANet+/SLANeXt 继续使用 CUDA。首次启动会生成 engine，可能需要数分钟；后续复用 `runtime.tensorrt_cache_dir`。改变 GPU 型号、ONNX Runtime 或 TensorRT 后需要重建缓存。生成的 provider 库依赖决定所需 CUDA/cuDNN/TensorRT ABI，应在 Linux 使用 `ldd` 检查，不应直接照搬旧性能报告的版本。参阅官方 [CUDA 要求](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider#requirements)和 [TensorRT 要求及缓存说明](https://onnxruntime.ai/docs/execution-providers/TensorRT-ExecutionProvider.html#requirements)。

## CLI

### 编译与解析

CPU 基线需要已下载模型，不需要数据库或 Node.js：

```sh
cargo build --locked --release -p docparse-cli
./target/release/docparse --help
./target/release/docparse inspect-model --config docparse.toml
./target/release/docparse parse input.pdf --config docparse.toml \
  --format json --output result.json
./target/release/docparse parse input.pdf --config docparse.toml \
  --format markdown --view semantic --output result.md
```

package 名称是 `docparse-cli`，可执行文件名称是 `docparse`。`cargo build` 不会将其加入 `PATH`。`target/release/` 是默认输出目录；覆盖 Cargo target 目录时使用实际路径。

`--output` 原子写入文件；覆盖已有文件需要 `--force`。不传 `--output` 时写到 stdout。`--overlay-dir diagnostics` 增加逐页 PNG/SVG 诊断文件。`--continue-on-error true|false` 覆盖可恢复页面失败的继续策略；致命文档/传输错误仍会终止解析。

需要安装到 PATH 时：

```sh
cargo install --path crates/cli --locked
```

按需为构建或安装命令添加所选后端 feature。安装可执行文件不会安装模型，也不会让构建机的 PDFium 缓存自动变成可迁移资源。移动到其他机器前，完成[原生产物打包](#原生产物打包)。

## Web Server 与 HTTP 工作台

### 1. 准备 PostgreSQL、存储和配置

创建 PostgreSQL 数据库及有权执行应用迁移的角色。应用不会自动安装 PostgreSQL 或创建数据库。服务连接数据库后、接受任务前，会自动执行待处理的 SeaORM migration；独立的[迁移 CLI](crates/migration/README.md)是可选管理工具。

通过服务环境/secret manager 提供 `DOCPARSE_DATABASE__URL`，覆盖示例 `database.url`，所有服务角色都需要这个连接。`docparse.toml` 中的账号口令只用于本地示例。每个副本都有独立连接池，应按 PostgreSQL 容量规划总连接数。

选择可写存储目录，如 `/srv/docparse/shared`。多个 API/worker 副本必须看到相同文件，挂载路径可以不同。存储需要一致的读取、原子发布、文件/目录同步和跨客户端文件锁。各容器独立的本地目录不能替代共享存储。

### 2. 先构建工作台，再编译两个原生可执行文件

仓库配置选择 `server.webui = "embedded"`，需先构建前端再编译 release 服务：

```sh
npm ci --prefix packages/web
npm run build --prefix packages/web
cargo build --locked --release -p docparse-core \
  --features pdfium-ipc --bin docparse-pdfium-worker
cargo build --locked --release -p docparse-server --features embed-web
```

将 `target/release/docparse-server` 和 `target/release/docparse-pdfium-worker` 放在同一目录交付。二者必须来自同一源码版本/协议。只构建 server package 不会生成 worker 可执行文件，也没有进程内回退方案。

CUDA 构建使用独立目录，并为两个产物选择相同后端：

```sh
CARGO_TARGET_DIR=target/cuda cargo build --locked --release -p docparse-core \
  --features pdfium-ipc,cuda --bin docparse-pdfium-worker
CARGO_TARGET_DIR=target/cuda cargo build --locked --release -p docparse-server \
  --features embed-web,cuda
```

按硬件换成 `coreml`、`metal`、`tensorrt` 或 `openvino`，保留 worker 的 `pdfium-ipc`，嵌入工作台时保留 `embed-web`。前述 TensorRT/自定义 runtime 的准备要求同样适用。

### 3. 启动与检查

通过环境提供数据库 URL 后，启动组合 API/worker：

```sh
./target/release/docparse-server --config docparse.toml \
  --storage-dir /srv/docparse/shared --role all
```

CUDA 示例改用 `./target/cuda/release/docparse-server`。

仓库配置监听 `127.0.0.1:8080`，`server.api_prefix = "/api/v1/docparse"`；代码默认值是 `"/api"`。本指南命令统一使用仓库配置的前缀：

```sh
curl --fail http://127.0.0.1:8080/api/v1/docparse/health
curl --fail http://127.0.0.1:8080/api/v1/docparse/ready
curl --fail http://127.0.0.1:8080/api/v1/docparse/openapi.json --output openapi.json
```

工作台地址为 `http://127.0.0.1:8080/api/v1/docparse/webui/`，Scalar 为 `http://127.0.0.1:8080/api/v1/docparse/docs`。Readiness 检查数据库/schema 和存储，不代表真实 PDF 已解析成功。默认 Scalar 页面从 jsDelivr 加载 JavaScript。

用新 UUID 上传真实 PDF，保存该 UUID；重试同一次上传时复用它：

```sh
job_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
curl --fail -i -H "Idempotency-Key: $job_id" \
  -F 'file=@input.pdf;type=application/pdf' \
  http://127.0.0.1:8080/api/v1/docparse/jobs
curl --fail "http://127.0.0.1:8080/api/v1/docparse/jobs/status?id=$job_id"
curl --fail -N "http://127.0.0.1:8080/api/v1/docparse/jobs/events?id=$job_id"
```

上传应返回 `202`。等待 `succeeded`（或检查 `failed`），再结束 SSE 客户端并下载结果：

```sh
curl --fail "http://127.0.0.1:8080/api/v1/docparse/jobs/result?id=$job_id" \
  --output result.json
curl --fail "http://127.0.0.1:8080/api/v1/docparse/jobs/result?id=$job_id&format=markdown" \
  --output result.md
```

普通业务 JSON 响应以 `ApiResponse` 包装；CLI/WASM 的文档是直接的 `DocumentResult`。SSE 重连不会取消持久化任务。入口代理需要保留上传/SSE 流式传输与压缩响应头。

### 工作台交付与开发

| 模式 | 配置/构建 | 运行时文件 |
| --- | --- | --- |
| 嵌入 | `webui = "embedded"`；先构建 `packages/web`，再以 `embed-web` 构建服务 | release 服务包含 UI，运行时无需工作台 `dist/` |
| 磁盘 | 将该值替换为 `webui = { disk = "packages/web/dist" }`；构建前端，服务不带 `embed-web` | 完整前端目录与可读 `index.html`；相对目录以配置文件为基准 |
| 仅 API，不提供 UI | 移除 `server.webui`，不带 `embed-web` | 无工作台资源 |

只保留一个 `webui` 值。debug 嵌入构建仍读取构建时的前端目录，UI 自包含行为适用于 release。

浏览器构建的 `VITE_API_PREFIX` 必须与 `server.api_prefix` 一致。需要覆盖时，将 `packages/web/.env.example` 复制为 `packages/web/.env.local`，在构建前设置前缀；改变前缀后重新构建前端和嵌入服务。`VITE_API_TARGET` 控制开发代理，不是生产后端 URL。

开发 UI 时先启动配置好的原生后端，再在另一个终端执行：

```sh
npm run dev --prefix packages/web
```

访问 `http://127.0.0.1:5173/api/v1/docparse/webui/`。Vite 是开发前端服务器，不是生产解析服务。

### 拆分 API 与 worker 角色

分别使用终端或服务单元，配置相同数据库与共享存储：

```sh
./target/release/docparse-server --config docparse.toml \
  --storage-dir /srv/docparse/shared --role api
./target/release/docparse-server --config docparse.toml \
  --storage-dir /srv/docparse/shared --role worker
```

`api` 提供 HTTP，不加载模型，也不要求 worker 可执行文件。`worker` 消费队列任务，需要模型与同目录 PDFium worker，不监听 HTTP。`all` 同时执行二者。没有可用解析 worker 时，API-only 的 readiness 仍可能成功，因此必须验证真实任务完成。

需要通过入口代理对外监听时，可设置 `DOCPARSE_SERVER__HOST=0.0.0.0`。入口应提供 TLS、认证、授权及浏览器 CORS 策略。任务 UUID 只是标识符；历史记录在部署内共享，不具备租户隔离。SIGTERM/Ctrl-C 会触发服务收尾并关闭 PDFium 子进程，需预留清理时间。

## WASM 浏览器 SDK

### 1. 安装 target 和固定工具

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.125 --locked
npm ci --prefix packages/wasm-web --ignore-scripts
```

### 2. 构建完整 SDK

```sh
npm run build --prefix packages/wasm-web
npm run check --prefix packages/wasm-web
npm run check:example --prefix packages/wasm-web
```

构建脚本依次执行 `docparse-web` release Cargo 构建、wasm-bindgen、npm 固定的 Binaryen `wasm-opt -O4`、TypeScript、import 校验、runtime/许可打包。脚本明确使用仓库的 `target/` 目录。不需要原生加速器 feature、Emscripten SDK 或系统安装的 `wasm-opt`。

直接执行 `cargo build -p docparse-web --target wasm32-unknown-unknown` 只生成 Rust 产物，不会生成可部署的浏览器 SDK。需要交付完整 `packages/wasm-web/dist/`，保留相对结构和 `build-manifest.json`。它包含 SDK/Worker、Rust WASM、ORT Web JS/WASM、WASI adapter 与许可；模型仍需独立交付。

### 3. 运行本地示例

下载所需模型并构建 SDK 后：

```sh
npm run example --prefix packages/wasm-web
```

访问 `http://127.0.0.1:8768/example/`。服务器仅监听 loopback、提供静态资源，不需要 PostgreSQL 或原生服务。`PORT` 可改变端口。示例默认启用自动 OCR、TATR 和 Texo；切换可选 PP 公式需要 `pp-formulanet-plus-s`。示例允许 CPU 回退并显示实际后端。

### 4. 静态托管并集成 SDK

为自己的应用，将完整 SDK 复制到如 `/assets/docparse/` 的静态目录，按传给 SDK 的 URL 托管启用的模型。JavaScript/MJS 使用 JavaScript MIME，WASM 使用 `application/wasm`。跨域资源需要 CORS。WebGPU 要求安全上下文（HTTPS 或 localhost）及兼容浏览器/设备；显式单线程 `"wasm"` 推理不需要跨源隔离。

以下最小示例只启用版面与本地表格规则，关闭 OCR/公式，只需要 `pp-doclayout-v3`。`file` 是调用方选择的浏览器 `File`：

```javascript
import { prepareModels } from "/assets/docparse/index.js";

const parser = await prepareModels({
  artifacts: {
    kind: "urls",
    model: "/models/pp-doclayout-v3/inference.onnx",
    config: "/models/pp-doclayout-v3/inference.yml",
    manifest: "/models/pp-doclayout-v3/model-manifest.json",
  },
  executionProvider: "wasm",
  config: {
    render: { workers: 1, queue_size: 2 },
    layout: { queue_size: 1 },
    tsr: { mode: "rules_only", queue_size: 1, cell_detection: { queue_size: 1 } },
    ocr: {
      policy: "disabled",
      detection: { queue_size: 1 },
      recognition: { queue_size: 1 },
      orientation: { queue_size: 1 },
    },
    formula: { queue_size: 1, inline_enabled: false, display_enabled: false },
  },
});
try {
  const document = await parser.parse(new Uint8Array(await file.arrayBuffer()));
  const markdown = await parser.render(document, "markdown");
  console.log(document.pages.length, markdown);
} finally {
  await parser.close();
}
```

使用 `executionProvider: "webgpu"` 请求加速。SDK 的 `allowCpuFallback` 默认是 `false`，允许 GPU 不可用时回退到 CPU，需要显式开启。多个 PDF 可以复用已准备 parser，释放时调用 `close()`；模型或 provider 设置改变后重新创建。

完整解析需要提供 `tsrArtifacts`、`tsrCellArtifacts` 及启用的 `ocrArtifacts`。本地公式 preset 使用 `/models/texo/` 或 `/models/pp-formulanet-plus-s/`，改变 URL 可传 `formulaArtifacts`。不支持原生文件路径、profile 或环境覆盖。各模型来源示例见 [SDK 指南](packages/wasm-web/README.md)。

原样部署仓库示例时，需要保留 `/example/`（HTML、CSS、构建后的 `example/dist/`）、`/dist/`（SDK）及模型路由。示例版面 URL 是特殊的 `/models/inference.onnx`、`/models/inference.yml` 和 `/models/model-manifest.json`，由 `serve-example.mjs` 映射到 `models/pp-doclayout-v3/`；其他模型使用 `/models/<name>/`。静态服务器需复现这些映射，或修改宿主页面 URL。只复制 `example/dist/` 不够。

ORT Web 仍以全局 guard 串行执行实际推理/readback，增加 session 不保证 GPU 并行。大 PDF/模型仍受浏览器内存限制。HTTP 公式引擎需要合适的 CORS、HTTPS/混合内容处理，并向浏览器之外发送公式裁剪图。

## 配置与资源限制

原生优先级为：默认值 → 主 TOML → 选定 profile → `DOCPARSE_...` 环境变量 → 显式调用/CLI 覆盖。不传 `--config` 时，CLI 只读取 `./docparse.toml`。相对模型、日志、缓存路径以主配置文件为基准；移动配置却不调整模型/路径，会改变加载位置。`--storage-dir` 是独立服务进程选项，部署时使用绝对路径。

| 必填设置 | 含义 |
| --- | --- |
| `render.workers` | 原生 PDFium 进程池大小；WASM 必须为 1 |
| `render.queue_size` | 跨文档未完成页面数，直到资源真正清理才释放 |
| `layout.queue_size` | 等待处理的版面输入 |
| `tsr.queue_size`、`tsr.cell_detection.queue_size` | 等待处理的结构/单元格输入 |
| `ocr.detection.queue_size`、`ocr.recognition.queue_size`、`ocr.orientation.queue_size` | 等待处理的 OCR 输入 |
| `formula.queue_size` | 各公式 consumer 共享的等待裁剪图容量 |

所有容量必须为正，禁用阶段的队列也不例外。`session_size` 控制独立模型 session，本地公式引擎使用 `formula.engine[].worker_size`。`batch_size` 限制单次取走的已就绪输入，不会等待凑满批次。增加 session 会复制模型内存。小机器可先将每个本地模型 session/consumer 设为 1，用真实 PDF 测量后再提高并发。

仓库原生配置使用五个 PDFium worker、多个模型 session 和四个 Texo consumer。这是吞吐示例配置，不是最低硬件要求。`server.max_uploads` 独立限制同时上传数（文件中为 100，代码默认 4），不控制解析并发。原生文档 admission 由 `render.workers + render.queue_size` 约束；完整结果内存仍随 PDF 大小增长。

`runtime.optimization_level` 与 `runtime.memory_pattern` 是全局 ONNX 设置。`runtime.continue_on_error` 控制可恢复页面失败后的继续策略。`server.jobs`、`server.pdfium_workers`、`runtime.stage_pages`、`sessions`、`formula.enabled` 等旧字段会被拒绝。

服务日志使用 `log.directives` 和可选 `log.file`；仓库配置向配置旁的 `logs/docparse.log` 追加写入，需保证目的地可写。CLI 诊断使用 `RUST_LOG` 和 stderr。除退出码外，也应检查启动/模型错误和页面告警。资源生命周期、重试、错误行为见[公共运行时](crates/common/README.md)与[服务端](crates/server/README.md)说明。

## 原生产物打包

`embed-web` 不会嵌入模型和原生库。默认原生 ONNX Runtime core 静态链接，但构建在可执行文件旁生成的 provider 库仍需交付；自定义动态链接 runtime 还需要其 core 库。

从 CPU release 构建准备 Linux 服务包：

```sh
mkdir -p release/docparse/bin release/docparse/lib
cp target/release/docparse-server target/release/docparse-pdfium-worker release/docparse/bin/
cp -L target/release/deps/libpdfium.so release/docparse/lib/
for library in target/release/*.so*
do
  [ -e "$library" ] || continue
  cp -L "$library" release/docparse/lib/
done
```

CLI 包改为复制 `target/release/docparse`，不需要两个服务可执行文件。CUDA/TensorRT 使用对应 target 目录。`cp -L` 会复制真实运行库字节，避免把指向构建机缓存的符号链接交给部署机。macOS 则打包 `target/release/deps/` 的 `libpdfium.dylib` 及生成的 `.dylib` 文件。

安装模型、配置、notice 后，服务目录可以如下组织：

```text
/srv/docparse/
  bin/docparse-server
  bin/docparse-pdfium-worker
  lib/                         # PDFium and emitted provider/runtime libraries
  models/                      # Complete enabled model installations
  docparse.toml
  shared/                      # Persistent PDF/result storage
  logs/
  cache/tensorrt/               # If using TensorRT
  webui/                       # Only when serving a disk build
```

保留根目录和随包第三方 notice。配置在 `/srv/docparse/docparse.toml` 时，`models/...` 路径可直接使用。磁盘 UI 需要将 `server.webui.disk` 改成安装后的目录。

在服务环境设置 `PDFIUM_LIB_PATH=/srv/docparse/lib`。将该目录及 GPU/OpenVINO 依赖目录加入系统动态库搜索路径（Linux 为 `LD_LIBRARY_PATH`，macOS 适用场景为 `DYLD_LIBRARY_PATH`）。否则 PDFium 会查找固化的构建缓存路径、相邻文件和系统目录；在 checkout 中运行成功不代表迁移后的包可运行。

使用绝对配置/存储路径启动安装后的可执行文件。Linux 用 `ldd`、macOS 用 `otool -L` 检查运行依赖，包括生成的 provider 库。应在部署机验证，不能依赖构建机缓存。替换二进制时保留 PostgreSQL 与共享存储，并同步更新 server/worker。

## 部署验证与排障

| 检查项/现象 | 处理 |
| --- | --- |
| 缺少模型、manifest/hash 校验失败 | 对所选模型执行 `--verify-only`；检查相对配置目录的路径；重跑下载器修复 |
| TATR exporter 缺少兼容的 macOS wheel | 在 Linux 准备 TATR 后复制完整、已校验的安装目录，或显式选择其他支持的 TSR 模型 |
| CLI 冒烟 | 执行 `inspect-model`，再解析真实 PDF 输出 JSON/Markdown，检查告警/错误 |
| 服务冒烟 | 检查 health/ready，上传真实 PDF，确认 `succeeded`，取回两种格式，刷新工作台确认持久化历史 |
| PDFium worker 缺失/协议不兼容 | 以 `pdfium-ipc` 构建 `docparse-core`，将匹配 worker 放在 server 旁 |
| `server.webui` 初始化错误 | 先构建前端；`embedded` 匹配 `embed-web`，或提供可读的磁盘 `index.html` |
| 工作台资源/API 404 | 统一构建的 `VITE_API_PREFIX`、服务前缀与代理路由，改变后重新构建 |
| 任务一直 queued | 确认 worker 角色运行，且使用相同数据库/共享存储；API-only readiness 不够 |
| 原生 provider 初始化失败/找不到库 | 检查实际运行依赖和编译 feature，安装匹配运行库或重建 CPU 版本 |
| GPU 内存不足 | 提高并发前，先降低 session/consumer、batch 与队列输入数 |
| WASM 加载/import 错误 | 部署完整 SDK `dist/`；保持固定 wasm-bindgen/ORT 版本、相对文件与 MIME |
| 浏览器模型 URL 返回 HTML | 修正静态模型映射，不对模型/JS/WASM 资源使用 SPA fallback |
| WebGPU 不可用 | 使用支持的安全上下文/设备，或显式选择 `"wasm"` / 允许 CPU 回退 |

浏览器验收应使用真实 PDF、生产 SDK/原生服务和其配置的 provider，不能替换成 fixture 解析后端。[HTTP 工作台](packages/web/README.md)、[服务端](crates/server/README.md)和 [WASM SDK](packages/wasm-web/README.md)说明中有现有验收命令。编译成功或 readiness 成功不能单独证明端到端验收通过。
