# 资源生命周期背压实施计划

> 在当前会话中按 executing-plans 顺序实施并自行审查。遵循用户限制：不使用 subagent、worktree，不创建 commit。

**目标：** 删除公式水位降级，并让资源预算覆盖分配到实际清理、解析到发布。

**架构：** 用共享 RAII 许可衔接已有 CPU、图像和请求所有权；原生队列使用 Tokio 公平信号量。预算由现有消费者、批量和队列配置推导，上传行为保持原样。

**技术：** Rust、Tokio、现有 typed-builder 与集成测试，不增加依赖。

**设计依据：** [设计文档](../specs/2026-10-02-resource-backpressure-design.md)。

## 全局约束与审查重点

- 所有新增函数添加英文作用注释，非平凡修改说明原因，依赖与测试位置遵守 AGENTS.md。
- 重点覆盖：旧配置拒绝、跨页面共享预算、取消中的原生资源、同一预算重复申请、发布阻塞后的领取上限。
- 文档数量有界不等于单本文档字节数固定；不更改完整文档返回结构。

## 任务与验证

### 1. 删除公式水位策略

- [x] 在 config 集成测试增加旧字段拒绝断言，先验证失败。
- [x] 删除 config/common/formula/core/HTTP/Texo 的压力支持，以及浏览器字段和现行说明；删除专属策略测试。
- [x] 运行 config 和 common 测试，验证配置拒绝且普通队列仍阻塞。

### 2. 公平原生队列

- [x] 在 common 队列测试复现新生产者插队，先验证失败。
- [x] 用 Tokio 信号量替代生产者 Notify 广播；保留原有消费者等待和批量语义。
- [x] 验证 FIFO 准入、取消、关闭、批量边界；测量队列正常与竞争路径开销。

### 3. 资源许可与预处理准入

- [x] 新增取消后真实 CPU/图像/模型仍保留许可的行为测试。
- [x] 实现 ResourceLease，并接入 run_cpu、PageLease、PageImage 和各模型请求。
- [x] 在 Layout、OCR、TSR 重型分配之前申请共享预算；表格与公式的裁剪许可随资源传播。
- [x] 用多页/多表格与取消用例验证分配前阻塞及恢复；运行各模型与 core 测试。

### 4. 文档结果与发布预算

- [x] 增加完成解析但发布未结束、或后台清理仍运行时容量不回收的回归测试。
- [x] Worker 在领取前申请共享文档许可，覆盖完整尝试和后台清理；预算来自 render.workers + render.queue_size。
- [x] 验证空闲 PDFium 与页面推理仍能重叠，Worker 达到预算后暂停领取，上传仍可持久化排队。

### 5. 全量审查与验证

- [x] 更新当前 README、浏览器接口及架构说明，保留既有历史审计文件。
- [x] 运行受影响测试、cargo fmt、clippy、WASM 边界与编译检查；记录环境限制和性能测量。
- [x] 自行审查取消、关闭、资源销毁顺序及死锁关系，检查最终 diff。

## 执行记录

- 初始工作区仅有用户既有未跟踪报告 `docs/reports/2026-10-02-queue-backpressure-audit.md`；不覆盖该报告。

- 配置旧字段拒绝、原生队列插队、取消公式图片仍持有容量、跨页面表格容量、表格准入超时、发布阶段继续领取、阻塞输出清理提前归还许可，均先观察到对应失败，再修复并通过。
- 补充所有权边界：HTTP 公式消费者恢复请求资源作用域；临时文件创建与 fsync/rename 使用共享阻塞执行边界，未收集输出先销毁、后归还许可。显式调用 Worker.process 时，等待文档许可也处于租约心跳与超时监督中。
- 保留原有页内并发测试，但把要求同时启动三张表的配置显式设为一张活跃加两张等待，避免测试隐含要求突破新预算。
- 由本人完成最终审查，遵循用户禁止 subagent 的要求。未创建 commit；未改变上传或数据库积压策略。

## 最终验证记录

| 检查 | 结果 |
| --- | --- |
| `rtk cargo test` | 658 通过，58 个需外部模型/数据库等条件的用例默认忽略，102 组 |
| `rtk cargo clippy --tests --examples -- -D warnings` | 通过，无告警 |
| `rtk cargo fmt --all -- --check` | 通过 |
| `rtk cargo check -p docparse-web --target wasm32-unknown-unknown` | 通过 |
| `rtk proxy uv run --locked scripts/check_wasm_compat.py` | 通过 |
| 在 `packages/wasm-web` 中运行 `rtk npm run check` | 通过 |
| `pending_completion_bounds_claims_without_blocking_uploads --ignored` | 独立临时 PostgreSQL、真实 PDFium：发布完成确认被锁住时不再领取第三份文档，上传仍返回 202 |
| `idle_pdfium_claims_next_job_before_previous_inference_finishes --ignored` | 独立临时 PostgreSQL、真实 PDFium：页面推理和下一份文档打开仍能重叠 |
| Layout `fixed_images_match_python_detections --ignored` | 5 份固定样本与 Python 检测结果一致 |
| OCR `disabled_orientation_does_not_expand_recognition_admission --ignored` | 真实模型通过，关闭方向模型不会扩大识别预处理容量 |
| TSR `tatr_recognizes_table_with_shared_cell_detection --ignored` | 真实模型输出保持一致；预先占满预算的图片能复用已有许可，不会二次申请死锁 |

上述数据库验收使用新建的临时 PostgreSQL 容器和独立数据库；验收后容器已停止并自动清理。没有使用或修改项目配置中的业务数据库。默认忽略的其余专项测试未全部运行，未测试其他 GPU 执行提供者。

## 队列性能对照

在同一临时程序中以相同指标代码和 `rustc -C opt-level=3` 编译旧、新原生队列，交替执行 7 轮。异步场景为 4 个生产者、1 个原生消费者、容量 16、批量上限 16，每轮各处理 200000 个请求；无竞争场景每轮各执行 100000 次同步入队/出队。中位数如下：

| 场景 | 修改前 | 修改后 | 耗时变化 |
| --- | --- | --- | --- |
| 无竞争同步路径 | 703 ns/请求 | 667 ns/请求 | -5.1% |
| 异步生产者竞争路径 | 1849 ns/请求 | 1384 ns/请求 | -25.1% |

批量出队合并归还许可，再于队列锁外唤醒等待者，避免每个输入各自操作信号量等待队列。该对照只衡量队列开销，不等同于完整 PDF 的吞吐或 GPU 性能。原始程序及记录位于 `/tmp/docparse-queue-perf-4oewhtko/compare.rs`、`comparison.txt`。

文档预算仍按数量计数，单本文档完整结果仍与文档大小成正比；调用方在 API 之前分配或自行缓存的输入/结果不计入服务端 Worker 的预算。

## 复查修复计划

- [x] 把共享图片并发推理复现写入 TSR 集成测试，确认失败；增加独立推理预算后验证通过，并保留取消资源生命周期。
- [x] 引入容量与许可一致的 ResourceBudget，迁移表格提供者接口；把自定义容量为 3 的窗口复现改为永久测试。
- [x] 直接测试表格阶段在 CPU 拥堵时的截止时间，避免把后续页面 CPU 阶段误算为表格超时；裁剪捕获轻量元数据并统一截止时间。
- [x] 按业务边界拆分 TableContext、Worker 与 OCR，复用 run_cpu 的上下文传播。
- [x] 运行相关测试、真实模型、数据库验收、全量测试、clippy 与 WASM 检查；记录最终结果。


## 复查修复结果

- 共享图片的张量越界复现、提供者容量与窗口不一致复现、表格阶段 CPU 等待超时复现，均已记录失败后修复通过。
- TSR 的裁剪预算与单次推理预算分离；每次 predict 都申请张量容量。删除 ResourceLease.contains，图片不再充当绕过新工作准入的凭据。
- ResourceBudget 将不可变容量与共享信号量封装在一起，表格窗口直接读取实际提供者容量。容量占满也不会把窗口误判成零。
- 裁剪只把块 ID、几何与共享图片引用传给 CPU；完整源文本留在调用方，超时能立即返回。永久测试直接调用表格阶段，验证 30ms 超时能在 CPU 饱和时完成，并验证取消清理仍持有许可。上一轮临时探针观察的是整页返回，混入了后续页面 CPU 阶段，因此不将它作为单个表格识别时限的断言。
- TableContext 分开识别与回填，Worker 分开监督、解析、发布与完成确认，OCR 分开调度与单行处理。移除重复 writer span/dispatcher。
- 重构暴露的大 future 告警通过让 ResourceLease.scope 直接返回 Tokio TaskLocalFuture 解决，删除额外 async 包装，没有为热路径增加 Box。
- 一个真实 OCR 旧用例原先要求三个页面越过容量为二的准备预算。修订为明确的两槽配置：先验证两个页面可并行准备，再验证第三页等待，最后验证三页完整识别结果一致；未放宽生产容量。

### 本轮验证

| 检查 | 结果 |
| --- | --- |
| `rtk cargo test` | 660 通过，60 默认忽略，102 组 |
| 隔离执行 `cpu_wait_is_within_table_deadline --ignored` | 通过，约 0.04 秒 |
| 真实 TATR 共享图片预算与结构/单元格识别 | 2 项通过 |
| 真实 OCR 批量一致性、关闭方向模型的预算、跨页有界并发 | 3 项通过；跨页用例按上述预算契约修订后单独重跑 |
| 临时 PostgreSQL：发布确认阻塞时限制领取、上传仍接收 | 通过 |
| 临时 PostgreSQL：空闲 PDFium 与前一份文档推理重叠 | 通过 |
| 临时 PostgreSQL：`pdf_logs_follow_jobs_across_execution_boundaries` | 通过，日志保持正确的任务关联 |
| `rtk cargo clippy --tests --examples -- -D warnings` | 通过，无告警 |
| WASM 编译、平台边界检查、SDK TypeScript 检查 | 通过 |

该轮队列 FIFO 实现未修改，前面的队列微基准不重复作为本轮性能收益。原有上传策略不变，未创建 commit、worktree 或 subagent。本轮临时 PostgreSQL 容器已停止并自动清理。
