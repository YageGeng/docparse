# 可观测性口径修复

## 目标与范围

按已确认的评估结论，修复 PDFium 标签冲突和历史窗口盲区，补齐页面处理、交付完整性、重试与错误分类，并同步 WebUI。保留物理 ONNX 调用和现有阶段指标，不改变解析、重试、租约与降级策略。

## 统计约定

- PDFium 活跃文档以及页面占用、容量、阻塞统一使用 `pool="render"`，初始化与更新不能产生不同序列。
- 历史速率窗口取原有最小窗口与两倍查询步长的较大值；默认 5 秒抓取下长周期窗口重叠，仍保持约 600 点预算。
- 保留 `docparse_pages_parsed_total` 的旧口径以兼容外部查询。新增 `docparse_pages_processed_total`，在分析进度增加时立即计数，包括失败尝试已经完成的页面和正常降级页面。
- 新增 `docparse_output_pages_total{status="full|degraded|missing"}`，只在成功终态被数据库接受后计数。`full` 表示无已知降级，不代表识别内容必然准确；明确的渲染、原生提取、版面、OCR、公式、表格、图片交付失败计入 `degraded`；源页面未出现在有效结果中计入 `missing`。普通布局警告不降级。
- `docparse_job_attempts_started_total` 增加 `kind="initial|retry|recovery"`，在 claim 事务提交后记录。旧无标签序列不再初始化，跨标签求和仍代表全部尝试。
- 解析/发布返回的错误以固定原因记录在 `docparse_job_attempt_errors_total{reason}`；监督退出单独使用 `docparse_job_supervision_exits_total{reason}`，不冒充被数据库接受的尝试完成。
- WebUI 展示两类速度、完整性累计数量、尝试类型和错误分类；首个采样、重启、API-only 与缺失指标继续显示未知。增加冷启动提示和 ONNX 中断调用展示，文案明确其并非业务取消总数。
- 历史视图新增交付页面、尝试类型、尝试错误、监督退出查询；页面处理曲线采用新增实时处理计数。

## 验证与边界

后端用真实 recorder、PromQL 执行以及 PostgreSQL 集成测试验证序列唯一性、长窗口突发、进度去重、降级分类和租约接受边界。前端执行类型检查和构建，使用生产 `app` 后端及既有 provider 做浏览器验收。所有新增函数和修改原因使用英文注释；不创建 commit，不使用子代理或 worktree。

本轮不引入新数据库字段、迁移、依赖框架、停滞阈值或自动报警策略；这些需要额外的运行数据决定。

## 查询与兼容说明

- 交付速度为 `full + degraded` 的增量，不包含 `missing`；完整性表格和历史图保留缺页数量。
- 页面处理量包含重试和未成功结束尝试中已经完成的页面；原有 `docparse_pages_parsed_total` 仍只在整份解析返回后增加，不能混用。
- 新版本尝试类型计数增加 `kind` 标签；外部总量查询应跨类型求和。历史分类图只查询有明确类型的序列，不将旧版本无标签数据猜测为首次尝试。
- 新指标不会补写升级前历史；首次进程采样、进程切换和计数器重置时实时速度显示未知。
- 交付统计属于成功终态文档的处理完整性。整份文档失败由任务失败数描述，不虚构其未知源页数；也不将处理完整性宣称为识别准确率。
- `degraded` 目前识别 `NativeExtractionUnavailable`、`RenderUnavailable`、`LayoutUnavailable`、`OcrUnavailable`、`OcrFailed`、`InvalidOcrResult`、`FormulaRecognitionFailed`、`TableStructureUnavailable`、`VisualAssetUnavailable`；单页多个此类警告仍只计一页。
- 错误原因使用固定类别 `timeout`、`database-timeout`、`database`、`storage`、`serialization`、`parse`、`task`、`request`，监督退出另含 `lease-lost`、`panic`、`cancelled`。这些表示进程观察到的事件，不替代数据库终态，也不保证捕获进程强制退出。
- 所有计数器仍是进程内统计：崩溃或提交确认不确定时不构成持久化账本；严格核账应读取数据库和结果文件。
