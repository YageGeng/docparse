# docparse-server

Native Linux/macOS HTTP service with durable PDF parsing jobs. API instances and
workers share PostgreSQL and a mounted file directory. HTTP connections never own
the job: refreshes, SSE disconnects, and API replacement do not cancel parsing.

## Structure

- `crates/migration`: CLI-generated SeaORM 2.0 migrations, edited with SeaQuery
  table/index builders. Run migrations once as a deployment step.
- `crates/database`: connection setup, CLI-generated `entities`, and typed `query`
  methods. Uses SeaORM/SeaQuery and `thiserror`; no Snafu or HTTP dependencies.
- `crates/server`: `app`, `routers`, `state`, `middlewares`, `model`, storage, and
  worker lifecycle. Snafu is confined to this crate. Success/error envelopes follow
  WisLand's `ApiResponse` and APICODE conventions.

## Start

Configure `[server]` and `[database]` in `docparse.toml`. Provision the database
first; the separate migration CLI still reads `DATABASE_URL`. Supply credentials
through your deployment environment rather than checked-in files or CLI arguments.

```bash
rtk proxy sea-orm-cli migrate up -d crates/migration
rtk cargo run -p docparse-server --release -- \
  --storage-dir /srv/docparse/shared --config docparse.toml
```

The default listener uses `server.host = "127.0.0.1"` and `server.port = 8080`.
`server.api_prefix` defaults to `/api` and applies to every endpoint, including
health probes, OpenAPI and Scalar. Set it to a literal path such as `/service/docparse/v2`,
or use `""` or `"/"` for root routes. Non-root prefixes must not end with `/` or
contain empty segments, captures, query strings or fragments. Environment overrides
use `DOCPARSE_SERVER__API_PREFIX`. Examples below use the default `/api` prefix.
Host names and IPv4/IPv6 addresses are supported. Native backend selection is
fixed at compilation; model configuration has no `execution_provider` field.
The server exposes four backend features: `coreml`, `cuda`, `metal`, and
`openvino`. Each enables that backend for layout, OCR, and TSR together. Select
one accelerator feature for a build, or none for CPU. CUDA builds use:

```bash
rtk cargo build -p docparse-server --release --features cuda
```

Run the same binary with `--role api`, `--role worker`, or `--role all` (default).
API-only instances do not load models. `/api/ready` checks the task schema as well as
shared storage; a reachable database without migrations is not ready. Every API and worker must use the same
database and shared directory contents, even if their mount paths differ. The
filesystem must provide coherent cross-host reads, atomic publication, and file
and directory synchronization. An unshared container directory is insufficient.

API-only instances also load the configuration file, without validating or loading
parser models. `database.url` must be supplied for every server role. The pool
uses WisLand's millisecond-based settings:

| Field | Default | Meaning |
|---|---|---|
| `database.max_connections` | 10 | Maximum connections per process |
| `database.min_connections` | 1 | Minimum retained connections; zero is supported |
| `database.timeout_ms` | 5000 | Connection timeout |
| `database.acquire_timeout_ms` | 5000 | Pool acquisition timeout |
| `database.idle_timeout_ms` | 600000 | Idle connection timeout |

Pool maximum must be positive and at least the minimum. Each timeout must be
between 1 and 86400000 milliseconds. Connection setup validates these settings
before allocating the pool; it does not apply migrations.

The existing configuration precedence applies: defaults, main TOML, selected
profile, and `DOCPARSE_` environment values. For example, use
`DOCPARSE_SERVER__HOST`, `DOCPARSE_SERVER__PORT`, `DOCPARSE_DATABASE__URL`, or
`DOCPARSE_DATABASE__MAX_CONNECTIONS`. Legacy `--bind`/`SERVER_BIND` and
`--database-url`/`DATABASE_URL` remain optional overrides of the merged configuration;
explicit CLI arguments take priority over their legacy environment equivalents.
`SERVER_ROLE` and `SERVER_STORAGE_DIR` continue to control process role and storage.
Run `rtk cargo run -p docparse-server -- --help` for all process limits.

The API assumes a trusted deployment boundary. Put authentication, authorization,
and any browser CORS policy at your ingress. UUID job IDs are identifiers, not a
multi-tenant authorization system. Credentials and request/response bodies are
excluded from request logging. Responses include `x-request-id` for correlation.

## API reference

Open `http://127.0.0.1:8080/api/docs` for the interactive Scalar reference, or download
`http://127.0.0.1:8080/api/openapi.json` for the OpenAPI 3.1 document. Both are available
on API-only and combined instances. Scalar's default page loads its JavaScript
from jsDelivr.

Every handler uses `#[utoipa::path]`; `utoipa-axum` registers the HTTP routes and
collects their specification together. Existing response types derive `ToSchema`,
including the full document, table, and geometry graph. The reference describes
multipart uploads, the required idempotency header, APICODE errors, and SSE frame
examples with reconnect semantics. Documentation responses contain raw OpenAPI
JSON or HTML, while business endpoints retain their existing `ApiResponse` envelopes.
Each leaf module under `routers/` owns one documented endpoint; category `mod.rs`
files aggregate the handlers. `app.rs` adds the configured prefix to both routing
and documentation. Job operations use the `JOBS` tag and query parameters instead
of path captures.

## Upload and retrieve

Generate and save a UUID before uploading. Reuse it if the upload response is
lost. `Idempotency-Key` is also the returned job ID, so even a lost `202` response
can be recovered by querying that UUID.

```bash
rtk curl -i -H 'Idempotency-Key: 07bd9078-a15f-4b41-bbca-e341047db61e' \
  -F 'file=@document.pdf;type=application/pdf' http://127.0.0.1:8080/api/jobs
rtk curl 'http://127.0.0.1:8080/api/jobs/status?id=07bd9078-a15f-4b41-bbca-e341047db61e'
rtk proxy curl -N 'http://127.0.0.1:8080/api/jobs/events?id=07bd9078-a15f-4b41-bbca-e341047db61e'
rtk curl 'http://127.0.0.1:8080/api/jobs/result?id=07bd9078-a15f-4b41-bbca-e341047db61e'
```

The multipart body must contain exactly one `file` field starting with `%PDF-`.
The default PDF limit is 512 MiB, with at most four concurrent uploads per API and
a 300-second upload deadline. Files are streamed to disk. File names supplied by
clients do not affect storage paths. Uploading the same bytes with the same key
returns the existing task; different bytes with that key return `4091001`.

Ordinary success responses use:

```json
{"success":true,"message":"Success","data":{"id":"...","status":"queued","version":1,"attempts":0,"progress":null,"error":null}}
```

`POST /api/jobs` returns HTTP 202 with a `Location` header pointing to
`/api/jobs/status?id=<uuid>`. `GET /api/jobs/status?id=<uuid>` returns
HTTP 200. The result endpoint returns the same success envelope with the complete
configured `DocumentResult` in `data`. It streams the persisted file instead of
allocating another full JSON document in the API process. Worker serialization
uses `ApiResponse::write` with `JsonRenderer::view_with_config`, sharing the same
Serde envelope as ordinary HTTP and SSE responses while preserving visibility
filtering. JSON whitespace and object-key order are not part of the API contract.

Each `ApiError` variant carries a `code: ApiCode` supplied when its Snafu context
is constructed, plus a short stage marker identifying the failing operation.
`ApiCode` contains only `http_code: u16` and `code: u64`; business failures use
single-argument `const fn` constructors such as `ApiCode::bad_request(4001002)`.
Only `COMMON_*` fallbacks remain named `ApiCode` instances. The `ErrorCode` trait
supplies the business number, HTTP status, and `message()` via `ApiError::to_string()`.
HTTP responses, SSE errors, and persisted worker failures therefore include the
same stage and any source error formatted by the Snafu variant's Display implementation.
`ApiErrorResponse` handles serialization and transport status; router fallbacks,
extractor rejections, and caught panics enter the same typed boundary directly.
There is no middleware that reclassifies completed HTTP responses. Errors use:

```json
{"success":false,"error":{"code":4091002,"message":"request failed at job-result-pending"}}
```

| Code | HTTP | Meaning |
|---|---|---|
| 4001001 | 400 | Invalid PDF upload or multipart fields |
| 4001002 | 400 | Missing, invalid, duplicate, or otherwise malformed job query |
| 4001003 | 400 | Missing/invalid idempotency UUID |
| 4041001 | 404 | Unknown job |
| 4081001 | 408 | Upload deadline exceeded |
| 4091001 | 409 | Idempotency key conflict |
| 4091002 | 409 | Result not ready |
| 4091003 | 409 | Job failed |
| 4131001 | 413 | Upload exceeds the configured limit |
| 4291001 | 429 | All upload slots are occupied |
| 5031001 | 503 | Instance is draining |
| 5031002 | 503 | Database unavailable |
| 5031003 | 503 | Shared storage unavailable |
| 500000 | 500 | Internal error or caught panic |

## SSE reconnect semantics

The event name is `job`, its `id` is the durable database version, and `data` is
the same `ApiResponse<JobSnapshot>` returned by the status endpoint. Every new
connection sends the latest snapshot, including after `Last-Event-ID` reconnects.
Intermediate snapshots are coalesced, not maintained as an event audit log.
Clients may deduplicate by version and must close `EventSource` when status is
`succeeded` or `failed`, then retrieve the result with a normal GET.

```js
const jobId = localStorage.getItem("docparse-job-id");
const events = new EventSource(`/api/jobs/events?id=${encodeURIComponent(jobId)}`);
events.addEventListener("job", ({ data }) => {
  const job = JSON.parse(data).data;
  if (job.status === "succeeded" || job.status === "failed") events.close();
});
```

Parsing runs in an owned task separate from the lease supervisor, so synchronous
parser work cannot prevent heartbeats. Heartbeat database waits are bounded.
Workers coalesce progress into a watch channel and persist changes at most twice
per second. Each subscriber polls the database once per second and receives SSE
keep-alives every 15 seconds. Slow/disconnected subscribers never backpressure the
parser. Responses set `X-Accel-Buffering: no` and `Cache-Control: no-cache,
no-transform`; disable proxy response buffering and set its idle timeout above the
keep-alive interval. A proxy restart can close the connection; reconnect to any API.

## Recovery and rolling releases

Workers atomically claim rows with `FOR UPDATE SKIP LOCKED`. Every claim gets a
fresh UUID fencing token, increments `attempts`/`version`, and clears previous
attempt progress. PostgreSQL's clock controls all lease checks and deadlines.
The default lease is 60 seconds, renewed at least every 20 seconds. Old workers
cannot publish progress or success once the lease expires or another worker owns
the task. Defaults are two concurrent documents per worker, three attempts, and a
one-hour deadline per attempt; tune `--worker-concurrency`, `--lease-seconds`,
`--max-attempts`, and `--job-timeout-seconds` for your documents and hardware.

SIGTERM stops new claims, causes `/api/ready` to return 503, closes SSE subscriptions,
and drains active parses. Configure the deployment's termination grace period to
allow normal work to finish. If a process is forcibly stopped, another worker
reclaims the task after lease expiry. Recovery re-parses the document from the
beginning, retaining its job ID. It is at-least-once execution, not page-level
checkpoint resumption. Retries use the receiving worker's model/configuration;
keep them compatible during a rolling release.

Inputs are content-addressed. Duplicate publication compares existing bytes and
rejects conflicting or corrupt objects instead of acknowledging the filename alone. Results use per-attempt names and are synchronized
and published atomically before the database records success. Crashes can leave
unreferenced files, but they cannot expose partial JSON as a successful result.
Accepted inputs/results are retained until an operator removes them; no automatic
retention deadline is imposed. Back up PostgreSQL and the shared directory together.
Only remove files after proving no retained task references them. Never delete
active temporary files or expire inputs while attempts may still be retried.

## PDF log correlation

The service uses existing `tracing` spans and durable task fields for correlation:

- `job_id`: identifies one submitted task across API requests, worker attempts,
  process restarts, and replicas.
- `pdf_hash`: the existing BLAKE3 content hash, shared by identical PDF bytes even
  when they are submitted under different task IDs. It becomes available after
  the upload body has been read.
- `attempt`: distinguishes worker retries of the same task.
- `request_id`: identifies an individual HTTP request and is returned through
  `x-request-id`.

Each attempt creates a local `pdf_parse` span. Page tasks, blocking CPU work,
PDFium's document thread, and individual calls on shared ONNX session threads
inherit that span and the caller's subscriber. Session calls restore their own
dispatcher independently, so a shared session never adopts the first caller's
logging destination. API status/result requests and SSE polling attach the same
stored job/content identifiers to their own request spans. Span objects remain
process-local; durable identifiers allow matching the logs from different processes.

Typical output:

```text
INFO pdf_parse{job_id=... pdf_hash=... attempt=1}: starting document parse with layout engine ...
DEBUG pdf_parse{job_id=... pdf_hash=... attempt=1}: stage LayoutInference for page Some(1) elapsed ... ms
```

Configure the default server event filter in `docparse.toml`:

```toml
[log]
directives = "info,ort=warn,sqlx=warn"
```

The shared loader also supports profiles and `DOCPARSE_LOG__DIRECTIVES` overrides.
A valid `RUST_LOG` takes precedence; if it is absent or invalid, the server uses
`log.directives`. Invalid selected configuration directives stop startup before
database connections or models are initialized.

Use `RUST_LOG=info,docparse_core=debug,docparse_layout=debug,docparse_ocr=debug,docparse_tsr=debug`
to include existing stage timings and page diagnostics. The reserved
`docparse::context` target keeps HTTP and PDF correlation spans enabled even with
`RUST_LOG=warn`, `error`, or a module-only filter. Ordinary events retain their
module targets and still obey `RUST_LOG`; retaining a span does not enable its
INFO events. Embedders configuring their own subscriber can use
`docparse_server::logging::filter` to apply the same policy. HTTP timing logs measure
when the response is ready, not completion of an SSE or file response body.

## Parser throughput

Full-document native extraction still precedes global watermark/font statistics.
After this pass, render, layout/preparation, OCR/composition, and TSR/completion
run as separate bounded stages. Each analysis stage admits at most
`runtime.page_concurrency` pages. Up to roughly `3 * page_concurrency +
render_queue_capacity + 1` page rasters can be retained per document, plus native
facts, model tensors, and final results. Queue pressure intentionally stops
further rasterization instead of growing memory without bound.

PDFium remains process-serialized for safety and closes immediately after the
last raster has been delivered, allowing other documents to open while inference
finishes. Native OCR `max_in_flight` defaults to two, overlapping different model
stages across pages while serializing each session on a dedicated native thread. Browser OCR remains
single-page bounded. Layout session pools retain their existing configuration;
TSR retains one session. Tune against the existing stage/queue timings and actual
GPU measurements. More replicas sharing one GPU can duplicate model memory rather
than increase throughput; place workers according to GPU and memory budgets.

## Validation

```bash
rtk cargo test -p docparse-server
# Use a disposable database shared by these serial acceptance runs.
rtk cargo test -p docparse-database --test jobs -- --ignored --test-threads=1
rtk cargo test -p docparse-server --test http -- --ignored --test-threads=1
```

The integration runs use `DOCPARSE_TEST_DATABASE_URL`. They apply migrations to
that database and leave their uniquely identified task rows in place. The HTTP
test exercises real PDFium and a controlled injected layout engine to verify
disconnects, heartbeat renewal, draining, and cross-instance persistence.
