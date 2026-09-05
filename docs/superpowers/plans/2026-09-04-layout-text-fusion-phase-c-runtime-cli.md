# DocParse 阶段 C：异步运行时、输出与 CLI 实施计划

> **供当前会话执行者使用：** 必须使用 superpowers:executing-plans，按任务逐项执行本计划。禁止使用 SubAgent 或 worktree；步骤使用复选框（- [ ]）跟踪。

**目标：** 将 阶段 A 的真实/可注入 LayoutEngine 与 阶段 B 的纯页内融合连接成受控 Tokio 文档流水线，并提供稳定 API、降级语义、DocumentRelations、渲染器和 docparse CLI。

**架构：** 每次文档解析创建独占的 PdfiumExecutor，document handle 始终留在专用阻塞线程；预扫描完成后冻结 Arc<DocumentContext>。渲染生产者通过有界队列交给并发页面 worker，worker 调用 LayoutEngine 后执行纯 PageAnalyzer。全部 PageResult 按页码归并后，DocumentLinker 只生成旁路关系。

**技术栈：** Rust 2024、Tokio、futures、clap、tracing、serde_json、image、docparse-config、docparse-layout、docparse-core

**设计规范：** docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md

**前置条件：** 阶段 A 与 阶段 B 门禁全部通过。

## 全局约束

- 不使用 SubAgent 或 worktree，不创建 commit。
- 每个新增函数必须有英文函数级注释；修改非平凡旧逻辑时必须用英文注释说明变动原因。
- 私有实现的测试只能放在对应源码文件或模块内声明为 `#[cfg(test)] mod tests`；crate 级 `tests/` 仅通过公开 API 测试，禁止为测试在生产代码中增加独立的 `#[cfg(test)]` 字段、函数、实现或支持模块。
- 所有超过 3 字段的 struct，包括 clap 参数和内部 command/event，使用 typed-builder；Option 字段使用 builder default，并按调用方类型决定 strip_option。
- Arc clone 写成 Arc::clone(&value)；不通过 value.clone() 隐藏共享所有权。
- 不在 await 期间持有 PDFium handle guard、std MutexGuard、ORT lease 或其他同步锁。
- Page worker 只读共享 Arc<DocumentContext>，不得回写跨页统计。
- 渲染器与 DocumentLinker 不得修改 PageResult。
- 日志必须覆盖文档生命周期、页面关键阶段、第三方调用前后、降级分支和错误返回；字段值放在正文格式参数中。
- 默认测试离线，使用 fake LayoutEngine；真实模型路径只出现在 ignored 测试。

---

### 任务 C1：实现独占文档生命周期的 PdfiumExecutor

**文件：**
- 新建：crates/core/src/runtime/mod.rs
- 新建：crates/core/src/runtime/pdfium_executor.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：PdfiumExecutor、DocumentSource、PreScanOutput、RenderedPage。
- 输入：PdfInput、docparse-pdfium、阶段 B extractor。

- [ ] **步骤 1：在 pdfium_executor.rs 内写句柄生命周期 RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接访问 crate-private actor。测试 path 与 Arc<[u8]> 输入；observer 与 fake backend 只能定义在该 tests 模块内，生产模块不得增加独立 test-only 字段、函数、impl 或 hook。记录 OpenDocument、OpenPage、ClosePage、CloseDocument，并断言：document 覆盖整个任务；每次预扫描/渲染 page 成对开关；page handle 不进入响应类型。

- [ ] **步骤 2：写串行 FFI RED 测试**

从多个 Tokio task 并发请求 render，observer 的 active PDFium call 计数始终 <=1。取消一个请求后 executor 仍能服务后续页面，关闭 executor 时 document 被释放。

- [ ] **步骤 3：写错误传播 RED 测试**

覆盖打开失败、页码越界、提取失败、渲染失败和 worker thread panic。返回给调用方的顶层错误可携带输入 path，写入 DocumentResult 的 PageError 只能保存稳定 error code/page/stage/sanitized message，不得保存绝对路径；任何错误都不打印输入 bytes。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib runtime::pdfium_executor::tests
~~~

- [ ] **步骤 5：实现 actor 边界**

专用阻塞线程拥有 PDFium library 与 document handle。async 侧通过有界 Tokio mpsc 发送 command，并用 oneshot 接收拥有值；command 只含页号和选项，不含 handle。

~~~rust
pub(crate) enum PdfiumCommand {
    PreScanPage(PreScanRequest),
    RenderPage(RenderRequest),
    Shutdown,
}

impl PdfiumExecutor {
    pub(crate) async fn open(
        input: PdfInput,
        limits: &RuntimeConfig,
    ) -> Result<Self, PdfiumRuntimeError>;
    pub(crate) async fn pre_scan_page(
        &self,
        page_number: u32,
    ) -> Result<ExtractedPage, PdfiumRuntimeError>;
    pub(crate) async fn render_page(
        &self,
        page_number: u32,
    ) -> Result<RenderedPage, PdfiumRuntimeError>;
}
~~~

- [ ] **步骤 6：明确 shutdown 与 cancellation**

正常路径发送 Shutdown 并 join；调用 Future 被取消时 Drop 关闭 sender，worker 清理当前 page/document 后退出。Drop 中不得异步阻塞；提供显式 close().await 给正常路径。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib runtime::pdfium_executor::tests
~~~

---

### 任务 C2：实现三阶段 ParseRuntime 与有界页面流水线

**文件：**
- 新建：crates/core/src/runtime/pipeline.rs
- 修改：crates/core/src/runtime/mod.rs

**接口：**
- 产出：ParseRuntime::parse_document。
- 输入：PdfiumExecutor、Arc<ValidatedConfig>、Arc<dyn LayoutEngine>、可选 Arc<dyn OcrEngine>、PageAnalyzer。

- [ ] **步骤 1：在 pipeline.rs 内写预扫描/冻结顺序 RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接测试 crate-private ParseRuntime；fake observer、slow LayoutEngine 和计数器只定义在该 tests 模块内。断言所有页 PreScan 完成后才调用 DocumentContextBuilder::build，所有 PageAnalyzer 调用收到同一个 Arc<DocumentContext>。LayoutEngine::detect 只接收中立 LayoutRequest；docparse-layout 不得反向依赖 docparse-core 的 DocumentContext。

- [ ] **步骤 2：写 bounded pipeline RED 测试**

用 slow fake LayoutEngine 与大位图计数器，断言同时驻留的 RenderedPage 不超过 render_queue_capacity + 正在分析 task 数，JoinSet 活跃页面不超过 page_concurrency。

- [ ] **步骤 3：写乱序完成/稳定汇总 RED 测试**

让 fake 页面按 3、1、2 顺序完成，DocumentResult.pages 必须按 1、2、3 排列；不同 page_concurrency 的规范 JSON 必须一致。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib runtime::pipeline::tests
~~~

- [ ] **步骤 5：实现预扫描阶段**

串行读取 metadata、outline、页数并逐页提取 Native TextItem/PageProbe。保留 ExtractedPage，不保留渲染位图或 page handle。冻结 Arc<DocumentContext> 后才进入第二阶段。

- [ ] **步骤 6：实现 render -> detect -> analyze 管线**

一个 producer 按页号请求串行 render，并发送持有 Arc<PageImage> 的 RenderedPage 到容量为 render_queue_capacity 的 Tokio mpsc。Receiver 不可克隆，只由单一 dispatcher 持有；dispatcher 通过 JoinSet 与 page_semaphore 启动最多 page_concurrency 个页面 task，并用 tokio::select! 同时接收新页面和回收完成结果。LayoutEngine 自己管理 Session Pool，core 不知道 ort 类型。页面 task 用同一 Arc<PageImage> 调用 detect，在 PageAnalyzer::prepare 生成缺失区域后再按需传给 OcrEngine，最后用 PageAnalyzer::finish 完成唯一一条融合路径；不得复制整页 RGB buffer。

~~~text
Pdfium producer --bounded RenderedPage--> single dispatcher
single dispatcher --JoinSet + permit--> page analysis task
page task -> LayoutEngine::detect -> PageAnalyzer::prepare
          -> optional OCR -> PageAnalyzer::finish
page task --PageOutcome--> stable collector
~~~

blocking_task_limit 只限制 core 自身 CPU blocking 工作；不包裹 LayoutEngine 已经管理的 spawn_blocking，避免双重 semaphore 死锁。

- [ ] **步骤 7：实现资源释放顺序**

正常路径在 producer 关闭 sender、dispatcher 排空 channel 且 JoinSet 全部结束后 close PdfiumExecutor。fail-fast 路径先关闭/丢弃 Receiver，让 producer 的 send 失败并停止新页面；可以终止尚未进入 PDFium 的 producer future，但不得 abort 已进入 ORT spawn_blocking 的页面 task，必须排空 JoinSet、等待 Session lease 归还后再 close PdfiumExecutor。调用方直接 drop parse Future 时通过 channel/RAII 触发最终清理，测试等待 observer 证明资源最终归零，不宣称同步取消阻塞推理。

- [ ] **步骤 8：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib runtime::pipeline::tests
~~~

---

### 任务 C3：实现 DocParser API、默认引擎与依赖注入

**文件：**
- 新建：crates/core/src/parser.rs
- 新建：crates/core/tests/parser_api.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：DocParser、DocParserBuilder、parse_path、parse_bytes、parse_page、parse_path_blocking。
- 输入：ValidatedConfig、PpDocLayoutV3Engine 或自定义 LayoutEngine/OcrEngine。

- [ ] **步骤 1：写自定义 LayoutEngine RED 测试**

配置给出不存在的 PP 模型路径，但 builder 注入 fake LayoutEngine 时构造成功并可解析 fixture。这是配置/引擎边界的回归测试。

- [ ] **步骤 2：写默认引擎 RED 测试**

不注入 LayoutEngine 时调用 PpDocLayoutV3Engine::from_config；任一模型/配置/manifest 文件不存在、manifest hash 错误、YAML 合同错误、schema 错误和未编译 EP 必须在构造阶段失败。

- [ ] **步骤 3：写 API 语义 RED 测试**

覆盖 path、bytes、公开 parse_page。parse_page 构建单页 DocumentContext；文档内部 analyze_page 必须接收共享文档 Context，不能调用公开 parse_page。

- [ ] **步骤 4：写 blocking wrapper RED 测试**

普通同步线程调用成功；Tokio runtime 内调用 parse_path_blocking 返回 BlockingInsideRuntime，不创建嵌套 runtime。

- [ ] **步骤 5：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test parser_api
~~~

- [ ] **步骤 6：实现 builder 与构造路径**

~~~rust
impl DocParserBuilder {
    pub fn config(self, config: Arc<ValidatedConfig>) -> Self;
    pub fn layout_engine(self, engine: Arc<dyn LayoutEngine>) -> Self;
    pub fn ocr_engine(self, engine: Arc<dyn OcrEngine>) -> Self;
    pub async fn build(self) -> Result<DocParser, DocParseError>;
}

impl DocParser {
    pub async fn from_config(config: ValidatedConfig) -> Result<Self, DocParseError>;
    pub async fn parse_path(&self, path: impl AsRef<Path>) -> Result<DocumentResult, DocParseError>;
    pub async fn parse_bytes(&self, bytes: Arc<[u8]>) -> Result<DocumentResult, DocParseError>;
    pub async fn parse_page(&self, input: PageInput) -> Result<PageResult, DocParseError>;
    pub fn parse_path_blocking(&self, path: impl AsRef<Path>) -> Result<DocumentResult, DocParseError>;
}
~~~

若 builder 已注入 engine，build 不读取默认模型文件。所有共享依赖显式 Arc::clone。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --test parser_api
~~~

---

### 任务 C4：固定错误分类、降级矩阵与日志合同

**文件：**
- 修改：crates/core/src/error.rs
- 新建：crates/core/src/diagnostics.rs
- 新建：crates/core/tests/degradation.rs
- 修改：crates/core/src/runtime/pipeline.rs
- 修改：crates/core/src/parser.rs

**接口：**
- 产出：DocParseError、PageError、PageWarning、Stage、DegradationReason。
- 输入：PDFium/Layout/OCR/Context/Analyzer 错误。

- [ ] **步骤 1：写降级矩阵 RED 测试**

| 失败点 | continue=true | continue=false |
|---|---|---|
| 文档打开 | 文档失败 | 文档失败 |
| 单页提取 | 保留 PageError，继续生成该页可得结果 | 文档失败 |
| 单页渲染 | 使用 Native 纯几何 fallback | 文档失败 |
| Layout detect | 使用 Native 纯几何 fallback | 仍按安全降级继续 |
| OCR | 保留 Native/Layout，写 warning | 同左 |
| DocumentLinker | 返回完整 pages，写文档 warning | 同左 |
| ResultValidator 内部不变量 | 文档失败 | 文档失败 |

Layout/OCR 的可恢复失败不受 continue_on_page_error 控制；该选项只控制 PDFium 页面阶段失败。

- [ ] **步骤 2：写日志 RED 测试**

使用 tracing test subscriber 断言文档开始/结束、预扫描、render、Layout 前后、降级和错误返回存在；日志正文含 page/stage/error，不依赖 structured field；不含完整配置、像素、tensor 或 PDF 内容。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test degradation
~~~

- [ ] **步骤 4：实现 typed error conversion**

使用 thiserror 与 From/TryFrom/ResultExt 风格集中添加上下文，避免在每层创建只转发字符串的 helper。对外错误保留 source chain。

- [ ] **步骤 5：实现页面 shell 语义**

continue=true 且提取失败时，PageResult 仍占据原页号，可包含可成功检测的无文字 Atomic Block，并记录 NativeExtractionUnavailable。不得伪造 TextItem。

- [ ] **步骤 6：加入日志**

统一使用 tracing::info!/warn!/error!，值通过正文格式参数写入。错误返回分支先记录一次，调用方不得重复逐层记录同一错误。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --test degradation
~~~

---

### 任务 C5：完成只读 DocumentLinker

**文件：**
- 修改：crates/core/src/context/relations.rs

**接口：**
- 产出：repeated chrome、paragraph continuation、heading hierarchy、table continuation candidate。
- 输入：&DocumentContext、&[PageResult]。

- [ ] **步骤 1：在 relations.rs 内写四类关系 RED 测试**

在文件末尾已有/新增的唯一 `#[cfg(test)] mod tests` 中测试 crate-private DocumentLinker。构造多页结果并断言每条 relation 只引用稳定 ID，保存 score/evidence，不拥有 Block/Line 副本，不为测试公开 linker。

- [ ] **步骤 2：写只读与顺序 RED 测试**

link 前后 PageResult 规范 JSON hash 不变。输入页面 Vec 顺序改变后，relations 仍按 relation kind、source page/id、target page/id 稳定排序。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib context::relations::tests
~~~

- [ ] **步骤 4：实现 relation heuristics**

重复 chrome 复用冻结 Context；段落 continuation 看句末、首行 indent、字号与栏位；标题层级只生成 heading relation；跨页 table 只标候选，不物理合并。

- [ ] **步骤 5：接入 ParseRuntime**

所有页面按页码稳定完成后调用一次。失败写 document warning 并返回 pages，不修改 page 内 order/owner。

- [ ] **步骤 6：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib context::relations::tests
rtk cargo test -p docparse-core --lib runtime::pipeline::tests
~~~

---

### 任务 C6：实现 JSON、Raw/Semantic 与诊断 overlay

**文件：**
- 新建：crates/core/src/render/mod.rs
- 新建：crates/core/src/render/json.rs
- 新建：crates/core/src/render/text.rs
- 新建：crates/core/src/render/markdown.rs
- 新建：crates/core/src/render/overlay.rs
- 新建：crates/core/tests/render.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：JsonRenderer、TextRenderer、MarkdownRenderer、OverlayRenderer、RenderView。
- 输入：&DocumentResult；单页 overlay 额外输入 Arc<PageImage> 与 &PageResult。

- [ ] **步骤 1：写 raw/semantic RED 测试**

构造重复页眉、跨页 continuation、fallback、RTL 和 Missing formula。Raw 保留所有内容与分页；Semantic 只在展示层隐藏重复 chrome、连接 continuation，并按配置输出 formula_placeholder。

- [ ] **步骤 2：写不可变 RED 测试**

每种 renderer 前后对 DocumentResult 计算规范 JSON hash，必须完全相同。renderer 不接收 &mut DocumentResult。

- [ ] **步骤 3：写 overlay RED 测试**

断言 SVG 包含页面背景引用、model detection、最终 Block bbox、label、order、Line baseline、fallback 和 removed/conflict edge。所有 raw_label/ID 经过 XML escaping。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test render
~~~

- [ ] **步骤 5：实现规范渲染器**

JSON 直接序列化 schema。Text/Markdown 严格遍历 Block -> Line -> TextItem，不重新排序。Semantic view 读取 DocumentRelations 构造派生文本，不回写 pages。

- [ ] **步骤 6：实现无仓库字体资产的 SVG overlay**

OverlayRenderer::render_page 接收同页 PageImage/PageResult，输出一份页面 PNG 和一份引用该 PNG 的 SVG；SVG 用 viewBox 保持 viewport 坐标，并使用 generic monospace `<text>` 显示 ASCII label/ID/order。几何与文本坐标由 PageTransform 转换，不引入或分发字体文件，因此不增加字体许可证义务。

CLI 在规范解析完成后，为 --overlay-dir 单独重新打开 PDF，按 page_number 串行 render_page -> render overlay -> drop PageImage；不重跑 LayoutEngine，不把全部位图留在内存。overlay 失败只影响显式请求的诊断产物并使 CLI 返回失败，不修改已经生成的 DocumentResult。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --test render
~~~

---

### 任务 C7：实现可测试的 docparse CLI

**文件：**
- 新建：crates/cli/src/lib.rs
- 新建：crates/cli/src/args.rs
- 修改：crates/cli/src/main.rs
- 新建：crates/cli/tests/cli.rs
- 新建：crates/cli/tests/cli_with_fake.rs

**接口：**
- 产出：docparse parse、docparse inspect-model。
- 输入：ConfigLoader、DocParser、renderers。

- [ ] **步骤 1：写二进制 RED 测试**

assert_cmd 覆盖 --help、未知子命令、省略 --config 时固定查找 ./docparse.toml、缺失主配置、缺失默认模型和 inspect-model 错误。不得向父目录搜索配置。stderr 必须给出路径和 uv 下载提示，不能打印 backtrace 作为默认用户输出。

- [ ] **步骤 2：写可注入 CLI app RED 测试**

cli crate 的 lib 接收 ParserFactory trait。fake factory 覆盖 JSON 输出、raw/semantic view、overlay-dir 和 output 文件。这样默认离线测试不需要模型，同时生产 main 只能使用 DefaultParserFactory。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-cli
~~~

- [ ] **步骤 4：实现 clap 类型**

~~~text
docparse parse INPUT [--config PATH] [--profile NAME]
                       [--format json|text|markdown]
                       [--view raw|semantic]
                       [--output PATH]
                       [--continue-on-page-error BOOL]
                       [--overlay-dir PATH]

docparse inspect-model [--config PATH] [--profile NAME]
~~~

超过 3 字段的 Args 同时派生 clap 与 TypedBuilder；Option builder 使用 default。

- [ ] **步骤 5：实现 app 与薄 main**

main 只初始化 tracing subscriber、解析参数并调用 async run。输出文件使用同目录临时文件完成后原子替换；stdout 模式不混入日志。

- [ ] **步骤 6：实现 inspect-model**

复用 docparse-layout schema/manifest 检查，输出模型路径、revision、hash、input/output schema 与 EP，不执行页面推理。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-cli
rtk cargo clippy -p docparse-cli --all-targets -- -D warnings
~~~

---

### 任务 C8：完成合成多页端到端集成与 阶段 门禁

**文件：**
- 新建：crates/core/tests/fixtures/pdf/multipage_layout.pdf
- 新建：crates/core/tests/parser.rs
- 新建：crates/core/tests/common/mod.rs

**接口：**
- 产出：不使用真实模型的完整 path/bytes -> DocumentResult 集成证据。
- 输入：真实 docparse-pdfium、fake LayoutEngine、完整 ParseRuntime。

- [ ] **步骤 1：写多页 E2E RED 测试**

合成 PDF 至少三页：全宽标题+双栏、部分模型覆盖、空 detection。断言 PageResult 数量、Context 共享、fallback、Region 拆分、DocumentRelations、schema round-trip 与文本守恒。

- [ ] **步骤 2：写并发/取消 RED 测试**

page_concurrency=1 与 4 输出一致。中途取消 parse future 后，测试 observer 最终看到 page/document handle 全部关闭且后续新解析可成功。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test parser
~~~

- [ ] **步骤 4：连接所有组件**

保持 parser.rs 只做 orchestration；PageAnalyzer、DocumentLinker、renderer 不互相持有。不得为了通过集成测试复制一份融合算法。

- [ ] **步骤 5：运行 阶段 C 全部门禁**

运行：

~~~bash
rtk cargo test -p docparse-config
rtk cargo test -p docparse-layout
rtk cargo test -p docparse-core
rtk cargo test -p docparse-cli
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo fmt --all -- --check
~~~

预期：默认离线完成；无模型、网络或 Downloads 目录依赖。

---

## 阶段 C 完成门禁

- PDFium document/page handle 生命周期和串行 FFI 有测试证据。
- 页面渲染队列与并发 task 均有明确上限。
- 所有页面共享冻结 Context，并按页号稳定归并。
- 自定义 LayoutEngine 不触发默认模型校验；默认引擎严格校验 artifact/schema。
- 降级矩阵、日志合同和 blocking API 行为固定。
- DocumentLinker/renderer 不修改 PageResult。
- CLI 默认离线测试通过，真实模型行为留给 阶段 D。
- 合成多页 path/bytes 端到端通过，取消时无 PDFium 资源泄漏。

本计划不包含 commit。提交需用户单独授权。
