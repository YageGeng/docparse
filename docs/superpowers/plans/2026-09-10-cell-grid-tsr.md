# CellGrid 与外部 TSR 实施计划

> 使用 executing-plans 在当前工作区逐项实施。用户已批准设计及最终提交、推送；在当前工作区实施，不创建 worktree。

**目标：** 统一候选网格状态，拆解表头与稀疏表格恢复，并提供外部 TSR 输入及 Native/Web 异步扩展点。

**架构：** TableGeometry 只读证据，CellGrid 管理拓扑及原文绑定；本地规则与外部输入共用填字/校验。按解析调用提供 TableOptions 和可选 TableStructureEngine。

**技术栈：** 现有 Rust、typed-builder、tokio、PDFium、wasm-bindgen、TypeScript Worker。

**设计：** docs/superpowers/specs/2026-09-10-cell-grid-tsr-design.md。

**边界：** 基线 867bf40；页号从 1 开始；最大 256 行、64 列、4096 个位置；默认 rules_only；本轮不实现具体外部服务/模型适配，真实外部模型效果不作为已验证结果。

## 任务 1：统一 CellGrid 并拆解规则恢复

涉及 table/grid/{cells,mod,header,spans,sparse,aligned,ruled,tagged}.rs、table/assemble.rs。

接口：CellGrid::try_from(Table)、replace_region、replace_rows、try_merge、bind_words、into_parts；GridRow 保存带边界、物理行和词索引。TableGeometry 负责只读几何判定。

- [x] 在 cells.rs 的 tests 模块验证占用空洞/重叠、非矩形合并、跨边界裁切和失败原子性；原文绑定后冻结结构。
- [x] 实现有界拓扑操作：先构建候选并验证，再替换自身；合并位置共享 owner。
- [x] 迁移所有策略和 populate；移除 RecoveredGrid，分离本地表头推断与纯文字绑定。
- [x] HeaderRecovery 拆出层级计划、短语列归属、表头构建和折叠；通过 CellGrid 同步更新行带与单元格。
- [x] SparseLayout 分 infer_columns、infer_rows、ruled_bands、anchored_rows；保持原算法选择及默认输出。
- [x] 运行 `rtk cargo test -p docparse-core --locked`，要求全部既有真实词框用例和反例通过。

## 任务 2：TSR 输入契约与纯转换

新增 table/tsr.rs（按职责需要拆子模块），修改 table/mod.rs、core/lib.rs。

接口：TableMode、TableOptions、TsrTableRequest、TsrTableInput、TableStructureError、TableStructureEngine::recognize；TryFrom<请求上下文与输入> 转换为 CellGrid。

- [x] 测试结构 token 的正跨度、嵌套覆盖、空单元格及 bbox 数量；拒绝无限/非法坐标、极大结构、错配 request_id 和任意 HTML。
- [x] 实现受限 token 状态机与 4/8 坐标框转换，使用实际 crop_to_viewport。
- [x] 外部赋值保留显式 header 标记，不重新应用本地规则；沿用完整原文覆盖校验。
- [x] 为旋转/CropBox/裁剪偏移及外缘空单元格加入坐标和原文来源测试。

## 任务 3：Native 异步协调

新增 runtime/table.rs，修改 page.rs、parser.rs、runtime/pipeline.rs、wasm_compat/native.rs。

接口：按次调用的 ParseOptions；兼容的 parse_bytes/path/page 默认入口；拥有本次预算的表格协调器。

- [x] 将同步 finish 分为稳定源归属与页面收尾，中间保留 TableEvidence 和图像变换。
- [x] rules_only 不调用外部，fallback 仅在完整本地失败后调用，external_only 跳过所有本地结构推断。
- [x] 测试提供器缺失、一次性调用、超时、并发上限、失败保留原文、成功清除中间失败告警及同页部分成功。
- [x] 裁剪已有页面图像，使用真实像素变换；不持有活 PDFium 句柄跨 await。
- [x] 使用兼容边界实现计时/超时，记录阶段与错误码，不记录图像和原始服务响应。

## 任务 4：Web/WASM 桥接与文档

修改 crates/web/src、packages/web/src/{types,protocol,index,worker}.ts、公开 README 和设计状态。

接口：parse options.table 与 onTableStructure(request, signal)；Worker 的结构请求、响应及取消控制消息。

- [x] 控制消息先于 executing 检查处理；回调留在主线程，WASM 接收拥有所有权的序列化数据。
- [x] 正确处理取消、超时、重复、迟到、错误和 close；不影响现有 parse/render 互斥。
- [x] 更新 external_tsr 输出枚举、阶段计时以及 Native/TS 接入示例。
- [x] 运行原生和 wasm32 Clippy、格式及兼容性边界检查，构建真实浏览器包。
- [x] 复测已有四份 PDF 全量 WebGPU，以及 CPU 重复解析；桥接协议使用受控输入验证，明确不声称真实外部识别模型验收。

## 完成检查

- [x] 本地默认输出与基线一致，修改后的大函数按完整职责拆分。
- [x] 外部输入不会创建新 table 或抢其他 block 的文字。
- [x] 规则/外部都经过同一占用、UTF-8、几何和原文唯一归属校验。
- [x] 更新本计划实际完成项和设计状态，报告测试证据及真实服务尚未接入的边界。

## 实际验收记录（2026-09-10）

- `rtk cargo test -p docparse-core --locked`：222 项通过，2 项忽略；包括既有真实词框回归和新增输入/调度检查。
- 原生 Clippy、wasm32 Clippy、格式、WASM 平台边界检查及 example TypeScript 检查通过。
- 浏览器生产包构建通过；WASM 优化后 6,937,103 字节，145 个导入校验通过。
- 四份 PDF 全量 WebGPU 共 223 页：MathNet 32 页、2603 23 页、EnterpriseRAG 24 页、综述 144 页。表格、单元格引用/坐标和警告与 `867bf40` 对应已验收基线精确一致。
- MathNet 两页及 2603 五页各重复两次 CPU 解析，并验证字形符号 PDF；总计 9 轮浏览器回归通过。
- 真实 SDK/Worker/layout 模型的调用方输入检查通过：5 个 table 中 2 个请求 fallback；external_only 请求 5 个，其中 4 个具备原文并成功，1 个缺少 Native/OCR 文字，明确报告 `TableTextAssignmentFailed`。
- 同页部分成功、错配请求 ID、5 个外部请求的超时取消、解析取消及迟到回复丢弃均通过；这些检查不评价 TSR 模型识别准确率。
- 本轮没有发现已验收默认表格结果退化。测试输入修正包括：空行边界不得切穿词框；没有 Native/OCR 原文的 table 不能仅凭结构输入补出文字。

本地详细产物（未纳入版本控制）：

- `packages/web/test-results/cell-grid-tsr/browser-verification.json` 与各 PDF 的 `browser-*.json`。
- `packages/web/test-results/cell-grid-tsr/table-input-report.json`。
- `packages/web/test-results/cell-grid-tsr/final-build.log`。

可复跑输入检查：先构建并启动 example 服务，再运行
`rtk proxy node crates/web/tests/table_input.mjs <实际表格PDF>`。
输入需包含同页多个 table 及至少一个本地未恢复的 table。

具体 TSR 服务/模型仍按用户要求暂不实现；本轮未创建 commit。

## 审查后的通用边界修复

审查发现：将按像素取整后的 crop_bbox 覆盖到 Block.bbox，可能将两个原本部分相交的表格变成包含关系，触发最终页面校验失败。

- 已将调用方无法修改的原表格边界传给现有 TryFrom 转换路径；外部输入格式保持不变。
- crop_bbox 仅描述采样范围，Block.bbox、polygon、source_regions 和文字归属在 TSR 阶段保持不变。
- 单元格仍先经过裁剪坐标合法性校验，再映射到 viewport；仅裁掉原表格外的采样余量，保留内部边界和行列跨度。
- 单元格与原表格无正面积交集时拒绝整个候选，沿用单表失败告警及原文保留，不静默删格。
- 新增相邻表格回归覆盖三种渲染尺寸、四种旋转、非零页面原点及 fallback/external_only；另有合并格、内部边界和边缘退化检查。
- 正式回归先在旧实现下失败，修复后通过；原生测试 225 项通过、2 项忽略。浏览器输入检查已加入区域及文字父级归属的精确对比。

边界修复的最终验证：

- 原生 225 项测试通过、2 项忽略；原生及 wasm32 Clippy、格式和平台边界检查通过。
- 生产浏览器包构建及 example TypeScript 检查通过；WASM 优化后 6,938,128 字节，145 个导入校验通过。
- 6 项调用方输入检查通过，包括固定布局边界、文字父级归属、部分失败、超时和取消。
- 四份 PDF 全量 WebGPU 共 223 页与既有默认表格基线精确一致；加上 CPU 重复解析及符号用例，9 轮浏览器回归全部通过。
- 新结果保存在 `packages/web/test-results/cell-grid-tsr/boundary-fix-{browser-verification,input-report}.json`，构建日志为 `boundary-fix-build.log`。
- 本修复未改变全局包含关系校验或本地表格启发式规则，未创建 commit。
