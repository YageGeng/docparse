# PDFium IPC pool CUDA benchmark, 2026-09-14

The latest `benchmark` branch was fast-forwarded from `3186058` to **`0decb48d47c8d299ed02ffdf87d752b417edd8f0`**. Every measured case uses the production server's **five-process PDFium IPC pool**, release CUDA inference, and the same **12 real PDFs / 368 pages** from `~/Downloads/docs`.

Two mode sweeps were completed: OCR disabled and OCR `missing_regions`, each at **2, 3, 4, and 5 documents in flight**, with one full-corpus trial per setting. All eight trials succeeded: **96 document executions / 2,944 pages**, excluding warmup. Disabling OCR completes the corpus in approximately **27 seconds**; enabling it takes **147–153 seconds**. PDFium opening contention no longer explains the OCR-enabled wall time.

## Measurement scope

The [server benchmark](../../../crates/server/examples/pdfium_benchmark.rs) constructs the same [PdfiumPool](../../../crates/server/src/pdfium_pool/mod.rs) used by the HTTP server and passes it to the shared parser benchmark. It does not use the previous local, same-process PDFium provider. The parent owns the CUDA models; five separate CPU-only companion processes scan and render PDF pages through IPC. The pool size stays at five throughout; only document admission concurrency changes.

Each case starts a fresh process, explicitly warms **all five PDFium workers** while holding their leases, and performs **two model warmup passes**. On-demand OCR and TSR sessions are explicitly exercised when enabled. Loading and warmup finish before `round_start`; telemetry summaries use only the measured interval. Input inspection warmed the OS cache, so this is a warm-cache benchmark.

Batch wall time includes parsing, compact configured JSON serialization, buffered writes, and `fsync` of temporary output files on the report filesystem. Individual times exclude waiting for admission into the document concurrency window. HTTP upload/download, database operations, and durable job queue time are **excluded**. Temporary parsed results are removed after measurement; filenames, counts, timings, and telemetry are retained.

Multi-file selection currently uses the frontend's bounded queue of separate single-file upload requests. Upload slots (`max_uploads = 100`), parsing concurrency (`worker_concurrency = 5`), and PDFium pool size (`pdfium_max_workers = 5`) are separate limits. This run measures the production parsing path, not multi-upload HTTP throughput or browser acceptance.

## Hardware and effective configuration

- Intel Core i9-14900HX, 24 cores / 32 logical CPUs, approximately 62.6 GiB RAM.
- NVIDIA RTX 4060 Laptop, 8,188 MiB, driver 615.71.09; Linux 7.2.4-1-cachyos.
- Release CUDA for layout, OCR, and TSR. All five pinned model artifacts passed checksum verification.
- Layout session pool: **1**. OCR maximum in flight: **2**; orientation classification enabled.
- TSR mode: **fallback**, maximum in flight: **8**, timeout: 60 seconds. The previous benchmark used **2**, so that comparison is not a controlled IPC-only A/B test.
- Page concurrency: **4**; render queue capacity: **2**; blocking task limit: **4**.
- Render DPI: **144**; maximum long edge: **2400**. JSON includes evidence and excludes diagnostics.
- OCR modes were selected with process-local `DOCPARSE_OCR__POLICY` overrides. The repository configuration was not edited.
- Benchmark logging: `warn,ort=error`. The restored service retains its original logging configuration.

[Input checksums](inputs.json) match the previous corpus for every PDF. [Mode configurations](ocr-on-config.json) record the effective settings; their envelope corresponds to concurrency 5, while each raw case records its own concurrency and configuration.

## Whole-corpus results

**Every row is the total for all 12 PDFs / 368 pages, not the time per PDF.**

| OCR policy | PDFs in flight | Batch wall (s) | Pages/s | Mean individual time (s) | Mean GPU busy | Peak GPU MiB | Mean family CPU | Peak family RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Disabled | 2 | 27.766 | 13.254 | 4.44 | 91.2% | 3217 | 232.5% | 2426 |
| Disabled | 3 | 27.379 | 13.441 | 6.48 | 94.3% | 3228 | 245.5% | 2561 |
| Disabled | 4 | 27.006 | 13.627 | 8.84 | 97.0% | 3197 | 263.5% | 2726 |
| Disabled | 5 | 26.968 | 13.646 | 9.02 | 96.8% | 3376 | 267.8% | 2649 |
| Missing regions | 2 | 147.331 | 2.498 | 24.03 | 50.6% | 6917 | 139.5% | 2957 |
| Missing regions | 3 | 149.760 | 2.457 | 36.47 | 47.0% | 6914 | 141.4% | 3005 |
| Missing regions | 4 | 153.054 | 2.404 | 49.28 | 47.8% | 7051 | 141.4% | 3178 |
| Missing regions | 5 | 150.498 | 2.445 | 60.02 | 48.8% | 6656 | 141.7% | 3315 |

With OCR disabled, moving from two to five documents reduces measured wall time by **2.9%**, while GPU activity is already 91–97%. With OCR enabled, more documents bring no observed throughput improvement; average individual execution time increases from **24.03** to **60.02 seconds**. One trial per condition does not establish statistical significance for small differences.

## Where time is spent

### PDFium opening and pool admission

| OCR policy | PDFs in flight | Longest PdfOpen (ms) | Longest PdfiumQueue (ms) | Cumulative OCR queue (s) | Longest OCR page (s) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Disabled | 2 | 1.157 | 0.007 | 0.00 | 0.00 |
| Disabled | 3 | 0.820 | 0.007 | 0.00 | 0.00 |
| Disabled | 4 | 1.039 | 0.005 | 0.00 | 0.00 |
| Disabled | 5 | 1.075 | 0.008 | 0.00 | 0.00 |
| Missing regions | 2 | 0.843 | 0.007 | 726.37 | 24.47 |
| Missing regions | 3 | 0.874 | 0.005 | 1209.91 | 27.13 |
| Missing regions | 4 | 0.846 | 0.028 | 1675.65 | 37.25 |
| Missing regions | 5 | 1.493 | 0.008 | 2218.48 | 35.10 |

Across all cases, `PdfOpen` is at most **1.493 ms**, and pool admission at most **0.028 ms**. The [previous report](../2026-09-14-cuda-performance.md) recorded a longest opening interval of **47.33 seconds** at four-document concurrency because a document held the process-wide PDFium lock. The present pool isolates that lock in each process, and this workload stays within its five available slots. This does not establish zero queueing when document concurrency exceeds pool size.

Previous OCR-enabled median batch times were **146.47 seconds at c2** and **149.16 seconds at c4**; the current single samples are **147.33** and **153.05 seconds**. Removing opening contention has not materially improved OCR-enabled corpus throughput in this run. Source changes, TSR admission 2 versus 8, sample counts, and uncontrolled clocks prevent attributing these small differences to IPC alone. There is no comparable earlier OCR-disabled measurement.

### OCR remains the main throughput limit

Each OCR-enabled case detects **111 pages**, recognizes **6,146 lines**, and classifies orientation **6,146 times**. OCR is on demand for missing regions, not forced across every native-text page. At c2:

| Stage | Calls | Cumulative interval (s) |
| --- | ---: | ---: |
| OcrRecognitionInference | 6146 | 121.757 |
| OcrOrientationInference | 6146 | 18.683 |
| OcrDetectionInference | 111 | 17.797 |
| OcrReadback | 12403 | 14.033 |
| OcrDecode | 6146 | 0.021 |
| LayoutInference | 368 | 33.340 |
| TsrInference | 12 | 3.705 |

The [OCR engine](../../../crates/ocr/src/engine.rs) awaits orientation and recognition for each selected line. Each model has a serialized session, and only two OCR pages are admitted at a time. Recognition runtime remains about **122–128 cumulative seconds** across the four cases; GPU activity averages only **47–51%**. The cumulative OCR queue grows from **726** to **2,218 seconds**, and a single OCR-page interval reaches **37.25 seconds** at c4. This supports investigating line batching and per-call synchronization/transfer overhead before adding more PDF concurrency.

These are overlapping elapsed intervals, including runtime CPU operators, transfers, and synchronization. They are not pure GPU-kernel measurements, and nested/concurrent stage totals **must not be summed into batch wall time**. `OcrQueue` includes both page admission and model session queues. No CUDA kernel profiler was collected, so the exact GPU idle cause is not established.

### OCR-disabled layout saturation and table latency

Without OCR, layout inference occupies **25.45–25.82 cumulative seconds** while the entire batch takes about 27 seconds. The single layout session and high sampled GPU activity leave little throughput gain from extra document admission.

TSR still executes **12 calls** per case. Without OCR, its cumulative runtime is **12.86–22.07 seconds**, with longest individual calls **6.40–6.90 seconds**. At c4, cumulative TSR queueing reaches **29.36 seconds** and the longest complete external-table stage **18.87 seconds**. With OCR enabled, TSR runtime totals only **2.94–3.76 seconds** per batch. This is consistent with different GPU contention/scheduling while layout stays busy, but a GPU trace would be needed to prove the cause. It remains a visible source of table/page latency even though increasing PDF concurrency hardly changes batch throughput. `tsr.max_in_flight = 8` does not create eight inference sessions.

Serialization plus file sync totals **0.65–0.75 cumulative seconds** per batch. Output size is approximately **237.7 MiB without OCR** and **239.9 MiB with OCR**; persistence is a small part of these measurements.

## Individual PDF timing

The following timings include parser-internal waits, but exclude pre-admission queueing. OCR page/line counts are from the enabled mode. All 96 detailed records, including c3 and c4, are in [documents.csv](documents.csv).

| PDF | Pages | OCR pages | OCR lines | Off c2 (s) | Off c5 (s) | On c2 (s) | On c5 (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `2303.18223v16.pdf` | 144 | 16 | 717 | 17.90 | 26.97 | 46.81 | 98.11 |
| `2403.01632v4.pdf` | 47 | 13 | 417 | 5.03 | 14.07 | 26.83 | 85.83 |
| `2410.05779v3.pdf` | 16 | 5 | 291 | 2.45 | 3.40 | 23.67 | 37.85 |
| `2412.05210v1.pdf` | 12 | 6 | 439 | 2.15 | 3.27 | 19.14 | 41.21 |
| `2603.01919v2.pdf` | 23 | 10 | 1101 | 7.92 | 16.88 | 31.28 | 71.24 |
| `2604.18583v1.pdf` | 15 | 8 | 100 | 2.61 | 5.58 | 12.43 | 43.16 |
| `2604.18584v1.pdf` | 32 | 13 | 882 | 4.34 | 10.16 | 39.05 | 90.63 |
| `2604.21959v1.pdf` | 3 | 2 | 87 | 0.57 | 1.09 | 6.53 | 19.68 |
| `2606.17056v1.pdf` | 25 | 15 | 831 | 2.80 | 7.14 | 37.93 | 69.49 |
| `2609.04180v1.pdf` | 23 | 9 | 755 | 3.23 | 5.82 | 23.59 | 62.51 |
| `2609.04184v1.pdf` | 8 | 5 | 239 | 1.92 | 4.33 | 14.33 | 49.47 |
| `2609.04203v1.pdf` | 20 | 9 | 287 | 2.30 | 9.52 | 6.75 | 51.02 |

## Validity and limitations

- All eight processes exited successfully with `valid = true`, zero failed documents, zero page errors, and zero model-unavailable/runtime-failure/timeout degradation warnings.
- Every case observed the benchmark parent and five PDFium children. Workers were shut down after each case; no additional process identities appeared in telemetry.
- Pages, every stage call count, and warning counts are stable across concurrency within each OCR mode. OCR-disabled outputs have identical byte sizes. OCR-enabled per-document byte sizes vary by at most **7 bytes** across cases; the parsed content was not retained for bytewise comparison, so no output-equality or accuracy claim is made.
- Two PDFs, `2603.01919v2.pdf` and `2604.18584v1.pdf`, each retain one `TableTextAssignmentFailed` and one `TableStructureUnavailable` warning in both modes. Existing content/layout and partial OCR overlap warnings remain recorded in the raw metrics.
- GPU and process telemetry is sampled approximately once per second. GPU utilization and memory include desktop activity. CPU 100% represents one logical CPU; family CPU sums the parent and five children. Family RSS is summed resident memory and may count shared mappings more than once; it is not unique physical memory.
- Family samples are grouped by each parent row and the following child rows, then restricted to `round_start` through `summary`. GPU timestamps are interpreted in the host's Asia/Shanghai timezone. Peaks are sampled, not continuously recorded.
- Conditions run in order: disabled c2 through c5, then enabled c2 through c5. GPU peak temperature ranges from 72 to 86 C; clocks, power, and CPU affinity were not fixed. These are single warmed samples per condition.

For this workload, **two PDFs in flight is a reasonable baseline with OCR enabled**. Without OCR, **two to four** already delivers near-maximum observed throughput; five yields little extra. Prioritize OCR line batching/runtime analysis, and separately investigate TSR latency under sustained layout work. Existing deployment settings were restored, not retuned from a single trial.

## Artifacts and reproduction

- [Summary CSV](summary.csv), [summary JSON](summary.json), [metadata](metadata.json)
- [Per-document records](documents.csv), [all stage records](stages.csv), [input checksums](inputs.json)
- [OCR-disabled configuration](ocr-off-config.json), [OCR-enabled configuration](ocr-on-config.json)
- `ocr-off/` and `ocr-on/` retain `p5-c2` through `p5-c5` metrics JSONL, GPU CSV, process CSV, and case exit records. Runner stderr logs remain local under the repository log ignore rule.

Build the CPU-only companion separately and place it beside the benchmark executable:

```sh
rtk cargo build --release --locked -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker
rtk cargo build --release --locked -p docparse-server --features cuda --example pdfium_benchmark --bin docparse-server
rtk proxy cp target/release/docparse-pdfium-worker target/release/examples/docparse-pdfium-worker
rtk proxy env DOCPARSE_OCR__POLICY=disabled UV_CACHE_DIR=/tmp/docparse-perf-uv-cache \
  uv run --locked --group dev scripts/benchmark.py \
  --config docparse.toml --pdf-dir "$HOME/Downloads/docs" \
  --output-dir /tmp/docparse-ipc-ocr-off \
  --concurrency 2 3 4 5 --pdfium-processes 5 --repeats 1
rtk proxy env DOCPARSE_OCR__POLICY=missing_regions UV_CACHE_DIR=/tmp/docparse-perf-uv-cache \
  uv run --locked --group dev scripts/benchmark.py \
  --config docparse.toml --pdf-dir "$HOME/Downloads/docs" \
  --output-dir /tmp/docparse-ipc-ocr-on \
  --concurrency 2 3 4 5 --pdfium-processes 5 --repeats 1
```

Use new output directories. Stop other model-serving processes before reproducing the measurements. The original idle HTTP service was gracefully stopped under the user's existing authorization, then restored with its original launch arguments and environment using the updated release binaries. Readiness succeeded after restoration. Its file configuration remains OCR **disabled**, parsing concurrency **5**, and PDFium workers **5**. No parser code or deployment configuration was edited during this rerun.
