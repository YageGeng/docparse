# Formula integration review and rendered previews

## Scope

Reviewed the current staged and unstaged feature changes against `HEAD`, plus
the new formula crate and browser checks: configuration and feature forwarding,
artifact identity, preprocessing, tokenizer caching, native/browser ownership
and cancellation, crop/source association, JSON/Markdown/table projections,
validation, the HTTP result representation, SDK transport, model provisioning,
build glue, both frontends and their tests. Historical benchmark files were
treated as recorded evidence rather than current performance guarantees.

## Findings addressed

1. **P2 — HTTP results omitted recognized formulas from the visible inspector.**
   `ResultInspector` read only `block.text` and `cell.text`. It now consumes
   `page.formulas`, renders LaTeX and Markdown with math support, renders enriched
   table cells, and displays formulas without a block anchor. Formula-only blocks
   show recognized mathematics instead of repeating their raw extracted text.
2. **P2 — WASM formula cards displayed source strings rather than mathematics.**
   Both representations now use typeset previews with locally bundled fonts.
   Each copy button retains its exact JSON source, including Markdown delimiters.
   Formula previews precede the optional prose context.
3. **P2 — Selecting a block removed its associated formulas from HTTP JSON inspection.**
   The selected-region projection now includes those records, preserving access
   to the original LaTeX and Markdown fields.
4. **P2 — Unanchored formulas had no selectable WASM inspector entry.**
   The region menu now exposes their stable formula IDs independently of blocks.
   This branch was reviewed structurally; the acceptance document's recognized
   formulas had block anchors.

The example build and existing E2E setup now use the same esbuild bundle path;
plain TypeScript emission would leave bare rendering-library imports unresolved
in a browser. The parser SDK remains an external production artifact, and the
rendering dependencies stay outside the distributed WASM SDK.

No additional blocking backend correctness issue was identified in this review.
This does not establish semantic recognition accuracy or CUDA/OpenVINO runtime
compatibility; those accelerators were not hardware-tested in this pass.

## Rendering boundary

Both frontends use pinned KaTeX, MarkdownIt and the math delimiter plugin.
Markdown raw HTML and image loading are disabled. KaTeX trusted commands are
disabled, with bounded expansion and size. Parse failures show a preview error
while keeping source copy actions available. MathML accompanies the rendered HTML.
The executable renderer checks cover actual Markdown formatting, inline/display
math, malformed formulas, raw HTML/image injection and unsafe LaTeX links.
See [KaTeX's security options](https://katex.org/docs/security).

## Validation

- `cargo test --locked`: 425 passed, 33 ignored.
- All pre-commit hooks passed: platform boundary, formatting and strict Clippy.
- Ten model-provisioning tests passed.
- HTTP workbench production build and WASM example bundle/type checks passed.
- Real HTTP acceptance uploaded `2410.05779v3.pdf` through the production UI to
  the production `docparse-server` with its configured CoreML build. All 16 pages
  completed; every one of its 24 formulas rendered in both representations and
  copied byte-for-byte matching source strings. Formula inference used the
  explicitly reported Plus-S CPU compatibility executor.
- Real WASM acceptance used the production release SDK and WebGPU, yielding
  24 formulas on the same 16 pages. Inline/display typeset output and exact copy
  were checked, together with warm-output parity, default-on state, switch-off
  behavior, session rebuild and the narrow-screen inspector.
- Desktop and narrow screenshots were inspected for both applications. No mock
  parser or fixture backend was used for either WebUI acceptance run.

Compact results are in `checks.json`. Full local results and screenshots are in
`target/formula-render-review/`.

```sh
rtk proxy node crates/web/tests/math_rendering.mjs
rtk proxy node crates/server/tests/formula_web.mjs /absolute/path/to/formulas.pdf
rtk proxy node crates/web/tests/formula_ui.mjs /absolute/path/to/formulas.pdf
```

These browser commands require the corresponding built UI and production server
to be running. No commit was created, and the pre-existing index was preserved.
