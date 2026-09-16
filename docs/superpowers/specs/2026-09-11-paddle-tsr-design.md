# Paddle TSR ONNX 接入方案

状态：已完成实现与本机真实端到端验收；当前分支 feat/paddle-tsr，基于 main 的 666a69f。在当前工作区实施，不创建 worktree；本轮未获新的 commit/push 授权。

## 选型与依据

| 模型 | 本轮判断 |
| --- | --- |
| SLANet | 同时预测结构和位置，是轻量旧版本；不作为新增默认。 |
| SLANet_plus | 增强无线和复杂表格，仍输出结构与位置；选作默认模型。 |
| SLANeXt_wired / wireless | 分有线、无线权重，结构准确率更高，但官方声明其单元格位置输出无效，需要独立 cell detector；本轮不增加分类器和额外检测模型。 |

来源：[官方 TSR 模块](https://www.paddleocr.ai/main/en/version3.x/module_usage/table_structure_recognition.html)、[官方 ONNX 仓库](https://huggingface.co/PaddlePaddle/SLANet_plus_onnx)、[PaddleX 前后处理](https://github.com/PaddlePaddle/PaddleX/tree/develop/paddlex/inference/models/table_structure_recognition)。

采用 Apache-2.0 的 PaddlePaddle/SLANet_plus_onnx，固定 revision `7dbe640e127602bf506815e822c09758de73c482`：

- inference.onnx：7,782,138 字节，SHA-256 `7790c0c13ce064782c9d22ebeb16b4da8216f83d3ba576da962c106ef58386da`。
- inference.yml：SHA-256 `8a6372d3269a6f112fe13a2da7952a84da6e112c10a3146cbb43de5bd01d19fa`。
- 已验证 ONNX opset 17；输入 `x` float32 NCHW，按比例缩放最长边 488，归一化后右下补零至 488×488。
- PDFium RGB 转 BGR；LINEAR resize；mean=[0.485,0.456,0.406]，std=[0.229,0.224,0.225]。
- 输出 fetch_name_0=[1,T,8]，fetch_name_1=[1,T,50]。argmax 解码，跳过 sos、遇 eos 停止，位置乘以原裁剪最长边。
- 原始预测可能轻微越出裁剪，模型适配器将有限位置转为裁剪内 AABB。core 适配器复用 CellGrid 解析模型拓扑，再依据各行列的预测包围边缘形成共享分界，避免独立位置头的重叠与长标签裁切。
- Predicted 路径保留模型独立位置，并允许有原文、分隔线依据的行列、跨度及表头修正；Block 边界和原始文本事实保持不变。每个词具有唯一引用，所属单元格仍须覆盖至少 80% 的文字墨迹。Declared 外部输入继续严格校验，不自动修补。具体修正规则与新验收见 [TSR 后处理更新](./2026-09-11-tsr-postprocessing.md)。

## 模块和依赖

新增 docparse-tsr，与 layout 同为独立 crate；复用 layout 中的图像/几何、计时、WASM Future 与 CPU 执行边界，以及现有模型文件容器和校验机制。不新增泛化规则注册框架。

SlanetPlusEngine 提供 `from_artifacts`、原生 `from_config`、`predict`。TSR crate 只接收图像并返回结构、单元格框及置信度，不依赖 core 的 Block/TextItem。core 实现现有 TableStructureEngine trait 的适配，继续走同一填字/校验路径。

ONNX 图包含 Loop。第一版模型明确使用 CPU：原生 ORT CPU、浏览器 ORT WASM CPU；layout 保留其独立配置的 CoreML/WebGPU。每个 parser 一个 TSR session，队列及实际执行资源在调用方取消后安全释放；未实测吞吐不足前不增加 session pool 配置。

## 配置与模式

新增 `[tsr]`：mode、模型/配置/manifest 路径、max_in_flight=2、timeout_ms=60000。

- `external_only`：默认，每个 layout table 使用内置 TSR；保持现有枚举名称和输出兼容性。
- `fallback`：完整本地恢复/文字覆盖校验成功则使用规则，否则请求 TSR 一次。
- `rules_only`：显式兼容开关，不加载 TSR 模型。

Per-call ParseOptions.table 改为可选覆盖：缺省继承 parser 配置，显式选项保持原有 builder 调用方式。调用方注入的 table_engine/onTableStructure 优先于内置模型。TSR 模型失败保留原文并告警，不静默切换本地拓扑。缺失默认模型在初始化时明确失败。

Web 增加显式 tsrArtifacts，与现有 layout 的 URLs/bytes 模型输入一致；生产 example 指向本地模型路径。TSR 运行于现有 Worker 内，不要求主线程伪造结构响应。

## 验收

1. 固定模型 provenance/hash、50 类词典、输出 shape 和有限值检查；预处理与 OpenCV/PaddleX 定义对照。
2. 保留规则路径全部旧 badcase；默认模式与规则模式分开统计，不能用规则结果替代模型结果通过验收。
3. 原生和 WASM 编译、Clippy、真实模型推理、循环解析与取消检查。
4. 默认 TSR 配置使用用户四份 PDF、生产 PDFium/layout/TSR/填字/渲染跑端到端；记录每表结果来源、模型错误和覆盖失败、关键页结构以及阶段耗时。
5. 真实模型的错误结构与未恢复表格如实列入报告，不以置信度或流程运行成功冒充表格准确率。


## 初版集成验收结果（后处理更新前，2026-09-11）

全部运行使用本机生产 PDFium、固定 PP-DocLayoutV3、固定 SLANet_plus ONNX 和真实原文填充路径；没有替换模型返回值。原生使用配置中的 CoreML layout 与 CPU TSR，浏览器使用实测 WebGPU layout 与 CPU/WASM TSR。模型文件不随代码提交，需通过下载脚本安装。

| PDF | 页数 | 原生默认 TSR：通过校验/检测表格 | 浏览器默认 TSR：通过校验/检测表格 | 原生规则优先：通过校验/检测表格 |
| --- | ---: | ---: | ---: | ---: |
| 2604.18584v1 | 32 | 10/16 | 10/18 | 16/16 |
| 2603.01919v2 | 23 | 12/18 | 13/18 | 17/18 |
| EnterpriseRAG | 24 | 5/11 | 7/11 | 10/11 |
| 2303.18223v16 | 144 | 13/24 | 12/24 | 23/24 |
| 合计 | 223 | 40/69 | 42/71 | 66/69 |

- 默认 TSR 的所有检测表格均调用了真实模型；原生 69 次、浏览器 71 次。两个 layout 执行后端的检测数量略有不同，分母应分开看。
- 原生规则优先仅调用 TSR 4 次，其中 1 个表格成功补回；浏览器两页补充用例含 5 个 table，2 次模型调用、1 个模型恢复，最终 4 个表格结构化。
- 原生 MathNet 第 8、10 页的模型结果分别为 20×7、10×8，全部单元格文字及跨度与既有参考一致。
- 数量表示通过结构和原文守恒校验的覆盖率，不是表格语义准确率。浏览器已恢复结果中，41 个可与原规则结果对照，其中 27 个单元格文字及跨度一致，14 个有差异；规则结果也不是完整人工金标准，不能直接把差异数当错误率。
- 有差异的浏览器页面：MathNet 3/20/24/28，2603 的 6/23，EnterpriseRAG 2/4/10，综述 35/67（两表）/82/88。模型输出仍需按业务场景审阅。
- 未恢复结果主要是位置或拓扑与原文不一致、缺失网格位置、空位置预测或序列未正常终止；保持原文并发出表格告警，不静默改成规则结果。少量检测区域没有 Native/OCR 文字，TSR 结构不能替代 OCR。
- `rules_only` 对四份 PDF 全量及 CPU 重复解析、符号用例的 9 轮回归通过，原有表格和警告与基线精确一致。
- 真实模型生命周期测试覆盖模型请求发出后的取消、新 Worker 恢复、会话复用，以及 1ms 限额的超时。同步 CPU 推理延迟定时器的问题已通过返回前检查实际耗时修复；超时的结果不发布，物理计算资源直到实际结束才释放。
- 可视复核确认生产 UI 展示真实 PDF 栅格和模型表格。测试截图在 SVG 图像解码后获取，避免把尚未绘制的预览误判为空白页。

### 可复现入口

Run these commands from the repository root. Keep the example server running in this terminal:

```sh
rtk uv run scripts/download_models.py --model slanet-plus
rtk cargo test -p docparse-tsr -- --include-ignored
rtk npm run build --prefix packages/wasm-web
rtk npm run example --prefix packages/wasm-web
```

Run browser acceptance from another terminal at the repository root:

```sh
rtk proxy node crates/web/tests/paddle_tsr.mjs /absolute/path/to/document.pdf
rtk proxy node crates/web/tests/paddle_tsr.mjs --mode fallback --output packages/wasm-web/test-results/paddle-tsr/browser-fallback.json /absolute/path/to/document.pdf
```

原生完整验收使用 `crates/core/tests/paddle_tsr_e2e.rs`：以 `TSR_E2E_PDFS` 提供 JSON 路径数组，使用 `cargo test -p docparse-core --features layout-coreml --test paddle_tsr_e2e --release -- --ignored --nocapture`。设置 `TSR_E2E_MODE=fallback` 可复测规则优先流程。

本地明细保存在 `packages/wasm-web/test-results/paddle-tsr/acceptance-summary.json`、`native-aligned/`、`native-fallback/`、`browser-default/`、`rules-regression.json` 和 `lifecycle.json`。这些运行产物未纳入版本控制。

默认配置按用户要求保持 `external_only`。对于当前四份文档，实测 `fallback` 的结构化覆盖率更高；提供该选项供实际使用选择。

### 最终检查

- Native 默认测试集：293 项通过、7 项按原有模型/环境要求忽略；涉及本次模型的真实 ONNX 测试和真实 PDF 端到端已另外显式执行。
- 原生 Clippy、wasm32 Clippy、格式、平台条件边界、SDK/example 类型检查通过。
- 最终浏览器构建通过，优化后 WASM 7,032,121 字节，145 个导入校验通过。
- 外部调用方输入契约的 6 类检查仍通过；最新真实模型生命周期验证了中途取消、恢复、复用和超时。
- 保持工作区改动供审核，本轮未创建 commit 或 push。

## 后处理更新

`2303.18223v16.pdf` 已在 Native 和真实 Web 浏览器的 `external_only` 模式下达到 24/24 表格结构化；上述初版覆盖率用于保留改动前基线。新版还检查关键行列、合并表头、跨行标签和原文完整性，详见 [后处理实现与验收](./2026-09-11-tsr-postprocessing.md)。
