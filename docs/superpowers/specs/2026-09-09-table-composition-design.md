# Table Composition Design

## Scope

Recover structured cells inside final blocks labeled `table`. Keep PDFium and PP-DocLayoutV3 as the existing extraction/detection stack. Native and browser targets use the same Rust reconstruction and rendering code. No additional model, PDF parser, global table detector, or cross-page table merge is introduced.

Reference implementations inspected locally: pdf-inspector `eb02bf8` (`tables/detect_struct.rs`, `detect_lines.rs`, `grid.rs`, `structured.rs`) and LiteParse `4eae636` (`markdown_layout/tables.rs`, `blocks.rs`). The reusable ideas are evidence precedence, physical-rule topology, repeated text alignment, multiline-cell recovery, and geometry-backed splitting of long source runs. Document-specific thresholds and whole-page detectors are outside this change.

## Source facts and ownership

- Capture compact word geometry, UTF-8 byte ranges, and marked-content IDs while PDFium characters are already being extracted. Keep these facts transient in `ExtractedPage`; do not duplicate them for every ordinary text item in canonical JSON.
- Capture axis-aligned visible vector rules and tagged table cells using the existing PDFium path and structure-tree APIs. Discard curves and irrelevant path payloads. Bound reconstruction dimensions and reject inconsistent grids.
- Existing `Block.lines[].text_items` remain the sole owners of immutable source text facts. Table cell lines hold non-owning `TableTextSpan` references: source item ID, a half-open UTF-8 byte range, and its measured bbox.
- Every non-whitespace source character in a successfully reconstructed table must be referenced exactly once. Validate reference boundaries, source ownership, cell occupancy, bounds, and derived cell/block text. Empty cells carry no text references.

## Structure recovery

After model ownership and containment normalization, reconstruct each table independently. Use tagged `Table/TR/TH/TD` structure with MCID attribution when it accounts for the owned source text. Otherwise use drawn horizontal/vertical separators; missing separators establish merged cells only when the resulting connected component is rectangular. Thin filled rectangles may supply rules. Sparse horizontal rules support text-derived columns and preserve line wrapping within a ruled row. A coarse partial grid containing repeated numeric subcolumns is refined using text alignment instead of hiding the missing separators inside large cells.

For borderless tables, use repeated whitespace gutters/alignment across physical text rows, with thresholds relative to font metrics and gutters narrowed by complete rows or contiguous subheaders. Require at least two supported columns and multiple rows. Preserve sparse rows, empty cells, and multilevel headings when evidence permits. Do not interpret every word gap as a column, every physical line as a row, or a numeric first row as a header. Unsupported/ambiguous structures retain line-preserving text with a `TableStructureUnavailable` warning.

Table model bounds retain the grid's whitespace during initial block construction so cell recovery happens inside the same parent geometry used by containment normalization. Captions, watermarks, reference annotations, and neighboring blocks are not pulled into a table by the reconstruction pass.

## Public result

Add optional `Block.table`, omitted for ordinary and unresolved blocks. `Table` contains `row_count`, `column_count`, ordered `cells`, and a typed structure source (`tagged_pdf`, `ruled`, or `text_alignment`). Each `TableCell` contains zero-based row/column, positive row/column spans, optional bbox, header flag, derived text, and cell-local lines of text references. Tagged empty cells may lack measurable bounds. Covered grid positions never become duplicate cells.

The addition is backward-compatible when deserializing existing results. Canonical table text is a row-major tab/newline projection with merged-cell continuations empty. Cell-local wrapping is preserved in cell lines. Plain-text output uses the table projection. Markdown uses a pipe table only for a rectangular, single-header-row table without merged cells; otherwise emit escaped HTML with explicit spans. Raw source lines remain available as evidence.

The browser SDK exposes the typed table result. Selecting a table in the example displays a safely constructed HTML table in the inspector; copying uses canonical table text. The page overlay stays one parent table region. Add a `table_structure` stage timing.

## Validation

Regression coverage includes ruled and borderless tables, narrow column gaps inside one native run, blank cells, wrapped headers/body cells, merged rows/columns, UTF-8 byte boundaries, duplicate/out-of-range references, numeric header rejection, and ambiguous fallback. Verify source facts are unchanged and owned once. Check native and WASM builds and real-model browser parsing; frontend unit tests are not added. Use generated PDFs for deterministic structure checks and the existing real 47-page research PDF for model/pipeline acceptance. Embedded fonts receive strict parity checks; unembedded-font differences remain recorded separately.
