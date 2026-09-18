# DocParse HTTP workbench

A React and TypeScript workbench for the native Axum server. Parsing runs on
`docparse-server`; PDF.js renders the persisted original PDF. The separate
[`@docparse/wasm-web`](../wasm-web/README.md) package provides browser-local inference.

## Run

Run every shell command in this guide from the repository root. Select the
workbench with `--prefix packages/web`.

Create the PostgreSQL database and configure the server's database connection
before starting. The server automatically applies pending migrations on that
connection, including upload metadata and the task-history index. Start the
native server in one terminal, choosing the backend feature for your host:

```sh
rtk cargo run -p docparse-server --release --features cuda
```

In a second terminal, install and start the workbench:

```sh
rtk npm ci --prefix packages/web
rtk npm run dev --prefix packages/web
```

Open <http://127.0.0.1:5173>. Use Node.js 22.13 or newer for the pinned Vite and PDF.js;
this package was validated with Node.js 26.8.2. The dev server listens on loopback
and proxies `/api/v1/docparse` to `http://127.0.0.1:8080` by default.

Copy `packages/web/.env.example` to `packages/web/.env.local` for local settings
and API type generation.
`VITE_API_PREFIX` must match `server.api_prefix`. `VITE_API_TARGET` controls the
development/preview proxy, and `VITE_BASE_PATH` controls the static application
mount point. Neither is a place for credentials. Restart Vite after changing
environment settings or dependencies.

```sh
rtk npm run check --prefix packages/web
rtk npm run build --prefix packages/web
rtk npm run preview --prefix packages/web
```

The build produces `packages/web/dist/`, including the PDF.js worker, fonts, CMaps, ICC profiles, image
decoders and their license notices. Production needs only a static web server and
a same-origin proxy to Axum. Route application navigations such as `/document` to
`index.html`; preserve the configured API prefix, allow streaming uploads, and
disable response buffering for SSE. Keep SSE idle timeouts above the server's
15-second heartbeat. Configure upload limits consistently at the ingress and API.

The existing trusted-ingress boundary applies: task history is deployment-wide.
Tenant-specific access requires corresponding authorization and query filtering
in the backend before deploying a multi-tenant workbench.

### HTTP compression

`npm run build` creates gzip siblings for compressible assets using Node's built-in
zlib. Configure the static host to serve them with `Content-Encoding: gzip` and
`Vary: Accept-Encoding`; see `nginx.conf.example`. The API independently compresses
JSON and Markdown through tower-http. Do not manually decompress fetch/XHR
responses or set `Accept-Encoding` in browser JavaScript. Proxies must preserve
encoding headers and stream uploads and SSE without buffering. Match the API prefix
in the browser build, proxy configuration and server configuration.

## Behavior

History and document details show the server-persisted parsing duration, including
result saving and excluding upload and queueing. Reloads retain the same value.
Retries use the final attempt's duration; unfinished or historical jobs without a
measurement show `—`.

Completed and failed rows offer a confirmed delete action. It removes the task
from public history and deletes its result JSON, while retaining the shared source
PDF. Queued and running tasks cannot be deleted. Refreshing the page keeps deleted
tasks hidden; submitting the PDF again creates a new job identity.
Storage cleanup is retried by the server if interrupted; accepted deletion stays
hidden during recovery. Offline deletion fails immediately instead of queuing an
action that runs silently on reconnect. Missing task responses clear cached success
snapshots and release the PDF/result readers, including after deletion in another tab.

- History comes from `GET /jobs/list`, ordered by creation time and UUID. Filename
  search, status filters and cursor pagination run on the server. Older jobs can
  have unknown filenames or sizes.
- File selection and drag-and-drop accept multiple PDFs. Up to 100 files upload
  concurrently through the existing single-file API, with independent XHR progress
  and errors. The backend's `server.max_uploads` independently limits concurrent
  uploads across all clients. A failed file does not cancel the remaining queue.
  The history page stays open; each accepted row links to its own task.
  HTTP 429 responses retain the same UUID and retry up to six times with jittered
  exponential delays capped at 30 seconds. Cancellation interrupts the wait;
  exhausted retries remain available for manual retry. Other errors affect only
  their own file.
  A UUID is saved per file before sending bytes. New selections always get new
  identities; row-level retry and reselection reuse only that row's UUID, without
  matching other files by name or size. Lost acknowledgements can be recovered
  individually by checking those UUIDs.
  The upload transfer itself is not resumable: an input not yet committed must be
  selected again after a refresh. Cancelling stops active and queued transfers;
  already accepted tasks keep running. Removing an upload row discards only its
  local identity, not any server task. Parsing concurrency remains server-controlled.
- The current job uses SSE, with bounded status polling during reconnects.
  Monotonic versions prevent stale snapshots from replacing newer progress.
  Offline status is independent of a socket that has not yet reported failure.
  Terminal jobs close their SSE connection.
- Task ID, page and selected block live in the URL. Refreshing restores them and
  reads the original PDF from `/jobs/source?id=...`; it does not require selecting
  the local file again. The source endpoint supports HEAD and byte ranges.
- Only visible thumbnails and the active PDF page are rendered. Obsolete renders
  are cancelled, and PDF resources are released on navigation. Failed range
  requests are restarted when connectivity returns.
- A separate result worker fetches only `jobs/result?id=<uuid>&page=<number>`.
  It first reads page one to learn the page count, then clamps deep-link page numbers
  before fetching the selected page and normalizes the URL. This also works when
  the PDF preview cannot be loaded.
  Switching pages aborts stale reads; both network traffic and retained result
  memory scale with the selected page. Browsers automatically decode HTTP gzip
  before the worker parses JSON. Complete downloads remain available separately.
- Text, canonical merged-cell tables and per-page/per-block JSON are rendered
  without interpreting document text as HTML. Complete JSON downloads stream
  directly from the persisted server result.

## API types

`src/api/schema.d.ts` is generated from utoipa rather than handwritten SDK types.
Normal builds use the checked-in declaration file and do not contact the backend.
After changing the server contract, run the server and regenerate. The local
environment file described above supplies the OpenAPI URL for the default API prefix:

```sh
rtk npm run api:generate --prefix packages/web
# With a different server or prefix:
rtk proxy env DOCPARSE_OPENAPI_URL=http://127.0.0.1:8091/api/openapi.json \
  rtk npm run api:generate --prefix packages/web
```

The generator reads `DOCPARSE_OPENAPI_URL` from `packages/web/.env.local`; the
explicit environment variable takes precedence.

## Organization

- `src/api`: generated contract, shared response handling and streamed upload.
- `src/features/jobs`: upload recovery, persisted history and SSE synchronization.
- `src/features/viewer`: URL-based inspection, lazy PDF rendering, result worker
  and text/table/JSON presentation.
- `src/components/ui`: the selected shadcn/Radix components.
- `scripts`: API generation and self-hosted PDF.js asset preparation.

Browser acceptance uses the production release CUDA server with isolated
PostgreSQL/storage and real PDFs. Frontend unit tests are not required by this
repository; backend contract and persistence regressions live in crate `tests/`.

## Formula previews

The content inspector renders recognized inline/display formulas in both LaTeX
and Markdown views using KaTeX. Copy buttons copy the exact corresponding JSON
source, including Markdown delimiters. Formula-only regions show the recognized
math. Paragraphs use the backend-projected `block.markdown` to typeset inline
formulas at their original UTF-8 source ranges, preserving adjacent prose.
Formula details and individual source-copy actions are collapsed below such
paragraphs; the content copy action copies their Markdown source. Older stored
results without this optional projection retain the previous view until reparsed. Table-cell Markdown renders
its embedded formulas, and formulas without a block anchor remain visible.
Selected-region JSON includes its associated formula records.

Markdown raw HTML and automatic image loading are disabled. LaTeX trusted
commands are disabled, with bounded macro expansion and layout size. Unsupported
LaTeX shows an explicit preview error while retaining the source copy buttons.
Fonts and rendering libraries are served locally.

Verify against the production server and built workbench:

```sh
rtk proxy node crates/server/tests/formula_web.mjs /absolute/path/to/formulas.pdf
```
