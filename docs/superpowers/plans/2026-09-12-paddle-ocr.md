# PaddleOCR ONNX 实现计划

目标：按 [独立 OCR 设计](../specs/2026-09-12-paddle-ocr-design.md) 完成完整 OCR 与 Native/Web 接入。用户已授权实现，在当前工作区顺序执行，不使用 SubAgent、worktree 或 commit。

- [x] 锁定官方检测、识别及方向分类模型；扩展统一下载、配置与依赖声明。
- [x] 新建 docparse-ocr，测试并实现检测预处理、DB 解码、透视裁切和 CTC 识别解码。
- [x] 实现共享后端执行器和完整检测→方向→识别流水线；验证真实 CPU/CUDA 模型。
- [x] 接入内置 OcrEngine、缺失区域策略、原生/OCR 去重和文本合成，保留几何、方向及原生证据。
- [x] 接入 Web 资源协议、Worker 和 UI OCR 策略，构建实际 WebGPU/CPU 模型运行路径。
- [x] 运行真实扫描/旋转/混合 PDF 与 Downloads/docs 浏览器端到端测试，记录 OCR 来源、模型调用和逐文件结果。
- [x] 完成代码审查、相关 tests/Clippy、后端编译和 pre-commit，并更新英文报告。

验收结果：12 份真实 PDF、368 页全部解析完成；111 页调用 OCR，6146 次识别推理，无 OCR 失败提示。32 页保留解析警告，其中 2 页表格结构未恢复但原始文字保留。英文报告见 [验收报告](../../reports/2026-09-12-paddle-ocr.md)。
