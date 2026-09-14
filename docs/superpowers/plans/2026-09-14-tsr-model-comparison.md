# TSR 模型对照实施计划

目标与约束见 [设计](../specs/2026-09-14-tsr-model-comparison.md)。直接在当前分支执行，不提交。

- [x] 配置与模型供应：先加入 `tsr_only` 兼容、模型选择与检测模型路径的失败测试；扩展 `crates/config` 与 `scripts/download_models.py`，固定官方 ONNX 身份并下载校验。
- [x] 推理：复用 `crates/tsr` 的预处理与 Native/WASM SessionRunner，扩展 SLANeXt 512 输入及 RT-DETR 640 输入，保留真实执行结束前的资源所有权；分别采集 timings。
- [x] 结构融合：先测试乱序检测框、合并关系和缺失框，扩展 `TsrTableInput` 并在 core 解码阶段将独立几何与逻辑结构匹配，再进入既有文字填充与验证。
- [x] 解析器集成：扩展 artifact 输入、原生加载、浏览器 artifact 传递和既有 benchmark 预热，测试配置设为 `tsr_only`。
- [x] 真实对照：采集上述目录的表格，运行三组生产 TSR 流程，保存原图、原始预测、结构结果及分阶段测量，人工核验代表性行列、跨度和文字归属。
- [x] 验收：运行相关 Rust/Python 回归、格式、Clippy、WASM 编译和边界检查；报告各组改善、退化、失败原因及运行环境，不将运行成功当成准确率。

执行记录：19 页、29 张真实表格完成五组对照；354 个 Rust 测试和 9 个模型下载 Python 测试通过；Native/WASM Clippy、TypeScript、平台边界与格式检查通过。CPU 参考推理一致性通过。结果与限制见 `docs/reports/2026-09-14-tsr-model-comparison/report.md`。浏览器文件 URL 策略阻止 HTML 自动预览，未绕过；静态报告与全部数据已生成。
