# CoreML release HTTP corpus run — 2026-09-15

## Result

The production HTTP server completed **20 real PDFs / 694 pages in 369.649 seconds (6m 9.65s), or 1.877 pages/s**. All 20 durable jobs succeeded on their first attempt. Every result was downloaded over HTTP and checked for complete, ordered pages, empty page-error lists, and one layout inference per page.

| Measurement | Observed value |
| --- | ---: |
| Full HTTP batch, including uploads, queueing, status polling and result downloads | 369.649 s |
| Corpus throughput | 1.877 pages/s |
| Initial upload batch, 20 PDFs / 124,336,812 bytes | 0.760 s |
| Successful / failed / retried jobs | 20 / 0 / 0 |
| Median individual client latency, including queueing | 230.863 s |
| Median worker attempt duration | 71.745 s |
| Downloaded result JSON | 371.30 MiB |
| Sampled server + PDFium family peak RSS | 4.687 GiB |

Fresh-process startup through HTTP readiness took **9.091s**. Two sequential HTTP warmups using the 23-page `2603.01919v2.pdf` took **21.338s** and **20.232s**. Startup and warmups are excluded from the corpus time. CoreML and filesystem caches were not flushed, so startup is not a cache-cold measurement.

See [summary.json](summary.json), [documents.csv](documents.csv), and [corpus.json](corpus.json) for the measured values, per-file timings, job IDs, transport measurements and final HTTP snapshots.

## Build and deployment

- Source commit: `366ebdc0674397c9e2be269d721cc440910ed528` (`perf(inference): tune CoreML and benchmark WebGPU layouts`). The tracked checkout was clean during the run.
- Both companion and server were freshly checked with `--release --locked`; the server build explicitly enabled `--features coreml`. Binary SHA-256 values are retained in [metadata.json](metadata.json).
- Apple M4, 10 logical CPUs, 16 GiB memory, macOS 26.6.2, AC power. No thermal or performance warning was reported by `pmset` before or after the run. Other desktop applications remained active.
- Production `docparse-server --role all`, at `http://127.0.0.1:8080/api/v1/docparse`, using the existing `docparse.toml`, PostgreSQL and `data/docparse` storage. The configured database resolved to loopback.
- Configuration was retained: document concurrency **5**, PDFium processes **5**, page concurrency **4**, layout session pool **1**, render queue **2**, blocking limit **4**, upload limit **100**.
- OCR **disabled**. PP-DocLayoutV3, SLANet+ and wireless RT-DETR cell detection enabled; TSR `max_in_flight=8`, timeout 60s. Rendering used DPI 144 and maximum long edge 2400. Evidence was included and diagnostics hidden by the configured JSON renderer.
- Startup logs confirmed **three CoreML registrations**, each with `ComputeUnits::All` and `FastPrediction`. CoreML partitioning remains partial: the layout graph reported 757 of 1,400 nodes supported across 123 partitions. This is real CoreML execution, not a claim that every operator runs on GPU or ANE. See [server-evidence.log](server-evidence.log).

## HTTP method and timing boundaries

All 20 inputs directly inside `/Volumes/Yage/Downloads/docs` matched the prior corpus's SHA-256, size and expected page count before measurement; see [inputs.json](inputs.json). An API-only preflight confirmed zero queued or running jobs, alongside the 20 existing successful jobs. No other parser server was running.

Each warmup and measured upload used a fresh UUID `Idempotency-Key`, forcing a new durable parse job. Existing content-addressed PDF objects could be reused after byte verification; cached filesystem contents were not cleared. Results were never reused. The measured batch submitted all 20 files with four upload clients, then queried the real job-list API approximately once a second and fetched successful results with two download clients. The server enforced its own five-document processing limit.

The headline monotonic interval starts before the first upload client and ends after every complete result download. It includes HTTP transport, content verification/storage, database admission, durable queueing, parsing, result serialization/publication, polling delay, client orchestration and local download writes. Post-run JSON validation and hashing are excluded. The downloaded files were not explicitly `fsync`ed by the client; server publication used its normal synchronized storage path.

`duration_ms` is the server's persisted worker-attempt duration: parsing plus result publication, excluding upload, queueing and the final database completion update. The sum across 20 concurrent attempts is **1,767.702s**, not the batch wall time. `created_at` to `updated_at`, minus `duration_ms`, estimates admission/queue and final database-update time; this is labeled separately in [documents.json](documents.json).

## Bottleneck observations

| Stage | Calls | Cumulative duration |
| --- | ---: | ---: |
| Layout inference | 694 | 367.928 s |
| TSR inference | 175 | 36.772 s |
| Cell detection | 159 | 83.135 s |
| PDF rendering | 694 | 16.812 s |
| PDF text extraction | 694 | 8.742 s |
| Waiting for a PDFium process | 20 | 0.000131 s |

The single layout session's inference intervals total approximately **99.5% of batch wall time**. Its queue accumulated 6,394.165s across 694 page requests, averaging 9.213s per request. Concurrent waiting pages account for this value exceeding batch elapsed time. These observations identify layout inference as the main throughput constraint for this configuration; PDFium process admission was not limiting this run.

Stages overlap or nest across concurrent documents and must not be added into a wall-time total. Native timings measure host-observed calls, including CPU work and synchronization, not isolated accelerator kernels. Full stage aggregates and individual observations are in [stages.csv](stages.csv) and [stage-measurements.jsonl](stage-measurements.jsonl). Timing DEBUG logs were enabled for this run; their overhead was not measured separately.

## Output checks and limits

- All **694** expected pages were present in order, with **10,133 blocks** and **147 table-bearing blocks**. There were no page errors, task failures or retries, and no model-unavailable/inference-failure/timeout warning codes. Each document produced one `ParseTotal` observation and exactly one `LayoutInference` per expected page.
- Successful task completion does not mean warning-free extraction. **16 documents** contained warnings, including **6 with table warnings**. Totals: `ContentLayoutOverlap=51`, `EmptyModelRegion=157`, `TableStructureUnavailable=12`, `TableTextAssignmentFailed=10`, `InvalidTsrInput=2`. Counts per document are retained in [documents.json](documents.json).
- The checks establish HTTP delivery, page completeness and successful model execution. This run did not repeat the separate semantic corpus assertions or prove equality against a new same-configuration baseline. The earlier report's known semantic assertion failures remain outside this throughput run.
- Only **one measured corpus round** was requested and run. This is an observed throughput result, not a statistical speedup claim. The earlier native-library benchmark used document concurrency 2 and different timing boundaries; it is not a controlled baseline for this five-concurrent HTTP result.
- Memory was sampled once per second across the server and five PDFium children. Summed RSS can count shared pages more than once and is not physical footprint or GPU/ANE memory. No GPU kernel profiling or power measurement was performed.
- After the final downloads, the owned server exited with code 0. All five PDFium children acknowledged graceful shutdown and were reaped. Uploaded jobs and synchronized results remain in the configured PostgreSQL/storage history.

## Reproduction artifacts

```sh
rtk cargo build --release --locked -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker
rtk cargo build --release --locked -p docparse-server --features coreml --bin docparse-server
rtk proxy target/release/docparse-server --config docparse.toml --storage-dir data/docparse
```

Use the unchanged configuration recorded in [metadata.json](metadata.json), confirm the queue is idle, warm the 23-page document twice, and submit the corpus with fresh idempotency UUIDs. The exact local orchestration and analysis scripts, raw server/client logs, process samples and full HTTP JSON downloads remain under `target/coreml-http-release-20260915/`. They are local test artifacts; the retained report contains timings, hashes and selected lifecycle evidence only. No production source changes were needed for this run.
