# PP-FormulaNet_plus-L integration verification — 2026-09-15

## Delivered behavior

The parser recognizes every existing `inline_formula` and `display_formula` layout
region with the pinned PP-FormulaNet_plus-L model. Formula numbers retain their
native representation. The main local configuration enables formula recognition;
library defaults remain opt-in for existing callers supplying their own engines.

JSON includes both `latex` and `markdown` for each item in `pages[].formulas[]`,
plus its actual engine, geometry and non-owning block/line/cell anchors. Exact
UTF-8 `text_spans` reuse measured native words: mixed runs such as
“learning rate of 7” keep their prose when only the final number is a formula.
Unselected content between disjoint formula slices remains present. Failed
recognition retains explicit errors and source facts. Unknown confidence is not
replaced with a fabricated score.

Markdown substitutes recognized expressions in prose, display blocks and table
cells. Source text items, canonical block text and table source spans remain
unchanged. Table presentation handles Markdown and escaped HTML fallback separately.

## Use

```toml
[formula]
enabled = true
batch_size = 4
timeout_ms = 120000
```

Default artifact paths are inside `models/pp-formulanet-plus-l/`. Provision with:

```sh
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-l
```

- `GET /api/v1/docparse/jobs/result?id=<uuid>` returns the stored JSON envelope,
  including both formula representations.
- Append `&format=markdown` for complete document Markdown. This reads the stored
  result and does not rerun inference.
- CLI `--format json` and `--format markdown` use the same result/rendering logic.
- Browser callers supply `formulaArtifacts` with model, tokenizer and manifest
  URLs/bytes, and enable `config.formula.enabled`; see the
  [SDK documentation](../../../packages/wasm-web/README.md#formula-artifacts-and-output).

No whole-document `.tex` exporter was requested or added.

## Native release HTTP validation

| Metric | Observed result |
| --- | ---: |
| Real PDFs | 3 |
| Complete pages | 73 |
| Formula results | 45 |
| Formula failures | 0 |
| HTTP upload-to-final-download interval | 185.080 s |
| Fresh server startup to readiness | 11.635 s |

The corpus covers 39 inline and 6 display formulas, including 3 formulas inside
recovered table cells. Every formula produced both representations and appeared
in Markdown. The “learning rate of” mixed-run regression was checked in the full
Terminal-Universe document, not only in a synthetic test.

The server was a real `--release --features coreml` build with PostgreSQL, shared
storage, durable jobs, HTTP polling and result downloads. Configuration retained
five document slots, five PDFium workers, one layout session, page concurrency 4,
TSR enabled and OCR disabled. Only three document jobs were submitted. Formula
inference used real CPU tensor batches of up to four, with a shared session.

These are functional acceptance timings from one pass, including first-input
work; no separate corpus warmup was used. They are not a controlled speedup
comparison against the earlier 694-page benchmark.

For all three PDFs, canonical block JSON matched the earlier CoreML HTTP output
exactly after excluding only newly derived cell `markdown` and execution-local
TSR request IDs. Text, geometry, line order, table structure and source references
were retained. See [native-documents.json](native-documents.json),
[native-stages.json](native-stages.json), and [native-summary.json](native-summary.json).

## CoreML compatibility decision

The pinned export's first CoreML invocation produced the correct formula, but
repeated invocations failed or grew resource usage excessively. Restricting calls
to microbatch 1 and disabling memory patterns/CPU arenas did not establish a
stable reusable session. Dynamic MLProgram compilation rejected unbounded shapes;
fixed-shape attempts either failed prediction or exceeded the bounded diagnostic
window. These observations do not establish a general CoreML limitation for other
models or exports.

The retained implementation explicitly uses **CPU for Plus-L on macOS
CoreML/Metal builds**. Other model families retain their selected provider.
The result records `engine = "pp-formulanet-plus-l-onnx-cpu"`, and initialization
logs the compatibility decision. No accelerated CoreML formula performance is
claimed. CPU batch sizes 1/2/3, identical-input output consistency, cancellation
and subsequent session reuse were checked with a real formula crop.

## Production browser validation

The production SDK was built with Rust release, wasm-bindgen and `wasm-opt -O4`.
All packaged build-manifest hashes were verified after the final run.

| Metric | Observed result |
| --- | ---: |
| Browser | Chrome 153.0.8010.37 |
| Provider | webgpu |
| Pages | 32 |
| Formula results / failures | 7 / 0 |
| Parser initialization, including downloads | 87.713 s |
| Document parse | 21.914 s |
| Optimized WASM | 10,164,339 bytes |

The actual adapter reported Apple / Metal-3 and `isFallbackAdapter=false`.
Both layout and formula sessions made GPU submissions. The formula session's
instrumentation originally labels generic `x`-input models as `tsr`; the report
preserves that raw label and separately identifies the actual formula role.
The test supplied only layout and formula model assets. TSR used local rules and
OCR was disabled, so this is not a native/browser full-pipeline timing comparison.
Unsupported operators may execute on CPU inside the WebGPU session.

The new tokenizer exposed a browser WebIDL incompatibility: WebCrypto rejects
resizable WASM buffers. The pinned generated binding now obtains cryptographic
randomness into a normal JS array and copies the small result into WASM memory.
No insecure randomness or whole-model buffer workaround was introduced.

[Browser summary](browser-summary.json), [formula records](browser-formulas.json),
[release manifest](build-manifest.json).

## Verification and retained artifacts

- 347 ordinary Rust tests passed; 20 artifact-dependent tests were ignored in the
  general run and selected real-model checks were executed separately.
- Strict Clippy, rustfmt and the explicit platform-boundary hook passed.
- Model provisioning tests, model SHA verification, SDK/workbench TypeScript,
  generated OpenAPI declarations and JavaScript syntax checks passed.
- Tests cover exact mixed-run byte replacement, UTF-8, disjoint source ranges,
  Markdown/HTML table escaping, batch tails, explicit model failures and JSON
  representation consistency.
- Native source conservation and final HTTP JSON/Markdown were checked on all
  three selected documents. Browser formula execution used the real release SDK.

This is execution/output validation, not a manually labeled mathematical-accuracy
benchmark. Model recognition can still make mathematical errors. An earlier
native/WebGPU comparison of the seven Terminal-Universe formulas differed in
trailing punctuation for one slightly different crop; no universal byte parity
between providers is claimed. CUDA inference was not hardware-tested here.

Source remains uncommitted. [source-state.json](source-state.json) records the base
commit and hashes of changed implementation files; native binary hashes are in
[metadata.json](metadata.json). Full PDFs were not added to Git. Full results,
server logs, process samples and orchestration scripts remain in the ignored
`target/formula-integration-verified/` directory. Bounded CoreML experiments remain
under `target/formula-setup/`. Owned HTTP and browser processes were closed after
their checks.
