# Paddle TSR ONNX 实施计划

> 使用 executing-plans 在当前分支逐项实施；不使用 worktree，不自行提交。用户已授权完整实现与真实端到端验收。

**目标：** 默认使用内置 Paddle SLANet_plus ONNX，并保留规则失败后 TSR 的模式。
**架构：** docparse-tsr 独立推理，core 适配既有 TableStructureEngine；model 输入只负责结构，原文及布局由既有校验控制。
**技术栈：** Rust / ort / ndarray / wasm-bindgen / 现有 Worker / Python OpenCV 参考。
**设计：** docs/superpowers/specs/2026-09-11-paddle-tsr-design.md。

## 1. 固定模型与独立引擎

- [x] 核验官方模型家族、ONNX revision、许可、SHA-256 与真实输入输出；CPU 实际运行成功。
- [x] 扩展现有 download_models 脚本支持选择 slanet-plus，复用下载/校验逻辑。
- [x] 新增 crates/tsr，复用 ModelArtifacts/ModelContract；固定输入输出校验，BGR/LINEAR/normalize/pad 前处理和 token/box 后处理。
- [x] 先编写预处理和解码检查，再实现引擎；保存真实模型输出与独立 Python 参考。
- [x] 通过 Native ORT 和 Web WASM 的单 session 执行边界，保持 cancellation-safe owned buffers。

## 2. 配置与 core

- [x] 新增 TsrConfig 和共享 TableMode；默认 external_only，fallback 和 rules_only 显式配置；相对模型路径按配置目录解析。
- [x] DocParser 支持注入或加载 TSR；ParseOptions 未指定 table 时继承实例配置，显式 override 和用户引擎优先。
- [x] 保持冻结 Block 边界和既有 TableStructureSource/引用格式；加入独立 TSR 阶段计时与上下文日志。
- [x] 既有规则测试显式选择 rules_only，新增默认模型、fallback 选择和初始化失败检查。

## 3. Web 和真实模型输入

- [x] WebParser 初始化接收独立 TSR artifacts；SDK/Worker 下载并持有模型，移除“外部模式必需主线程回调”的旧前置限制。
- [x] example 提供 TSR/规则失败后 TSR/规则模式，默认 TSR；使用生产 SDK 与真实模型。
- [x] 运行 WASM build、example 类型检查，验证 CPU TSR 与 WebGPU layout 同 Worker 协作。

## 4. 验收与交付

- [x] 原生测试、Clippy、wasm32 Clippy、平台边界及格式检查通过。
- [x] 使用默认 TSR 配置对四份真实 PDF 跑原生和浏览器端到端，保留每表状态/来源/结构和耗时。
- [x] 规则模式复测既有 223 页基线；对默认 TSR 的关键页做可视审查并列出实际模型限制。
- [x] 更新 README、spec/plan 和最终中文报告，明确模型结果及尚未恢复的表格。

## 运行中记录

- 独立引擎已实现，官方 ONNX 真模型测试通过；MathNet 第 8、10 页在原生真实流水线中分别产生 20×7、10×8，单元格文字和跨度与既有参考一致。
- `[tsr]`、默认模型与 per-call 覆盖已贯通；Web model artifacts、真实 Worker 模型和 UI 模式选择已实现，第一次浏览器真模型检查完成 2 次模型调用。
- 首轮原生全量 223 页、69 个 table 均调用了模型；28 个通过完整源文字校验。多数失败是位置框与原文不完全对齐，不是推理加载失败。
- 正在验证有幅度限制的源文字边界对齐：不放宽最终覆盖阈值，不改写模型拓扑，不静默使用本地规则。
- 初始报告：packages/wasm-web/test-results/paddle-tsr/native/report.json。后续报告使用 native-aligned 与 browser-default 独立路径，避免混淆运行版本。

## 完成记录

已完成独立模型模块、配置及两种生产流程，保留 rules_only 兼容模式。原生和浏览器默认 TSR 均已完成 223 页真实端到端；详细覆盖率、未恢复原因及对照差异见设计文档的实际验收结果。

真实生命周期检查确认取消后恢复、稳定复用和超时结果拒绝；既有规则路径 9 轮回归通过。本轮不创建 commit，不推送。

最终默认测试集 293 项通过、7 项忽略；原生/Web Clippy、格式、平台边界和生产构建通过。具体模型和端到端检查已显式执行，结果见 spec。
