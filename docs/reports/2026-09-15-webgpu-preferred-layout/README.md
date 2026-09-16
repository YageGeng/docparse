# WebGPU preferredLayout A/B — 2026-09-15

## Result

On this **Apple M4 / Chrome 153.0.8010.37**, **NHWC reduced median cumulative SDK parse latency by 15.24%**, from **381.824s to 323.641s** for **20 real PDFs / 694 pages**. Corresponding parse throughput increased by **17.98%**.

| preferredLayout | Trial 1 (s) | Trial 2 (s) | Median parse time (s) | Pages/s |
| --- | ---: | ---: | ---: | ---: |
| NCHW | 379.244 | 384.405 | 381.824 | 1.818 |
| NHWC | 327.717 | 319.565 | 323.641 | 2.144 |

All four runs completed **80 document executions / 2,776 pages**, with zero parse errors and zero model-unavailable/runtime-failure/timeout degradation. Every document's normalized canonical JSON hash is identical across **all four runs**. Only the execution-local TSR request number in `external_table_structure` evidence is normalized; confidence, text, geometry, ordering, cells, other evidence, relations and diagnostics remain in the comparison.

For this model set and corpus, **NHWC is the better measured choice**. This task adds a reproducible benchmark and its evidence; it does not change the production WebGPU preferred-layout default or the earlier CoreML work.

## Release build and parameter verification

- The existing production build script ran `cargo build -p docparse-web --target wasm32-unknown-unknown --release --locked`, followed by wasm-bindgen 0.2.125 and Binaryen 132.0.0 `wasm-opt -O4` with the required SIMD and exception-handling options.
- The post-bindgen WASM decreased from 11,683,975 to **8,021,613 bytes**. All four cases loaded the same build.
- WASM SHA-256: `81161c1fa032a68d0b7d9a4bf12d73264e27ea32efaa222fb53ea8e35c5f0398`. WASM and all packaged ORT asset hashes still matched the build manifest after the runs.
- Runtime: `onnxruntime-web` **1.27.0**. The browser was headed installed Chrome, using Apple M4 through Metal; the actual ORT GPU device reported vendor `apple`, architecture `metal-3`, and `isFallbackAdapter=false`.
- The existing recording Worker imports the actual release production Worker. Its session-creation hook changes only `executionProviders[].preferredLayout` to NCHW or NHWC, then calls the original ORT implementation. Each of the three enabled model sessions recorded the requested value. Actual GPU queue submissions were checked during warmup and every measured PDF.
- No graph capture, precision, adapter preference, model artifact, input shape or inference-lock changes were made.

See [build-manifest.json](build-manifest.json), [metadata.json](metadata.json), and the initialization records in [measurements.jsonl](measurements.jsonl).

## Workload and timing boundaries

All PDFs directly inside `/Volumes/Yage/Downloads/docs` were used. Their names, sizes and SHA-256 values are recorded in [inputs.json](inputs.json); they match the previously verified 694-page native corpus. Every browser case prefetched and verified all PDF bytes before initialization and measurement.

The same parser configuration was used throughout:

- Layout session pool 1, score threshold 0.5.
- OCR disabled; TSR `tsr_only`, SLANet+ and wireless RT-DETR cell detection enabled.
- Browser page concurrency, render queue and blocking limit all 1, as required by the production WASM boundary. ORT WASM threads also remained 1.
- Render DPI 144 and maximum long edge 2400.
- The SDK `parse()` API returns its canonical `DocumentResult`, including diagnostics. Renderer visibility flags do not remove them from this parse result. No page previews were requested.

The order was **NCHW-1 → NHWC-1 → NHWC-2 → NCHW-2**. Each case used a fresh browser context, Worker and model sessions. Before its measured corpus, `2603.01919v2.pdf` was parsed twice; all three enabled model sessions had observed calls and GPU submissions. Model loading, downloads and those warmups are excluded from the headline timings. Unseen data-dependent work can still occur in the corpus.

The headline is the **sum of 20 sequential public `parser.parse()` promise durations**. It includes PDFium, model calls, fusion, WASM-to-JavaScript result serialization and Worker message delivery. It excludes PDF fetching/hashing and the subsequent output hashing, JSON persistence, and controller gaps. This boundary avoids retaining the entire corpus's results in browser memory just to defer validation.

For reference, complete corpus loop wall times, including validation, persistence and controller overhead, were **401.047s / 406.152s for NCHW**, and **349.255s / 341.242s for NHWC**. Validation/persistence inside the page averaged about **13.63s** and **13.44s**, respectively.

Model/runtime initialization times were NCHW **19.20s / 19.05s**, NHWC **19.16s / 18.67s**. Two-pass warmup times were NCHW **50.01s / 47.64s**, NHWC **43.99s / 42.80s**. These are separately observed phases, not parts of the parse-time total.

## Where the difference appears

The following values are mean cumulative intervals per complete 694-page corpus:

| Stage | Calls, both layouts | NCHW (s) | NHWC (s) |
| --- | ---: | ---: | ---: |
| Layout inference | 694 | 222.233 | 170.396 |
| TSR inference | 174 | 96.245 | 95.827 |
| Cell detection | 160 | 23.001 | 19.940 |

Most of the observed reduction appears in layout inference. Model invocation counts are unchanged, so the shorter time was not obtained by skipping table or page work. These stage intervals may overlap or nest and can include CPU operations, synchronization and readback; they are not pure GPU-kernel timings and must not be summed as disjoint wall time.

## Output checks and limits

[output-comparison.json](output-comparison.json) records every file and all four hashes. [documents.csv](documents.csv) provides per-PDF repetitions and medians; [stages.csv](stages.csv) contains all recorded stages. Full normalized documents stay in the ignored `target/webgpu-layout-abba/documents/` directory rather than in this report.

A preliminary three-page smoke test also passed with identical output: 1.109s NCHW versus 0.807s NHWC. Its small sample is not substituted into the corpus result; see [smoke.jsonl](smoke.jsonl).

This comparison establishes output equivalence between these two settings on the selected corpus, not a new semantic-accuracy evaluation of the PDFs. Existing parser warning counts are retained in the raw measurements. There are only two corpus repetitions per setting. Desktop activity, clocks, caches and other applications' GPU use were not fixed; the ABBA order reduces linear drift but does not eliminate every source of variance. GPU/process memory and isolated GPU-kernel execution were not profiled. These findings should not be generalized to other models, GPUs or browsers without measurement.

## Reproduce

```sh
rtk npm run build --prefix packages/wasm-web
rtk proxy node packages/wasm-web/tests/preferred-layout.mjs --pdf-dir /Volumes/Yage/Downloads/docs --output target/webgpu-layout-repeat
```

Use a fresh output directory. Add `--smoke` to run both settings on the smallest input PDF after the same model-family warmup. The runner uses installed headed Chrome, a loopback-only asset server, actual production WASM parsing, and strict GPU initialization. It closes only the browser and server it owns.

The Node server serves approved PDF/model/SDK files and persists results; all parsing and inference occur in the production browser Worker. Timing callbacks retain their normal behavior, while the instrumentation avoids cloning a growing inference history on every callback.

Validation completed: production release build and ABI/import checks, SDK TypeScript checking, JavaScript syntax checks, whitespace checks, two smoke cases, four full-corpus cases, effective session-option/device checks, and all-document output hashes.
