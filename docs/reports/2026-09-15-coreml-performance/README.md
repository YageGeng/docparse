# CoreML inference evaluation — 2026-09-15

## Result

The retained change requests `FastPrediction` specialization for reusable native CoreML/Metal sessions and logs that choice at initialization. Model files, the NeuralNetwork representation, compute-unit selection, precision settings, concurrency and parsing algorithms remain the same. No dependencies were added.

On this Apple M4, the warmed **20-PDF / 694-page** corpus decreased from **428.669s to 371.729s median elapsed time: 13.3% less time, or 15.3% more pages/s**. Each condition has two complete-corpus repetitions. These are observed results on an active desktop, not a statistically established hardware-wide speedup.

| Configuration | Round 1 (s) | Round 2 (s) | Median (s) | Pages/s | Failed / degraded documents |
| --- | ---: | ---: | ---: | ---: | --- |
| Original specialization | 398.649 | 458.688 | 428.669 | 1.619 | 0 / 0 |
| FastPrediction | 390.064 | 353.394 | 371.729 | 1.867 | 0 / 0 |

The four timed rounds cover **80 document executions / 2,776 pages**, excluding warmup, model probes and correctness replays. Individual document medians and both repetitions are in [documents.csv](documents.csv). Raw measurements are in [baseline-c2.jsonl](baseline-c2.jsonl) and [fast-c2.jsonl](fast-c2.jsonl).

## Method

- Apple M4, 10 logical CPUs, 16 GiB memory, macOS 26.6.2; AC power. No thermal/performance warning was reported by `pmset` when checked.
- Native release builds; ONNX Runtime 1.28 through `ort` 2.0.0-rc.13. Baseline source: `500475a0029107540e97eb16dff04a311f43903e`.
- All PDFs directly inside `/Volumes/Yage/Downloads/docs`; [inputs.json](inputs.json) records SHA-256 and size, and [corpus.toml](corpus.toml) records verified page counts.
- Production five-process PDFium IPC pool, document concurrency 2, page concurrency 4, layout session pool 1, render queue 2, blocking limit 4.
- Render DPI 144, longest edge 2400. OCR disabled. TSR `tsr_only`, SLANet+ and wireless RT-DETR cell detection enabled. Evidence included and diagnostics hidden.
- Each configuration starts a fresh process, warms all five PDFium workers, and performs two warmup rounds covering the enabled model families. Both repetitions reuse those sessions. Model loading and warmup are excluded.
- Input hashing warmed filesystem caches; caches were not flushed. Compilation and other parser benchmarks did not run during timed rounds. The existing HTTP service was observed idle and was not restarted. Desktop applications remained active; clocks, CPU affinity and desktop GPU activity were not fixed.
- Timing includes parsing, compact JSON serialization, buffered file output and `fsync`. It excludes HTTP upload/download, database work and durable-job admission.

[benchmark.toml](benchmark.toml) contains the credential-free parser configuration. [builds.json](builds.json) records preserved executable hashes. [process-summary.json](process-summary.json) records `/usr/bin/time -l` observations for the benchmark parent, including initialization and warmup; it does not sum worker memory. Peak parent memory footprint was approximately 8.1 GiB in both configurations.

## Remaining bottleneck

In the first baseline round, layout inference accumulated **396.757s** while the corpus took **398.649s**. Layout is still served by one shared session. [stages.csv](stages.csv) contains per-stage averages across the two rounds.

Stage intervals overlap across pages, documents and models. Queue time can accumulate far beyond corpus elapsed time. Do not sum these values or interpret ONNX Runtime intervals as pure GPU-kernel timings.

## Alternatives evaluated

The ignored `coreml_configuration_probe` test reuses production image preprocessing and output decoding for five fixed images. Every configuration warms those images twice, then records three repetitions per image. Repeated detections were deterministic within every successful recorded probe. [probe-summary.json](probe-summary.json) contains all successful runs, including later repetitions whose timings changed with desktop activity.

- **MLProgram:** rejected at model compilation. `MaxPool.0` has kernel `[2, 2]`, stride `[1, 1]`, `SAME_UPPER` padding and `ceil_mode=1`; this CoreML converter rejects that combination. [Failure details](mlprogram-failure.json). Model bytes and artifact validation were not changed to bypass it.
- **Fixed layout batch dimensions:** executed successfully but changed marginal detections and model ranks. One fixture changed from five to seven accepted regions (the Python CPU oracle also has seven). This is an output difference, not proof of an accuracy regression or improvement; the setting was not retained.
- **GPU low-precision accumulation / restricted compute units:** changed some outputs and did not establish a safer improvement than the retained setting.
- **CPU thread limits / disabling spinning:** one-thread and no-spinning cases were slower. Four-thread probes did not justify a new production thread policy.
- **Removing the unused mask output with ONNX Runtime Model Editor:** preserved detections but showed no incremental benefit in adjacent probes: FastPrediction 304.17ms, FastPrediction plus output pruning 305.46ms. No model-editor path was added to production.

The comparison was informed by [oar-ocr's example configuration at 7feb044](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/examples/utils/device_config.rs#L129) and checked against [ONNX Runtime's CoreML options](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html#available-options-new-api). More enabled options did not automatically produce a better result on these models.

## Output validation

Both original and FastPrediction builds replayed all 20 PDFs through the **same local PDFium provider**. All 20 configured JSON results matched after normalizing only `pages[].blocks[].evidence[kind=external_table_structure].details.request_id`. That field is generated by a process-local atomic counter; concurrent table admission can change its numbering. Text, labels, confidences, geometry, reading order, table cells, other evidence and relations were compared without tolerance. Raw JSON was identical for 14 documents; the remaining six differed only in those request IDs. See [output-comparison.json](output-comparison.json).

Historical HTTP output was also captured and its PDF bytes verified, but it differed numerically from fresh local-provider output. That comparison did not isolate this change and was not used as an equivalence claim. The same-path replay establishes equivalence for this change; the cause of the historical server/local differences was not diagnosed in this optimization task. Historical provenance and differences remain in [baseline-output-provenance.json](baseline-output-provenance.json) and [historical-http-comparison.json](historical-http-comparison.json).

`ResultValidator` passed for all original and candidate results. The stricter existing corpus assertions retained the same failures in three documents:

- `2303.18223v16.pdf`, page 4: formula/header-table expectation (`survey_formula_and_tables`).
- `2609.13019v1.pdf`, page 19: a block extends left of the page.
- `Towards Conversational AI for Disease Management.pdf`, page 1: a block extends left of the page.

These failures existed before optimization and were not hidden or repaired by this performance change. The original `real_pdf_corpus` check remains unchanged and fails on the first of them. The new snapshot replay checks that both serialized output and the existing assertion status remain equal.

## Verification checks

- 368 regular Rust tests passed; 20 artifact-dependent tests were ignored in that general run and selected real-model checks were run separately.
- The final 47-page snapshot replay passed with the narrowly defined request-ID normalization.
- Strict Clippy passed for the affected native crates, tests and examples.
- WASM `docparse-web` compilation, the platform-boundary checker, formatting and whitespace checks passed.
- The complete OCR smoke test recognized 13 fixture lines successfully. The separate OCR batch-size matrix failed inside CoreML with the same runtime error on both the original and FastPrediction configurations. That pre-existing failure remains unresolved; no broad CoreML batch-size compatibility claim is made.
- No CUDA, OpenVINO, Metal-only or browser throughput measurements were performed.

## Reproduction

Build the CPU-only companion separately, then the CoreML benchmark:

```sh
rtk cargo build --release --locked -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker
rtk cargo build --release --locked -p docparse-server --features coreml --example pdfium_benchmark --bin docparse-server
rtk proxy cp target/release/docparse-pdfium-worker target/release/examples/docparse-pdfium-worker
rtk proxy env RUST_LOG=warn,ort=error target/release/examples/pdfium_benchmark docs/reports/2026-09-15-coreml-performance/benchmark.toml /Volumes/Yage/Downloads/docs /tmp/docparse-coreml-repeat.jsonl 2 2 5
```

Use a fresh output filename. The preserved local `target/coreml-performance/bin/baseline` and `fast` executables reproduce the measured configurations. Their sibling PDFium worker and runtime libraries must remain available.

A model probe can be repeated independently:

```sh
rtk proxy env DOCPARSE_COREML_PROBE_CASE=legacy-fast DOCPARSE_COREML_PROBE_OUTPUT=/tmp/docparse-coreml-probe.json cargo test --release --locked -p docparse-layout --features coreml --lib coreml_configuration_probe -- --ignored --nocapture
```

The real-output replay accepts the standard `DOCPARSE_E2E_PDF_DIR`, `DOCPARSE_E2E_MANIFEST`, `DOCPARSE_E2E_CONFIG`, `DOCPARSE_E2E_OUTPUT_DIR` variables plus `COREML_E2E_BASELINE_DIR` (API-format JSON files named by manifest logical ID). Local snapshots are retained under `target/coreml-performance/fast-local-reference`; PDF contents are not checked into this report.
