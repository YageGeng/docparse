# Native OCR batching optimization — 2026-09-15

The retained changes reduce the warmed 12-PDF, 368-page corpus from **121.931s to 110.285s**
at document concurrency 2: **9.6% less elapsed time**.
Concurrency 5 takes **110.492s**. These are complete-corpus elapsed times, not averages per PDF.

## Retained changes

- Fill fixed-shape OCR orientation batches across recognition-width boundaries.
- Retain equal-width recognition groups, original padding, line identities and a bounded crop window.
- Reduce CTC probabilities with eight independent standard-library lanes, validating every value and retaining the first exact maximum.
- Reuse the existing native session ownership, cancellation and shutdown behavior. No new dispatcher, model-session copies or dependencies remain.

The comparison baseline already includes OCR batch 16 and heuristic cuDNN convolution selection from the previous iteration.
This report measures the additional orientation/CTC changes, not the cumulative gain over the original scalar OCR implementation.

The local oar-ocr implementation informed the separation of orientation and recognition batching
(`src/oarocr/ocr.rs::classify_line_orientations`) and the compact, vectorizable argmax reduction
(`oar-ocr-core/src/processors/simd.rs::argmax_simd`). DocParse keeps strict probability validation and uses standard Rust arrays.

## Method

- RTX 4060 Laptop GPU, 8 GiB; native release build with the unified CUDA feature.
- All 12 real PDFs in `~/Downloads/docs`, 368 pages; input hashes are in `inputs.json`.
- Five production PDFium IPC workers; page concurrency 4; OCR `missing_regions`, max-in-flight 2, line batch 16.
- TSR `tsr_only`, SLANet+ and wireless RT-DETR cells enabled.
- Every run warms all five PDFium workers, then runs two warmup rounds for the enabled models before timing. Models are shared within each run.
- One complete-corpus repeat per case. Small differences between concurrency levels or TSR experiments are not evidence of a stable speedup.
- Timing includes parsing and compact JSON writing/fsync; model startup, warmup, HTTP upload and database work are excluded.
- The HTTP server was stopped during measurement. Desktop applications remained running.
- GPU figures are whole-device observations sampled at 1 Hz during the measured corpus interval; they include desktop activity.
- `builds.json` records hashes of the preserved baseline, temporary experiment and final binaries; `before.toml` is the credential-free benchmark configuration.

## Results

| Case | Corpus seconds | Pages/s | Mean GPU use | Peak GPU MiB |
| --- | ---: | ---: | ---: | ---: |
| before-ocr-off/c2 | 36.468 | 10.091 | 89.6% | 3396 |
| before-ocr-on/c2 | 121.931 | 3.018 | 60.7% | 7306 |
| final-ocr-on/c2 | 110.285 | 3.337 | 62.3% | 7255 |
| final-ocr-on/c5 | 110.492 | 3.331 | 62.7% | 7277 |
| ocr-optimized/c2 | 110.823 | 3.321 | 63.5% | 7272 |
| tsr-batch2-experiment/c2 | 36.113 | 10.190 | 90.1% | 3623 |
| tsr4-cell2-experiment/c2 | 36.218 | 10.161 | 85.7% | 4464 |

`ocr-optimized` used the temporary queue implementation with all extra model batch limits set to one.
`final-ocr-on` restores the original layout/TSR scheduling and contains only the retained OCR changes.
All listed runs parsed 368 pages with zero failed or degraded documents.
See `documents.csv` for individual PDF latency and `stages.csv` for stage aggregates.

## OCR observations, final concurrency 2

| Stage | Before calls | Final calls | Before seconds | Final seconds |
| --- | ---: | ---: | ---: | ---: |
| OcrDetectionInference | 111 | 111 | 17.519 | 18.474 |
| OcrOrientationInference | 1798 | 436 | 31.049 | 14.835 |
| OcrRecognitionInference | 1798 | 1813 | 98.735 | 99.807 |
| OcrReadback | 3707 | 2360 | 13.318 | 2.174 |
| OcrDecode | 6146 | 6146 | 0.018 | 0.017 |

Stage totals are accumulated intervals across overlapping documents and models, not disjoint pieces of corpus time.
Readback includes output synchronization and CPU probability validation/reduction; it is not a pure device-transfer measurement.
Both runs decoded 6,146 selected text lines. Recognition remains the largest OCR inference cost.
Orientation windows occasionally split an equal-width recognition group, adding a small number of recognition calls while greatly reducing orientation calls.

A separate release CPU probe measured approximately 92.9ms for the original scalar scan and 19.9ms for the lane reduction
(100 rows × 18,710 classes × 50 repetitions). This synthetic reduction result is not a whole-parser speedup.

## Experiments not retained

- Layout batch 2 executed successfully at the tensor level, but mixed real fixtures changed accepted detections (one result changed from six boxes to seven).
  Absolute model ranks also changed. The experiment failed equivalence checks and was removed instead of relaxing the production contract.
- TSR batch 2 reduced physical structure calls from 133 to 73, but elapsed time changed only from 36.468s to 36.113s with OCR disabled.
- TSR batch 4 plus cell batch 2 reduced physical structure/cell calls to 60/102, but elapsed time was still 36.218s.
  Fewer ONNX calls alone did not provide a material corpus speedup. The extra queues, configuration fields and batching code were removed.
- The temporary experiment's per-request TSR inference timers overlap within a physical batch. Its `TsrBatchInference` and
  `TableCellBatchInference` stages count physical calls once; they are not part of the final API.
- Cross-page OCR pooling and detector batching were not added. The current 8 GiB workload already has limited memory headroom;
  those changes need a separate measured accuracy/memory comparison.

## Validation

- 367 Rust unit/integration tests passed; ignored real-model and corpus checks were run separately.
- Real CUDA OCR tests passed, including exact fixture text/quad agreement for batch 1/4/8/16 and reduced orientation calls.
- Full real-PDF validation passed for all 12 documents: 368 pages, zero parse errors, zero invariant failures.
  Existing overlap/table-binding warnings remain visible in `validation-summary.json`.
- Probability tests cover vector lanes, tails, strided arrays, exact ties, signed zero and invalid probabilities.
- Workspace Clippy with tests/examples, formatting, WASM compilation and the platform-boundary checker passed.
- Web SDK TypeScript checking and seven Python tests passed.
- The HTTP server was restored after measurement; readiness returned HTTP 200 and all five PDFium workers were present (`service-restored.json`).

Corpus validation checks source binding, geometry, ordering invariants and serialization. It is not a byte-for-byte comparison
against a newly regenerated pre-change corpus. GPU batching can still change marginal OCR predictions on other inputs.
Canonical validation metadata is retained in `validation-canonical-hashes.json`; full local JSON results are under
`target/docparse-model-batching-validation/documents`.

## Reproduce the final measurement

```sh
rtk cargo build --release --locked -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker
rtk cargo build --release --locked -p docparse-server --features cuda --example pdfium_benchmark
rtk proxy cp target/release/docparse-pdfium-worker target/release/examples/docparse-pdfium-worker
rtk proxy env DOCPARSE_TSR__MODE=tsr_only DOCPARSE_OCR__POLICY=missing_regions UV_CACHE_DIR=/tmp/docparse-perf-uv-cache uv run --locked --group dev scripts/benchmark.py --config docs/reports/2026-09-15-model-batching/before.toml --pdf-dir /home/isbest/Downloads/docs --output-dir /tmp/docparse-final-repeat --concurrency 2 5 --pdfium-processes 5 --repeats 1
```

Use a fresh output directory for each invocation. Warmup covers the fixed model fixtures; previously unseen recognition widths can still incur first-use work.
