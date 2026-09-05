# DocParse 版面识别与文本融合设计规范

## 1. 文档状态

- 状态：已确认设计
- 日期：2026-09-04
- 目标仓库：https://github.com/YageGeng/docparse.git
- 参考实现：LiteParse crates-v2.14.3
- 版面模型：PaddlePaddle/PP-DocLayoutV3_onnx
- ONNX Runtime Rust 绑定：ort 2.0.0-rc.13

## 2. 背景与核心原则

DocParse 需要同时利用 PDF 原生文本与视觉版面模型，输出内容完整、阅读顺序稳定、可追溯的结构化结果。

单独依赖 PDF 文本流无法可靠处理多栏、跨栏标题、图表、公式和复杂阅读顺序；单独依赖版面模型又会受到漏检、误检、区域不完整和标签错误影响。因此第一版采用双轨约束融合：

- PDFium 文本决定内容完整性；
- PP-DocLayoutV3 优先提供 Block 标签和模型阅读顺序；
- 几何与 LiteParse 风格启发式负责组行、补洞、消除顺序冲突，并只在 fallback 区域拆分自然段；
- 任何模型错误都不能删除已经成功提取的原生文字；
- 融合严格以 Page 为单位，各页共享冻结的 DocumentContext。

## 3. 第一版目标

第一版必须实现：

1. 使用 PDFium 提取尽可能丰富的连续文本片段。
2. 使用 PP-DocLayoutV3 ONNX 完成页面级版面检测。
3. 在每个 Page 内独立完成 TextItem、Line、Block 的融合。
4. 在所有页面之间共享冻结的 DocumentContext。
5. 输出严格嵌套的 Block -> Line -> TextItem 结构。
6. 模型漏检时使用 XY-cut 和文本几何生成 fallback Block。
7. 综合模型顺序和几何顺序，得到确定性的页内阅读顺序。
8. 保证一个有效模型 Region 对应一个最终 Block，模型边界不被段落启发式拆分。
9. 对 inline_formula 做最小结构化处理，保持行内位置。
10. 提供公开的异步 OCR trait，但不内置 OCR 实现。
11. 使用 Tokio 作为异步运行时。
12. 使用 Figment 和独立的 docparse-config crate 管理配置。
13. 使用 uv 管理外置 Python 模型下载脚本。
14. 使用真实 PP-DocLayoutV3 和真实 PDF 完成最终端到端测试。

## 4. 第一版非目标

- WASM 或浏览器运行；
- 内置 OCR 引擎；
- 表格行列、单元格或合并单元格恢复；
- 公式 OCR、公式语义分析或 LaTeX 恢复；
- 跨页表格物理合并；
- 跨页段落或 Block 的物理合并；
- Rust 运行时自动联网下载模型；
- 逐字符 Glyph 输出；
- 将模型或真实测试 PDF 提交到 Git 仓库。

## 5. 核心不变量

1. 每个有效 TextItem 恰好属于一个 Line。
2. 每个 Line 恰好属于一个 Block。
3. Page 内不得因为版面模型漏检、误检或失败而丢失原生 TextItem。
4. Block、Line、TextItem 在 JSON 数组中的顺序就是最终阅读顺序。
5. 同一输入、配置和模型产生确定性一致的结果。
6. 并发数不能改变结构化输出。
7. 页面任务不能修改共享的 DocumentContext。
8. 文档级后处理不能移动、删除或重新归属 Page 内节点。
9. 原始文本和原始坐标必须保留，规范化结果不得覆盖事实数据。
10. 模型标签通过几何有效性门槛后优先成为 Block 主标签，启发式不得静默覆盖。

## 6. Workspace crate 划分

| Crate | 职责 |
|---|---|
| docparse-pdfium-sys | PDFium 原始 FFI |
| docparse-pdfium | PDFium 安全包装 |
| docparse-config | Figment 配置类型、加载、覆盖和校验 |
| docparse-layout | LayoutEngine trait、ort 集成、PP-DocLayoutV3 前后处理 |
| docparse-core | 文本提取、组行、页内融合、启发式、结果类型、OCR trait |
| docparse-cli | Tokio CLI、配置定位、解析入口和结果渲染 |

第一版不创建额外的 docparse facade crate。Rust 使用方直接依赖 docparse-core，CLI 二进制名称为 docparse。

~~~mermaid
flowchart TD
    CLI[docparse-cli] --> Core[docparse-core]
    Core --> Config[docparse-config]
    Core --> Layout[docparse-layout]
    Layout --> Config
    Core --> Pdfium[docparse-pdfium]
    Pdfium --> Sys[docparse-pdfium-sys]
~~~

docparse-config 不依赖其他内部 crate。docparse-layout 只输出通用 LayoutDetection，不依赖 docparse-core 的 Block 类型，避免循环依赖。

## 7. 外部版本与模型契约

### 7.1 PP-DocLayoutV3

- 仓库：https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx
- revision：46bbdf188bb0a772c08aed74882ce7e51a8f1ea6
- 文件：inference.onnx、inference.yml
- 许可证：Apache-2.0
- inference.onnx SHA-256：45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba
- inference.yml SHA-256：506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc

第一版已知模型契约：

- 输入尺寸 800 x 800；
- keep_ratio 为 false；
- 图像解码为 RGB；
- 使用 BICUBIC 插值直接缩放到 800 x 800；
- Python oracle 显式调用 cv2.setNumThreads(1)、cv2.setUseOptimized(false) 和 cv2.ipp.setUseIPP(false)，固定 OpenCV 4.10.0 generic CPU INTER_CUBIC 路径，避免 IPP/CPU dispatch 产生跨平台字节差异；Rust 对齐该确定性路径；
- 像素转换为 float32 并乘以 1/255，随后使用 mean=[0,0,0]、std=[1,1,1]；
- 输入从 HWC 转换为 NCHW；
- image shape 为 [batch,3,800,800]；
- image size 输入为 resize 后的 [height,width]，固定 [800,800]；
- scale_factor 输入为 [800/original_render_height,800/original_render_width]；
- tensor parity hash 对 C-contiguous NCHW 中每个 f32 的 IEEE-754 little-endian bytes 计算 SHA-256；
- 默认检测阈值 0.5；
- 标签集合包含 25 类文档区域；
- 固定 artifact 有三个输出：float32 bbox rows、int32 bbox_num 和 int32 instance masks；bbox row 为 [class_id,score,xmin,ymin,xmax,ymax,order_seq]，mask shape 为 [sum(bbox_num),200,200]；
- order_seq 由导出 ONNX 图内部的 global-pointer 后处理生成；该 artifact 不导出 order_votes，Rust 只读取 order_seq，不重复执行模型内部排序算法，也不得伪造 votes；
- polygon 在公共 LayoutDetection 中保持 Option。固定 artifact 虽输出 instance masks，但 PaddleX DetPostProcess 不消费它们；Python oracle 记录 mask hash 作为 artifact 诊断，Rust 只校验 mask dtype/shape 后释放，不从 mask 近似生成 polygon。polygon 保持 None，仅可记录由 bbox 明确派生的矩形 quad，并标记 geometry_source=DerivedFromBbox。

固定 ONNX 图已内置 PPDocLayoutV3PostProcess。它通过 origin_shape=round(image_size/scale_factor) 输出原始渲染图像像素坐标，因此 Rust 端不得再执行 800 x 800 到渲染图的逆缩放；只执行 rendered pixels -> canonical viewport points。外层 lossless fusion profile 固定为：

- score 使用严格大于 threshold，与 PaddleX LayoutAnalysisProcess 一致；
- bbox 坐标先使用 NumPy round 的 ties-to-even 语义取整，再裁剪到原始渲染图边界，退化框丢弃；
- layout_nms=false、layout_unclip_ratio=None、layout_merge_bboxes_mode=None、filter_overlap_boxes=false；
- 不运行会删除 reference、微小框或重叠 inline_formula 的展示型过滤；这些 detection 交给页内双轨融合判断；
- 有效 detection 按 order_seq 升序、source_detection_index 升序稳定排序，同时保留原始 order_seq。

这一 profile 有意不同于 PaddleOCR 面向最终可视化的默认 filter_overlap_boxes=true，因为 DocParse 必须保留 reference、inline_formula 和重叠候选作为融合证据。Python oracle 必须显式使用同一 profile，不能拿默认展示结果与 Rust 比较。

Rust 初始化时必须检查模型输入输出 schema。名称、数量、元素类型或维度与支持的 schema 不一致时初始化失败，不能在推理阶段猜测。

上述合同固定到 PaddleX commit ffb64904d23708863ff5b8da312a5cbd52a7f462。参考脚本固定 paddleocr==3.6.0，并通过 uv lock 固定其完整依赖树。实现时以该 commit 的 object_detection predictor/processors、PPDocLayoutV3PostProcess 和 layout_analysis processors 为真值来源，不能跟随浮动分支。

### 7.2 ort

第一版精确固定 ort = 2.0.0-rc.13。该版本仍是 release candidate，因此 ort 类型只能存在于 docparse-layout 内部，不能泄漏到 docparse-core 公共 API。

ort 关闭默认 feature，启用 api-28、std、ndarray、tracing、download-binaries、copy-dylibs，并为 build-time ONNX Runtime 下载启用唯一 TLS 实现 tls-rustls；不得同时启用其他 ort TLS feature。

默认启用 CPU。可选 Execution Provider 通过 layout-cuda、layout-coreml、layout-openvino 等 Cargo feature 隔离。第一版一次构建最多启用一个可选 EP；同时启用多个时使用 compile_error 给出明确错误。配置要求某 Provider，但构建未启用对应 feature 时直接返回初始化错误，不静默回退。

ort rc.13 的 EP 类型受编译期 feature 控制，且 download-binaries 会要求下载产物覆盖所选 EP。因此 --all-features 不是本项目的合法验证方式。默认 CPU 在所有开发机执行；CUDA、CoreML、OpenVINO 分别在具备对应 ONNX Runtime 1.28 产物和宿主条件的 CI job 中单独 check/test。

## 8. 配置系统

配置归属 DocParser 实例，不使用进程级 LazyLock、ArcSwap 或全局单例。同一进程可以创建多套模型路径、阈值和执行提供器不同的解析器。

~~~text
ConfigLoader
  -> RawConfig
  -> TryFrom<RawConfig>
  -> ValidatedConfig
  -> Arc<ValidatedConfig>
  -> DocParser
~~~

所有超过 3 个字段的结构，包括内部 BlockSeed、LineFragment、PageProbe 和图边类型，都按项目约束使用 typed-builder；Option 字段使用 builder default，并根据调用方实际持有类型决定是否使用 strip_option。配置转换和校验优先使用 From/TryFrom 或类型自身方法，不增加大量单参数 free helper。

Figment 覆盖优先级：

~~~text
代码默认值
< docparse.toml
< docparse.{profile}.toml
< DOCPARSE_* 环境变量
< 调用方显式 override
~~~

- Profile 环境变量：DOCPARSE_PROFILE
- profile 选择优先级：调用方/CLI 显式 profile > DOCPARSE_PROFILE > 无 profile；DOCPARSE_PROFILE 只参与选文件，不进入 RawConfig 字段覆盖。
- 环境变量前缀：DOCPARSE_
- 嵌套分隔符：__
- 相对路径基于主配置文件目录解析，不依赖当前工作目录。
- profile 名仅允许 ASCII 字母、数字、下划线和连字符，不能为空；profile 文件固定为主配置同目录下的 docparse.{profile}.toml，禁止路径分隔符和 ..。

初始配置：

~~~toml
[layout]
model_path = "models/pp-doclayout-v3/inference.onnx"
model_config_path = "models/pp-doclayout-v3/inference.yml"
model_manifest_path = "models/pp-doclayout-v3/model-manifest.json"
score_threshold = 0.5
execution_provider = "cpu"
session_pool_size = 1

[runtime]
page_concurrency = 4
render_queue_capacity = 2
blocking_task_limit = 4
continue_on_page_error = true

[render]
dpi = 144
max_long_edge_pixels = 2400

[fusion]
minimum_line_coverage = 0.30
center_minimum_line_coverage = 0.10
assignment_coverage_weight = 0.55
assignment_center_weight = 0.20
assignment_baseline_weight = 0.10
assignment_confidence_weight = 0.10
assignment_specificity_weight = 0.05
paragraph_gap_multiplier = 1.5
indent_tolerance_points = 6.0
font_size_tolerance_points = 0.5
estimated_font_size_tolerance_points = 1.5

[ocr]
policy = "disabled"

[output]
formula_placeholder = "[formula]"
include_evidence = true
include_diagnostics = false
~~~

这些值是可调默认值，不允许在算法文件中复制成散落常量。所有浮点配置必须有限；比例/阈值位于 [0,1]；assignment 权重非负，并以 abs(sum-1.0) <= 1e-6 判定总和为 1，不能直接使用浮点相等。

ValidatedConfig 只校验反序列化、三个模型路径的词法解析和参数范围，不无条件访问文件。只有构建默认 PpDocLayoutV3Engine 时才读取 model_path、model_config_path、model_manifest_path，并校验来源/revision/hash、YAML 合同和 ONNX schema；调用方注入自定义或 fake LayoutEngine 时不要求 PP-DocLayoutV3 文件存在。

主配置文件必须存在。设置 DOCPARSE_PROFILE 后，对应的 docparse.{profile}.toml 也必须存在；未设置 profile 时不加载 profile 文件。该行为避免拼错 profile 后静默使用默认配置。

RawConfig 及所有子配置拒绝未知字段，错误必须包含 Figment key path，避免拼错键后静默使用默认值。

CLI 的 --config 可选；省略时只使用当前工作目录下的 ./docparse.toml，不向父目录搜索。库 API 不隐式读取配置文件，只接受 ConfigLoader 的显式路径或调用方已经构造的 ValidatedConfig。

## 9. 模型下载

仓库提供 scripts/download_models.py 和对应的 uv script lock。调用方式：

~~~bash
rtk uv run scripts/download_models.py
~~~

脚本使用 PEP 723 声明依赖，并执行：

1. 从固定 Hugging Face revision 下载 inference.onnx 和 inference.yml。
2. 写入临时目录。
3. 校验已知 SHA-256。
4. 校验后原子移动到目标目录。
5. 生成 model-manifest.json，记录来源、revision、hash、时间和许可证。
6. 文件已存在且 hash 正确时跳过。
7. 支持 --output 和 --force。

Rust 核心不包含联网下载逻辑。模型缺失错误需要在正文中给出模型路径和 uv 下载命令。

## 10. 事实数据模型

结构化 JSON 是规范输出，使用显式 schema_version。

~~~text
DocumentResult
├── schema_version
├── context: DocumentContext
├── pages: Vec<PageResult>
├── relations: DocumentRelations
└── errors: Vec<PageError>

PageResult
├── page_number
├── width / height / rotation
├── blocks: Vec<Block>
├── warnings
└── diagnostics

Block
├── id
├── label / raw_label
├── text
├── label_source
├── confidence
├── bbox / polygon
├── source_region
├── model_region_id
├── model_order / final_order
├── evidence / semantic_hints
└── lines: Vec<Line>

Line
├── id
├── text
├── bbox / baseline
├── rotation / direction
├── model_region_coverage
├── inline_spans
└── text_items: Vec<TextItem>
~~~

Line 和 TextItem 不在 Page 顶层重复存储。需要扁平访问时由 Rust 迭代器提供。

schema_version 当前固定为字符串 2.0。v2 删除旧版派生文本字段，仅保留 raw_text；新增可选字段或新增可忽略枚举值提升 minor，删除、重命名字段、改变数组顺序语义或改变所有权规则提升 major。反序列化器必须拒绝未知 major，并允许已知 major 下更高 minor 中的未知可选字段。

DocumentResult 的 context、warnings、errors、evidence 和 diagnostics 都属于规范确定性输出，不得包含运行时生成的时间戳、耗时、峰值内存、随机数、绝对本地路径或 pointer；PDF 自带的 creation/modification metadata 作为输入事实可原样保留。所有可序列化 map 使用 BTreeMap 或先按明确 key 排序；性能数据只写入日志和 target 下的 E2E summary，不进入 DocumentResult。

公开 page_number 从 1 开始；PDFium extraction index、模型 source detection index、fallback Block split ordinal 和 Line ordinal 均从 0 开始。稳定 ID 只承诺在相同输入、schema major、模型 revision 和配置下可重复，不承诺跨算法 major 永久不变：

- TextItemId = p{page_number}:t{pdfium_extraction_index}；
- OCR TextItemId = p{page_number}:o{source_result_index}，source_result_index 在 OCR Vec 任何过滤前固定；
- ModelRegionId = p{page_number}:m{source_detection_index}；
- FallbackRegionId = p{page_number}:f{stable_xy_cut_region_path}；
- 模型 BlockId = p{page_number}:b:m{source_detection_index}:s0；保留 `s0` 以维持稳定 ID 形状；
- fallback BlockId = p{page_number}:b:f{stable_xy_cut_region_path}:s{split_ordinal}；
- LineId = {BlockId}:l{line_ordinal}。

任何 ID 都不能使用随机 UUID、内存地址或 HashMap 迭代位置。DocumentRelations 只引用这些稳定 ID。

XY-cut path 根节点固定为 r；水平 whitespace cut 产生的上下子区按 top、left、bottom、right 排序并追加 .h{ordinal}，垂直 cut 产生的左右子区按 left、top、right、bottom 排序并追加 .v{ordinal}，ordinal 从 0 开始。例如 r.h0.v1。路径排序与 RTL/Vertical 阅读方向无关，方向只影响叶内 Line 顺序。

### 10.1 Block 标签

- raw_label 原样保留模型字符串。
- label 使用可扩展规范化类型。
- 新模型标签进入 Unknown(String)，不能导致页面失败。
- label_source 为 Model、Heuristic 或 Fallback。
- 模型检测满足置信度和几何有效性门槛后，模型 label 为主标签。
- 启发式只能增加 evidence、semantic hints、内部 Line 和顺序约束，不能拆分模型 Block。
- 一个通过校验的模型 Region 必须且只能产生一个 Block，并保留模型 label 和 model_region_id。

### 10.2 TextItem

TextItem 是 PDFium 连续文本片段，不包含逐字符 Glyph。尽可能保留：

- 唯一文本事实 raw_text，不保存或推断第二套文本；
- 原始提取序与最终行内序；
- bbox、polygon、baseline、rotation；
- PDF 坐标、viewport 坐标和坐标变换标识；
- 字体名、字号、字重和 font flags；
- bold、italic、monospace；
- text matrix；
- fill/stroke color；
- char codes、MCID、可选 text_object_index；
- Unicode 映射状态和 generated-space；
- link、strike；
- Native/Ocr 来源与置信度；
- 已确认的子集字体控制字形 `0x01/0x02` 分别恢复为空格和独立的 `EncodedHyphen` 连字符事实；其他控制字符清理与片段合并继续记录，不恢复几何空格。

PDFium 当前没有通用的 text page-object number API。text_object_index 仅在能把 TextChar 的临时 text-object handle 映射回当前页稳定 object enumeration 时填写，否则为 None；裸 FPDF_PAGEOBJECT 指针不得进入结果、日志或序列化。

### 10.3 Block 与 source region geometry

- Block.text 是便捷派生摘要：普通文本按最终 Line 阅读顺序使用一个 ASCII 空格连接；FlowText/Title 对以独立 `EncodedHyphen` 结尾且几何上确属下一正文行的边界保留连字符并省略行间空格，不猜测或删除连字符。algorithm 使用换行并保留行首缩进。其他标签不执行正文连字符规则；无文本 Line 时为空字符串，完整 Lines 必须继续保留。
- Block.bbox 是最终拥有内容的 canonical viewport 包围盒：有 Line 时取所有 Line.bbox 的稳定 union；无 Line 的模型对象取 source region bbox。
- Block.polygon 只表示能够被事实 geometry 支持的最终内容边界；第一版文本 Block 可为 None，原始模型 polygon 保存在 source_region。
- Block.source_region 保存原始模型或 fallback Region 的 ID、bbox、可选 polygon、geometry_source、置信度和原始顺序证据。模型 Block 与模型 Region 一一对应；同一 fallback Region 拆出的 Block 共享只读 source_region 事实。
- Line.bbox 是其 TextItem bbox 的 union；允许 Line/Block 内容部分越出模型 source region，因为唯一归属门槛允许部分覆盖。
- ResultValidator 检查 Line 位于最终 Block.bbox 的浮点容差内，但不能错误要求 Line 完全位于 source_region geometry 内。

## 11. 坐标系统

唯一事实坐标为 PDFium 左上角、Y 向下、72 DPI viewport points。

每页创建 PageTransform，记录 PDF page space 到 viewport、viewport 到渲染像素、渲染像素到 800 x 800 模型输入，以及逆变换、页面旋转和缩放。

PP-DocLayoutV3 使用非等比 800 x 800 resize，因此 X/Y 比例分别记录。模型坐标不得覆盖 TextItem 原始坐标。测试要求坐标往返误差小于明确浮点容差。

## 12. DocumentContext

融合以 Page 为单位，但页面共享冻结的文档上下文。

~~~text
DocumentContextBuilder
  -> 收集文档信息和各页轻量信号
  -> 冻结
  -> Arc<DocumentContext>
  -> 多个独立 PageAnalyzer
~~~

DocumentContext 包含页数、元数据、outline、页面尺寸与旋转、内容边界、全局字体统计、正文基准字号、重复页眉页脚指纹、页码模式、标题字号候选、配置快照、模型版本和文档级诊断。

页面任务只读 Arc<DocumentContext>，不得动态修改。

所有 PageResult 完成后，DocumentLinker 只读生成 DocumentRelations：

- 重复页眉页脚引用；
- 跨页段落 continuation 候选；
- 标题层级关系；
- 跨页表格 continuation 候选。

这些关系不能删除、移动或重新归属页内节点。

## 13. 文档执行阶段

### 13.1 预扫描

1. 打开文档，读取页数、元数据和 outline。
2. 串行通过 PDFium 提取每页原生 TextItem 和轻量信号。
3. 不缓存整本文档的渲染位图。
4. 构建并冻结 DocumentContext。

PdfiumExecutor 在整个文档任务期间独占 document handle，并在串行队列中逐页打开和关闭 page handle。预扫描结束后保留输入 bytes/path、document handle 和 ExtractedPage，不保留 page handle。第二阶段按需重新打开页面进行渲染，渲染位图发送到有界队列后立即关闭 page handle。

### 13.2 页内分析

每页独立执行：

1. 渲染页面。
2. 运行 PP-DocLayoutV3。
3. 由 Native TextItem 与 LayoutDetection 构建 PageAnalysisDraft、原子归属片段和建议 OCR 缺失区域。
4. 注入 OcrEngine 时执行 OCR；未注入时记录 OcrUnavailable。OCR 结果只转换为候选 TextItem，不直接创建结构。
5. 合并并去重 Native/OCR 文字事实。
6. 建立模型 BlockSeed。
7. 以单个 TextItem 为最小边界映射到唯一主 BlockSeed，部分覆盖的视觉行不得吞并框外文字。
8. 在每个模型 owner 内最终组行，同时将未归属 TextItem 重新组行后运行 residual XY-cut。
9. 每个模型 Region 生成一个 Block；fallback XY-cut 叶内允许按自然段拆分。
10. 将模型 Block 与 fallback Block 一起送入并排序页内阅读顺序 DAG。
11. 生成 PageResult。

### 13.3 文档后处理

1. 按页码稳定排序 PageResult。
2. 生成 DocumentRelations。
3. 生成 raw 或 semantic 渲染视图。

## 14. 页内双轨融合

### 14.1 保守 LineFragment

文本提取后不立即跨 owner 进行最终组行。归属确定后，同一模型 owner 或 fallback 集合中的文字先按带容差的 y-band 聚类，再在 band 内按 x 排序；字号兼容且水平距离合理时才合并。遇到大间距先拆分，避免左右栏同高度内容被拼成一行。

### 14.2 模型 BlockSeed

每个有效模型检测创建 BlockSeed，保存 raw label、class id、confidence、bbox/polygon、model order、模型版本和坐标变换。

模型返回 NaN、退化 polygon、越界 class id 或无法逆变换的 detection 时，忽略并记录 diagnostics。

### 14.3 唯一归属

所有评分 geometry 先转换到 canonical viewport points，并优先使用有效模型 polygon，否则使用模型 bbox。归属评分以仅含一个 TextItem 的原子 LineFragment 为最小边界，避免一条跨越模型边界的视觉行整体归入模型 Region。候选资格为：coverage >= minimum_line_coverage，或中心点位于 Region 且 coverage >= center_minimum_line_coverage。inline_formula 使用独立非占有匹配，不进入主 owner 竞争。

- coverage = clamp(intersection_area(line_bbox, region_geometry) / line_bbox_area, 0, 1)；line_bbox 面积为 0 或任一值非有限时不产生候选；
- center_inside 为 LineFragment bbox 中心点位于 region geometry 内时的 1，否则为 0；边界点视为 inside；
- baseline_intersection = clamp(位于 region geometry 内的 baseline 长度 / 完整 baseline 长度, 0, 1)；baseline 缺失或退化时为 0；
- model_confidence 在模型结果校验时限制到 [0,1]；
- specificity = 1 - clamp(region_area / page_area, 0, 1)，page_area 必须为正且有限。

对合格候选计算：

~~~text
specificity = 1 - clamp(region_area / page_area, 0, 1)
assignment_score =
    coverage * assignment_coverage_weight
  + center_inside * assignment_center_weight
  + baseline_intersection * assignment_baseline_weight
  + model_confidence * assignment_confidence_weight
  + specificity * assignment_specificity_weight
~~~

权重必须非负且总和为 1。评分使用 f64 和 total_cmp，不使用 epsilon 合并不同分数。候选依次按 assignment_score、coverage、model_confidence、较小 region_area、较小 model_order、较小 source_detection_index 排序。最高候选成为唯一主 Block；其他候选只写入 evidence，不复制 Line/TextItem。label compatibility 只能作为诊断信息，第一版不参与评分，避免启发式间接覆盖模型标签。

### 14.4 residual fallback

未归属 TextItem 先按 y-band 重建保守 LineFragment，再运行 LiteParse 风格 XY-cut。已确认模型区域作为障碍物参与切分，避免 fallback 横穿表格、图片或正文区域。模型整页失败时整页使用纯几何 XY-cut。任何 residual 文字必须由 `label_source = Fallback` 的 Block 唯一拥有，不得吸附到最近模型 Region，也不得丢弃。

### 14.5 模型 Region 保持 Block

每个有效模型 Region 固定生成一个 Block。模型 Region 内可以包含多条最终 Line，但行距、缩进、字体、样式和 anchor 不得改变模型 Block 边界。自然段拆分只应用于没有模型 owner 的 fallback XY-cut 叶。

## 15. 标签策略

| 策略 | 典型标签 | 第一版行为 |
|---|---|---|
| FlowText | text、content、abstract、reference_content、aside_text、footnote、vision_footnote、vertical_text | 模型 Region 保持单一 Block；fallback 允许拆自然段；vertical_text 使用独立方向排序 |
| Title | doc_title、paragraph_title、figure_title | 合并紧邻换行标题，不与正文合并 |
| Atomic | image、chart、seal、header_image、footer_image | 保持独立，可无 Line |
| Formula | display_formula、inline_formula、formula_number | 保持位置和关联，不识别 LaTeX |
| Chrome | header、footer、number | 保留，由 DocumentContext 标记重复性 |
| Structured | table、algorithm、reference | 只保证内部阅读顺序 |

每个通过模型阈值与 geometry 校验的 detection 至少保留一个 BlockSeed。没有文字的 image/chart/table/formula 等 Region 仍输出空 lines 的 Block，以保留 layout 映射；没有文字的 FlowText/Title Region 同样保留，但增加 EmptyModelRegion warning。Text/Markdown renderer 可以不输出空 Block 的正文，JSON 不得删除它。

## 16. Line 与段落启发式

每个 Line 计算字符加权字体、样式比例、bbox、baseline、line height、Block 内相对 indent、anchor、文字方向、相邻行间距、行宽比例和文本边界信号。

Fallback FlowText 内相邻行默认继续同段需要：

- 相同 FallbackRegionId 和局部 XY-cut 叶子；
- Center 与非 Center anchor 没有切换；
- 真实字号差不超过 0.5pt；
- 估算字号差不超过 1.5pt；
- 整行粗体状态没有明显切换；
- 下一行没有突然右缩进超过 6pt；
- 垂直间距不超过 1.5 倍两行最大高度；
- 中间没有公式、图片、表格或分隔线。

特殊规则：

- 首行缩进后恢复左对齐仍属于同段；
- 子集字体的编码连字符以独立 TextItem 保留在原始文本中并记录 `EncodedHyphen`；canonical Block 摘要不得猜测并删除该字符；
- 中文、日文不依赖英文小写开头；
- RTL 行反转片段阅读顺序，不反转片段内部字符；
- Vertical 使用 LiteParse 风格的虚拟阅读轴：角度以循环距离在 ±2° 内吸附到 90°/270°，90° 按 top 升序，270° 使用与 `max_y - y - height` 等价的 bottom 降序；虚拟轴只决定 TextItem 顺序，不修改公开 bbox、rotation 或 raw_text；
- 同一纵向带内若相邻项沿页面 y 轴的间距超过该带最大 item height 的 3 倍，则拆成不同 LineFragment，避免合并远距轴标签；
- fallback 大文本区域内部可再次运行局部 XY-cut；
- 列表仅写 semantic hint，不覆盖模型 label。

## 17. 阅读顺序

Block 顺序以有向约束图表示。边来源包括模型 model order、明确上下关系、同带左右关系、局部 XY-cut 前序、标题与正文或 caption 与视觉对象的局部关系，以及 fallback Block 的几何插入关系。

模型 Region 先按 (order_seq 升序、source_detection_index 升序) 排列，只在相邻 Region 之间加边，避免 O(n²) 全序边。模型 Region 各自只有一个 Block；同一 fallback Region 的多个自然段 Block 按局部 Line geometry 连成 IntraRegion 边。空模型 Block 仍参与 Region 顺序。

每条边保存 source、reason、source confidence 和 preservation_weight。schema 2.x 固定权重为：

| Edge source | preservation_weight |
|---|---:|
| StrongVertical：明确不重叠上下 | 1.00 |
| IntraRegion：同一 fallback Region 子 Block 局部顺序 | 1.00 |
| Model：相邻 model order | min(from_confidence,to_confidence) |
| XyCut：同一切分树前序 | 0.85 |
| CaptionRelation：caption 与局部对象 | 0.80 |
| BandHorizontal：同一明确栏带左右 | 0.65 |
| FallbackInsertion：fallback 几何插入 | 0.60 |

若图存在环，对每个强连通分量重复删除 preservation_weight 最低的边；同权重依次优先删除 Model、FallbackInsertion、BandHorizontal、CaptionRelation、XyCut、IntraRegion、StrongVertical，最后按稳定 edge key。这样低置信模型边会先于可靠几何边删除，高置信模型顺序仍优先于弱几何。StrongVertical/IntraRegion 理论上应无环；若只能删除这两类边，记录 InternalOrderConflict error，而不是静默产生任意顺序。

消环后使用稳定拓扑排序。零入度 tie-break 固定为 XY-cut region path、quantized_y_band=floor(block_bbox.top/0.5pt)、Y、X、source priority、BlockId；坐标使用 canonical viewport f64 total_cmp，0.5pt 量化只用于 band，不改原始 geometry。改变上述边权或 tie-break 会改变数组顺序语义，必须提升 schema major。

Block 内 Line 顺序由局部 XY-cut、baseline 和文字方向确定。table Block 第一版采用视觉阅读顺序，不恢复单元格。

## 18. inline_formula

inline_formula 是非占有型 Layout Region，不与普通 text Region 竞争整行归属。

当公式与某 Line 垂直重叠、baseline 接近且 X 位置位于行内时，挂载为 InlineSpan，包含 label、confidence、bbox/polygon、text_item_range、extracted_text 和 content_status。

- PDFium 可提取公式字符时原样保留。
- 部分可提取时标记 Partial。
- 完全无文字时标记 Missing，不伪造公式内容。
- 文本或 Markdown 渲染可按配置输出 [formula]。
- 没有匹配 Line 的较大公式区域成为独立 Formula Block。

## 19. OCR 扩展

第一版只提供公开、可注入的异步 OcrEngine trait，不提供实现、不引入 OCR 模型。

OcrPolicy 第一版只有 Disabled 和 MissingRegions：Disabled 即使注入引擎也不调用；MissingRegions 仅在 prepare 产生非空缺失区域时调用。MissingRegions 未注入引擎不是初始化错误，对应区域标记 OcrUnavailable。扫描页没有 Native TextItem 时，整页内容区域自然成为缺失区域，不需要额外 Always 模式。

OCR request 包含 page number、页面图像、尺寸、像素格式、DPI、PageTransform、建议 OCR 的缺失区域和原生文本覆盖统计。结果包含 text、bbox/polygon、confidence 和 engine metadata。

每个解析器只注入一个 OcrEngine。docparse-core 收到原始 OCR Vec 后先按返回位置赋予从 0 开始的 source_result_index，再做坐标校验与 Native 去重；自定义引擎必须保证同一输入的返回顺序确定，才能满足稳定 ID 合同。

页面 RGB8 像素由 Arc<PageImage> 共享给 LayoutRequest 与 OcrRequest；PageImage 内部使用 Arc<[u8]> 并在构造时校验 width * height * channels 与 buffer 长度。不得为 Layout/OCR 各复制一份整页位图，最后一个页面阶段结束后释放该 Arc。

docparse-core 负责坐标转换、去重、组行和融合。外部 OCR 不能直接创建或修改 Block。

调用时序固定为 PageAnalyzer::prepare(native, layout, context) -> OcrRequest -> PageAnalyzer::finish(draft, optional_ocr_result)。prepare 只计算可复用中间事实和缺失区域，不产生可观察 PageResult；finish 统一执行文字去重、最终组行、归属和顺序，避免 Native 与 OCR 走两套算法。

未注入 OCR 不是错误。扫描页或缺失区域标记为 OcrUnavailable。自定义 OCR 失败时保留已有 Layout 和 Native TextItem。注入引擎但 policy=Disabled 时不产生 OcrUnavailable warning，因为 OCR 是调用方明确关闭。

## 20. 异步 API 与运行时

主要 API：

~~~text
DocParser::from_config(config).await
DocParser::parse_path(path).await
DocParser::parse_bytes(bytes).await
DocParser::parse_page(page).await
~~~

公开的 parse_page 为单页独立使用场景构建单页 DocumentContext。文档解析内部调用不公开的 analyze_page，并显式传入整本文档共享的 Arc<DocumentContext>，避免同名 API 隐式使用不同上下文。

同步入口 parse_path_blocking 仅包装异步实现。若调用线程已经处于 Tokio runtime 内，返回 BlockingInsideRuntime 错误，要求调用方使用异步入口，不在嵌套 runtime 中阻塞。

DocParserBuilder 接收 Arc<ValidatedConfig>、可选 Arc<dyn LayoutEngine> 和可选 Arc<dyn OcrEngine>。默认 LayoutEngine 为 PP-DocLayoutV3，默认无 OcrEngine。

运行时资源：

~~~text
ParseRuntime
├── PdfiumExecutor
├── Arc<DocumentContext>
├── Arc<dyn LayoutEngine>
├── page_semaphore
└── blocking_task_semaphore
~~~

- PDFium FFI 在受控阻塞执行器中串行访问。
- ORT 同步推理通过 spawn_blocking 执行。
- PpDocLayoutV3Engine 在 docparse-layout 内部拥有 LayoutSessionPool，并使用 semaphore 限制并发；docparse-core 不接触 ort Session。
- 页面渲染与推理之间使用容量为 render_queue_capacity 的 Tokio mpsc 有界队列限制位图内存。单一 dispatcher 持有不可克隆的 Receiver，并通过 JoinSet 与 page_semaphore 启动最多 page_concurrency 个页面分析任务。
- blocking_task_limit 只限制 docparse-core 自身的阻塞 CPU 工作；不得再次包裹 LayoutEngine 内部的 ORT spawn_blocking，以免形成双重 semaphore 等待。
- 不得在 await 期间持有 PDFium lock、Session mutex 或普通同步锁。
- fail-fast 时先关闭/丢弃 render Receiver 使 producer 的 send 返回，并停止启动新页面；已进入 ORT spawn_blocking 的页面任务不可强制 abort，必须等待 JoinSet 自然回收后再显式关闭 PdfiumExecutor。调用方直接 drop parse Future 时无法同步等待阻塞推理，只承诺 channel 关闭后 PDFium worker、ORT closure 和 Session lease 最终释放，并用 cancellation 回归测试证明不会永久泄漏。

## 21. 错误、降级与日志

- 配置无效、模型不存在或模型 schema 不支持：初始化失败。
- PDF 无法打开：文档失败。
- 单页 PDFium 提取失败：记录 PageError，是否继续由配置决定。
- 单页 Layout 失败：默认纯几何 fallback，并记录 warning。
- 非法 detection：丢弃 detection，不丢弃页面。
- 未注入 OCR：缺失区域标记 OcrUnavailable。
- 自定义 OCR 失败：保留 Native 和 Layout 结果。
- DocumentRelations 失败：返回完整 PageResult 和文档级 warning。

禁止用 panic 处理外部输入错误。

日志只放在模型加载、文档开始结束、页面阶段切换、ORT 或其他第三方调用前后、降级分支和错误返回前。使用 tracing 完整宏路径和正文格式参数，不记录完整图片、张量、凭证或整份配置。

## 22. 输出与 CLI

CLI：

~~~bash
docparse parse input.pdf --config docparse.toml --format json
docparse parse input.pdf --format text --view raw
docparse parse input.pdf --format markdown --view semantic
docparse inspect-model --config docparse.toml
~~~

Raw 视图保留所有 Block、页眉页脚、占位符和分页。Semantic 视图根据 DocumentRelations 隐藏重复页眉页脚，并只在展示层连接跨页段落候选。渲染器不能修改 PageResult。

诊断 overlay 显示模型区域和 label、最终 Block 边界和顺序号、Line baseline 和行序、fallback 区域、inline formula 与映射冲突。

## 23. 测试

### 23.1 默认测试

默认测试不依赖真实模型，使用可注入 fake LayoutEngine，覆盖：

- 坐标往返；
- TextItem 保守组行；
- 唯一归属；
- model Region 一对多拆 Block；
- residual XY-cut；
- inline formula；
- 阅读顺序 DAG 和消环；
- 中文、RTL、Vertical；
- 段落间距、缩进、字号和样式；
- 模型全覆盖、部分覆盖、错误覆盖和完全失败；
- 多页并发与单线程一致；
- JSON round-trip。

### 23.2 模型一致性

由 uv 管理的 Python 参考脚本固定 paddleocr==3.6.0，并对固定图片运行官方 ONNX 流水线。该参考 fixture 必须在 Rust 前后处理实现之前生成，内容包含预处理 tensor 的 dtype、shape、min/max、SHA-256，image size、scale factor，以及 lossless fusion profile 下最终 label、confidence、bbox/polygon、detection 数量、source detection index、model reading order、threshold 和各过滤开关。

真实模型测试默认 ignored，仅在模型存在时运行。

### 23.3 真实 PDF E2E

真实测试输入固定为运行用户 `~/Downloads/` 顶层的所有常规 PDF 文件，扩展名匹配不区分大小写。测试目录通过环境变量指定：

~~~bash
rtk uv run scripts/download_models.py

DOCPARSE_E2E_PDF_DIR="$HOME/Downloads" \
rtk cargo test -p docparse-core --test real_pdfs -- --ignored --nocapture
~~~

设计确认时该目录有 5 份 PDF、110 页，覆盖 A4/Letter、8 至 47 页文档，均未加密且无 Tagged PDF 结构。

仓库提交 tests/e2e-corpus.toml，只记录 logical_id、期望 basename、SHA-256、文件大小和页数，不提交 PDF。E2E 启动时必须先扫描目录中的全部 PDF，再验证发现的 basename 集合与 manifest 完全相等，并逐项匹配 SHA-256、文件大小和页数；缺失或额外 PDF 都必须失败，禁止静默忽略。仅校验“5 份/110 页”不足以证明测试语料一致。更新 `~/Downloads/` 语料后必须显式更新 manifest 并经过代码审查。

E2E 必须使用真实 docparse-pdfium、PP-DocLayoutV3、ort Session、Figment 配置、DocumentContext、Page 融合、DocumentRelations 和 JSON 序列化，不允许 fake LayoutEngine。

每页验证：

- 页数一致；
- 无 panic、死锁或任务丢失；
- 坐标有限且合理；
- TextItem 与 Line 唯一所有权；
- Native TextItem 不丢失；
- 顺序稳定；
- 模型漏检产生 fallback；
- 模型失败可降级；
- JSON round-trip；
- 重复运行结果一致。

汇总记录文件数、页数、耗时、峰值内存、模型/Fallback Block 数、未覆盖文字比例、冲突边、消环数量、各 label 数量和 warning/error 数量。

确定性门禁分别以 page_concurrency=1 和 4 对完整语料重新执行真实 ORT。两次运行各自产生 canonical-hashes.json，其中包含 corpus manifest、模型和影响规范输出的配置指纹，以及所有 document/page hash；随后必须调用仓库内比较器。比较器仅在身份和全部 canonical hash 完全一致时退出 0，任何差异都阻断验收，不能只依赖人工查看两个文件。

机器不变量覆盖全部 110 页。视觉验收每份 PDF 至少检查首页、中间页和末页，并自动追加 fallback 比例最高、映射冲突最多、消环最多的页面。

真实 PDF 不提交到仓库。第一版性能只记录，不使用跨机器不稳定的硬耗时阈值。

## 24. 许可证与分发

- DocParse 派生 PDFium crate 继续遵守 Apache-2.0 和现有 NOTICE。
- PP-DocLayoutV3 模型许可证和固定来源写入 model-manifest。
- ort 与 ONNX Runtime 许可证加入 THIRD_PARTY_NOTICES。
- 发布包不内置真实测试 PDF。
- 后续若分发模型文件，必须同时分发模型许可证和来源说明。

## 25. 第一版完成标准

1. 所有新 crate 构建、测试和 Clippy 通过。
2. 默认离线测试全部通过。
3. Python 与 Rust 模型前后处理一致性测试通过。
4. `~/Downloads/` 当前全部 5 份、110 页真实 E2E 全部完成。
5. E2E 不变量全部通过。
6. 指定页面 overlay 经过视觉检查，无明显跨栏、漏字、重复字或顺序错误。
7. 模型失败和漏检场景不丢失原生文本。
8. JSON schema、配置示例、模型下载方法和第三方许可证文档完整。
9. 第一版不包含 WASM、内置 OCR、表格结构恢复和公式识别。

## 26. 参考

- PP-DocLayoutV3 ONNX：https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx
- 模型配置：https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx/blob/main/inference.yml
- ort 2.0.0-rc.13：https://docs.rs/ort/2.0.0-rc.13/ort/
- LiteParse 文本提取：https://github.com/run-llama/liteparse/blob/b2e76ec5b0c1cb4eb11d67296e916792f4fb5858/crates/liteparse/src/extract.rs
- LiteParse 网格与 XY-cut：https://github.com/run-llama/liteparse/blob/b2e76ec5b0c1cb4eb11d67296e916792f4fb5858/crates/liteparse/src/projection.rs
- LiteParse 段落启发式：https://github.com/run-llama/liteparse/blob/b2e76ec5b0c1cb4eb11d67296e916792f4fb5858/crates/liteparse/src/markdown_layout/paragraphs.rs
- LiteParse OCR trait：https://github.com/run-llama/liteparse/blob/b2e76ec5b0c1cb4eb11d67296e916792f4fb5858/crates/liteparse/src/ocr/mod.rs
- PaddleX 图像预处理：https://github.com/PaddlePaddle/PaddleX/blob/ffb64904d23708863ff5b8da312a5cbd52a7f462/paddlex/inference/models/object_detection/predictor.py
- PaddleX scale factor：https://github.com/PaddlePaddle/PaddleX/blob/ffb64904d23708863ff5b8da312a5cbd52a7f462/paddlex/inference/models/object_detection/processors.py
- PaddleX PP-DocLayoutV3 后处理：https://github.com/PaddlePaddle/PaddleX/blob/ffb64904d23708863ff5b8da312a5cbd52a7f462/paddlex/inference/models/object_detection/modeling/pp_doclayout_v3.py
- PaddleX Layout 后处理：https://github.com/PaddlePaddle/PaddleX/blob/ffb64904d23708863ff5b8da312a5cbd52a7f462/paddlex/inference/models/layout_analysis/processors.py
