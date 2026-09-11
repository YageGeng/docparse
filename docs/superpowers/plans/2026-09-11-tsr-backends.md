# TSR 后端对齐计划

1. 提取 layout 与 TSR 共用的 ONNX 后端初始化，支持 CPU、CUDA、CoreML、OpenVINO、WebGPU；Apple GPU 使用 CoreML CPU+GPU 通路并明确命名。
2. TSR 使用同一 execution_provider 配置与平台限制；WASM 和 Web SDK 默认 WebGPU，保留显式 CPU 选择及可见的初始化回退。
3. 保留现有推理资源生命周期与原文合成流程，日志和结果证据报告选择的后端。
4. 验证真实 SLANet_plus WebGPU、原生 Apple 后端与 CPU；无对应硬件的后端完成构建与失败路径检查，不声称实测加速。
5. 使用生产浏览器默认配置复跑 Survey 全文，检查 24 张表与关键语义回归。

不创建 worktree，不创建 commit。
