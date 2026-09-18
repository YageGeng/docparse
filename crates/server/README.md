# docparse-server

Native Linux/macOS HTTP service with durable PDF parsing jobs. API instances and
workers share PostgreSQL and a mounted file directory. HTTP connections never own
the job: refreshes, SSE disconnects, and API replacement do not cancel parsing.

## Structure

- `crates/migration`: CLI-generated SeaORM 2.0 migrations, edited with SeaQuery
  table/index builders. Pending migrations run automatically when the database pool connects.
- `crates/database`: connection setup, CLI-generated `entities`, and typed `query`
  methods. Uses SeaORM/SeaQuery and `thiserror`; no Snafu or HTTP dependencies.
- `crates/core/src/bin/pdfium_worker.rs`: feature-gated PDFium executable, built and installed beside the server. Its implementation lives in `core/src/pdfium/`.
- `crates/server`: `app`, `routers`, `state`, `middlewares`, `model`, storage, and
  worker lifecycle. Snafu is confined to this crate. Success/error envelopes follow
  WisLand's `ApiResponse` and APICODE conventions.

## Start

Configure `[server]` and `[database]` in `docparse.toml`. Provision the database
first; `connection::connect` applies pending migrations before HTTP or workers start.
The separate administration migration CLI still reads `DATABASE_URL`. Supply credentials
through your deployment environment rather than checked-in files or CLI arguments.

```bash
rtk cargo build -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker --release
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
`openvino`. Each forwards core's unified backend for layout, OCR, TSR, and
formula. Select one accelerator feature for a build, or none for CPU. CUDA builds use:

```bash
rtk cargo build -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker --release
rtk cargo build -p docparse-server --release --features cuda
```

`render.workers` is required and bounds all PDFium children, including startup
and cleanup. `render.queue_size` bounds unfinished pages across all documents:

```toml
[render]
workers = 5
queue_size = 16

[server]
max_uploads = 4
```

Use `DOCPARSE_RENDER__WORKERS` or `--render-workers` to override the pool size.
Idle process reservations drive database claims directly. A worker can open the
next PDF after the previous document's last raster is delivered and Close is
acknowledged, even while the previous document still runs inference. Completed
post-render tasks never gate new claims. Unused reservations return without
restarting their processes; job heartbeat and timeout supervision continues
until the complete attempt ends.

Idle-process inventory uses a handle channel without a buffer limit so stale
crash tokens cannot evict healthy returned reservations. This channel contains
only process handles; supervisors still enforce the configured process limit.
Page and model work queues retain their bounded admission.

`render.queue_size` is not merely a raster buffer: reservations remain occupied
through the page's complete pipeline and any actual background cleanup after
cancellation. A full queue pauses the next Render operation; idle processes may
still claim, open and pre-scan a new document. Model `queue_size` settings retain
their pending-input semantics, independently of `session_size` and `batch_size`.

`server.max_uploads` remains an independent HTTP admission limit (1–1024,
default 4), with `DOCPARSE_SERVER__MAX_UPLOADS` and `--max-uploads` overrides.
The retired server `jobs`/`pdfium_workers` and runtime
`stage_pages`/`render_queue_capacity`/`blocking_task_limit` fields are rejected.
There is no replacement page-limit or full-document concurrency setting.
E2E scripts use `--render-queue-size` for page delivery capacity.

Install matching `docparse-server` and `docparse-pdfium-worker` executables in the
same directory. Worker startup fails on missing or incompatible artifacts; there
is no configurable worker path or silent in-process fallback. For a local install:

```bash
rtk cargo install --path crates/core --features pdfium-ipc --bin docparse-pdfium-worker --locked
rtk cargo install --path crates/server --locked --features cuda
```

Cancellation aborts the owned render producer and retires uncertain document
sessions. Replacements start only after the old PID and bridge are reaped. Repeated
crashes before a replacement completes one document stop the pool and server
admission. Invalid/empty PDFs reuse the healthy worker, and intentional cancellation
neither consumes nor resets the crash budget. Normal service shutdown drains jobs
and explicitly closes the pool. Both cancellation and pool shutdown request
worker shutdown through IPC, including when a document is open. The worker finishes
its active operation, releases the document and input mapping, acknowledges shutdown,
and exits normally. If acknowledgement or process exit takes more than five seconds,
the supervisor kills and reaps the child before replacement. Broken transports and
crashed workers bypass the graceful request. Install both binaries together: IPC
protocol version 2 requires this shutdown behavior. Synchronous custom glyph
resolvers must return; an indefinitely blocked callback cannot be safely terminated as a Rust
thread and makes pool cleanup report failure.

Workers use separate process groups so terminal Ctrl-C reaches the supervising
server without interrupting PDFium directly. The server handles the signal, drains
accepted jobs, and closes its workers through the same IPC shutdown sequence.

Run the same binary with `--role api`, `--role worker`, or `--role all` (default).
API-only instances do not load models or start PDFium children, and do not require the worker executable. `/api/ready` checks the task schema as well as
shared storage, including detecting schema damage after startup. Every API and worker must use the same
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
before allocating the pool. Pending migrations then run in a transaction on that
same database; a PostgreSQL transaction lock serializes concurrent application
startup. Migration failure aborts startup before model initialization or job claims.

The existing configuration precedence applies: defaults, main TOML, selected
profile, and `DOCPARSE_` environment values. For example, use
`DOCPARSE_SERVER__HOST`, `DOCPARSE_SERVER__PORT`, `DOCPARSE_DATABASE__URL`, or
`DOCPARSE_DATABASE__MAX_CONNECTIONS`. Legacy `--bind`/`SERVER_BIND` and
`--database-url`/`DATABASE_URL` remain optional overrides of the merged configuration;
explicit CLI arguments take priority over their legacy environment equivalents.
`SERVER_ROLE` and `SERVER_STORAGE_DIR` continue to control process role and storage.
Run `rtk cargo run -p docparse-server -- --help` for CLI process limits; the PDFium ceiling is configured in `[server]`.

The API assumes a trusted deployment boundary. Put authentication, authorization,
and any browser CORS policy at your ingress. UUID job IDs are identifiers, not a
multi-tenant authorization system. Credentials and request/response bodies are
excluded from request logging. Responses include `x-request-id` for correlation.

## API reference

The [HTTP workbench](../../packages/web/README.md) uses this API for uploads,
durable history, progress recovery and PDF/result inspection. Starting the updated
server automatically applies the `add_job_file_metadata` migration when pending.

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
rtk curl 'http://127.0.0.1:8080/api/jobs/list?limit=20&status=succeeded'
rtk curl -H 'Range: bytes=0-65535' 'http://127.0.0.1:8080/api/jobs/source?id=07bd9078-a15f-4b41-bbca-e341047db61e'
```

The multipart body must contain exactly one `file` field starting with `%PDF-`.
The default PDF limit is 512 MiB, with at most four concurrent uploads per API and
a 300-second upload deadline. Files are streamed to disk. File names supplied by
clients do not affect storage paths. Uploading the same bytes with the same key
returns the existing task; different bytes with that key return `4091001`.

Snapshots additionally expose `filename`, `size_bytes`, `created_at`, and
`updated_at`. The display filename is bounded to 255 characters, stripped of
directory components/control characters, and never used as a filesystem path.
Metadata is inserted with the task and is not changed by an idempotent replay.
Jobs created before the metadata migration have null filename and size fields.

`GET /api/jobs/list` accepts an optional status, literal case-insensitive filename
search, limit (1–100; default 20), and UUID cursor. Its envelope contains `items`
and `next_cursor`; keep filters unchanged when following that cursor. History is
ordered by descending creation time and UUID. Invalid/missing cursor anchors are
reported as `4001002`.

`GET /api/jobs/source?id=<uuid>` streams the immutable input independently of
parse status. HEAD, byte ranges and conditional requests use tower-http's file
service. Unsatisfiable ranges return 416 with the file size in Content-Range.
Successful range/file responses contain binary PDF data rather than a JSON envelope.

Ordinary success responses use:

```json
{"success":true,"message":"Success","data":{"id":"...","filename":"document.pdf","size_bytes":1024,"created_at":"2026-09-13T00:00:00Z","updated_at":"2026-09-13T00:00:00Z","status":"queued","version":1,"attempts":0,"progress":null,"error":null}}
```

`POST /api/jobs` returns HTTP 202 with a `Location` header pointing to
`/api/jobs/status?id=<uuid>`. `GET /api/jobs/status?id=<uuid>` returns
HTTP 200. The result endpoint returns the same success envelope with the complete
configured `DocumentResult` in `data`. It streams the persisted file instead of
allocating another full JSON document in the API process. Worker serialization
uses `ApiResponse::write` with `JsonRenderer::view_with_config`, sharing the same
Serde envelope as ordinary HTTP and SSE responses while preserving visibility
filtering. JSON whitespace and object-key order are not part of the API contract.

Responses negotiate streaming gzip with `Accept-Encoding`; SSE and PDF responses
retain their original encoding. Full JSON and cached Markdown stream with 256 KiB
read buffers. Results use weak ETags and `Cache-Control: private, no-cache` so
repeat reads can return 304 after checking that the task remains visible.

`GET /api/jobs/result?id=<uuid>&page=1` returns an `ApiResponse<Pagenation<PageResult>>` containing
`page_count`, `errors`, and `page` (null for a failed/missing source page). Page
numbers outside 1..=page_count and combining page with Markdown return 400.
The first page request creates a small byte-offset index beside the immutable JSON;
subsequent reads seek directly to that page without decoding the whole document.
Index creation temporarily reads the source bytes but does not build the full
document object graph. This works for existing results without reparsing PDFs.

Markdown is rendered once on first demand and cached on disk per placeholder
policy. Writable files in the reserved `.locks` directory coordinate cache creation
and deletion across replicas without locking result payloads. The shared filesystem
must support cross-client file locking. Lock files remain after result deletion to
keep waiting replicas on the same inode; reclaim them only with all replicas stopped.
No database connection or
transaction is retained during these operations. Deletion removes derived files
before removing the canonical JSON. Uploads remain single-request multipart
streams, with buffered writes and early PDF signature validation, not resumable
chunk sessions. HTTP completion logs report encoded body bytes handed to the
transport, total elapsed time, errors and cancelled bodies; they do not prove the
remote client has consumed every byte.

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

`POST /api/jobs/delete?id=<uuid>` removes a completed or failed task from all public
reads and deletes its result JSON. The original PDF remains available to other jobs
sharing its content hash. Repeated deletion succeeds; active tasks return `4091004`.
Internal tombstones preserve pagination anchors and prevent reuse of deleted
idempotency keys. Deletion intent commits before filesystem cleanup. HTTP 200 means
cleanup completed; HTTP 202 means the task is already hidden and cleanup remains
pending. Every server role retries pending cleanup at startup and every thirty
seconds, independently of parser concurrency. Cleanup syncs the storage directory
before clearing the result path, so a restart can safely retry an interrupted unlink
or acknowledgement. No database transaction is held while accessing storage.

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
per second. Subscribers to the same job share one database poller per API instance,
which stops when the last subscriber disconnects, the job finishes, or shutdown
begins. Each connection receives an immediate snapshot and SSE keep-alives every
15 seconds. Slow/disconnected subscribers never backpressure the
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
one-hour deadline per attempt; tune `render.workers`, `--lease-seconds`,
`--max-attempts`, and `--job-timeout-seconds` for your documents and hardware.

SIGINT/Ctrl+C and SIGTERM stop new claims, cause `/api/ready` to return 503, close SSE subscriptions,
and drains active parses. Configure the deployment's termination grace period to
allow normal work to finish. If a process is forcibly stopped, another worker
reclaims the task after lease expiry. Recovery re-parses the document from the
beginning, retaining its job ID. It is at-least-once execution, not page-level
checkpoint resumption. Retries use the receiving worker's model/configuration;
keep them compatible during a rolling release.

After draining, dropping the parser closes its model queues and joins the dedicated
native threads, including ONNX destruction and thread-local cleanup. Idle sessions
use no Tokio blocking capacity and can outlive their construction runtime. Finite
initialization and inference waits retain ownership through caller cancellation;
runtime shutdown waits for that outstanding work. This prevents CUDA cleanup from
racing process-wide library teardown without reserving blocking workers for idle models.

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
# Optional: also append a plain-text copy of stdout logs.
file = "logs/docparse.log"
```

The shared loader also supports profiles and `DOCPARSE_LOG__DIRECTIVES` overrides.
A valid `RUST_LOG` takes precedence; if it is absent or invalid, the server uses
`log.directives`. Invalid selected configuration directives stop startup before
database connections or models are initialized.

`log.file` (or `DOCPARSE_LOG__FILE`) adds an append-only file copy alongside stdout
and creates missing parent directories. Relative paths resolve beside the primary
configuration file. File output always disables ANSI colors; stdout uses colors
only when attached to a terminal. Both outputs share the same event filter. Omit
`file` for stdout-only logging. Unwritable destinations stop startup before
database or model initialization.

SQLx query logging has separate database settings with these defaults:

```toml
[database]
sqlx_logging_level = "debug"
sqlx_slow_statements_logging_level = "warn"
sqlx_slow_statements_threshold_ms = 1000
```

Levels accept `off`, `error`, `warn`, `info`, `debug`, and `trace` (case-insensitive).
Queries taking at least the threshold use the slow query level; shorter queries
use the ordinary level. Set either level to `off` to disable that category.
The threshold accepts 1 through 86400000 milliseconds. These settings support the
same profile and environment layers, for example `DOCPARSE_DATABASE__SQLX_LOGGING_LEVEL`.
Events still obey `log.directives` or `RUST_LOG`: the default `sqlx=warn` shows slow
query warnings, while `sqlx=debug` also shows ordinary queries at their default level.

Use `RUST_LOG=info,docparse_core=debug,docparse_layout=debug,docparse_ocr=debug,docparse_tsr=debug`
to include existing stage timings and page diagnostics. The reserved
`docparse::context` target keeps HTTP and PDF correlation spans enabled even with
`RUST_LOG=warn`, `error`, or a module-only filter. Ordinary events retain their
module targets and still obey `RUST_LOG`; retaining a span does not enable its
INFO events. Embedders configuring their own subscriber can use
`docparse_server::logging::filter` to apply the same policy. HTTP timing logs measure
when the response is ready, not completion of an SSE or file response body.

## Parser throughput

Job snapshots (`jobs/status`, `jobs/list`, and SSE) expose nullable `duration_ms`.
It measures the latest completed attempt with a monotonic clock, including parsing
and result publication, and excludes upload, queueing, and the final database update.
The duration is saved atomically with the attempt outcome; the completion log uses
the same value. Retries clear it, while historical or interrupted attempts remain null.

Full-document native extraction precedes global watermark/font statistics.
After that pass, each admitted page advances independently through layout, OCR,
tables and formulas. All documents share `render.queue_size` unfinished page
deliveries. The queue reserves before rendering and retains capacity through
result collection and the final native/IPC/model resource owner after cancellation.

This bounds retained page work, not total service RSS: model weights, document
text facts, results, IPC copies and independently retained observer buffers also
consume memory. Keep result collection and publication moving; completed PDFium
leases do not cap documents still in post-processing.

Native Texo uses a shared ready-crop queue across documents and pages. The
`formula.engine.session_size` independent encoder/decoder pairs drain batches up to
`formula.batch_size` without waiting to fill them. The session count defaults to
one; the sample configuration selects two owners and batches of eight. Admission
queues at most `formula.queue_size` crops in addition to active batches and
existing page tasks. Increasing sessions duplicates model resources; measure
throughput and peak GPU memory before increasing it further. Cancellation and
sequence-level failures stay scoped to the original caller. Native formula
timings are per crop, so shared batch durations must not be summed as GPU busy time.

PDFium remains process-serialized for safety and closes immediately after the
last raster has been delivered, allowing other documents to open while inference
finishes. OCR stages overlap across pages through per-model shared queues;
`session_size` determines consumers and `queue_size` bounds pending requests.
The former `ocr.max_in_flight` page gate and browser-only single-page cap are
removed. ORT Web retains its global execution/readback guard. Layout and TSR
also use independent model sessions. Tune against stage/queue timings and actual
GPU measurements. More replicas sharing one GPU can duplicate model memory rather
than increase throughput; place workers according to GPU and memory budgets.

## Validation

```bash
rtk cargo build -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker
rtk cargo test -p docparse-server
# Use a disposable database shared by these serial acceptance runs.
rtk cargo test -p docparse-database --test jobs -- --ignored --test-threads=1
rtk cargo test -p docparse-server --test http -- --ignored --test-threads=1
```

The integration runs use `DOCPARSE_TEST_DATABASE_URL`. They apply migrations to
that database and leave their uniquely identified task rows in place. The HTTP
test exercises real PDFium and a controlled injected layout engine to verify
disconnects, heartbeat renewal, draining, and cross-instance persistence.


## Formula recognition output

`GET /api/jobs/result?id=<uuid>&format=markdown` projects the stored document into
`text/markdown; charset=utf-8` without running inference again. Default
`format=json` remains the streamed success envelope. Enabled formula recognition
stores both `latex` and `markdown` in `pages[].formulas[]`; Markdown uses those
results in prose, independent equations and table cells. Failures retain source
geometry and an error while original text stays available in JSON. The configured
API prefix applies to this route as usual.
