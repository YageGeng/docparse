# 可观测性修复实施计划

> 执行方式：使用 executing-plans 在当前目录逐项实施；用户禁止子代理、worktree 和未经允许的 commit。

**目标：** 修复已确认的监控问题，让页面交付、降级、重试与界面口径一致。

**架构：** 复用 recorder、解析进度观察者、终态接受边界和固定 PromQL 列表。通过小型统计值类型和现有 API 返回值传递交付计数，不改变业务状态机。

**技术栈：** Rust、metrics、Prometheus、SeaORM、React、TypeScript。

**规格：** `docs/superpowers/specs/2026-10-04-observability-corrections.md`。

## 全局约束

- 文档中文、代码注释英文；新增函数都有作用说明，修改非平凡逻辑说明原因。
- 不创建 commit、worktree 或子代理；不新增原始 SQL。
- 不记录任务 ID、原始错误、文档内容等高基数或敏感标签。
- WebUI 不要求单元测试，浏览器验收使用真实生产 app 和配置的 provider。

## 重点验证

- 同名无标签零值不能遮蔽活动资源；页面最终持有者释放后归零。
- 七天历史图不能遗漏落在旧一分钟窗口之外的突发。
- 进度重复或回退不能重复计数；未完成文档也能报告已处理页面。
- 普通警告不误判降级；多个失败告警只计一个降级页。
- 失败发布、失效租约或重复终态确认不能增加交付量。
- 首采样、进程重启、空流量与 API-only 保持明确语义。

## 任务 1：修复资源标签与历史窗口

- [x] 在 server recorder 测试中复现重复标签；在历史查询测试中验证突发覆盖。
- [x] 统一 common/page 与 server 初始化标签；按查询步长生成窗口。
- [x] 执行相关 server/common 测试和 PromQL 验证。

## 任务 2：处理量、交付量与尝试分类

- [x] 添加进度计数、页面分类、真实 PostgreSQL claim/终态边界回归测试并确认旧实现失败。
- [x] 通过 `From<&DocumentResult>` 生成页面汇总，随发布结果传到终态接受边界。
- [x] 在进度观察者中记录新增页面；在 claim 提交后记录尝试类型；使用固定错误原因区分解析错误和监督退出。
- [x] 执行 server/database 针对性测试。

## 任务 3：同步 WebUI 和接口

- [x] 新增允许的历史图类型，更新快照投影、卡片、表格和解释文案。
- [x] 从真实后端 OpenAPI 重新生成前端类型；运行类型检查、构建。
- [x] 使用生产后端验证桌面/移动监控、空闲/工作状态和历史图。

## 任务 4：收尾验证

- [x] 更新监控说明，记录兼容口径与验证结果。
- [x] 运行相关 crate 测试、格式检查与 lint，人工复查完整 diff。

## 执行记录

- 用户已批准实施上一轮建议并明确要求同步 WebUI，直接执行本计划，不重复请求授权。
- 当前工作区开始时干净；计划进度在本文件维护，不使用依赖 commit 的外部执行脚本。


## 验证结果

- 常规相关测试：`cargo test -p docparse-server -p docparse-common -p docparse-database`，123 项通过，22 项按其环境/性能要求默认跳过。
- 另行启用 server 全部库测试（含 PostgreSQL 和 PromQL 条件测试）：14 项通过；真实 PostgreSQL 尝试分类测试 1 项通过；数据库 jobs 集成测试 5 项通过。
- `cargo clippy -p docparse-server -p docparse-common -p docparse-database --tests -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` 和平台 cfg 边界检查通过。
- WebUI 已从实际服务端 OpenAPI 重新生成类型；生产构建与最终 TypeScript 类型检查通过。单独检查了快照投影在首采样、进程切换、API-only、缺页排除和重复标签下的行为。
- 生产 `docparse-server` 使用 CUDA 与原有模型/容量配置，在独立 PostgreSQL 和 Prometheus 上完成真实运行验证。3 页 PDF 成功，处理量与无已知降级交付量均为 3；损坏 PDF 经 3 次尝试后失败，累计首次尝试 2、重试 2、解析错误 3，交付量仍为 3。
- 浏览器桌面和 390 像素视口验证了新卡片、完整性/重试/错误表格、推理冷启动提示及七天历史图。六类历史查询均返回有效矩阵，已有事件对应的曲线包含非零点。
- 浏览器扩展缺少本地文件访问权限，因此测试输入通过真实后端 API 提交；没有宣称本轮验证了上传控件。浏览器仅观察到扩展资源错误和既有 favicon 404，监控接口及页面未出现运行异常。
- 最终人工复查由当前执行者完成，遵循用户禁止子代理的要求。已确认：无新依赖、无数据库迁移、无原始 SQL、无高基数标签、无测试专用生产字段，也没有创建 commit。
- 独立验证服务、浏览器会话和容器在结束时清理；临时截图与日志位于 `/tmp/docparse-observability-tl3y78ev/`。
