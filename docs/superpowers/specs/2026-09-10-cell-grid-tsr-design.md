# CellGrid 重构与外部 TSR 接入方案

状态：核心重构、Native 异步协调与 Web 输入桥接已实现并通过本轮验收；具体证据见同日实施计划。本轮按用户要求不接入具体外部服务或模型；最终提交及推送已获用户授权。

代码基线：`867bf40`（现有表格恢复、字形修复及其回归用例已提交）。

## 1. 已确认的需求与边界

- 表格区域由 layout 模型识别，不新增表格检测器，不让外部 TSR 创建或重新划分页面上的 table 区域。
- 吸收 LiteParse CellGrid 的状态统一管理方式，解决行列变更时多组数组需要手工同步的问题。
- 支持规则恢复失败后调用外部 TSR，也支持所有 layout table 都直接使用外部 TSR。
- 吸收 pdf-inspector TsrTableInput 的“结构 tokens + 单元格框”输入能力；外部模型或服务由调用方提供。
- 保留 DocParse 现有结构化单元格、rowspan/colspan、原文 UTF-8 引用和 Markdown/HTML 输出契约。

## 2. 推荐方案与取舍

推荐“统一 CellGrid + 异步外部结构提供器”。本地规则和外部结构都产生候选网格，然后使用同一套文字归属、单元格填充和最终校验。

| 方案 | 取舍 |
| --- | --- |
| 统一 CellGrid，外部通过异步提供器返回结构 | 推荐。一次 parse 内完成 fallback，不丢失临时词框和页面图像，也无需再次运行 layout。 |
| 完成 parse 后，由业务层重新提交 DocumentResult 和 TSR 结果 | 适合独立人工编辑场景，但当前 JSON 不保存完整 TableEvidence。需要额外的会话缓存、原文证据导出或重新提取 PDF，不作为本轮主入口。 |
| 外部直接返回最终 Table 或 HTML | 会绕过原文归属、缓存文字和几何校验，无法稳定维持现有输出契约。 |

本轮提供器既可以实际调用识别服务，也可以读取调用方预先获得的结构结果。它返回的数据都必须经过同一个输入转换流程。

## 3. 调用模式

增加按本次解析生效的 TableOptions，默认值维持现有行为。

| mode | 行为 | 外部不可用或结果无效时 |
| --- | --- | --- |
| rules_only | 使用现有 TaggedPdf → Ruled → TextAlignment → Sparse 顺序 | 保留原文，保持现有警告行为。 |
| fallback | 本地结构恢复与文字填充校验均成功则采用；否则请求一次外部 TSR | 保留原文，输出外部失败原因和最终未恢复状态。 |
| external_only | 每个 layout table 直接请求外部 TSR，跳过所有本地拓扑推断，包括 tagged 路径 | 保留原文并报告失败，不静默改用本地规则。 |

- fallback/external_only 没有提供器时，在开始解析前报配置错误。
- fallback 的触发点是本地候选无法成功生成并通过完整校验；不能只判断是否识别到了行列。
- 原文覆盖失败、单元格归属冲突和无有效候选均触发 fallback。
- 规则结果如果结构自洽但语义错误，现有校验未必能发现。这种情况不能承诺自动触发 fallback；external_only 提供明确的替代路径。
- 外部结果一旦通过校验，就保持它声明的拓扑和表头，不再运行本地表头压平、分组行推断等规则。

## 4. CellGrid 的职责

原 TableGrid 持有 bounds、LocatedSpan、物理行、字号等只读证据。现已更名为 TableGeometry，避免与新的可变 CellGrid 混淆。

| 类型 | 职责 |
| --- | --- |
| TableGeometry | 只读文字、物理行、线条、坐标容差等证据视图。 |
| CellGrid | 候选行列、单元格、位置占用关系和结构来源的唯一可变入口。 |
| TableCell | 复用现有单元格类型表达行列位置、跨度、可选 bbox 和表头标记；候选期不复制整段原文。 |
| GridRow | 行带和对应物理行/词索引；行重映射时与单元格同步更新。 |
| Table | 完成填字与校验后发布的现有输出类型。 |

CellGrid 吸收 LiteParse 的集中管理思想，但不照搬 text/repl 等平行字符串网格。DocParse 的原文仍只由 Block.lines 中的 TextItem 持有。

### 4.1 占用关系与变更

- 每个逻辑位置映射到一个单元格 owner；合并单元格覆盖的位置共用 owner，不产生重复单元格。
- 显式空单元格仍有 owner；“空单元格”和“被跨度覆盖的位置”必须区分。
- 行数、列数、跨度、bbox 和词引用不能由恢复器分别修改。
- 最初可在每次结构变更后重建占用索引。沿用最多 4096 个网格位置的边界，优先保证实现可审查，不引入复杂的增量索引。
- 标签来源的空单元格允许 bbox 缺失；外部和几何输入不能强制套用同一组均匀 x/y 轴。

实际操作通过以下关联方法完成：

- try_merge：合并一个矩形范围，并重新建立占用关系。
- replace_region / replace_rows：一次性替换矩形区域或行带，同步行带、跨度和单元格框。拓扑变更只在绑定原文之前进行，bind_words 后结构冻结。
- set_header：修改表头标记。
- bind_words：使用明确的归属方案绑定原文词索引。
- TryFrom<Table> / into_parts：验证完整占用；绑定完成后交给共用的 populate 和 Table::validate。

删除非空行列、切断已有跨度、产生非矩形合并或重复归属都应失败。操作先验证计划再应用；返回错误时保持候选不变。

### 4.2 必须拆开的两类判断

1. 通用不变量：有限坐标、合法跨度、占用无重叠、无缺失 owner、原文引用有效、文字不重复不丢失。所有来源都必须满足。
2. 本地推断规则：竖线阻断、字号/粗体判断、N/A 等数据标记、居中标题和行间距推断。用于本地候选，不能再悄悄改写外部声明的拓扑。

TableGeometry::assign_words 仅处理文字归属；assign 在本地路径单独标记推断表头。外部输入直接使用纯归属方法，保持声明的表头语义。

## 5. 两个大函数如何拆

### spans.rs

- HeaderRecovery 负责标题识别、表头层级、跨列表头和纵向 stub。
- 正文分组恢复留在独立的正文阶段，处理 section row 和 rowspan 标签。
- 两个阶段通过 CellGrid 操作，不再直接同步修改 row_spans、groups、ys、cells 和 row_count。
- 新增 grid/header.rs 承载表头阶段；spans.rs 保留简短编排和正文分组，不按每个 if 拆 helper。

### sparse.rs

- SparseLayout::infer_columns 产生表头短语和稳定列间隙。
- SparseLayout::infer_rows 明确返回 AnchoredRows 或 RuledBands 两类行方案。
- SparseLayout::build_grid 将方案转换成 CellGrid，共用占用和输出校验。
- 全横线纯文本表格分支从大段 else 中移到具名方法；初期可仍放在 sparse.rs 内，避免仅为缩短文件增加转发模块。

原有 tagged、ruled、aligned 也统一产出 CellGrid。当前 RecoveredGrid.table + assignment 逐步收敛到这个中间表示，不额外维持两套长期候选类型。

## 6. 外部请求和 TsrTableInput

沿用 OcrEngine 已有的异步 trait 和 WASM 兼容 Future 模式，新增 TableStructureEngine::recognize。提供器只接收一个已确定的 table 区域，返回该区域内的结构。

### 请求 TsrTableRequest

| 字段 | 含义 |
| --- | --- |
| request_id | 关联本次 parse 和本次表格请求，防止迟到结果串入下一次解析。 |
| page_number | 使用 DocParse 现有的一基页号。 |
| block_id | 当前表格 block 的稳定身份。 |
| crop_bbox | 完整 table 裁剪区域，使用旋转/CropBox 归一化后的 viewport points，左上角为原点。 |
| image | 该区域的实际裁剪图像及像素宽高。Rust 使用拥有所有权的数据，Web 使用可传输的编码图像。 |
| crop_to_viewport | 实际裁剪像素到 viewport points 的变换。 |
| reason | 本地失败原因，或者 external_only。 |

### 返回 TsrTableInput

保留 pdf-inspector 输入的两个核心部分：

| 字段 | 含义 |
| --- | --- |
| request_id | 必须对应仍在等待的请求。 |
| structure_tokens | 表格结构 token 序列，包括行、单元格、表头和跨度信息。 |
| cell_bboxes | 与单元格开始 token 一一对应的 4/8 数值框，坐标必须位于请求图像的原始像素空间。 |

页号、裁剪范围和坐标变换由请求上下文持有，不允许返回结果重新指定目标页面或扩大表格区域。外部实现如果缩放、padding 或旋转了图像，需要先把模型输出还原到请求图像坐标。

这种分工保留 TsrTableInput 的输入能力，同时适合自动 fallback 的请求/响应关系。独立保存结果的提供器可以根据请求中的页面和区域找到已有结果，再回填当前 request_id。

### 6.1 转换与校验

- 使用 TryFrom 将 request、调用方无法改写的原始 Block.bbox 和 TsrTableInput 转换成经过验证的候选网格。
- 使用有上限的专用 token 状态机，仅解析表格结构，不执行任意 HTML，不将模型输出 HTML 直接交给浏览器。
- 检查 token 数量/总长度、单元格数量、bbox 数量、有限值、有效尺寸、正跨度、拓扑占用和现有行列上限。
- 4/8 数值框先验证，再转换到规范坐标；不接受通过溢出或 NaN 绕过检查。
- bbox 转换容许裁剪边缘最多 1 像素的舍入误差并夹回有效范围，不自动修复单元格间的几何重叠，也不能改变模型声明的行列数、rowspan 或 colspan。无法唯一归属的文字应使候选失败，不使用无距离限制的最近单元格兜底。
- 可直接使用已有结构 token 的外部服务；其他服务通过调用方适配器转换成同一输入契约。本轮不内置具体表格识别模型。

## 7. 坐标与文字来源

不能照搬“坐标乘以 72/render_dpi”的唯一换算方式。当前 PDFium 渲染会受到 max_long_edge_pixels 限制，实际 DPI 可能低于配置值；应复用 PageTransform 和实际图像宽高，记录整数裁剪偏移后的真实变换。

- 裁剪范围取原始 layout table 区域与当前 block 已归属文字范围的联合，并夹到页面内；参考 source_regions，不能只使用可能收缩过的 Block.bbox，也不能把相邻 block 的文字加入当前表格。
- 当一个最终 table 由多个原始 table 区域合并得到时，使用其有效区域的包围范围并夹到页面内；目标仍是该最终 block。
- layout 归一化后冻结 Block.bbox、polygon 和 source_regions。crop_bbox 仅描述实际像素采样范围，不能写回 Block.bbox。外部单元格先校验裁剪范围，再映射到页面坐标并裁去原表格边界外的采样余量；内部边界及行列跨度保持不变。裁剪后无正面积或原文覆盖失败时，仅该表格保留原文并报告错误，其他表格继续处理。
- 只使用该 block 已经拥有的 Native/OCR 文字，不从相邻 caption 或其他 block 抢原文。
- 原文词框来自本次解析仍存活的 TableEvidence，不尝试仅凭 DocumentResult 的粗粒度 TextItem 框重新猜词框。
- 外部结构成功不等于已经取得文字。无可用文字的扫描区域仍依赖现有 OCR；必须明确报告文字不可用或覆盖失败。本轮 TSR 输入仅负责结构，不把外部生成的文字冒充 Native 事实。

## 8. 异步时机与运行时

PageAnalyzer::finish 原先同步完成所有阶段；现已分出 compose 和 complete，在两者之间协调外部结构。

实现流程：

1. 保留现有 PDF 提取、layout 和 OCR 流程。
2. PageAnalyzer 先产生归属稳定的 blocks 和一个内部页面草稿，保留 TableEvidence、公式区域及相关源索引。
3. runtime/table.rs 根据 mode 对各 table 运行本地恢复或外部请求。
4. 本地失败后，在仍持有当前页面图像和词框时制作 TSR 请求并 await 提供器。
5. 外部结果转换成 CellGrid，经过共同的 bind/populate/validate 后替换该 table 的最终结构。
6. 全部表格得到确定结果后再进行页面收尾、文档关系链接和结果序列化。

页面草稿不是公共结果，外部等待期间不能丢弃 TableEvidence，也不持有跨线程 PDFium 对象。

按一次解析共享外部并发预算，不能让每页各自建立完整并发池。默认 max_in_flight=2、timeout_ms=60000，可由调用方覆盖；本轮不自动重试外部调用。取消时关闭等待中的请求，丢弃迟到响应，并保留现有 Worker 终止语义。

## 9. Rust 与 Web/WASM 接口

### Rust

- 保留现有 parse_bytes、parse_path、parse_page 和 observer 入口，默认走 rules_only。
- 新增带 ParseOptions 的入口。TableOptions、可选 TableStructureEngine 和 observer 都按本次调用传递，避免把每个文档的外部等待状态放入可复用 DocParser。
- TableStructureEngine 使用已有 WasmCompatSend/WasmCompatSync/WasmBoxedFuture 约束。
- 提供器可被调用方以 Arc 复用；回调、认证和模型客户端由接入方管理。

### Web

已实现的调用形式：

```ts
await parser.parse(pdf, {
  table: { mode: "fallback" },
  onTableStructure: async (request, signal) => callYourTsrService(request, signal),
});
```

所有 table 走外部时，仅将 mode 改为 external_only。具体服务调用由调用方实现。

- 函数留在调用线程，不能直接通过 postMessage 传入 Worker。
- Worker 发出带 parse ID 和 table request ID 的 table_structure_request 事件；主线程执行回调并返回结果或错误。
- TSR 结果必须走独立控制消息通道，在当前 executing/ParserBusy 检查之前分发，否则 Worker 会在等待结果时把结果消息拒绝掉。
- 普通 parse/render 仍维持现有互斥语义。
- 主线程为每个表格回调提供本地 AbortSignal，并与本次 parse 的取消关联；Worker 超时后发出对应请求的取消通知。AbortSignal 本身不通过 postMessage 传递。
- 错误、取消、close、超时均清理关联 Promise；重复和迟到回复不能结算其他请求。Native 提供器 Future 被取消时也需要释放其等待资源。
- WASM 边界只传拥有所有权的数据，不把临时线性内存视图跨 await/Worker 保留。

本轮不需要把 provider、URL 或认证信息加入持久化 TOML 配置。可序列化 TableOptions 与非序列化回调应分离；CLI 默认行为保持不变。

## 10. 输出、错误和日志

- TableStructureSource 增加 external_tsr，Rust 与 TypeScript 同步。
- 默认规则路径的 Table/TableCell 输出保持不变；启用外部路径的调用端需要更新 source 枚举，不能假设旧版本消费者能识别新增 external_tsr。原文引用结构继续沿用现有格式。
- 中间本地失败不立即发布最终 TableStructureUnavailable；若 TSR 恢复成功，在 evidence/diagnostics 中保留采用 fallback 的原因即可。
- 外部失败、输入非法、文字覆盖失败分别给出稳定错误码，最终 table 未恢复时保留 Block.lines，不把结构化区域自动压成普通段落。
- 日志使用正文参数记录 page、block、request、来源和拒绝原因；阶段计时单独提供耗时。不记录图像、完整服务响应或凭据。
- 增加可区分本地恢复、外部等待和填字校验的阶段计时，页面及 document 完成事件只能在 TSR 结束后发出。

## 11. 文件改动地图

| 文件/目录 | 改动 |
| --- | --- |
| crates/core/src/table/grid/cells.rs | 新增 CellGrid、占用与原子网格变更操作。 |
| crates/core/src/table/grid/mod.rs | 将当前证据视图命名为 TableGeometry，保留 coverage 等证据计算，拆开归属与表头推断。 |
| crates/core/src/table/grid/header.rs | 新增表头恢复阶段及其状态类型。 |
| crates/core/src/table/grid/spans.rs | 收缩为正文分组和阶段编排，去掉手工同步多组数组的逻辑。 |
| crates/core/src/table/grid/sparse.rs | 按列方案、行方案和构建拆成关联方法，统一输出 CellGrid。 |
| crates/core/src/table/grid/{tagged,ruled,aligned}.rs | 适配新的候选网格，保留已有算法和优先级。 |
| crates/core/src/table/assemble.rs | 复用原文收集/填字逻辑，让所有结构来源汇合；返回可用于 fallback 的明确失败原因。 |
| crates/core/src/table/tsr/{mod,decode}.rs | 新增外部契约、提供器 trait、受限 token 转换与错误类型。 |
| crates/core/src/table/{mod,validate}.rs | 导出类型、扩展来源，统一最终数据校验。 |
| crates/core/src/runtime/table.rs | 新增异步协调、裁剪、并发和超时管理。 |
| crates/core/src/{page,parser,lib}.rs | 内部页面草稿、按调用传递选项、公共入口与类型导出。 |
| crates/core/src/runtime/pipeline.rs | 在原文归属后、最终收尾前调用表格协调器，并保留所需图像及 transform。 |
| crates/core/src/wasm_compat/native.rs | 保持路径入口兼容，增加对应 options 入口。 |
| crates/web/src/lib.rs | 外部提供器 JS/WASM 桥接和选项解码。 |
| packages/web/src/{types,protocol,index,worker}.ts | 类型、双向请求通道、回调生命周期、取消与错误路由。 |
| crates/layout/src/timing.rs | 新阶段计时；同步 Web 对应类型。 |

新增类型使用英文注释；超过三个字段的结构遵循 typed-builder；本方案不增加动态规则注册框架。

## 12. 实施顺序与验收

### 第一步：纯 CellGrid 重构

保持 rules_only 输出不变。先迁移候选状态，再拆 header/sparse；已有 tag、rule 和 alignment 都通过同一个单元格占用模型。现有 211 项测试、四份 PDF 的已确认 badcase，以及源码字节范围、rowspan/colspan、bbox、来源和警告对比是基线。

### 第二步：外部输入转换

实现纯数据 TsrTableInput → CellGrid → 既有填字/校验。覆盖嵌套跨度、空单元格、token/bbox 数量不一致、零/负跨度、极大跨度、非法框、相互覆盖、词框歧义、越界和 UTF-8 引用。

必须覆盖 0/90/180/270 度旋转、非零 CropBox、max_long_edge_pixels 降 DPI、整数裁剪偏移、模型缩放还原及空白外缘单元格。

### 第三步：Native 外部协调

接入三个 mode、提供器、取消、超时和并发预算。测试规则成功不调用外部、规则失败恰好请求对应表格、external_only 不执行本地推断、成功后不被规则再次改写、外部失败保留原文，以及同页多表的部分成功。

### 第四步：Web/WASM 输入契约与默认规则回归

验证 Worker 等待外部时仍能处理 TSR 控制回复；覆盖乱序、重复、迟到、错误和取消。通过真实 SDK、Worker、PDF 提取和 layout 模型验证调用方声明的结构输入，同时复测 WebGPU/CPU badcase。具体 TSR 服务按用户要求暂不实现，外部模型识别效果留待接入时验收。

供应输入检查用于验证结构传输、文字绑定及生命周期，不评价模型识别准确率。WebUI 默认规则回归继续运行真实生产 SDK 和 layout 模型；未来外部模型的 WebUI 验收必须运行实际提供器。

## 13. 本地源码依据

- LiteParse：crates/liteparse/src/markdown_layout/tables.rs，CellGrid 及 retain_rows/retain_cols。
- pdf-inspector：src/lib.rs，TsrTableInput 与 extract_tables_with_structure_cells_mem。
- DocParse：table/assemble.rs 的 populate 及 table/validate.rs 的原文/占用校验。
- DocParse：runtime/pipeline.rs 的 analyze_rendered_page 和 page.rs 的 finish。
- DocParse：layout/geometry.rs 的实际像素变换及 runtime/pdfium_executor.rs 的 limited_dpi。
- DocParse Web：worker.ts 的 executing 检查、protocol.ts 的操作定义和 index.ts 的解析回调生命周期。
