# DocParse HTTP workbench

A React and TypeScript workbench for the native Axum server. Parsing runs on
`docparse-server`; PDF.js renders the persisted original PDF. The separate
[`@docparse/wasm-web`](../wasm-web/README.md) package provides browser-local inference.

## Run

Create the PostgreSQL database and configure the server's database connection
before starting. The server automatically applies pending migrations on that
connection, including upload metadata and the task-history index:

```sh
rtk cargo run -p docparse-server --release --features cuda
rtk npm ci --prefix packages/web
rtk npm run dev --prefix packages/web
```

Open <http://127.0.0.1:5173>. Use Node.js 22.13 or newer for the pinned Vite and PDF.js;
this package was validated with Node.js 26.8.2. The dev server listens on loopback
and proxies `/api` to `http://127.0.0.1:8080` by default.

Copy `.env.example` to `.env.local` when using a different API prefix or server.
`VITE_API_PREFIX` must match `server.api_prefix`. `VITE_API_TARGET` controls the
development/preview proxy, and `VITE_BASE_PATH` controls the static application
mount point. Neither is a place for credentials. Restart Vite after changing
environment settings or dependencies.

```sh
rtk npm run check --prefix packages/web
rtk npm run build --prefix packages/web
rtk npm run preview --prefix packages/web
```

The build produces `dist/`, including the PDF.js worker, fonts, CMaps, ICC profiles, image
decoders and their license notices. Production needs only a static web server and
a same-origin proxy to Axum. Route application navigations such as `/document` to
`index.html`; preserve the configured API prefix, allow streaming uploads, and
disable response buffering for SSE. Keep SSE idle timeouts above the server's
15-second heartbeat. Configure upload limits consistently at the ingress and API.

The existing trusted-ingress boundary applies: task history is deployment-wide.
Tenant-specific access requires corresponding authorization and query filtering
in the backend before deploying a multi-tenant workbench.

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
- Uploads send the browser `File` as multipart data and report actual XHR upload
  progress. A UUID is saved before sending bytes and reused when retrying the same
  file. A lost acknowledgement can be recovered by checking that UUID first.
  The upload transfer itself is not resumable: an input not yet committed must be
  sent again. Starting a new upload discards only the local pending identity.
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
- A separate result worker fetches, parses and retains the complete JSON. Only
  the requested page is copied to the UI thread. Worker memory still scales with
  the full result, while the UI avoids retaining or stringifying all pages.
- Text, canonical merged-cell tables and per-page/per-block JSON are rendered
  without interpreting document text as HTML. Complete JSON downloads stream
  directly from the persisted server result.

## API types

`src/api/schema.d.ts` is generated from utoipa rather than handwritten SDK types.
Normal builds use the checked-in declaration file and do not contact the backend.
After changing the server contract, run the server and regenerate:

```sh
rtk npm run api:generate --prefix packages/web
# With a different server or prefix:
rtk proxy env DOCPARSE_OPENAPI_URL=http://127.0.0.1:8091/api/openapi.json \
  npm run api:generate --prefix packages/web
```

The generator also reads `DOCPARSE_OPENAPI_URL` from `.env.local`.

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
