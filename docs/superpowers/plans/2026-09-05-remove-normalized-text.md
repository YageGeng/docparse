# 删除 normalized_text 实施计划

> **执行要求：** 使用 `superpowers:executing-plans` 在当前工作区逐项执行。仓库禁止 SubAgent 和 worktree；步骤使用 checkbox 跟踪，未经用户明确授权不创建 commit。

**目标：** 从 schema、提取、融合、渲染和测试中彻底删除 `normalized_text` 与推断空格机制，将输出升级为 schema `2.0`，并统一使用根目录 `docparse.toml`。

**架构：** `TextItem.raw_text` 成为唯一文本事实，native 和 OCR 均只写入该字段；`Line.text` 与所有下游分析直接消费 raw 文本。字段删除通过 schema 主版本 `2.0` 明确表达，不保留 alias 或 v1 迁移层。

**技术栈：** Rust 2024、Serde、Tokio、PDFium、ONNX Runtime、Python 3.11+ E2E 驱动。

**规格：** `docs/superpowers/specs/2026-09-05-remove-normalized-text-design.md`

## 全局约束

- 新增函数必须有英文函数级注释；修改后的非平凡逻辑必须补充英文原因注释。
- 超过 3 个字段的结构体使用 `typed-builder`，`Arc` 字段克隆使用 `Arc::clone`。
- 测试代码仅放在 crate `tests/` 或精确命名的 `#[cfg(test)] mod tests` 中。
- 不新增 Unicode normalization、几何空格推断、兼容字段或 v1 迁移器。
- 不改变稳定 ID、bbox、provenance、reading order、layout/OCR provider 或文本所有权。
- 真实 E2E 使用 `~/Downloads` 全部顶层 PDF、生产 parser 和真实 CUDA provider。
- 所有命令使用 `rtk` 前缀；不创建 commit，不使用 SubAgent 或 worktree。

---

### 任务 1：用失败测试锁定 schema 2.0 与字段删除

**文件：**

- 修改：`crates/core/tests/types.rs`
- 修改：`crates/core/tests/render.rs`
- 修改：`crates/core/src/types.rs`
- 修改：`crates/core/src/runtime/pipeline.rs`

**接口：**

- 产生 `SchemaVersion::V2_0 == SchemaVersion::new(2, 0)`。
- `SchemaVersion` 反序列化接受 `2.x`、拒绝 `1.x` 和其他主版本。
- `TextItem` 不再包含 `normalized_text`。
- `RepairAction` 不再包含 `InsertedSpace`。

- [x] **步骤 1：添加 schema RED 断言**

在 `nested_schema_round_trips_without_reordering` 前增加测试，先用现有 `SchemaVersion::new(2, 0)` 构造文档并断言：

```rust
let value = serde_json::to_value(document_fixture()).expect("document must serialize");
assert_eq!(value["schema_version"], "2.0");
assert!(value["pages"][0]["blocks"][0]["lines"][0]["text_items"][0]
    .get("normalized_text")
    .is_none());
```

把 future minor 改为 `2.9`，unknown major 改为 `1.0`。

- [x] **步骤 2：运行 schema 测试并确认 RED**

运行：`rtk cargo test -p docparse-core --test types`

预期：当前 fixture 仍输出 `1.0` 或包含 `normalized_text`，测试失败。

- [x] **步骤 3：实现 schema 2.0 和最小字段删除**

在 `types.rs` 中新增 `V2_0`、将 supported major 改为 2，删除 `TextItem.normalized_text`、`RepairAction::InsertedSpace` 和 `Line` 的跨 item 空格推断方法。`Line::derive_text` 改为：

```rust
pub(crate) fn derive_text(items: &[TextItem]) -> String {
    let capacity = items.iter().map(|item| item.raw_text.len()).sum();
    let mut text = String::with_capacity(capacity);
    for item in items {
        text.push_str(&item.raw_text);
    }
    text
}
```

将 runtime 新文档版本改为 `SchemaVersion::V2_0`。

- [x] **步骤 4：继续任务 2/3 的编译迁移**

字段删除会使旧 builder 和消费者无法编译；这些编译错误作为后续迁移清单，不通过临时兼容字段规避。

### 任务 2：删除 native 字符级 normalization

**文件：**

- 修改：`crates/core/src/extract/text.rs`
- 修改：`crates/pdfium/src/text_page.rs`

**接口：**

- `TextCharFact` 不再携带 `space_width`。
- `TextItemDraft`、`CurrentSegment` 只保留 `raw_text`。
- `push_visible` 只追加源字符，不计算普通字符间距。
- `TextChar::font_space_width` 被删除；通用 `Font::glyph_width*` API 保留。

- [x] **步骤 1：修改字符级测试形成 RED**

将视觉间距测试重命名为 `visual_gap_does_not_invent_source_space`，要求：

```rust
assert_eq!(item.raw_text, "AB");
assert!(item.repair_actions.is_empty());
```

保留 source whitespace 测试并只断言 `raw_text == "A ..2"`。

- [x] **步骤 2：在删除实现前运行目标测试**

运行：`rtk cargo test -p docparse-core visual_gap_does_not_invent_source_space`

预期：旧实现记录 `InsertedSpace`，测试失败。

- [x] **步骤 3：删除字符级推断状态与代码**

删除 `space_width`、所有 `.normalized_text(...)` builder 调用、`CurrentSegment.normalized_text`、`push_visible` 的 gap/threshold 分支以及提取时 `font_space_width()` 调用。`push_source_space` 只合并并追加到 `raw_text`。

- [x] **步骤 4：删除 PDFium 专用空格宽度方法**

从 `text_page.rs` 删除 `font_space_width()`，确认 `glyph_width_from_char_code` 和 `glyph_width` 仍作为独立公开字体 API 保留。

- [x] **步骤 5：运行 extraction 测试**

运行：`rtk cargo test -p docparse-core extract::text`

预期：视觉 gap 保留 `AB`，源空白保留 `A B`/dot leader，控制字符与 Unicode provenance 测试通过。

### 任务 3：迁移 OCR、分析、语义和 renderer 到 raw_text

**文件：**

- 修改：`crates/core/src/extract/metadata.rs`
- 修改：`crates/core/src/context/relations.rs`
- 修改：`crates/core/src/line/assemble.rs`
- 修改：`crates/core/src/line/bidi.rs`
- 修改：`crates/core/src/line/metrics.rs`
- 修改：`crates/core/src/fusion/assign.rs`
- 修改：`crates/core/src/fusion/fallback.rs`
- 修改：`crates/core/src/page.rs`
- 修改：`crates/core/src/semantic/mod.rs`
- 修改：`crates/core/src/semantic/paragraph.rs`
- 修改：`crates/core/src/semantic/formula.rs`
- 修改：`crates/core/src/render/mod.rs`
- 修改：上述模块的 `#[cfg(test)] mod tests`

**接口：**

- OCR duplicate comparison使用 `native.raw_text` 与 `fact.text.trim()`。
- metadata、bidi、metrics、relations 和 paragraph 特征统一读取 `raw_text`。
- semantic line 构造调用只拼接 raw 的 `Line::derive_text`，不记录空格修复。
- raw renderer 只追加 `item.raw_text`，formula placeholder 插入顺序保持不变。

- [x] **步骤 1：增加跨 TextItem 不补空格测试**

修改 render 测试，将两个有正几何间距且 raw 分别为 `hello`、`world` 的 item 期望改为 `helloworld`：

```rust
assert_eq!(
    TextRenderer::new(RenderView::Raw, "[formula]").render(&document),
    "helloworld"
);
```

- [x] **步骤 2：运行 renderer 测试并确认 RED**

运行：`rtk cargo test -p docparse-core --test render text_renderers_do_not_invent_cross_item_spaces`

预期：旧 renderer 返回 `hello world`，测试失败。

- [x] **步骤 3：按编译错误迁移全部消费者**

把所有 `item.normalized_text` 替换为 `item.raw_text`，删除所有 `.normalized_text(...)` builder 调用和测试赋值。OCR 仍可用局部变量 `trimmed_text` 做空值/去重比较，但最终保存原始 `fact.text`。

- [x] **步骤 4：删除 semantic 的 spacing evidence**

删除 `Line::record_derived_spaces` 调用及相关 `InsertedSpace` 断言。保留 `MergedFragment`、`RemovedControl`、`SoftHyphenHint` 等独立 repair action。

- [x] **步骤 5：运行 core 单元与集成测试**

运行：`rtk cargo test -p docparse-core`

预期：全部通过，JSON 中只出现 `raw_text`。

### 任务 4：收敛唯一配置文件为 docparse.toml

**文件：**

- 删除：`docparse.toml.example`
- 修改：`README.md`
- 修改：`crates/config/README.md`
- 修改：`crates/config/tests/loading.rs`
- 修改：`crates/layout/tests/python_parity.rs`
- 修改：`crates/cli/tests/cli_with_fake.rs`

**接口：**

- CLI 默认继续读取 `./docparse.toml`。
- 仓库文档和测试只引用根目录 `docparse.toml`。

- [x] **步骤 1：先修改配置行为测试**

将 `repository_example_matches_documented_defaults` 重命名为 `repository_default_config_matches_documented_defaults`，路径改为 `../../docparse.toml`；layout parity 与 CLI fake fixture 同步改为 `docparse.toml`。

- [x] **步骤 2：运行配置与 CLI 测试**

运行：`rtk cargo test -p docparse-config --test loading repository_default_config_matches_documented_defaults`

运行：`rtk cargo test -p docparse-cli --test cli_with_fake`

预期：引用新唯一配置后通过。

- [x] **步骤 3：删除示例文件并更新文档**

删除 `docparse.toml.example`。README 删除复制步骤，明确仓库根目录配置可直接使用；crate README 同步只指向 `docparse.toml`。

- [x] **步骤 4：确认无活动引用**

运行：`rtk grep -R -n "docparse.toml.example" README.md crates scripts tests`

预期：无输出。

### 任务 5：更新 schema/序列化测试和公共文档

**文件：**

- 修改：`crates/core/tests/types.rs`
- 修改：`crates/core/tests/render.rs`
- 修改：`crates/cli/tests/cli_with_fake.rs`
- 修改：`docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md`
- 修改：与当前公共 schema 示例直接相关的 README 内容

**接口：**

- 所有 fixture 使用 `SchemaVersion::V2_0`。
- JSON writer 与 canonical serializer 继续逐字节一致。
- 当前公共设计文档声明 schema `2.0` 和唯一 `raw_text` 字段。

- [x] **步骤 1：更新 fixture 与版本断言**

将所有 `SchemaVersion::V1_0` 调用改为 `V2_0`，future minor 断言改为 `SchemaVersion::new(2, 9)`，unknown major 错误断言改为 major 1。

- [x] **步骤 2：更新原始设计文档**

把 schema 当前版本改为 `2.0`，删除当前数据模型中关于 `normalized_text` 与自动空格恢复的要求；保留版本升级原则和迁移历史说明。

- [x] **步骤 3：执行全仓残留扫描**

运行：`rtk grep -R -n "normalized_text" crates/core/src README.md`

运行：`rtk grep -R -n "InsertedSpace\|font_space_width" crates`

预期：生产实现与公共 README 均无输出；`crates/core/tests/types.rs` 只保留旧字段缺失断言。

### 任务 6：完整验证与真实 PDF schema 2.0 E2E

**文件：**

- 验证：全部修改文件
- 生成：`target/docparse-e2e/raw-text-v2-serial/`
- 生成：`target/docparse-e2e/raw-text-v2-parallel/`

**接口：**

- 使用生产 parser、真实 PP-DocLayoutV3 CUDA provider 和 `~/Downloads` 全 PDF。
- 新串并行结果必须 canonical 一致。

- [x] **步骤 1：运行格式、测试和静态检查**

运行：`rtk cargo fmt --all -- --check`

运行：`rtk cargo test --workspace`

运行：`rtk cargo clippy --workspace --all-targets -- -D warnings`

- [x] **步骤 2：运行 CUDA 与 parity 检查**

运行：`rtk cargo check -p docparse-cli --features layout-cuda`

运行：`rtk cargo clippy -p docparse-cli --all-targets --features layout-cuda -- -D warnings`

运行：`rtk cargo test -p docparse-layout --release --test python_parity -- --ignored --nocapture`

- [x] **步骤 3：运行真实串行与并行 E2E**

运行：`rtk uv --cache-dir /tmp/docparse-uv-cache run scripts/run_real_pdf_e2e.py --pdf-dir /home/isbest/Downloads --model-dir models/pp-doclayout-v3 --execution-provider cuda --cargo-profile release --page-concurrency 1 --run-id raw-text-v2-serial`

运行：`rtk uv --cache-dir /tmp/docparse-uv-cache run scripts/run_real_pdf_e2e.py --pdf-dir /home/isbest/Downloads --model-dir models/pp-doclayout-v3 --execution-provider cuda --cargo-profile release --page-concurrency 4 --run-id raw-text-v2-parallel`

- [x] **步骤 4：比较 canonical 并检查目标标题**

运行：`rtk python scripts/compare_e2e_runs.py target/docparse-e2e/raw-text-v2-serial/canonical-hashes.json target/docparse-e2e/raw-text-v2-parallel/canonical-hashes.json`

运行：`rtk jq '[.pages[].blocks[].lines[].text_items[] | select(.raw_text == "SynCode: LLM Generation with Grammar Augmentation")]' target/docparse-e2e/raw-text-v2-parallel/documents/arxiv-2403-01632v4.json`

预期：5 文档、110 页、0 canonical mismatch；目标对象只有 `raw_text`，没有 `normalized_text`。

- [x] **步骤 5：运行 pre-commit 并检查工作区**

运行：`rtk pre-commit run --all-files`

运行：`rtk git diff --check`

运行：`rtk git status --short`

预期：检查全部通过；工作区只包含用户原有修改和本任务修改，没有新增 commit。
