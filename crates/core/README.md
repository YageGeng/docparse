# docparse-core

Formula records retain the original layout `bbox`. When native glyph metrics
justify completing a clipped formula or an adjacent script, `crop_bbox` records
the refined inference bounds and `text_spans` includes the recovered source.
The original text facts remain unchanged; ambiguous or estimated script geometry
does not drive the native-glyph refinement.

Inline recognition crops additionally expand toward the first locally background-colored
row or column on each side of the existing detector/glyph extent. The probe uses the
already rendered page raster, estimates the background from a nearby perimeter, and
requires every pixel of the boundary line to match within a small RGB tolerance.
The search is bounded to one crop height per side, the page edges and unrelated
neighboring text rows; a side without a reliable separator stays unchanged. This
prevents a detector box grazing another row from expanding across that row's ink.
Confirmed cross-line scripts remain inside the seed. Each side probes independently
over the seed's original width or height and stops at the first matching line.
Resolved edges stay fixed, even if another side exposes ink in a corner.
Display formulas keep their existing cropping policy. `crop_bbox` records the final
recognition bounds while `bbox` and native text ownership remain unchanged. The final
RGB crop is copied from the page raster into a contiguous model input; no PDF rerender
or enlargement of an earlier formula thumbnail is involved.

Background sampling uses a fixed 12-bit RGB histogram. Neighbor rows are checked
geometrically before inspecting text ownership, and the extra diagnostic perimeter
scan runs only when DEBUG events are enabled. These optimizations preserve the
background tolerance, search limits and resulting crop pixels.

The ignored `benchmark_captured_formula_crops` test replays a saved `PageResult`
JSON and its matching rendered PNG without loading inference models. Set
`FORMULA_CROP_BENCH_PAGE`, `FORMULA_CROP_BENCH_RASTER`, and
`FORMULA_CROP_BENCH_OUTPUT` to absolute paths, then run
`rtk cargo test -p docparse-core --release --lib benchmark_captured_formula_crops -- --ignored --nocapture`.
The report records seven timing samples per stage and exact crop/limit metadata
plus RGB SHA-256 fingerprints for before/after comparisons. A local replay of 36
inline formulas measured 354 to 143 microseconds per page (median, 1,000 replays
per sample); this measures crop preparation only, excluding PDF rendering, text
refinement and model inference.

DocParse 的原生 PDF 文本提取、文档上下文、版面/文字融合、异步 parser、稳定 schema、关系和 JSON/Text/Markdown/SVG 输出 crate。

主要入口是 `DocParser`/`DocParserBuilder`。自定义 layout/OCR 通过 `Arc<dyn LayoutEngine>` 与 `Arc<dyn OcrEngine>` 注入；默认解析运行 production PP-DocLayoutV3。完整 API 和 E2E 命令见仓库根目录 `README.md`。

## Character recovery

Native extraction resolves untrusted PDF font mappings through PostScript/AGL
glyph names, the embedded font's Unicode cmap, then an optional `GlyphResolver`.
The existing `CIRCLE`, `Circle`, and `LEFTCIRCLE` aliases use the same name table
and suffix rules. Named symbols can repair ASCII fallbacks, including painted
spaces; meaningful Unicode remains authoritative unless its font is untrusted.
Coincident half-circle/outline glyphs compose by geometry and MCID, even across
fonts, while independent symbols retain their separate source codes.

One source glyph may expand into multiple characters without duplicating its
geometry or source code. Latin presentation ligatures and LiteParse's six
control-code ligatures expand; typographic quotes, primes and dashes normalize
to ASCII. `repair_actions` records name, cmap, outline, composition, ligature and
punctuation changes. Recovered mappings no longer count as unresolved Unicode
errors. OCR text remains the provider's text, and no geometry-based spaces or
second normalized-text field are introduced.

`ResolvedGlyph` finalizes character normalization and repair evidence before
`SegmentBuilder` receives a fact. AGL names and font caches retain raw candidates;
cached lookups never retain occurrence-specific trust or geometry decisions.
Each recovery stage loads lazily through a font/code-scoped `OnceCell`. The
shared `text_rules` module owns pure character and list-marker rules, so
renderers do not depend on semantic assembly internals. Test PDF serialization
is shared under `tests/common/pdf.rs`.

Inject a shared resolver with `DocParserBuilder::glyph_resolver(Arc<dyn GlyphResolver>)`.
It receives `(segment_type, x, y)` outline segments sampled at
`GLYPH_RESOLVER_FONT_SIZE` (10pt). On native platforms, `FontDbResolver::new(path)`
or `DOCPARSE_FONT_DB_DIR` selects a LiteParse-compatible directory of
`0000.msgpack`–`ffff.msgpack` shards containing `[hash, unicode]` records. Keys are
the first 16 BLAKE3 bytes of little-endian `(i32, f32, f32)` segments. The database
is supplied separately; missing, oversized or malformed shards yield no match.
Type3 glyphs for which PDFium exposes no outline cannot use this fallback.
Browser callers can inject a resolver without filesystem access.

Plain text removes common page margins and NUL placeholders. Semantic Markdown
collapses prose whitespace and joins lowercase continuations after line-end
hyphens, including adjacent prose blocks. This heuristic can also join genuine
hyphenated compounds. Raw Markdown retains physical lines; tables, algorithms,
vertical text and blocks containing inline formulas bypass prose cleanup.
Renderers do not mutate canonical text or table source ranges. List detection
recognizes circle/square bullets and the private-use Word bullet across source
item boundaries.

## Table composition

Model-owned table regions retain their whitespace and pass through shared table reconstruction after containment normalization. `src/table/evidence.rs` captures transient measured words, vector rules, and PDF structure tags. `grid/` separates tagged ownership, ruled topology, text alignment, and merged-cell recovery. `assemble.rs` reuses cell-local line assembly while preserving canonical text ownership; `render.rs` and `validate.rs` handle projections and invariants.

Sparse ruled tables with a single header band can also recover columns from persistent body gutters constrained by heading phrases. A column of independent labels or scores supplies logical rows, while horizontal bands preserve wrapped text and hierarchical row spans. This fallback rejects unsupported boundaries and remains subject to the same bounded-grid and complete-source-ownership validation as the other strategies.

Ordinary text and table cells share inline ordering in `src/line/inline.rs` and `scripts.rs`. Vector fraction bars require a nearby typographic baseline before numerator and denominator become one ordered unit. Script groups, including nested and multi-part indices, remain intact when lines join. Temporary glyph alignment never changes the original text, bounds, or measured baselines; uncertain alignment keeps the original grouping. This preserves source characters without performing Unicode repair or generating a mathematical expression tree.

Detached display-operator limits use centered, compact bands on both sides of a hanging glyph with an established body baseline. Unambiguous groups retain the operator followed by its lower and upper limits before ordinary line grouping; isolated notes and competing operators keep their original ownership.

Validated inline/display formula detections constrain script ownership before model assignment and remain available during semantic merges and cell-local assembly. `src/line/formula.rs` orders each formula scope independently, preserves separate display rows, and inserts intact inline atoms into surrounding prose before restoring the original glyphs. Confirmed formula regions can establish standalone fractions; undetected content retains conservative geometry-based handling. Small clipped glyphs stay in their formula when centered with majority coverage; multi-word runs require high coverage or bounded edge overflow. InlineSpan annotations are attached afterward to the final item order.

`Block.table` is an optional structured view. Rows/columns are zero-based; every grid position has exactly one cell or spanning owner. Cell lines reference UTF-8 slices of the existing TextItems instead of replacing or duplicating them. Grid size is bounded to 256 rows, 64 columns, and 4096 positions. Cells with unavailable geometry are allowed only when empty. When reconstruction fails, the table keeps its source lines and receives a recoverable warning.

Tagged cells use MCIDs and explicit spans; filtered empty tagged cells are realigned using complete-row geometry. Vector separators provide stronger topology evidence than text spacing. Coarse partial grids are refined when they still contain repeated numeric subcolumns. Text-aligned grids refine gutters using complete rows and contiguous subheaders, retain sparse rows, and use sparse rules and centered labels to recover supported spans. The same code runs natively and inside the browser Worker.

## Table model and external structures

`TableGeometry` holds immutable source evidence. `CellGrid` owns candidate cells,
logical row bands, and occupied grid positions. Its validated edits are atomic;
once source words are bound, topology is frozen. Header recovery and sparse row
planning operate on this shared state instead of independently mutating parallel
arrays. Local and external candidates use the same text population and source
validation.

Use `ParseOptions` with `parse_bytes_with_options`, `parse_path_with_options`, or
`parse_page_with_options` to select a per-call `TableOptions` policy:

- `RulesOnly`: preserves the existing local path and does not load or call a TSR model.
- `Fallback` (default): requests external structure only after local reconstruction or source validation fails.
- `TsrOnly`: every layout table uses the configured TSR engine; provider failures retain source lines without silently running local reconstruction.

The independent `docparse-tsr` crate supplies the default SLANet_plus ONNX engine.
Its artifacts load once through `[tsr]` configuration. `ParseOptions.table` inherits
that configuration when absent; an explicit value replaces the per-call policy.
Supply an `Arc<dyn TableStructureEngine>` through the parser builder or
`ParseOptions.table_engine` to override the built-in engine. The engine receives one
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

Ready table requests are submitted concurrently. The parser does not impose a
separate `table_jobs` limit; built-in models apply bounded-queue backpressure,
and custom providers own their admission policy.
`timeout_ms` (default 60000) includes queue wait; there is no automatic retry.
Dropping the provider future must release the adapter's outstanding resources.
An external success uses `source: "external_tsr"`; consumers enabling this mode
must accept the additional enum value. Failed input, provider failures, timeouts,
and source-assignment failures have distinct warning codes, while the original
block lines remain intact. `fallback` cannot detect a semantically wrong local
result that nevertheless passes all structural/source checks.

The built-in position head produces approximate boxes. Its adapter uses the
predicted topology to form shared row/column boundaries. `TsrGeometryPolicy::Predicted`
allows those boundaries to align to existing source-ink gaps within half a median
font size, with strict majority ownership and frozen block edges. Rows, columns,
spans, and header flags remain unchanged; the normal 80% ownership and complete
UTF-8 coverage checks still apply afterward. Caller-provided engines default to
`Declared` geometry and retain their supplied positions.

`ParserArtifacts::builder().layout(layout).tsr(tsr).ocr(ocr).formula(formula).build()` provides explicit model bytes for native and Web.
Pass it to `DocParser::from_artifacts` or `DocParserBuilder::artifacts`; enabled
TSR requires `Some(tsr)` unless a table engine is injected. Byte-based creation
never falls back to configured model paths. A single layout `ModelArtifacts`
remains accepted for `rules_only`. The browser ABI delegates to this same builder.

The shared TSR decoder applies `TsrGeometryPolicy` before grid occupancy is
validated: declared input remains strict, while model predictions may reconcile
learned spans. Built-in and external model adapters use that same path. Geometric
word coverage alone does not skip source-supported topology refinement.

## Render delivery capacity

All clones of a parser share `render.queue_size` unfinished page deliveries.
Slots are reserved before rasterization and remain occupied until page result
collection and actual resource cleanup. `parse_session_with_options` accepts a
PDFium session already opened on a reserved process, avoiding a second pool wait.
Standalone page parsing uses the same capacity; caller-allocated images predate
this admission boundary. Retaining a `PageImage` clone also retains its delivery;
observers that need an independent permanent copy must copy pixel data deliberately.
