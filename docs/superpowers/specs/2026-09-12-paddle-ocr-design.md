# 独立 PaddleOCR ONNX 设计

## 目标

实现独立的 docparse-ocr crate，不依赖 OAR 库。参考 OAR 与 PaddleOCR 的预处理、DB 后处理、透视裁切、方向分类和 CTC 解码；参考 liteparse 的原生/OCR 合并策略，接入 Native parser 与真实 WebUI，并运行真实模型浏览器端到端测试。

## 模型与后端

- 默认采用官方 PP-OCRv6 medium 检测及识别模型、匹配识别字典和 PP-LCNet 文本行方向分类模型。模型作为独立资源下载，版本、配置及 SHA-256 固定；方向分类可关闭。
- 复用现有 OnnxBackend，支持 CPU、CUDA、CoreML/Metal、OpenVINO，以及浏览器 WebGPU/CPU WASM。Native 与 Web 共用算法；平台分支仅放入 wasm_compat。
- Native 单会话队列/锁在真实推理与输出读取完成前保持输入有效；Web 使用现有全局推理锁及独立 actor，取消不能提前释放 JS 正在使用的 tensor。
- 本机有 NVIDIA GPU，实际验证 CPU、CUDA 与浏览器 WebGPU；CoreML 通过共享后端和 feature 编译验证，实际运行需要 macOS 硬件。

## 独立 OCR 流程

1. 校验 RGB 图像及模型契约，按模型配置完成 BGR、尺寸对齐、缩放和归一化。
2. 检测概率图执行阈值化、轮廓/连通域提取、最小旋转矩形、框内置信度和 unclip，恢复原图四边形。
3. 对四边形进行透视裁切；竖直文本转换为识别方向；可选文本行分类器修正 180 度方向。
4. 识别输入按比例缩放、零填充、归一化；执行 ONNX 并进行 CTC argmax、blank 删除、相邻重复折叠、字典映射及置信度计算。拒绝不匹配字典、无效维度、NaN 和越界结果。
5. 返回文本、置信度与有阅读方向的四边形；坐标始终可映射回源图。单页单检测批次、逐文本行识别，限制输入边长、候选数和识别宽度。

## Core 合并与文本合成

- 通过现有 OcrEngine 适配器接入；自定义 OCR 引擎继续可注入。
- 保留 Disabled 与 MissingRegions；增加 Always 用于强制真实 OCR 验证及用户选择。默认库策略保持 Disabled，WebUI 显式提供自动、全部页面和关闭选项。
- 缺失区域识别考虑空白/稀疏页、检测出的未覆盖文本以及明确无效原生映射。健康原生文字优先，只用原生快照判定 OCR 重叠，不能用新加入的 OCR 行压制相邻 OCR 行。
- 低置信度、边框伪字、重复结果和不相关区域受到约束；不对正常正文全局应用词形或语言猜测。
- 对明确无效的原生文本，仅在可靠 OCR 覆盖成功时替换；被替换的原生事实保留在页面证据中，并参与原生 ID 守恒验证。正常原生事实保持不变。
- OCR polygon、rotation、baseline、字号提示及来源进入现有行/段落/表格流水线。OCR 字符串的空白保留，独立 OCR 片段的合成不得引入原生文本的几何补空格机制。
- OCR 失败通过现有错误/警告路径暴露，不能将未调用模型当作成功识别；有效原生文字不因 OCR 失败消失。

## 组织与 API

- crates/ocr：模型契约与引擎、检测解码、图像预处理/裁切、识别解码、平台执行器。
- core OCR 模块：公共扩展协议、内置适配器、原生/OCR 合并。避免 OCR crate 反向依赖 core。
- 配置集中在 docparse-config；所有版本与路径集中在 workspace.dependencies，模型下载复用现有脚本。
- Web artifacts/protocol/Worker/Rust ABI 同时传递 OCR 模型资源；UI 使用同一 production SDK 与配置的后端。
- 所有新增函数及非平凡逻辑提供英文注释；测试仅位于 crate tests/ 或精确的 cfg(test) mod tests 内。

## 验证

- 测试检测框、unclip、裁切/旋转坐标、CTC 重复/空白/多字符字典、置信度与边界错误。
- 用真实 ONNX 输出对照参考预处理/解码；测试完整 CPU/CUDA OCR 以及原生/OCR 合成。
- 浏览器用真实扫描 PDF、旋转文本和混合原生/图像页面证明模型确实调用、输出来源为 OCR、文本非空且包含预期内容；再次解析 Downloads/docs 全部真实 PDF，检查页数、错误与模型阶段。
- 运行 cargo tests、相关后端编译、Web 构建与真实浏览器验收、pre-commit。
- 不创建 commit、不使用 SubAgent、不创建 worktree。
