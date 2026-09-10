# docparse-core

DocParse 的原生 PDF 文本提取、文档上下文、版面/文字融合、异步 parser、稳定 schema、关系和 JSON/Text/Markdown/SVG 输出 crate。

主要入口是 `DocParser`/`DocParserBuilder`。自定义 layout/OCR 通过 `Arc<dyn LayoutEngine>` 与 `Arc<dyn OcrEngine>` 注入；默认解析运行 production PP-DocLayoutV3。完整 API 和 E2E 命令见仓库根目录 `README.md`。

## Table composition

Model-owned table regions retain their whitespace and pass through shared table reconstruction after containment normalization. `src/table/evidence.rs` captures transient measured words, vector rules, and PDF structure tags. `grid/` separates tagged ownership, ruled topology, text alignment, and merged-cell recovery. `assemble.rs` reuses cell-local line assembly while preserving canonical text ownership; `render.rs` and `validate.rs` handle projections and invariants.

Sparse ruled tables with a single header band can also recover columns from persistent body gutters constrained by heading phrases. A column of independent labels or scores supplies logical rows, while horizontal bands preserve wrapped text and hierarchical row spans. This fallback rejects unsupported boundaries and remains subject to the same bounded-grid and complete-source-ownership validation as the other strategies.

Ordinary text and table cells share inline ordering in `src/line/inline.rs` and `scripts.rs`. Vector fraction bars require a nearby typographic baseline before numerator and denominator become one ordered unit. Script groups, including nested and multi-part indices, remain intact when lines join. Temporary glyph alignment never changes the original text, bounds, or measured baselines; uncertain alignment keeps the original grouping. This preserves source characters without performing Unicode repair or generating a mathematical expression tree.

Detached display-operator limits use centered, compact bands on both sides of a hanging glyph with an established body baseline. Unambiguous groups retain the operator followed by its lower and upper limits before ordinary line grouping; isolated notes and competing operators keep their original ownership.

Validated inline/display formula detections constrain script ownership before model assignment and remain available during semantic merges and cell-local assembly. `src/line/formula.rs` orders each formula scope independently, preserves separate display rows, and inserts intact inline atoms into surrounding prose before restoring the original glyphs. Confirmed formula regions can establish standalone fractions; undetected content retains conservative geometry-based handling. Small clipped glyphs stay in their formula when centered with majority coverage; multi-word runs require high coverage or bounded edge overflow. InlineSpan annotations are attached afterward to the final item order.

`Block.table` is an optional structured view. Rows/columns are zero-based; every grid position has exactly one cell or spanning owner. Cell lines reference UTF-8 slices of the existing TextItems instead of replacing or duplicating them. Grid size is bounded to 256 rows, 64 columns, and 4096 positions. Cells with unavailable geometry are allowed only when empty. When reconstruction fails, the table keeps its source lines and receives a recoverable warning.

Tagged cells use MCIDs and explicit spans; filtered empty tagged cells are realigned using complete-row geometry. Vector separators provide stronger topology evidence than text spacing. Coarse partial grids are refined when they still contain repeated numeric subcolumns. Text-aligned grids refine gutters using complete rows and contiguous subheaders, retain sparse rows, and use sparse rules and centered labels to recover supported spans. The same code runs natively and inside the browser Worker.

## External table structures

`TableGeometry` holds immutable source evidence. `CellGrid` owns candidate cells,
logical row bands, and occupied grid positions. Its validated edits are atomic;
once source words are bound, topology is frozen. Header recovery and sparse row
planning operate on this shared state instead of independently mutating parallel
arrays. Local and external candidates use the same text population and source
validation.

Use `ParseOptions` with `parse_bytes_with_options`, `parse_path_with_options`, or
`parse_page_with_options` to select a per-call `TableOptions` policy:

- `RulesOnly` (default): preserves the existing local path and never calls a provider.
- `Fallback`: requests external structure only after local reconstruction or source validation fails.
- `ExternalOnly`: every layout table uses the provider; provider failures retain source lines without silently running local reconstruction.

Supply an `Arc<dyn TableStructureEngine>` through `ParseOptions.table_engine` for
either external mode. No service or model is built in. The engine receives one
`TsrTableRequest` containing an owned RGB crop, one-based page number, block ID,
request ID, reason, and the actual crop-to-viewport transform. It returns a
`TsrTableInput` with the same request ID, structure tokens, and one 4/8-coordinate
pixel box per cell opening tag. Return boxes in the original request crop space,
undoing any adapter-side resize or padding first. The crop bounds describe raster
sampling, while the normalized block bounds remain the semantic table region.
Decoded cell boxes are trimmed to that original region without changing internal
dividers or spans. A cell with no positive intersection rejects the response;
sampling margins never enlarge the block or change neighboring text ownership.

```rust,ignore
let document = parser.parse_bytes_with_options(
    pdf_bytes,
    docparse_core::ParseOptions::builder()
        .table(docparse_core::TableOptions::builder()
            .mode(docparse_core::TableMode::Fallback)
            .build())
        .table_engine(Some(std::sync::Arc::clone(&your_engine)))
        .build(),
).await?;
```

Tokens support optional `html/body/table` wrappers, `thead/tbody/tfoot`, `tr`,
`td/th` openings and closings, combined `<td></td>`/`<th></th>` tokens, and
SLANet-style split openings (`<td`, ` rowspan="2"`, ` colspan="3"`, `>`).
Only positive quoted numeric span attributes are accepted. Invalid nesting,
unknown HTML, mismatched box counts, overlap, holes, and excessive dimensions
are rejected. Explicit external header flags are preserved. Cell text always
comes from the block's existing Native/OCR facts; external generated text is not
accepted as native content.

External requests share `max_in_flight` (default 2) across the entire parse.
`timeout_ms` (default 60000) includes queue wait; there is no automatic retry.
Dropping the provider future must release the adapter's outstanding resources.
An external success uses `source: "external_tsr"`; consumers enabling this mode
must accept the additional enum value. Failed input, provider failures, timeouts,
and source-assignment failures have distinct warning codes, while the original
block lines remain intact. `fallback` cannot detect a semantically wrong local
result that nevertheless passes all structural/source checks.
