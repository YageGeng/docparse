# 公共推理调度层实现计划

依据：[公共推理调度层下沉](../specs/2026-09-18-common-inference-design.md)。在当前目录完成，不使用子代理、worktree 或 commit。

- [x] 新建 common crate，迁移平台类型、CPU 执行、超时、计时和原生 SessionWorker；保留平台类型的兼容重导出。
- [x] 抽取 ThreadManager 和通用原生/异步队列，迁移调度生命周期测试。
- [x] Texo 与 Layout/OCR/TSR 复用公共 SessionManager；PP/MinerU 复用公共线程所有权。
- [x] 收敛浏览器的取批次循环和重复计时上下文，删除模型模块中的通用实现。
- [x] 运行公共层与相关模型测试、真实模型回归、原生/WASM 编译、Clippy、格式和 diff 检查。

用户补充要求：检查各模块的 wasm_compat；core 的 TaskSet/spawn 已纳入公共层，PDF、字体数据库、模型后端与 HTTP 业务策略保留在所属模块。

会话文件按用户要求组织为 common/src/session/{mod,manager,worker}.rs。

## 验证结果

- 438 项普通测试通过，包含迁入 common 的原生线程亲和性、取消、跨 runtime、浏览器任务所有权、队列容量及计时上下文测试。
- 7 项真实 CPU 模型测试通过：Layout batch/多 session、PP 跨 runtime、Texo 批量及取消、TSR 三项和 OCR 不同 batch/多 session 对照。
- 原生 workspace 全目标编译（排除仅支持 WASM 的 web crate）、WASM 编译、相关 Clippy、格式和 diff 检查通过。
- common 仅依赖 futures-util、serde、thiserror、tokio、tracing、web-time 及浏览器基础绑定，不依赖工作区业务 crate 或 ONNX。

- [x] 按用户后续要求删除 layout/src/timing.rs，迁移生产代码、测试及示例的计时引用，补齐 server/web 对 common 的直接依赖。
