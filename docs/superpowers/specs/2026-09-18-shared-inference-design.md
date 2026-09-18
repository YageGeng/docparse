# 统一模型推理队列设计

## 目标

根据本次 review 及用户确认，所有本地模型采用多个 session 消费同一个有界任务队列。`session_size` 表示该模型的消费者数量，`batch_size` 表示一次最多取出的就绪任务数；不等待凑满，不保留调用者批次边界。

## 配置

- Layout、TSR 结构模型、TSR 单元格模型各有 `session_size` 和 `batch_size`。
- PP 和 Texo 在 `formula.engine.session_size` 配置本地模型消费者数，继续使用 `formula.batch_size`。
- OCR 的 `detection`、`recognition`、`orientation` 分别配置模型路径、`session_size`、`batch_size`。页面并发限制独立保留。
- 删除旧的 `sessions` 和顶层 `ocr.batch_size` 配置，同步示例、校验、测试和浏览器 SDK。
- session 数量为 1..8，batch 大小为 1..32。浏览器保留运行时全局互斥，支持独立 session 消费同一个队列。
- MinerU 是外部 HTTP 服务，继续使用 `concurrency`，不把 HTTP 请求数量称作 ONNX session 数量。

## 执行和生命周期

复用 Texo 的原生所有权方式，在 layout 公共执行层提供有界队列和 session manager。每个 session 在自己的线程创建、推理和销毁；队列锁仅用于取任务，推理时不持锁。初始化失败、消费者异常退出及关闭时唤醒等待者并释放响应。创建者的 Tokio runtime 关闭后，engine 仍可用于后续 runtime。

OCR 以单页检测或单行识别/方向分类作为队列任务，移除页面内串行预分批。就绪任务取出后仅将相同 tensor 尺寸合并，保持原有预处理、padding 和识别语义；不同尺寸使用不同物理推理批次。Layout 合并输入并按照每页 bbox 数量拆回结果。

保留取消、结果顺序、逐请求错误和原始 tracing/timing 上下文。浏览器共享同一接收队列，推理及输出回读继续受现有 guard 保护。MinerU 消费任务由 engine 自己的执行线程/runtime 持有。

## 验证

测试配置独立性及边界；测试多个消费者并行、部分批次立即返回、取消跳过、跨调用者路由、初始化失败和跨 runtime 复用。运行相关 Rust 测试、真实模型集成测试、WASM 编译和浏览器 SDK 检查。禁止创建 commit、worktree 和子代理。
