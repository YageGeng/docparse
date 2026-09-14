# OCR batching performance and output validation, 2026-09-14

The selected implementation completes the **12-PDF / 368-page corpus in 129.99 seconds**, compared with **159.54 seconds** for the preserved pre-change executable: **18.5% less elapsed time**, or **22.7% more pages per second** in this measurement. The selected settings are native `ocr.batch_size = 16` and heuristic cuDNN convolution selection.

Full output comparison found identical structure, ordering and geometry, but **four OCR lines on one dense scan-collage page differ**. This is not a byte-identical optimization; the observed differences and their isolation test are documented below.

## Implementation

- [OCR scheduling](../../../crates/ocr/src/engine.rs) filters requested regions before grouping, pools only crop metadata, and batches lines with exactly matching recognition tensor widths. At most one current batch of crop pixels is retained per admitted OCR page. Results are restored to original detection order.
- [Preprocessing](../../../crates/ocr/src/preprocess.rs) constructs contiguous orientation and recognition tensors directly, preserving the original resize, channel order, normalization and per-line padding. Exact-width groups avoid adding padding solely to fill a batch.
- [Model output handling](../../../crates/ocr/src/model.rs) validates output batch correspondence and reduces each recognition member independently. Full vocabulary tensors are borrowed rather than copied into another owned tensor. Confidence thresholds, probability validation and CTC repetition rules remain in force.
- [Configuration](../../../crates/config/src/config.rs) adds `ocr.batch_size`, validated from 1 through 32, with default 16. It is independent of `ocr.max_in_flight`, which still limits overlapping pages to two in this configuration. Batch one preserves the previous line scheduling order.
- [CUDA initialization](../../../crates/layout/src/wasm_compat/backend.rs) uses `ConvAlgorithmSearch::Heuristic` in the shared CUDA session builder. Other providers retain their existing settings. Browser execution remains capped at one line per call through the existing platform boundary.

The approach was informed by local oar-ocr commit `7feb044`, particularly its crop grouping, original-index mapping and borrowed-output reduction. Its fixed `Default` convolution selection was tested and rejected on this machine. The selected heuristic mode avoids exhaustive benchmarking of convolution candidates while allowing cuDNN to select an implementation. The distinctions between these modes are documented by [ONNX Runtime](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html#cudnn_conv_algo_search).

## Conditions and timing scope

- Base revision: `0decb48d47c8d299ed02ffdf87d752b417edd8f0`, with the changes described above. Binary hashes are recorded in [metadata](metadata.json).
- Intel Core i9-14900HX, 24 cores / 32 logical CPUs; approximately 62.6 GiB RAM. NVIDIA RTX 4060 Laptop, 8,188 MiB, driver 615.71.09.
- Every timed case uses **all 12 real PDFs in `~/Downloads/docs`**, totaling **368 pages**. [Input hashes](inputs.json) identify the corpus.
- Release CUDA, the production server's PDFium IPC provider, **five PDFium worker processes**, and **two documents in flight**. The parent owns one shared set of model sessions.
- All five PDFium workers are explicitly warmed, followed by two model warmup passes. On-demand OCR and TSR are exercised. Sessions are warm, but previously unseen corpus-specific widths/batch shapes can still incur first-use runtime work; this is not a fully warmed shape-cache benchmark.
- OCR policy `missing_regions`; orientation enabled; OCR pages in flight 2; TSR `fallback`, maximum in flight 8; layout session pool 1; page concurrency 4; render queue 2; DPI 144; maximum image edge 2400.
- Wall time includes parsing, compact JSON serialization, buffered temporary-file output and `fsync`. Loading/warmup, HTTP transfers, database work and durable queue admission are excluded.
- One full-corpus trial per condition. There are nine accepted measured cases, totaling **108 document executions / 3,312 pages**, plus separate correctness runs and excluded experiments.

## Matched comparison

The pre-change executable took **159.54 seconds**, with 6,146 recognition calls. Every table entry below is a complete **368-page batch wall time**, not a per-document average.

| Maximum OCR line batch | Exhaustive selection (s) | Heuristic selection (s) | Recognition calls |
| ---: | ---: | ---: | ---: |
| 1 | 155.43 | 144.93 | 6146 |
| 4 | 155.46 | 144.14 | 2634 |
| 8 | 142.39 | 140.69 | 2079 |
| 16 | 133.84 | 129.99 | 1798 |

Every accepted case still performed OCR on **111 pages / 6,146 lines**, layout on 368 pages, and **12 TSR model calls**. No documents failed, no page errors occurred, and no model-unavailable/runtime-failure/timeout degradation was recorded.

Batch 16 reduces recognition calls by **70.7%**. The realized mean is **3.42 lines per recognition call**, because exact widths and page boundaries prevent every batch from reaching 16. Wider grouping could improve fill, but would change padding and requires separate output validation.

Most of the observed gain at the chosen size comes from batching. Heuristic versus exhaustive selection at batch 16 differs by only **3.86 seconds / 2.9%**; one sample cannot establish the statistical significance of that isolated difference. The earlier fixed-`Default` experiment was a substantial regression and is not part of the shipped code.

| Resource / stage | Pre-change | Selected implementation |
| --- | ---: | ---: |
| Pages/s | 2.307 | 2.831 |
| Mean individual execution, excluding admission (s) | 26.15 | 21.19 |
| Mean sampled GPU busy | 57.4% | 61.6% |
| Peak sampled GPU memory (MiB) | 6548 | 6591 |
| Mean process-family CPU | 137.5% | 150.5% |
| Peak summed family RSS (MiB) | 3008 | 3069 |
| Cumulative recognition runtime (s) | 133.84 | 107.41 |
| Cumulative OCR output readback/reduction (s) | 14.25 | 14.06 |

Stage intervals overlap and must not be summed into corpus wall time. Recognition runtime includes ONNX Runtime CPU work, transfers and synchronization, not solely GPU kernels. Output reduction remains approximately 14 cumulative seconds and was not replaced with a new SIMD/parallel framework in this change.

## Output validation

The existing production-model canonical E2E test was run against a temporary manifest covering all 12 PDFs; the repository's default manifest contains only five of these files. Both before and after runs passed document invariants and serialization round trips. Full outputs remain under `target/docparse-batch-validation/` and are not added to Git.

Every canonical field was compared. Results:

- **368 pages**, 5,903 blocks, 41,444 lines and 191,439 text items in both runs.
- **Zero structural mismatches** and **zero numeric differences outside confidence fields**: page/block/item geometry and ordering are unchanged.
- **Four unique OCR lines changed**, all in `2604.18584v1.pdf`, page 26. Their repeated representations produce nine differing string fields: four line strings, four raw-text strings and one aggregated block string.
- **1,290 confidence fields differ**, with maximum absolute difference **0.01150195**, approximately **1.15 percentage points**.
- An additional run of the affected PDF with **batch 16 and the original exhaustive strategy** produced the same four changed line strings as batch 16 with heuristic selection. These changes therefore also occur from batching without changing the convolution selection policy.

Page 26 was visually inspected: it is a collage of small scanned mathematics pages. The four affected OCR boxes are approximately 7–12 pixels high at the configured render DPI. Both versions contain recognition errors on these tiny images. The changed strings include `ae` to `are` and `Ror` to `for`, but no ground-truth accuracy improvement or degradation is asserted from these examples alone. No confidence threshold was relaxed and no extra native-text/OCR coverage was removed to obtain the timing result.

Detailed evidence: [field comparison](output-comparison.json), [four changed lines and isolation results](text-variation.json). Existing table warnings remain: two documents each report `TableTextAssignmentFailed` and `TableStructureUnavailable`; these are separate from runtime model failures.

## Verification and operational state

- 34 config/OCR unit and integration tests passed.
- Two real CUDA model tests passed, including text/geometry/order comparison at batch 1, 4, 8 and 16 on the printed fixture and reduced recognition invocation counts.
- Complete 12-PDF E2E invariants and JSON round trips passed before and after; the affected 32-page PDF also passed the batch-only isolation run.
- Workspace Clippy, platform-boundary validation, WASM compilation and the wasm-web TypeScript check passed. Browser runtime inference remains batch one; this task did not repeat a headed browser acceptance run.
- The idle HTTP service was stopped under the user's existing authorization and restored using its original arguments/environment with the updated release binary. Readiness succeeded. The file configuration keeps OCR disabled, server parsing concurrency 5 and PDFium workers 5; the new default line batch is 16 when OCR is enabled.

## Excluded experiments and limitations

An initial batch-one trial took 330.50 seconds while Vintagestory consumed substantial GPU time. Its results and the interrupted batch-four attempt are retained separately and excluded from the performance comparison. The user ended the game workload before the accepted sweep; per-process GPU samples then confirmed its exit. The fixed-`Default` convolution trial was interrupted after neither of its first two PDFs had completed at the 122.85-second checkpoint, without a competing high GPU workload.

GPU/process telemetry is sampled approximately once per second. GPU utilization and memory include desktop applications; clocks, power and CPU affinity were not pinned. CPU 100% means one logical CPU. Family RSS sums the parent and five PDFium processes and can double-count shared mappings. Means and peaks are restricted to the recorded measured interval. No Nsight kernel trace was collected, and these measurements do not establish universal speedups across models, corpora, GPUs or fully warmed shape caches.

## Artifacts and reproduction

- [Summary CSV](summary.csv), [summary JSON](summary.json), [per-document data](documents.csv), [all stage rows](stages.csv)
- `clean/`: preserved pre-change executable plus batch-only trials using exhaustive selection.
- `heuristic/`: selected CUDA strategy at batch 1/4/8/16.
- Each case retains metrics JSONL and GPU/process CSV. Stderr and per-process GPU logs remain local under the repository's log ignore rule.

Build the release server benchmark and CPU-only PDFium companion as documented in [the prior IPC report](../2026-09-14-pdfium-ipc-cuda/README.md). To reproduce the selected setting using a new output directory:

```sh
rtk proxy env DOCPARSE_OCR__POLICY=missing_regions DOCPARSE_OCR__BATCH_SIZE=16 \
  uv run --locked --group dev scripts/benchmark.py \
  --config docparse.toml --pdf-dir "$HOME/Downloads/docs" \
  --output-dir /tmp/docparse-ocr-batch16 \
  --concurrency 2 --pdfium-processes 5 --repeats 1
```

Release other model instances and GPU-heavy applications before measuring. The final parser code uses heuristic selection; reproducing exhaustive/fixed experiments requires the recorded earlier binaries or changing that builder setting explicitly.
