# 渲染队列背压实施计划

依据：[设计文档](../specs/2026-09-18-pipeline-backpressure-design.md)。在当前目录内顺序实施，不使用 subagent、worktree，不创建 commit。

目标：`render.workers` 仅设置 PDFium 进程数；空闲租约驱动任务领取。`render.queue_size` 统一约束尚未完成确认的页面，替代阶段上限。

## 1. 配置与协议回归

- [x] 在 config 集成测试中验证 render 两字段必填、旧字段拒绝、合法容量与覆盖优先级，先运行确认失败。
- [x] 修改 `config.rs`、`validate.rs`、`wasm_compat.rs`，移除 server 两字段和 runtime 三字段；更新 TOML、SDK、示例与现有测试配置。
- [x] `render.workers` 正整数且适合运行时容量；WASM 仅支持 1。`render.queue_size` 沿用可移植队列上界。

## 2. 页面交付容量与取消

- [x] common 增加 `PageQueue::new(size)`、`PageQueue::reserve()` 与可克隆 `PageLease`。回归验证未归还交付不会释放容量，克隆直到最后引用才释放。
- [x] `PageLease::scope(future)` / `scope_sync(operation)` 提供独立于 tracing 的任务执行上下文；`current()` 只用于跨实际执行边界捕获显式资源引用。
- [x] `run_cpu` 和任务 spawn 传播引用；阻塞任务的未收集输出也保留引用，避免取消提前释放。
- [x] 页面图像、模型请求及 PDFium/IPC 请求捕获引用。模型请求保持引用到实际批次结束，原生渲染失败时保持到进程清理结束。

## 3. 页面流水线

- [x] parser 初始化并共享 `PageQueue`，单页 API 同样接入。
- [x] 渲染生产者先 reserve，再调用 Render；以固定单槽通道交接结果和交付凭证。
- [x] 用一个整页 TaskSet 替代四阶段容量，复用现有 `analyze_rendered_page`；完成结果携带凭证直到收集。
- [x] 取消关闭并排空通道，后台引用未释放前不恢复容量。集成测试验证跨文档共享与 `queue_size=1` 不死锁。

## 4. PDFium 空闲预约与领取

- [x] pool 增加可转移的空闲预约；未使用预约归还不重启进程，打开后的异常继续清理。
- [x] parser 支持接收已打开 session，复用现有扫描与分析流程，不再二次 acquire。
- [x] Worker.run 等待空闲预约后 Jobs::claim；删除完整任务数判断。任务过程使用预约打开文档，Close 后池再次触发领取。
- [x] 验证一个 worker 下旧 PDF 推理尚未结束、新 PDF 已可 Open；队列满时新 PDF 只能等待 Render。

## 5. 验证与交付

- [x] 运行 common/config/core/OCR/公式/TSR/服务器相关单元与集成测试、原生 workspace all-target check、WASM check。
- [x] 运行真实模型回归和真实 PDFium 子进程取消/复用测试。
- [x] 构建生产 WASM SDK，检查 TypeScript 示例并验证浏览器真实模型及取消恢复；若执行 WebUI 验收，必须使用生产后端。
- [x] 更新 README 与迁移说明，检查差异及遗留字段；记录实际验证结果，不把未运行测试当成通过。

## 验证记录

- 模块常规测试：447 通过，31 个需要外部资源的用例默认忽略。
- 服务器常规测试：36 通过；PDFium 子进程集成测试包含在其中。
- 临时隔离 PostgreSQL：6 个 HTTP/任务测试、2 个 tracing/数据库测试通过，测试容器已停止并移除。
- 真实 OCR：3 个回归通过；原生 Layout/表格模型：单槽渲染队列完成三页真实 PDF。
- 浏览器 SDK：真实 Layout 模型，容量 1/2，三页结果一致；主动取消后重新创建 parser 并成功解析。
- 原生 workspace all-target 检查、WASM 编译、SDK/示例类型检查和格式检查通过。Clippy 无错误，保留原有 common 测试的 3 个告警。
- 生产 WASM 包已构建；完整 WebUI 页面验收未运行，本次浏览器检查直接调用生产 SDK 与真实模型。

实现采用独立的任务局部 PageLease 上下文，在图像、模型请求、CPU 和 IPC 边界显式保留引用，不依赖 tracing 或计时开关。
