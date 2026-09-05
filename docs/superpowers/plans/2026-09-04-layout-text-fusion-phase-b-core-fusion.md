# DocParse 阶段 B：文本事实与页内融合实施计划

> **供当前会话执行者使用：** 必须使用 superpowers:executing-plans，按任务逐项执行本计划。禁止使用 SubAgent 或 worktree；步骤使用复选框（- [ ]）跟踪。

**目标：** 在不依赖真实版面模型的默认测试中，完成 PDFium 文本事实提取、冻结的 DocumentContext、页内 Block -> Line -> TextItem 融合、稳定阅读顺序和可注入 OCR trait。

**架构：** PDFium 提取结果是文字事实源；docparse-core 以 TextItem 为最小归属边界，让有效模型 Region 与 residual XY-cut 共同提供 BlockSeed。每个 Page 独立完成唯一归属、owner 内组行、fallback 段落拆分和顺序 DAG，页面任务只读共享的 Arc<DocumentContext>。

**技术栈：** Rust 2024、serde、typed-builder、async-trait、Tokio、proptest、docparse-pdfium、docparse-layout

**设计规范：** docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md

**前置条件：** 阶段 A 完成，docparse-layout 的中立类型与 fake LayoutEngine 可用。阶段 B 默认测试不得加载 ONNX 模型。

## 全局约束

- 不使用 SubAgent 或 worktree，不创建 commit。
- 每个新增函数必须有英文函数级注释；修改非平凡旧逻辑时必须用英文注释说明变动原因。
- 私有实现的测试只能放在对应源码文件或模块内声明为 `#[cfg(test)] mod tests`；crate 级 `tests/` 仅通过公开 API 测试，禁止为测试在生产代码中增加独立的 `#[cfg(test)]` 字段、函数、实现或支持模块。
- 所有超过 3 字段的 struct，包括内部临时类型，使用 typed-builder；Option 字段使用 builder default，并按调用方类型决定 strip_option。
- 共享所有权克隆统一写 Arc::clone(&value)。
- 原始文本、原始坐标和 PDFium 提取索引不可被规范化流程覆盖。
- 每个有效 TextItem 最终恰好属于一个 Line，每个 Line 最终恰好属于一个 Block。
- HashMap/HashSet 只能用于查找，任何可观察输出都必须经过稳定排序。
- 裸 PDFium handle、指针值、随机 UUID 和内存地址不得进入结果、ID 或日志。
- 日志仅放在第三方调用、关键阶段、降级与错误返回前，使用 tracing 完整宏路径和正文参数。
- 每项行为先写失败测试，再写最小实现。

---

### 任务 B1：定义 schema、稳定 ID 与结果校验器

**文件：**
- 新建：crates/core/src/error.rs
- 新建：crates/core/src/types.rs
- 新建：crates/core/src/validate.rs
- 修改：crates/core/src/lib.rs
- 新建：crates/core/tests/types.rs

**接口：**
- 产出：SchemaVersion、稳定 ID newtype、DocumentResult、PageResult、Block、Line、TextItem、InlineSpan、Evidence、DocumentRelations、ResultValidator。
- 输入：docparse-layout 的 Bbox、Polygon、LayoutLabel、EngineMetadata。

- [ ] **步骤 1：写 schema round-trip RED 测试**

构造一页、一个 Block、两行、三个 TextItem 和一个 InlineSpan。序列化后反序列化，断言数组顺序、raw_text、normalized_text、raw_label、model_region_id、source_region geometry、repair_actions 与 schema_version 完全保留。

- [ ] **步骤 2：写 schema 版本 RED 测试**

断言 1.0 可读、同一 major 的更高 minor 且只有未知可选字段可读、未知 major 被拒绝。不要依赖字符串字典序比较版本。

- [ ] **步骤 3：写稳定 ID 与唯一所有权 RED 测试**

覆盖重复 TextItemId、重复 LineId、重复 BlockId、空 ID、跨页父子引用、非有限 geometry、错误 final_order。相同输入顺序重复构造 100 次，序列化结果必须一致。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test types
~~~

预期：FAIL，因为事实类型和 ResultValidator 尚不存在。

- [ ] **步骤 5：实现版本与 ID newtype**

核心构造器只接受有语义的索引：

~~~rust
pub struct TextItemId(String);
pub struct ModelRegionId(String);
pub struct FallbackRegionId(String);
pub struct BlockId(String);
pub struct LineId(String);

impl TextItemId {
    pub fn native(page_number: u32, extraction_index: u32) -> Self;
    pub fn ocr(page_number: u32, source_result_index: u32) -> Self;
}

impl ModelRegionId {
    pub fn detected(page_number: u32, source_detection_index: u32) -> Self;
}

impl FallbackRegionId {
    pub fn from_path(page_number: u32, xy_cut_path: &RegionPath) -> Self;
}
~~~

公开页号从 1 开始，其余 ordinal/index 从 0 开始。Native/OCR TextItemId 分别固定为 p{page}:t{index} 与 p{page}:o{source_result_index}；OCR source_result_index 在返回 Vec 过滤前固定。ModelRegionId 为 p{page}:m{index}；FallbackRegionId 为 p{page}:f{path}；模型/fallback BlockId 分别为 p{page}:b:m{index}:s{split} 与 p{page}:b:f{path}:s{split}；LineId 为 {BlockId}:l{ordinal}。

- [ ] **步骤 6：实现嵌套结果类型**

Block 直接拥有 Vec<Line>，Line 直接拥有 Vec<TextItem>。PageResult 不重复存储扁平副本，只提供 iter_lines 与 iter_text_items。TextItem 不含 Glyph，text_object_index 为 Option<u32>。Block.bbox 是最终 Line bbox 的 union，无 Line 时使用 source region bbox；SourceRegionEvidence 独立保存模型/fallback 原始 geometry，避免把完整模型 polygon 冒充内容 bbox。

- [ ] **步骤 7：实现 ResultValidator**

深度遍历并报告类似 pages[2].blocks[4].lines[1] 的精确 node path。检查 ID 唯一性、父子页号、有限 geometry、数组序与 final_order 一致、TextItem/Line 唯一所有权、InlineSpan range 合法。Line 必须位于最终 Block.bbox 容差内，但允许越出 source_region geometry。

- [ ] **步骤 8：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --test types
rtk cargo clippy -p docparse-core --test types -- -D warnings
~~~

---

### 任务 B2：建立合成 PDF fixture 并补齐 PDFium 安全包装

**文件：**
- 新建：crates/core/tests/fixtures/pdf/extraction_metadata.pdf
- 新建：crates/core/tests/fixtures/pdf/extraction_metadata.expected.json
- 新建：crates/core/tests/pdfium_surface.rs
- 修改：crates/pdfium/src/font.rs
- 修改：crates/pdfium/src/page.rs
- 修改：crates/pdfium/src/text_page.rs

**接口：**
- 产出：稳定的字符、字体、矩阵、颜色、MCID、链接、旋转与 generated-space 读取接口。
- 输入：已有 docparse-pdfium 安全类型与 pdfium-sys FFI。

- [ ] **步骤 1：盘点现有 wrapper 能力**

逐项对照 fixture 期望字段，记录现有安全 API、缺口和对应 PDFium C API。先确认对象生命周期与返回缓冲区规则，再增加 wrapper；不得在 core 直接绕过安全层调用 pdfium-sys。

- [ ] **步骤 2：创建确定性合成 fixture**

fixture 只含自有测试文本，至少包括：两栏同基线文字、两种字号、粗斜体、链接、90 度文字、显式换行、缺失空格、dot leader、不同 fill/stroke color。expected.json 记录人工可审查事实，不记录平台相关字体绝对路径。

- [ ] **步骤 3：写 wrapper RED 测试**

断言页面尺寸/旋转、字符 Unicode/char code、bbox、origin、font name/flags/weight、font size、matrix、颜色、MCID、generated-space、link 与临时 text-object handle 可被读取。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test pdfium_surface
~~~

- [ ] **步骤 5：最小扩展 docparse-pdfium**

使用已有 RAII page/text-page 生命周期增加缺失方法。TextChar 暴露临时 object identity 只供同一 page handle 生命周期内映射，不把裸 handle 转为整数或公开到序列化类型。

- [ ] **步骤 6：运行 wrapper 回归**

运行：

~~~bash
rtk cargo test -p docparse-pdfium
rtk cargo test -p docparse-core --test pdfium_surface
~~~

---

### 任务 B3：移植 SegmentBuilder 并提取连续 Native TextItem

**文件：**
- 新建：crates/core/src/extract/mod.rs
- 新建：crates/core/src/extract/text.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：ExtractedPage、TextItemDraft、按 PDFium 遍历顺序固定的 extraction_index。
- 输入：docparse-pdfium Page/TextPage 与 PageTransform。

- [ ] **步骤 1：在 text.rs 内写分段 RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接测试私有 SegmentBuilder，覆盖显式换行、Y 跳变、X 回跳、大水平间距、dot leader、缺失空格恢复、旋转变化、字体样式变化和 Unicode 映射失败。断言 raw_text 与 repair_actions 分离，不为测试提升 SegmentBuilder 可见性。

- [ ] **步骤 2：写提取索引稳定性 RED 测试**

对 fixture 连续提取三次，断言 TextItemId、raw_text 和原始 bbox 顺序完全一致。extraction_index 必须在过滤前按候选 segment 的确定性遍历位置生成；被判为无效的空 segment 记录诊断但不复用其索引。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib extract::text::tests
~~~

- [ ] **步骤 4：实现 TextCharFact 与 SegmentBuilder**

~~~rust
pub(crate) struct SegmentBuilder {
    page_number: u32,
    next_extraction_index: u32,
    current: Option<TextItemDraft>,
    completed: Vec<TextItemDraft>,
}

impl SegmentBuilder {
    pub(crate) fn push(&mut self, fact: TextCharFact) -> Result<(), ExtractError>;
    pub(crate) fn finish(self) -> Result<Vec<TextItemDraft>, ExtractError>;
}
~~~

从固定 LiteParse commit 的 extract.rs 移植分段判定，并在文件头保留来源与 Apache-2.0 声明。优先用 TryFrom<TextItemDraft> 构造最终 TextItem，避免字段搬运 helper 链。

- [ ] **步骤 5：实现 raw/normalized 双轨**

raw_text、PDF geometry 与原始 extraction_order 永不修改。空格恢复、控制字符清理、软连字符提示只写 normalized_text 与 repair_actions。

- [ ] **步骤 6：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib extract::text::tests
~~~

---

### 任务 B4：丰富 TextItem metadata 与稳定对象映射

**文件：**
- 新建：crates/core/src/extract/metadata.rs
- 修改：crates/core/src/extract/mod.rs
- 修改：crates/core/src/extract/text.rs

**接口：**
- 产出：完整 TextStyle、TextGeometry、PdfProvenance、PageProbe 输入信号。
- 输入：SegmentBuilder 的字符范围和临时 PDFium text-object identity。

- [ ] **步骤 1：在 metadata.rs 内写 metadata RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，断言字体名、字号、height/ascent/descent/weight/flags、bold/italic/monospace、text matrix、fill/stroke color、char_codes、MCID、Unicode mapping、generated-space、link、strike 与 source/confidence。

- [ ] **步骤 2：写 text_object_index RED 测试**

在 page handle 活跃期间稳定枚举 text page objects，建立 handle identity -> enumeration index 映射。可映射字符填写相同 index；不能可靠映射时为 None。测试明确禁止 pointer-as-u64 输出。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib extract::metadata::tests
~~~

- [ ] **步骤 4：实现 metadata 聚合**

片段级字体和样式采用字符数加权；存在真实字体值时保留事实值，同时保存 estimated 标志。颜色不一致时保留代表值并写 evidence，不能伪造单一事实。

- [ ] **步骤 5：实现 PageProbe 转换**

从 ExtractedPage 派生轻量 PageProbe：尺寸、旋转、内容边界、字符加权字号直方图、顶部/底部规范化指纹、页码候选和标题字号候选。PageProbe 不包含图片像素或 PDFium handle。

- [ ] **步骤 6：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib extract::metadata::tests
rtk cargo test -p docparse-core --lib extract::text::tests
~~~

---

### 任务 B5：构建并冻结 DocumentContext 与旁路关系类型

**文件：**
- 新建：crates/core/src/context/mod.rs
- 新建：crates/core/src/context/builder.rs
- 新建：crates/core/src/context/relations.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：DocumentContextBuilder::build -> Arc<DocumentContext>、DocumentRelations 类型与不可变 linker 接口。
- 输入：文档 metadata、outline、Vec<PageProbe>、配置与模型标识。

- [ ] **步骤 1：在 context 模块内写全局统计 RED 测试**

在 builder.rs 末尾声明唯一的 `#[cfg(test)] mod tests`，构造三页 probe，包含 9pt 正文、16pt 标题、重复页眉和连续页码。断言 body font、重复 chrome 指纹、page number pattern 和 heading candidates；relations.rs 的私有 linker 测试使用该文件自身的 `#[cfg(test)] mod tests`。

- [ ] **步骤 2：写确定性与冻结 RED 测试**

随机排列统计输入中的 map/set 插入顺序，结果序列化必须相同。Arc<DocumentContext> 只能暴露只读查询；页面分析 API 不接收 builder 或写锁。

- [ ] **步骤 3：写关系不改页结果 RED 测试**

先序列化 Vec<PageResult>，执行 DocumentLinker::link，再次序列化并断言字节一致；relations 只能引用稳定 BlockId/LineId。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib context::
~~~

- [ ] **步骤 5：实现 DocumentContextBuilder**

~~~rust
impl DocumentContextBuilder {
    pub fn push_page(&mut self, probe: PageProbe) -> Result<(), ContextError>;
    pub fn build(self) -> Result<Arc<DocumentContext>, ContextError>;
}
~~~

使用字符加权统计和明确 tie-break。重复页眉页脚依据规范化指纹、相对 Y band 与出现页比例，不直接删除对应文字。

- [ ] **步骤 6：定义 DocumentLinker 只读边界**

此阶段先实现类型和最小 repeated-chrome relation；完整跨页关系在 阶段 C 接入所有 PageResult 后完成。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib context::
~~~

---

### 任务 B6：构建保守 LineFragment、Bidi 与 Vertical 顺序

**文件：**
- 新建：crates/core/src/line/mod.rs
- 新建：crates/core/src/line/assemble.rs
- 新建：crates/core/src/line/bidi.rs
- 新建：crates/core/src/line/metrics.rs

**接口：**
- 产出：LineFragment、LineMetrics、WritingDirection。
- 输入：Vec<TextItem>、PageTransform、FusionConfig。

- [ ] **步骤 1：在 line 模块内写保守组行 RED 测试**

在 assemble.rs、bidi.rs 与 metrics.rs 各自文件末尾仅声明一个 `#[cfg(test)] mod tests`。assemble.rs 直接测试私有 LineAssembler，覆盖同一行多个片段、左右栏同 Y 不合并、大 gap 先拆分、字号异常 bbox、旋转侧栏，并证明 fragment 的 TextItemId 集合与输入集合完全相等；方向和 metrics 测试留在各自模块，不提升私有 trait 可见性。

- [ ] **步骤 2：写 Bidi/Vertical RED 测试**

覆盖 RTL 文本夹数字、LTR 文本夹 RTL 词、标点中立、90/270 度竖排。90° 使用 top 升序，270° 使用与 LiteParse `max_y - y - height` 等价的 bottom 降序；同一纵向带按最大 item height 的 3 倍间距拆分空间簇。所有规则只改变 fragment 内 TextItem 顺序，不反转 raw_text，也不修改公开 geometry。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib line::
~~~

- [ ] **步骤 4：实现 LineAssembler**

~~~rust
pub(crate) trait LineAssembler {
    fn fragments(
        &self,
        items: Vec<TextItem>,
        config: &FusionConfig,
    ) -> Result<Vec<LineFragment>, LineError>;
}
~~~

使用 baseline band、垂直重叠、可信 line height、字号兼容和水平 gap。先拆后合，不做全页最终组行。

- [ ] **步骤 5：实现方向与 metrics**

强 RTL/LTR Unicode 字符投票确定 base direction，数字和标点不投票。计算字符加权字号、style ratio、bbox、baseline、anchor、相对 indent、相邻 gap 和文本边界信号。

- [ ] **步骤 6：运行 GREEN 与属性测试**

运行：

~~~bash
rtk cargo test -p docparse-core --lib line::
~~~

---

### 任务 B7：实现 residual XY-cut 与稳定 RegionPath

**文件：**
- 新建：crates/core/src/fusion/mod.rs
- 新建：crates/core/src/fusion/fallback.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：RegionTree、RegionPath、fallback BlockSeed。
- 输入：未归属 LineFragment、已确认模型区域障碍物、FusionConfig。

- [ ] **步骤 1：在 fallback.rs 内写 XY-cut RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接测试私有 RegionTree/RegionPath 与切分实现，覆盖单栏不切、双栏左右顺序、全宽标题先于双栏、三栏、旋转侧栏，以及表格/图片/模型正文障碍物不被 fallback 横穿。

- [ ] **步骤 2：写路径稳定性 RED 测试**

同一 fragments 以不同 Vec 输入顺序运行，RegionTree、RegionPath 和 fallback RegionId 必须相同。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::fallback::tests
~~~

- [ ] **步骤 4：移植并收敛 XY-cut**

从固定 LiteParse projection.rs 移植 density valley、banner cut、column histogram 和稳定前序遍历。文件头保留来源与 Apache-2.0 声明；阈值全部来自 ValidatedConfig。

- [ ] **步骤 5：实现障碍物规则与 RegionPath**

已确认模型区域只参与 cut 可行性，不吞掉 residual fragments。路径根为 r；水平 cut 的子区按 top/left/bottom/right 追加 .h{0-based ordinal}，垂直 cut 的子区按 left/top/right/bottom 追加 .v{0-based ordinal}，例如 r.h0.v1。路径不受 RTL/Vertical 阅读方向影响，也不使用临时 Vec index。

- [ ] **步骤 6：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::fallback::tests
~~~

---

### 任务 B8：实现模型候选、精确评分与唯一主归属

**文件：**
- 新建：crates/core/src/fusion/assign.rs
- 修改：crates/core/src/fusion/mod.rs

**接口：**
- 产出：Assignment、AssignEvidence、已分配 BlockSeed 与 residual fragments。
- 输入：LayoutDetection、LineFragment、Page geometry、FusionConfig。

- [ ] **步骤 1：在 assign.rs 内写 eligibility RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接测试私有候选与评分实现。普通 Region 只有在 coverage >= minimum_line_coverage，或 center_inside 且 coverage >= center_minimum_line_coverage 时成为候选。阈值边界使用 >=，并覆盖零面积、NaN 和越页 detection。

- [ ] **步骤 2：写评分与 tie-break RED 测试**

固定各分量，调用 docparse-layout 自有 Polygon API，断言 coverage 为 line bbox 与 region geometry 交叠面积除以 line bbox 面积，center_inside 为含边界的 0/1 值，baseline_intersection 为 baseline 在 region 内的长度比例，specificity 为 1-clamp(region_area/page_area)。随后断言：

~~~text
score = coverage * 0.55
      + center_inside * 0.20
      + baseline_intersection * 0.10
      + model_confidence * 0.10
      + specificity * 0.05
~~~

同分依次比较 coverage、model confidence、较小 region area、较小 model order、较小 source detection index。label compatibility 只能出现在 diagnostics。

- [ ] **步骤 3：写文本守恒属性 RED 测试**

proptest 生成 TextItem 与 detection，融合后扁平 TextItemId multiset 必须与输入完全相等且每个计数为 1。模型完全失败时也必须成立。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::assign::tests
~~~

- [ ] **步骤 5：实现 TryFrom<LayoutDetection> for BlockSeed**

过滤 NaN、退化 geometry、越界 class；ModelRegionId 使用 source_detection_index，不使用推理返回 Vec 的后续排序位置。保留 raw_label、confidence、model_order 和 geometry_source；固定 artifact 没有 order_votes，不得生成替代值。

- [ ] **步骤 6：实现唯一归属**

每个 LineFragment 最多产生一个 primary owner，其余候选进入 evidence。inline_formula 完全排除于主 owner 竞争。未归属 fragment 交给 任务 B7 的 residual XY-cut。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::assign::tests
rtk cargo test -p docparse-core --lib fusion::fallback::tests
~~~

---

### 任务 B9：在 owner 内最终组行并保留模型 Block 边界

**文件：**
- 新建：crates/core/src/semantic/mod.rs
- 新建：crates/core/src/semantic/label_policy.rs
- 新建：crates/core/src/semantic/paragraph.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：每个模型 Region 一个 Block，以及按自然段拆分的 fallback Block。
- 输入：已唯一归属的 BlockSeed、LineFragment、DocumentContext、FusionConfig。

- [ ] **步骤 1：在 semantic 模块内写 label policy RED 测试**

在 label_policy.rs 与 paragraph.rs 各自文件末尾仅声明一个 `#[cfg(test)] mod tests`。覆盖全部 25 个固定标签对应的 FlowText、Title、Atomic、Formula、Chrome、Structured 策略以及 Unknown。有效模型标签必须成为 Block 主标签；启发式只能增加 semantic_hints/evidence。无文字 detection 仍保留空 lines Block，FlowText/Title 额外记录 EmptyModelRegion warning。

- [ ] **步骤 2：写模型 Region 一对一 RED 测试**

同一 text detection 内放置两个紧密行和一个明显段间 gap，断言仍只输出一个 Block，保留全部 Line、model_region_id、raw_label 和模型 label；另写部分覆盖行测试，断言框外 TextItem 进入 residual。

- [ ] **步骤 3：写段落边界 RED 测试**

覆盖真实/估算字号容差、Center anchor 切换、粗体切换、首行缩进恢复、后续突然右缩进、1.5 倍行高、中文句末、RTL、软连字符和列表 hint。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib semantic::
~~~

- [ ] **步骤 5：实现 Block 内 LineAssembler**

以 TextItem 为最小边界完成 owner 分配，只在同一 owner 和 residual 集合内按 y-band 重新评估 fragment 合并。LineId 在最终顺序确定后使用 BlockId + stable line ordinal 构造；Line.text 仍按序直接拼接 raw_text，segment 边界必须保留 PDFium 已提供的源空白。

- [ ] **步骤 6：实现 ParagraphSplitter**

使用 ParagraphDecision enum 表达 Continue、Split 与 EvidenceOnly，优先通过 trait/From 转换组合 metrics，避免堆积单字段 helper。该规则只用于 fallback XY-cut 叶；模型 Region 不套用拆段规则。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib semantic::paragraph::tests
~~~

---

### 任务 B10：实现 inline_formula 的最小位置语义

**文件：**
- 新建：crates/core/src/semantic/formula.rs
- 修改：crates/core/src/semantic/mod.rs

**接口：**
- 产出：InlineSpan 或独立 Formula Block。
- 输入：inline_formula LayoutDetection、最终 Line、原生 TextItem。

- [ ] **步骤 1：在 formula.rs 内写挂载 RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接测试私有公式匹配实现，覆盖垂直 overlap、baseline 距离和行内 X 位置；多个公式按 X 稳定排序；一个公式只能挂载到一个最佳 Line。

- [ ] **步骤 2：写内容状态 RED 测试**

原生字符完整覆盖为 Complete，部分覆盖为 Partial，无文字为 Missing。Missing 只保存位置和状态，不生成伪公式文本。

- [ ] **步骤 3：写独立公式 RED 测试**

未匹配 Line 的较大 display/inline formula Region 生成独立 Formula Block；不能复制邻近 TextItem。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib semantic::formula::tests
~~~

- [ ] **步骤 5：实现非占有匹配**

formula detection 只生成 InlineSpan evidence，不改变 Line 的 owner。text_item_range 使用最终行内 ordinal，并在 ResultValidator 中校验。

- [ ] **步骤 6：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib semantic::formula::tests
~~~

---

### 任务 B11：实现页内阅读顺序 DAG 与稳定消环

**文件：**
- 新建：crates/core/src/fusion/order.rs
- 修改：crates/core/src/fusion/mod.rs

**接口：**
- 产出：final_order 连续且 Vec 物理顺序一致的 Block 列表、RemovedOrderEdge diagnostics。
- 输入：model order、明确几何关系、XY-cut path、caption/object 局部关系。

- [ ] **步骤 1：在 order.rs 内写顺序 RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接测试私有 OrderGraph，覆盖单栏、双栏、全宽标题加双栏、fallback 插入、模型顺序与几何冲突、caption 与对象、table 内视觉顺序和相同坐标 tie-break，不为测试公开 OrderGraph。

- [ ] **步骤 2：写环与删边 RED 测试**

构造三节点环，断言按 spec 固定 preservation_weight 删除边：低置信 Model 通常先于可靠几何，高置信 Model 可保留到弱几何之后；同权重按 source 删除优先级和 stable edge key。若只能删除 StrongVertical/IntraRegion，返回 InternalOrderConflict。removed edge 保存 source、weight 与 reason。

- [ ] **步骤 3：写排列/并发确定性 RED 测试**

随机排列 detection 和 seed 输入 100 次，包含 diagnostics 的规范 JSON 必须完全相同；DocumentResult 不允许 timing 或其他易变字段。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::order::tests
~~~

- [ ] **步骤 5：实现 OrderGraph**

~~~rust
pub(crate) struct OrderGraph {
    nodes: BTreeMap<BlockId, OrderNode>,
    edges: Vec<OrderEdge>,
}

impl OrderGraph {
    pub(crate) fn resolve(self) -> Result<OrderResolution, OrderError>;
}
~~~

边来源明确区分 Model、StrongVertical、IntraRegion、BandHorizontal、XyCut、CaptionRelation、FallbackInsertion。模型 Region 按 (order_seq,source_detection_index) 排序且各自只有一个 Block，只连接相邻 Region；同一 fallback Region 的子 Block 使用 IntraRegion 连接。OrderEdge 保存 source_confidence 和 preservation_weight，并严格使用 spec 的固定权重。所有边带稳定 edge key。

- [ ] **步骤 6：实现 SCC 消环与稳定拓扑排序**

每轮只在 SCC 内按 preservation_weight、source 删除优先级、stable edge key 选择一条边删除；StrongVertical/IntraRegion 仅剩时返回错误。零入度优先队列 key 固定为 XY-cut path、floor(block_bbox.top/0.5pt)、Y、X、source priority、BlockId。权重/tie-break 集中在 OrderPolicy，改变它们需要提升 schema major。最后重排 Vec 并从 0 连续写 final_order。

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::order::tests
~~~

---

### 任务 B12：定义异步 OcrEngine 边界但不提供实现

**文件：**
- 新建：crates/core/src/ocr.rs
- 新建：crates/core/tests/ocr_trait.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：OcrEngine、OcrRequest、OcrResult、OcrTextItem、OcrContentStatus。
- 输入：PageImage、PageTransform、缺失区域与 Native 覆盖统计。

- [ ] **步骤 1：写 trait object RED 测试**

Fake OcrEngine 异步返回两个结果，断言 Arc<dyn OcrEngine> 可注入、metadata 保留、坐标可转换。请求不得含 Block 或可变 PageResult。policy=Disabled 时 fake 调用次数为 0；MissingRegions 只有缺失区域非空时调用一次。

- [ ] **步骤 2：写未注入与失败 RED 测试**

MissingRegions 未注入 OCR 时缺失区域为 OcrUnavailable，属于正常状态；Disabled 未注入时不产生 warning；fake 返回错误时已有 Native/Layout 结果不变，并产生 page warning。

- [ ] **步骤 3：写 Native 去重 RED 测试**

OCR 与 Native 高度重叠且文本等价时只保留 Native；不重叠 OCR 项进入与 Native 相同的后续组行路径，但 source 与 confidence 保留。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test ocr_trait
~~~

- [ ] **步骤 5：实现公开 trait 与请求/响应类型**

~~~rust
#[async_trait::async_trait]
pub trait OcrEngine: Send + Sync {
    fn name(&self) -> &str;
    async fn recognize(&self, request: OcrRequest) -> Result<OcrResult, OcrError>;
}
~~~

第一版不增加任何 concrete OCR crate、feature 或依赖。外部 OCR 只能返回文字事实，不能创建 Block、Line 或修改 model label。

- [ ] **步骤 6：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --test ocr_trait
~~~

---

### 任务 B13：组合纯页内 PageAnalyzer 并完成阶段门禁

**文件：**
- 新建：crates/core/src/page.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：PageAnalyzer::prepare -> PageAnalysisDraft、PageAnalyzer::finish -> PageResult。
- 输入：ExtractedPage、Vec<LayoutDetection>、Arc<DocumentContext>、ValidatedConfig、可选 OCR 结果。

- [ ] **步骤 1：在 page.rs 内写端到端页内 RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，直接测试 crate-private PageAnalyzer。使用合成 ExtractedPage 与 fake detections，覆盖全覆盖、部分覆盖、错误重叠、空 detections、layout failure 的预降级输入，以及一个 Region 拆多 Block。prepare 必须在 Native+Layout 之后产生稳定缺失区域，finish 在无 OCR、OCR 成功和 OCR 失败三种输入下复用同一融合路径。

- [ ] **步骤 2：写全排列文本守恒 RED 测试**

对输入 TextItem/detection 排列、page concurrency 模拟顺序和 HashMap seed 变化重复运行，最终 Block -> Line -> TextItem ID 序列必须完全一致，输入 Native ID multiset 必须守恒。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib page::tests
~~~

- [ ] **步骤 4：实现显式阶段流水线**

~~~text
ExtractedPage
  -> prepare conservative fragments / missing regions
  -> optional OCR facts
  -> Native/OCR deduplication
  -> model BlockSeed assignment
  -> residual XY-cut
  -> block-local final lines
  -> paragraph splits
  -> inline formula spans
  -> order graph
  -> ResultValidator
  -> PageResult
~~~

每个阶段以拥有值或不可变引用传递，不让 TextItem 同时存在于两个 owner 容器。PageAnalysisDraft 不是公共结果且不可序列化；外部 OCR 只能补充文字事实。关键阶段前后记录数量日志，不记录全文。

- [ ] **步骤 5：运行 阶段 B 全部门禁**

运行：

~~~bash
rtk cargo test -p docparse-core
rtk cargo test -p docparse-pdfium
rtk cargo clippy -p docparse-core --all-targets -- -D warnings
rtk cargo fmt --all -- --check
~~~

预期：默认离线完成；不需要模型或网络。

---

## 阶段 B 完成门禁

- 合成 PDF 的文本事实和丰富 metadata 可重复提取。
- 所有稳定 ID 符合 spec，结果不含裸 PDFium identity。
- 每页 Native TextItem 集合守恒且唯一所有。
- 模型完整覆盖、部分覆盖、错误覆盖和完全缺失均有测试。
- 一个模型 Region 必须对应一个 Block，并保留模型 label；只有 fallback Region 可拆成多个自然段 Block。
- 中文、RTL、Vertical、双栏、inline_formula 与顺序消环均有回归测试。
- PageAnalyzer 在输入排列变化下输出确定。
- OCR 只有 trait 与 fake 测试，没有内置实现。

本计划不包含 commit。提交需用户单独授权。
