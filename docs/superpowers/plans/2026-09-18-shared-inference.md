# 统一模型推理队列实现计划

目标：完成已确认的六项 review 修复，保持实际模型输出与资源所有权正确。

设计依据：[统一模型推理队列设计](../specs/2026-09-18-shared-inference-design.md)。在当前工作目录顺序执行，不创建 commit、worktree 或子代理。

- [x] 配置与回归：先增加新字段加载、模型配置独立性、MinerU 跨 runtime 和 Texo 尽量取满的失败测试。
- [x] 公共执行层：实现共享有界队列、多个 session 所有者和关闭/异常清理测试；避免新增依赖。
- [x] 公式：接入 PP 多 session，修复 MinerU 生命周期，调整 Texo 取批次及 session_size。
- [x] 表格与 Layout：接入多 session 共享队列，Layout 实现模型 batch 输入和按页输出拆分。
- [x] OCR：三模型独立队列和配置，单任务入队、按尺寸合批、滑动窗口提交并保持结果顺序。
- [x] 浏览器及文档：同步 session_size、OCR 配置、示例及全局推理互斥。
- [x] 验证：运行相关测试、真实模型对照、WASM/SDK 检查、格式检查和最终 diff review。

## 验证记录

- 434 项相关普通测试通过；真实 CPU 模型专项覆盖 Layout 批量输出、PP/Texo 公式、OCR 三模型多消费者和 TSR 双模型。
- 原生 workspace 全目标编译（排除仅支持 WASM 的 web crate）、相关 crate 的 Clippy、Rust 格式及 diff 检查通过。
- 浏览器 release SDK 构建、SDK/示例类型检查通过。实际浏览器已验证 OCR 和 TSR 各消费者均参与推理且页面无告警；PP 和 Texo 均识别出同页 4 个公式，两个消费者均参与实际推理。
