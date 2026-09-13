# HTTP 文档工作台实现计划

目标：在 packages/web 实现连接真实 Axum 服务的文档解析工作台，完成上传、持久化任务历史、刷新恢复、SSE 进度、原 PDF 预览、结果区域联动和 JSON 下载。

已确认的设计：React、TypeScript、Vite、Tailwind CSS、shadcn/ui、TanStack Query、PDF.js；通过 openapi-typescript 生成 HTTP 类型。现有浏览器内解析 SDK 位于 packages/wasm-web。

## 约束

- 在当前目录实现，不创建 subagent、worktree 或 commit；保留用户对 WASM logging 的独立修改。
- Rust 新函数写英文注释；数据库和 migration 只使用 SeaORM/SeaQuery。migration 先使用 sea-orm-cli 生成。
- 每个路由文件只定义一个端点，在 jobs/mod.rs 聚合并使用 JOBS tag；沿用配置的 API prefix 和 ApiError/APICODE。
- 页面验收使用 release CUDA 的生产 docparse-server、隔离 PostgreSQL 与共享目录，通过有头浏览器验证；不用模拟解析后端。

## 实现步骤

1. 任务数据和查询
   - 增加 nullable filename、size_bytes，兼容旧任务；保留已有 created_at/updated_at。
   - 提交任务时一次写入元信息，幂等重传不能改写原任务。
   - 按 created_at、id 倒序分页；cursor 使用任务 UUID，支持状态和文件名筛选，limit 限制为 1–100。
   - 先补 OpenAPI/参数回归，再实现查询，并用隔离数据库检查迁移、幂等性和分页。
2. HTTP 端点
   - GET /jobs/list 返回 items、next_cursor，公开快照补 filename、size_bytes、created_at、updated_at。
   - GET /jobs/source?id=UUID 按数据库 hash 定位输入，通过 tower-http ServeFile 流式提供 PDF 和 Range；客户端文件名不参与存储路径。
   - 延用统一错误响应和 utoipa 文档，验证部分读取、越界 Range、HEAD、无效参数与自定义 API prefix。
3. 前端基础及任务流程
   - npm 管理独立 packages/web；Vite 代理配置的 API prefix，生产通过同源网关部署。
   - 上传前生成并保存 Idempotency-Key，上传响应丢失先查状态；刷新后凭任务 ID 恢复，上传尚未完成时允许重选文件重传。
   - XHR 提供真实上传进度；TanStack Query 查询列表和快照，当前任务使用 EventSource；根据 version 去重，终态停止连接。
   - 明确区分网络中断、排队、解析中、成功、失败；显示真实阶段与完成页数。
4. 文档工作台
   - 列表页提供上传、搜索、状态筛选和分页；详情页提供页码导航、PDF 预览、SVG 区域框及文本/表格/JSON 面板。
   - job、page、block 保存在 URL；缩略图和 PDF 页按需渲染，及时取消旧渲染并释放资源。
   - 大结果按页展示，表格正确处理跨行跨列，用户文本只作为文本渲染。
   - 真实 144 页文档产生 117 MB JSON；增加独立结果 Worker，完整 JSON 在 Worker 中解析并保留，只把当前页传入 UI，避免主线程持有整个文档。
   - 响应式布局、键盘可操作、可见焦点和状态提示；沿用后端实际能力，不显示尚不存在的取消任务或模型配置按钮。
5. 验证与交付
   - 运行 Rust 相关测试、Clippy、格式和边界检查；前端类型检查与生产构建。
   - 使用真实 PDF 在有头 Chrome 验证上传、SSE、任务列表、预览/区域联动、刷新和重连、JSON 下载以及窄屏布局。
   - 更新运行文档，记录验证结果及启动命令。
