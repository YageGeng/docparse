# docparse-core

DocParse 的原生 PDF 文本提取、文档上下文、版面/文字融合、异步 parser、稳定 schema、关系和 JSON/Text/Markdown/SVG 输出 crate。

主要入口是 `DocParser`/`DocParserBuilder`。自定义 layout/OCR 通过 `Arc<dyn LayoutEngine>` 与 `Arc<dyn OcrEngine>` 注入；默认解析运行 production PP-DocLayoutV3。完整 API 和 E2E 命令见仓库根目录 `README.md`。

## Table composition

Model-owned table regions retain their whitespace and pass through shared table reconstruction after containment normalization. `src/table/evidence.rs` captures transient measured words, vector rules, and PDF structure tags. `grid/` separates tagged ownership, ruled topology, text alignment, and merged-cell recovery. `assemble.rs` reuses cell-local line assembly while preserving canonical text ownership; `render.rs` and `validate.rs` handle projections and invariants.

`Block.table` is an optional structured view. Rows/columns are zero-based; every grid position has exactly one cell or spanning owner. Cell lines reference UTF-8 slices of the existing TextItems instead of replacing or duplicating them. Grid size is bounded to 256 rows, 64 columns, and 4096 positions. Cells with unavailable geometry are allowed only when empty. When reconstruction fails, the table keeps its source lines and receives a recoverable warning.

Tagged cells use MCIDs and explicit spans; filtered empty tagged cells are realigned using complete-row geometry. Vector separators provide stronger topology evidence than text spacing. Coarse partial grids are refined when they still contain repeated numeric subcolumns. Text-aligned grids refine gutters using complete rows and contiguous subheaders, retain sparse rows, and use sparse rules and centered labels to recover supported spans. The same code runs natively and inside the browser Worker.
