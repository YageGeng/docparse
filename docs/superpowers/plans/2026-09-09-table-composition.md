# Table Composition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Recover and render tables inside existing table layouts while preserving immutable source facts.

**Architecture:** Collect transient PDFium evidence, reconstruct table-local grids after layout ownership, and attach validated non-owning cell references. Shared Rust renderers and the Web inspector consume the resulting typed table.

**Tech Stack:** Existing PDFium wrappers, Rust/Serde/typed-builder, shared line assembly, TypeScript DOM, real ORT CPU/WebGPU acceptance.

**Spec:** `docs/superpowers/specs/2026-09-09-table-composition-design.md`

## Global Constraints

- No commit or worktree; native/Web behavior shares Rust code and existing compatibility boundaries.
- Preserve raw text and primary source ownership; no additional inference model or PDF parser.
- Comments/documents are English; new nontrivial logic and functions are documented.
- Frontend acceptance uses the real production Worker and model; no frontend unit tests.

## Task 1: Source evidence and result contract

Files: `crates/core/src/table/{mod.rs,evidence.rs}`, `extract/{mod.rs,text.rs}`, `runtime/pdfium_executor.rs`, `types.rs`, `lib.rs`.

- [x] Add regression cases for a source run crossing narrow table columns and source byte-range preservation.
- [x] Add `Table`, `TableCell`, `TableCellLine`, `TableTextSpan`, `TableStructureSource`, and transient `TableEvidence` types. Use `Range<usize>` for byte references instead of the existing item-ordinal `TextItemRange`.
- [x] Collect word ranges/geometry without changing segmentation IDs or raw strings; collect compact vector rules and tagged cells from existing PDFium APIs.

## Task 2: Reconstruction and validation

Files: `crates/core/src/table/{grid/{mod,tagged,ruled,aligned,spans}.rs,assemble.rs}`, `semantic/mod.rs`, `page.rs`, `label_policy.rs`, `types.rs`, `validate.rs`, `crates/layout/src/timing.rs`.

- [x] Write failing table reconstruction cases for explicit rules, merged cells, borderless alignment, multiline/empty cells, and uncertain fallback.
- [x] Implement evidence-ranked grid recovery and cell-local text assembly; return diagnostics on unsupported structures.
- [x] Attach tables after containment normalization and before final ordering; keep table model padding and preserve original source facts.
- [x] Validate non-overlapping occupancy and exact, unique coverage of source characters; reject malformed public table payloads.
- [x] Add table-specific text projection and `table_structure` timing.

## Task 3: Renderers and example

Files: `crates/core/src/table/render.rs`, `render/{json.rs,markdown.rs,text.rs}`, `packages/web/src/types.ts`, `packages/web/example/{src/main.ts,index.html,style.css}`.

- [x] Test escaped Markdown/HTML, span handling, numeric first rows, and canonical table text.
- [x] Render table structure without re-running reconstruction or duplicating source content.
- [x] Display cell rows/spans in the inspector with textContent-based DOM construction; keep copy/selection behavior and viewport layout.

## Task 4: Real acceptance

Files: crate-level integration tests and PDF fixture generators; existing browser acceptance and native-reference generators; relevant READMEs.

- [x] Generate and visually inspect embedded-font ruled, borderless, and tagged table fixtures.
- [x] Run focused Rust tests, full relevant regressions, native/WASM Clippy, compatibility checks, SDK/example compilation, and optimized release build.
- [x] Run real-model CPU/WebGPU browser checks and inspect actual tables in the 47-page PDF. Compare raw source facts and record structure/fallback counts and timings.
- [x] Review the final diff and update documentation with observed limits and acceptance evidence.

## Verification

- 212 relevant Rust tests passed, 5 were ignored; all 19 table integration cases passed. The separate real-model native reference test passed.
- Native and WASM Clippy, compatibility-boundary checks, TypeScript checks, example build, and pre-commit hooks passed.
- CPU and WebGPU browser suites passed against the optimized production Worker/model, including three repeat cycles, forced memory growth, tensor cleanup, cancellation/recreation, table markup, source references, and stage timings.
- Ruled, borderless, and tagged embedded-font table fixtures had zero native/Web differences on both backends. Unembedded-font metric differences were recorded separately.
- The full 47-page research PDF produced seven structured tables in native and Web example runs: 14x8, 11x10, 8x8, 8x6, 5x6, 10x7, and 7x5. No table-structure fallback warning remained. Existing unrelated partial-overlap warnings were retained.
- UI inspection verified all seven tables, the page-18 six-row model labels, and a fixed 622x903 viewport. The observed WebGPU run took 18.1 seconds including initialization; table structure totaled 8.7 ms across six pages. These are observations, not controlled speed comparisons.
- Local detailed evidence is recorded in `target/table-analysis/verification.json` and the ignored browser reports it references.
