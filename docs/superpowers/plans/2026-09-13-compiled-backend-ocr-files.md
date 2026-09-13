# 编译期推理后端与 OCR 文件配置

- 删除 layout、tsr、ocr 的 `execution_provider` 配置字段。原生后端集中在兼容层按 Cargo feature 选择；无加速 feature 时为 CPU，Metal 优先采用其专用模式，互斥检查继续生效。
- 后端枚举与选择逻辑归属 layout 的 ONNX 兼容层，不再作为业务配置字段。浏览器保留已有的每 Worker WebGPU/WASM 选择，通过验证配置中的私有平台选项传递，不增加 TOML 入口或全局可变状态。
- OCR 按用户确认分为 detection、recognition、orientation 三组，每组使用 `model_path`、`model_config_path`、`model_manifest_path`。复用统一文件描述类型与 fallible 转换，按主配置目录解析路径。
- 更新模型加载、CLI 诊断、测试、基准脚本和原生/Web 文档；旧配置字段明确报未知字段。
- 验证原生回归、编译 feature 选择、真实 OCR 文件路径加载、CUDA release 和 WASM 构建；不提交 commit，不使用 SubAgent 或 worktree。

## 实施与验证结果

- 已完成后端配置删除、编译 feature 统一选择，以及 OCR 三组独立文件路径。旧 TOML/环境后端配置明确拒绝；九个路径按主配置目录解析。
- 原生默认回归：373 项通过，22 项依赖外部资源的测试默认忽略。追加环境覆盖校验后的 config 测试为 19 项通过。
- 默认 workspace 与 server CUDA 的 `clippy --all-targets -- -D warnings`、格式检查和平台 cfg 边界检查通过。
- `cargo build -p docparse-server --release --features cuda --offline` 通过。真实 OCR 三模型和 SLANet 表格的独立 release CUDA 推理测试通过。
- WASM release 和完整 Web 发布包构建通过。真实浏览器 WASM 推理、缺失 GPU 明确报错和允许回退到 WASM 三项通过，layout/TSR 的实际 session provider 均为 wasm。当前 Chrome 不提供 WebGPU，未完成真实 WebGPU 推理验证。

## 扩展验证发现的独立问题

- 启用真实模型的 layout 单元测试全部断言通过后，进程退出发生 SIGSEGV。GDB 显示 `docparse-onnx` 线程在 `CUDAExecutionProvider` 析构中调用 `cudnnDestroy`，同时主线程已进入 `exit()` 和 CUDA 库全局析构。现有 `SessionWorker` 的线程句柄被丢弃，退出阶段没有等待 session 销毁；本轮不改变线程生命周期。
- layout 的 Python 对照在 CPU 下通过；CUDA 下 `fixed_images_match_python_detections` 在区域数量断言失败，实际 6、期望 7。保留原断言，未放宽精度或改变模型算法。

## 后续有头浏览器验证

- 已在用户现有 Chrome 中完成 CPU WASM 与 NVIDIA WebGPU 验收，覆盖真实 OCR、TSR 和 144 页 PDF。之前的 WebGPU 限制来自 Headless Chrome 的运行环境。详细结果与测试断言修正见[有头 WASM 验证报告](../../reports/2026-09-13-headed-wasm-verification.md)。
