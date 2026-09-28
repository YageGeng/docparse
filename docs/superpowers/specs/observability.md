# Observability

The server exposes `/metrics` independently of `server.api_prefix`. Model code
records through `docparse-common`; only the server installs a recorder. CLI and
WASM consumers do not start listeners or contact Prometheus. Worker-only processes
also listen on `server.host:server.port`, exposing only `/metrics`.

The WebUI **运行监测** link opens a new tab so navigation does not cancel uploads.
Live data refreshes every five seconds and identifies the current process UUID and
role. Resource utilization and throughput are local to that process; DB backlog
is shared. API-only instances show worker-only values as unavailable, with an
explicit message directing operators to historical worker data. Switching API
replicas or restarting a process resets the UI delta baseline by UUID.
History refreshes every thirty seconds and
requires this configuration:

```toml
[monitoring]
prometheus_url = "http://127.0.0.1:19090"
```

The server proxies a fixed list of PromQL queries. It accepts neither arbitrary
PromQL nor a browser-supplied upstream URL. Windows are limited to seven days,
approximately 600 points per series, a ten-second HTTP timeout, and a two-MiB
response. Configure ingress access controls as for the other operational APIs.

## Podman without volumes

This Linux example uses host networking. Replace `18080` with the DocParse HTTP
port. `--image-volume=ignore` also suppresses anonymous image-declared volumes.
Configuration and TSDB files reside inside the container's writable layer.

```sh
cat > /tmp/prometheus.yml <<'YAML'
global:
  scrape_interval: 5s
scrape_configs:
  - job_name: docparse
    static_configs:
      - targets: ['127.0.0.1:18080']
YAML

podman create --name docparse-prometheus --network host --image-volume=ignore \
  docker.io/prom/prometheus:v3.13.3 \
  --config.file=/etc/prometheus/prometheus.yml \
  --storage.tsdb.path=/prometheus \
  --storage.tsdb.retention.time=15d \
  --web.listen-address=127.0.0.1:19090
podman cp /tmp/prometheus.yml docparse-prometheus:/etc/prometheus/prometheus.yml
podman start docparse-prometheus
podman inspect docparse-prometheus --format '{{json .Mounts}}'
```

The last command must print `[]`. Stop/start or restart preserves history; removing
or replacing this container removes its data. Reconfigure by copying the YAML
again and restarting this same container. The UI is at `http://127.0.0.1:19090`.
For separate API and worker processes, add every worker's scrape address.

## Metric interpretation

All durations are seconds, all counters reset at process restart, and all labels
have bounded operational values. There are no PDF names, paths, task IDs, or raw
errors in metric labels. Every exported series has `service="docparse"`.

| Metric family | Meaning |
| --- | --- |
| `docparse_jobs{state}` | Database-wide queued, live running, and expired-lease recovery tasks; excludes deleted rows |
| `docparse_job_oldest_age_seconds{state}` | Age of the oldest currently queued/active/recoverable task; exposes stalls before a duration histogram completes |
| `docparse_database_collection_success`, `docparse_database_collection_timestamp_seconds` | Freshness of five-second DB collection; failures retain previous values |
| `docparse_jobs_submitted_total` | Newly inserted tasks, excluding idempotent upload replays |
| `docparse_jobs_completed_total{outcome}` | Accepted terminal successes or exhausted failures; failed attempts that requeue are not terminal |
| `docparse_job_attempts_started_total`, `docparse_job_attempts_finished_total{outcome}`, `docparse_job_attempts_active` | Attempt-level starts, fenced completions, and local active supervision |
| `docparse_job_queue_wait_seconds` | Queue origin to claim; retries use `queued_at`, expired leases use `lease_until` |
| `docparse_job_parse_seconds` | Actual local opening/parse scope, excluding result publication; errors/cancellation also close this scope |
| `docparse_job_publish_seconds` | Serialization and durable publication, excluding temporary-file creation and final DB update; blocking writers and final fsync retain the timer after caller cancellation |
| `docparse_job_attempt_seconds{outcome}` | Monotonic attempt time persisted in `duration_ms`, observed only when fencing accepts completion |
| `docparse_job_end_to_end_seconds{outcome}` | Creation to terminal finish, including retries and waiting |
| `docparse_queue_items`, `docparse_queue_capacity_items` | Pending model inputs and capacity; active inference is excluded |
| `docparse_queue_blocked_producers` | Producers currently blocked for capacity, including packets too large for partially free space |
| `docparse_queue_admission_wait_seconds{outcome}` | One producer call's admission wait, including immediate admissions and cancelled waits; native packet calls can contain multiple items |
| `docparse_queue_residence_seconds{outcome}` | One admitted item's time until dequeue/discard |
| `docparse_queue_enqueued_items_total`, `docparse_queue_removed_items_total{outcome}` | Input flow; dequeued entries can subsequently be skipped for caller cancellation |
| `docparse_page_slots_used`, `docparse_page_slots_capacity`, `docparse_page_blocked_producers` | Completion-counted render capacity, including crops retaining a page after rendering |
| `docparse_page_admission_wait_seconds`, `docparse_page_hold_seconds` | Admission wait (admitted/cancelled outcomes) and full resource lifetime through final lease release |
| `docparse_model_workers_configured`, `docparse_model_workers_alive`, `docparse_model_workers_busy` | Configured consumers, live owners, and occupied batch execution scopes |
| `docparse_model_batch_service_seconds` | Whole consumer batch service, including preparation/readback; distinct from physical ONNX execution |
| `docparse_onnx_active_calls{model,graph}`, `docparse_onnx_run_seconds{model,graph,outcome}` | Physical local ORT calls; decoder steps are separate calls, never multiplied by observers |
| `docparse_onnx_batch_items{model,graph}` | Actual input batch at each physical call; average is sum/count |
| `docparse_pdfium_workers_configured`, `docparse_pdfium_workers_alive`, `docparse_pdfium_documents_active` | Process pool capacity, supervised processes, and documents retained through close/cleanup |
| `docparse_pdfium_operation_seconds{operation}`, `docparse_pdfium_restarts_total` | Open/extract/render/close IPC scopes and process replacements |
| `docparse_pages_parsed_total` | Canonical pages returned by successful parse scopes; includes repeated pages across retries and is not unique-document output |

Histograms expose classic `_bucket`, `_sum`, and `_count` series. Duration buckets
span 1 ms to 1 hour, with an implicit `+Inf`; batch buckets are 1, 2, 4, 8, 16, 32.
P95 is approximate and may be unavailable with no observations. Missing samples,
counter resets, and `NaN` are not displayed as zero. The first live throughput
value requires two snapshots; the first historical rate requires two scrapes.
MinerU measures remote consumer service, not an inaccessible remote ONNX runtime.

Database gauges are shared across replicas: aggregate with `max`, not `sum`.
Before aggregating, historical backlog/age queries require collection success and
an update within 15 seconds on the same `(job, instance)`. Failed or stalled
replicas produce gaps rather than fresh-looking repeated values. Admission-wait
history keeps admitted, closed, and cancelled outcomes separate.
Resource/throughput metrics are process-local and may be summed across workers.
The built-in history queries select this service across the configured Prometheus;
use a dedicated Prometheus or equivalent upstream tenant when deployments must
remain isolated.

## Durable timeline

The migration adds nullable timestamps without inventing values for old jobs:

| Column | Meaning |
| --- | --- |
| `created_at` | Uploaded input accepted and durable task created |
| `started_at` | First successful claim; immutable through retries |
| `attempt_started_at` | Latest fenced attempt's start |
| `queued_at` | Latest failed-attempt requeue; initial queue origin falls back to creation |
| `finished_at` | Terminal success or retry-exhaustion transition; independent of heartbeats/deletion |

Offline initial wait is `started_at - created_at`; total wall-clock processing
span is `finished_at - started_at`; total turnaround is `finished_at - created_at`.
The processing span includes retry gaps and is not summed GPU busy time.
`duration_ms` remains the latest accepted attempt's monotonic duration. This table
does not retain a full per-attempt event ledger; Prometheus retains aggregate
attempt history. Migrations run through the existing server connection startup.

## Diagnosing pressure

1. Check `up{job="docparse"}` and DB collection freshness before interpreting zeros.
2. Growing queued count/oldest age with full PDFium occupancy indicates sustained
   admission pressure. Check page pressure and model utilization before adding processes.
3. Full page capacity plus blocked page producers means downstream resource
   retention is applying render backpressure, not necessarily slow PDFium.
4. Model queue pressure plus busy consumers and higher ONNX P95 indicates inference
   saturation. Pressure without busy/live consumers points to a stopped owner or stall.
5. Low batch sizes with saturated consumers suggest incompatible input shapes or
   insufficient ready work; increasing `batch_size` alone does not force a full batch.
6. Rising recovery tasks/restarts or falling completion throughput warrant checking
   worker logs, lease supervision, GPU errors, storage and DB availability.

Suggested alert expressions (tune thresholds to document sizes and workload):

```promql
up{job="docparse"} == 0
docparse_database_collection_success{service="docparse"} == 0
time() - docparse_database_collection_timestamp_seconds{service="docparse"} > 15
max(docparse_job_oldest_age_seconds{state="queued",service="docparse"}) > 120
docparse_model_workers_alive < docparse_model_workers_configured
increase(docparse_pdfium_restarts_total{service="docparse"}[10m]) > 2
sum(rate(docparse_jobs_completed_total{outcome="failed",service="docparse"}[5m])) > 0
```

Use persistence windows (for example two minutes for saturation) to avoid alerts
on healthy short bursts. GPU device utilization/memory and host CPU/RAM remain
the responsibility of standard host/DCGM exporters, not per-inference shell calls.

## Local validation

Validated with the production CUDA server on an RTX 4060 Laptop GPU while the
existing user server remained running. A 47-page real PDF completed successfully;
three additional concurrent uploads completed 141 pages, followed by a final
47-page validation on the rebuilt server. The run observed queued
tasks, two occupied page slots, a blocked page producer, active Layout/TSR calls,
and nonempty historical P95 series. Browser validation used the real backend and
Prometheus on desktop and a 390-pixel viewport. Both task-owned containers have
empty mount lists. Prometheus stop/start preserved its history, and the WebUI
retained old values during the outage and recovered after readiness. Recorder tests cover final page-owner release and async/native
queue closure; PostgreSQL tests cover retries, exhausted failures and deletion.

The focused PromQL regression runs both actual history expressions through
`promtool test rules` with a healthy replica, a failed replica, a stalled
collector, and an entirely stale interval:

```sh
PROMTOOL=/path/to/promtool cargo test -p docparse-server --lib \
  database_history_excludes_failed_and_stalled_replicas -- --ignored
```

Queue regression tests also cancel pending model/page admissions and verify
elapsed-wait observations and released blocked gauges. Publication timing tests
abort the async waiter while its blocking writer still owns the timer.

## Code boundaries

- `crates/common/src/telemetry.rs` owns shared consumer measurements through
  `ModelMetrics`; native session managers, browser actors, and MinerU use the same
  configuration, live-owner and busy-batch lifetimes.
- `crates/server/src/model/monitoring.rs` defines wire contracts,
  `routers/monitoring.rs` adapts HTTP, and `service/monitoring.rs` owns recorder
  setup, background collection, snapshot projection and Prometheus queries.
- `packages/web/src/features/monitoring/queries.ts` owns polling and cancellation;
  `metrics.ts` interprets samples and counter deltas; `HistoryChart.tsx` draws
  history; `MonitoringPage.tsx` handles controls and presentation.
- Durable `submit` and `finish` require `DatabaseConnection`, not an arbitrary
  transaction. Compile-fail tests reject outer transactions so counters cannot
  announce writes that the caller later rolls back. `claim` continues to publish
  observations only after committing its own transaction.
