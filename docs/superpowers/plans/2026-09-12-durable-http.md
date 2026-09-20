# 持久化 HTTP 服务实施计划

**目标：** 支持 PDF 上传、后台解析、跨实例查询与 SSE 恢复，同时解耦 Layout、OCR、TSR 的有界调度。

**架构：** PostgreSQL 任务租约加共享文件目录；Axum API 与独立 Worker 循环复用同一 DocParser。核心阶段继续通过现有 TaskSet 和平台兼容层运行。

**设计：** [设计约定](../specs/2026-09-12-durable-http-design.md)

**约束：** 不创建 commit、worktree 或 SubAgent；计划和规格使用中文，代码注释和使用文档使用英文；依赖集中声明；新函数必须有作用注释。

## 1. 核心流水线

- [x] 在 `crates/core/tests/pipeline.rs` 用可阻塞的注入式 OCR 验证后续 Layout 可推进，并验证 PDFium 提前释放；先运行并确认旧实现失败。
- [x] 将页阶段实现移至 `crates/core/src/runtime/stages.rs`，保留 `analyze_rendered_page` 的独立页入口，完整文档分别调度 Layout、OCR、TSR；任一阶段失败时关闭渲染接收端并取消其他阶段。
- [x] 渲染生产者自身关闭 PDFium，结果类型改为关闭结果；收集器继续排序、跨页关联和最终验证。
- [x] 在 OCR 配置中增加受校验的并发上限，原生允许检测和识别跨页重叠，浏览器保留单页限制。
- [x] 运行核心解析、OCR、TSR 回归与配置校验。

## 2. 持久化服务

- [x] 新增 `crates/server`：`routers` 管理上传/查询/SSE，`storage.rs` 管理共享文件，`worker.rs` 管理领取/心跳/结果提交，`main.rs` 管理 CLI 与退出；独立 database 使用 SeaORM entities/query，migration 由 CLI 创建并通过 SeaQuery 编辑。
- [x] 根 Cargo 声明 Axum、SeaORM 2.0/PostgreSQL、UUID、Tower 测试工具与 Tokio 流适配依赖，Snafu 只用于 server，服务继承已有 GPU feature 转发。
- [x] 先写数据库集成测试：幂等提交、并发领取、租约接管、旧令牌拒绝、结果持久化；使用真实临时 PostgreSQL 数据库。
- [x] 实现数据库迁移和事务领取：`queued → running → succeeded/failed`；过期任务重新领取并增加尝试次数，超限置失败。
- [x] 上传流写同目录临时文件，执行同步与原子发布；Worker 流式写 JSON，通过租约条件更新成功状态。
- [x] SSE 使用数据库版本轮询和心跳，重新连接发送最新快照；消费者速度不阻塞解析器，进度通过 watch 合并并定期持久化。
- [x] 实现 API/Worker/all 运行模式及 SIGTERM 优雅退出，不使用请求局部后台任务持有持久任务生命周期。
- [x] HTTP 集成测试覆盖上传验证、幂等冲突、跨 Router 查询、SSE 终态、结果读取和超限拒绝。

## 3. 验证和文档

- [x] 编写 `crates/server/README.md`：数据库准备、共享目录、CLI、curl 上传与恢复、滚动发布、租约、失败重试和进度快照语义。
- [x] 更新根 README 的服务入口与阶段内存上限，检查 `cargo fmt`、相关 `cargo test` 和 Clippy。
- [x] 运行 `rtk proxy python3 scripts/check_wasm_compat.py` 与 `docparse-web` WASM 构建。
- [x] 用已有真实 PDF/模型验证服务或 Worker 解析；明确报告设备与外部服务不可用时未完成的测量。

## 验证结果

实现和验证已完成。GPU 驱动不可用，因此只记录真实 CPU 推理与阶段调度结果，不宣称 GPU 满载。用户数据库保留迁移、任务表和索引，本轮两条临时验证任务已通过 SeaORM 清理。
