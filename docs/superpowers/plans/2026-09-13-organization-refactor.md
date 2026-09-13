# 组织与接口重构计划

用户明确保留第一个校验入口问题，本次不修改 `AppState` 的字段、builder 或构造方式。

## 设计

- JSON：core 暴露配置化文档的借用序列化视图，server 的 `ApiResponse` 提供 writer 入口。Worker 使用同一个响应类型，不再手写 envelope 字节，保持流式与输出过滤。
- 任务类型：database 定义字符串映射的 SeaORM `JobStatus`，保留现有 varchar 字段及 HTTP 字符串；完成接口改用 `Result<&str, &str>`，消除两个 Option 的非法组合。无需数据库迁移。
- 编排：`parse_document_with_options` 只编排扫描、页阶段调度和结果收尾；一个私有扫描结果结构保存上下文、页事实与诊断。扫描失败由入口统一关闭 PDFium，阶段调度继续独占渲染生产者和取消清理。
- Session：`SessionWorker::new` 要求初始化错误实现 `From<TaskError>`，返回单层 Result；固定线程、队列与取消时的资源保留方式不变。
- 清理：multipart 错误统一实现 From；删除未使用的配置错误分支与 cors feature，同时删除重构后不再使用的错误分支。

## 执行与验证

- [x] 添加响应流式输出与 JSON 转义/过滤兼容检查，以及 JobStatus 序列化/数据库映射检查。
- [x] 实现响应、状态、完成结果和 Session 接口变更，更新全部调用点。
- [x] 拆分文档编排，保持回调顺序、错误策略、页顺序和资源释放。
- [x] 运行原生回归、PostgreSQL/HTTP 接管测试、严格 Clippy、WASM 构建和 CUDA 编译；检查 AppState 文件未修改。
- [x] 更新文档并给出验证结果，不创建 commit、worktree 或 SubAgent。

## 验证结果

原生默认成员测试 359 项通过；PostgreSQL/HTTP 接管与重试测试 5 项通过；真实 CUDA OCR 测试通过。严格 Clippy、格式化、平台边界检查、WASM release 和 server release CUDA 构建通过。`AppState` 文件的 SHA-256 与修改前相同；本次没有迁移数据库结构，也没有创建 commit。
