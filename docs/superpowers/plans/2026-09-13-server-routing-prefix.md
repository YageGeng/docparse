# Server 路由前缀与目录拆分

- 增加 `server.api_prefix`，默认 `/api`，允许空字符串或 `/` 表示根路径。校验前缀，避免动态捕获、重复斜杠等导致路由构造异常。
- 每个叶子路由文件只声明一个 utoipa 接口。job 接口放入 `routers/jobs/`，由 `jobs/mod.rs` 聚合；common 和 docs 同样按接口拆分并在各自 `mod.rs` 聚合。
- 上传保留 `POST /jobs`；状态、事件与结果分别为 `GET /jobs/status?id=...`、`GET /jobs/events?id=...`、`GET /jobs/result?id=...`。共享 Query DTO，保留既有 APICODE、stage 和 tracing 关联。
- job 接口统一使用 `JOBS` tag。`app.rs` 使用 utoipa-axum 的嵌套能力拼接最终前缀，OpenAPI、Scalar、健康检查和实际路由保持一致。上传的 Location 由实际请求路径生成。
- 验证配置覆盖与非法前缀、无路径参数的 OpenAPI、缺失或非法 query 参数、默认和自定义前缀、PostgreSQL 持久化上传/结果/SSE 跨实例重连、tracing 回归。只使用既有迁移，不提交 commit。

## 完成情况

- 已完成：每个叶子模块仅保留处理函数与 utoipa 接口声明，`mod.rs` 直接通过 `routes!` 组装，不保留叶子模块的 router 包装函数；`app.rs` 统一挂载前缀并生成对应 OpenAPI。
- config 21 项测试通过；server 17 项测试全部通过，包含临时 PostgreSQL 下的上传幂等、自定义前缀 Location、SSE 重连、结果读取与 tracing 验证。
- 配置与 server 的严格 Clippy、格式检查、平台 cfg 边界检查通过；每个叶子模块只有一个 utoipa 接口，路由注册集中在对应的 `mod.rs`。
