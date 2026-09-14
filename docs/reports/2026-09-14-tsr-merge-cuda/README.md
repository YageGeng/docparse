# TSR cell-detection merge and CUDA corpus evaluation

`origin/feat/tsr-cell-detection` was fast-forwarded into local `benchmark`, moving HEAD from `0decb48` to **`2229369`**. The existing uncommitted OCR batching and heuristic CUDA changes were preserved. The only stash-application conflict was an overlapping insertion in `crates/config/tests/loading.rs`; both test groups remain present. No new merge commit was needed, and the branch was not pushed during this task.

The combined code completed **12 valid measured cases / 144 document executions / 4,416 pages**. Each case uses all **12 PDFs / 368 pages** from `~/Downloads/docs`, with unchanged hashes. New cell detection has a small measured elapsed-time cost under `fallback`, but increases peak GPU memory. More PDF concurrency does not materially improve OCR-enabled throughput on this corpus.

## Review and integration

Reviewed the new model/configuration contracts, artifact verification, detector preprocessing, native/browser session ownership, independent-cell matching, structure decoding, and benchmark warmup. The default combination is **SLANet+ plus wireless RT-DETR cell detection**. SLANeXt variants remain explicit options; this CUDA evaluation does not benchmark those alternatives.

The branch changes the main configuration to **`tsr_only`** while library defaults remain rules-first `fallback`. The old `external_only` spelling is intentionally rejected by the branch's configuration test. Browser consumers now need cell-model artifacts when cell detection is enabled; the SDK, example and binding inputs were updated upstream.

One existing benchmark bug was found during review: its interruption cleanup killed PDFium workers before their still-running supervisor. The supervisor could create replacements after the recorded descendant snapshot, leaving orphan processes. Four verified workers from the earlier interrupted OCR experiments were still resident. The [runner](../../../scripts/benchmark.py) now stops the parent before stopping its workers. A [regression](../../../crates/core/tests/python/benchmark_test.py) reproduces a respawning supervisor, failed before the fix and passes afterward. The four old workers were removed after checking their executable, creation time and benchmark environment; current HTTP service workers were left alone until the planned graceful shutdown.

## Method

- Intel Core i9-14900HX, approximately 62.6 GiB RAM; RTX 4060 Laptop, 8,188 MiB, NVIDIA driver 615.71.09.
- Native **release CUDA**, one shared model set and the production **five-process PDFium IPC pool**.
- Native OCR line batch **16**, OCR page limit **2**, layout session pool **1**, page concurrency **4**, render queue **2**.
- Render DPI **144**, maximum long edge **2400**; output includes evidence and excludes diagnostics.
- Main comparison: TSR **fallback**, maximum in flight **8**. OCR is either disabled or `missing_regions`, with orientation classification enabled.
- Every case starts a fresh process, explicitly warms all five PDFium workers, and warms enabled model sessions twice. The merged benchmark explicitly exercises the cell detector using the real table fixture. The retained old executable used its previous warmup fixture. The file cache was warm; new corpus-specific tensor shapes can still incur first-use runtime work.
- Each row is one measured full-corpus trial. Wall time includes parsing, compact JSON serialization, buffered temporary-file output and `fsync`. Model loading/warmup, HTTP transfer, database operations and durable queue admission are excluded.
- The before-merge executable includes the prior OCR optimization and heuristic CUDA strategy. Its credential-free parser configuration and binary hash were captured separately so the new TSR configuration fields do not change the reference workload.

Six required model sets were checksum-verified, including the newly downloaded wireless cell detector. No active game workload appeared in the recorded per-process GPU samples.

## Fallback: OCR off/on and document concurrency

**Every time below is for all 12 PDFs / 368 pages, not the average per PDF.**

| PDFs in flight | OCR disabled (s) | OCR missing regions (s) | Mean GPU busy, off / on |
| ---: | ---: | ---: | ---: |
| 2 | 28.06 | 123.75 | 89.5% / 57.7% |
| 3 | 27.41 | 122.00 | 94.0% / 57.2% |
| 4 | 26.60 | 123.70 | 96.3% / 53.5% |
| 5 | 26.64 | 122.48 | 97.2% / 56.3% |

At the matched concurrency of two:

| Mode | Before merge (s) | After merge (s) | Observed change |
| --- | ---: | ---: | ---: |
| OCR disabled | 27.17 | 28.06 | +3.3% |
| OCR enabled | 120.41 | 123.75 | +2.8% |

All fallback trials perform **12 structure-model calls** for **six external table requests**. After merging, each additionally performs **six cell-detector calls**. OCR-enabled cases retain **111 OCR pages, 6,146 decoded lines and 1,798 recognition-model calls**. This comparison does not reduce OCR coverage to obtain its timing result.

At concurrency two, independent cell inference totals **0.372 seconds** with OCR disabled and **0.260 seconds** with OCR enabled. Corresponding cell preprocessing totals **0.059** and **0.054 seconds**. Table matching/filling remains small: cumulative **0.010 seconds** with OCR enabled.

The largest operational difference is GPU memory. At concurrency two with OCR enabled, the sampled peak rises from **6626 MiB (6.47 GiB)** to **7342 MiB (7.17 GiB)**. The added model and runtime allocations therefore consume roughly **0.70 GiB** in this measurement.

Increasing document concurrency mostly increases individual document waiting time. OCR-enabled corpus wall time remains around **122–124 seconds** from concurrency two through five. No PDFium opening stall or worker replacement was observed; all cases sampled one parent and five PDFium children.

## Additional coverage: tsr_only

The branch's main-file setting applies TSR to every layout table region instead of invoking it only when rules fail. These rows are reported separately because their model coverage is much greater:

| OCR policy | Fallback, c2 (s) | TSR-only, c2 (s) | Structure calls | Cell-detector calls |
| --- | ---: | ---: | ---: | ---: |
| Disabled | 28.06 | 36.79 | 133 | 121 |
| Missing regions | 123.75 | 128.64 | 133 | 121 |

The 133 structure calls include segmented retries. Cumulative cell inference is **3.75 seconds** without OCR and **4.69 seconds** with OCR. This additional work is observable and successful; it must not be compared with six-call fallback timing as if the workloads were identical.

## Validity and limits

All 12 measured cases exited successfully with `valid = true`, zero failed documents, zero page errors and zero model-unavailable/runtime-failure/timeout degradation. Page totals and the selected OCR work remain consistent across the matrix.

The pre-existing warnings remain in both policies: `2603.01919v2.pdf` and `2604.18584v1.pdf` each report one `TableTextAssignmentFailed` and one `TableStructureUnavailable`. Warning counts did not improve in this corpus. Successful model execution and unchanged warning counts do not establish better semantic table accuracy. The upstream branch's separately reviewed table fixtures are described in [its model comparison](../2026-09-14-tsr-model-comparison/report.md); those use a different corpus and CPU hardware.

Stage values are overlapping elapsed intervals. Do not sum them into wall time, and do not interpret ONNX Runtime intervals as pure GPU kernel time. GPU/process sampling is approximately once per second and is restricted to the recorded measured window. GPU busy and memory include desktop applications. Family CPU sums parent and children, with 100% meaning one logical CPU; family RSS may double-count shared mappings. Clocks, power and CPU affinity were not pinned. These single samples support the lack of a large concurrency benefit, not a statistically precise claim about a 3% difference.

## Verification and restored service

- **43** config/OCR/TSR tests and **283** core tests passed; artifact-dependent ignored tests are not counted as passes.
- **Nine** model-downloader tests and the real-process interruption regression passed.
- Clippy and platform-boundary validation passed.
- The release WASM package was rebuilt and optimized, with **145 imports validated**. SDK and example TypeScript checks passed after refreshing generated declarations. No headed browser acceptance run was performed in this task.
- All six required model sets passed integrity checks and were exercised when enabled in the CUDA benchmark.
- After measurement, a host process inventory confirmed **zero remaining native docparse benchmark/worker processes**. The original HTTP service was then restored using its saved launch arguments/environment and updated release binaries; its readiness endpoint returned `ready`.

The merged file configuration is `tsr_only` + SLANet+ + wireless cell detection; OCR remains disabled in the file and uses batch 16 when enabled. Server parsing concurrency and PDFium workers remain five. Benchmark overrides were process-local.

## Artifacts and reproduction

- [Summary CSV](summary.csv), [summary JSON](summary.json), [per-document results](documents.csv), [all stage rows](stages.csv)
- [Input hashes](inputs.json), [metadata](metadata.json), [case exit records](cases.json), [verified orphan cleanup](orphan-workers.json)
- Each case directory retains metrics JSONL and GPU/process CSV. Runner stderr and per-process GPU logs remain local under the log ignore rule.

Build the CPU-only companion separately from the CUDA server, then place it beside the benchmark executable:

```sh
rtk cargo build --release --locked -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker
rtk cargo build --release --locked -p docparse-server --features cuda --example pdfium_benchmark --bin docparse-server
rtk proxy cp target/release/docparse-pdfium-worker target/release/examples/docparse-pdfium-worker
rtk proxy env DOCPARSE_TSR__MODE=fallback DOCPARSE_OCR__POLICY=missing_regions \
  uv run --locked --group dev scripts/benchmark.py \
  --config docparse.toml --pdf-dir "$HOME/Downloads/docs" \
  --output-dir /tmp/docparse-tsr-merged-ocr-on \
  --concurrency 2 3 4 5 --pdfium-processes 5 --repeats 1
```

Use new output directories. Set `DOCPARSE_OCR__POLICY=disabled` for the other sweep, and `DOCPARSE_TSR__MODE=tsr_only` for the supplemental coverage comparison. Stop other model instances and GPU-heavy workloads before measuring.
