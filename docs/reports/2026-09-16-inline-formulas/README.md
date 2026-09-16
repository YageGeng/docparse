# Inline formula presentation

Both WebUIs render recognized inline math directly inside paragraph prose.
Rust supplies an optional `block.markdown` projection using the existing exact
UTF-8 source ranges; browser clients do not reproduce byte-offset replacement.
Original `block.text`, lines and text items are unchanged. Formula details and
individual LaTeX/Markdown copy actions remain in a collapsed section. Paragraph
copy returns the Markdown source, including its math delimiters.

Source subscripts/superscripts may occupy separate PDF text lines. Their measured
slices are replaced across those lines while the equation is inserted only once
at its primary anchor. Ambiguous anchors keep the original prose and the separate
formula presentation. Unsupported LaTeX produces a local placeholder without
removing the surrounding paragraph.

Validation passed:

- 427 Rust tests, with 33 ignored; all pre-commit hooks and native/WASM Clippy.
- UTF-8, literal Markdown punctuation, cross-line script and uncertain-anchor regressions.
- Both frontend production builds, the release WASM build, and renderer safety checks.
- Real production HTTP and WebGPU UI acceptance on `2410.05779v3.pdf`: 16 pages,
  24 formulas, in-paragraph math, preserved adjacent prose, exact source copy,
  collapsed details and narrow-screen layout. The page-9 split `T_extract`
  regression no longer duplicates the native subscript.
- Exact comparison confirmed that original block text, lines and text items
  remained unchanged between the real results before and after the script fix.

Old HTTP results remain readable but lack the new paragraph projection; reparse
them to enable in-place math. Checks are in `checks.json`; local results and
screenshots are under `target/formula-inline-final/`. No commit was created.
