# HTTP workbench validation

The implementation uses the production `docparse-server` release CUDA binary,
an isolated PostgreSQL container, shared filesystem storage, and the user's headed
Chrome browser. No fixture parsing backend was used for browser acceptance.

## Backend checks

- Native workspace tests: 384 passed, 23 ignored.
- Explicit isolated PostgreSQL integration run: all 9 tests passed, covering
  metadata immutability, cursor ordering, lease recovery, PDF ranges, HEAD,
  source size headers, SSE reconnect and error envelopes.
- Strict workspace Clippy passed with the browser-only Rust crate excluded.
- The metadata migration was generated with sea-orm-cli and exercised against
  PostgreSQL. All schema and query changes use SeaORM/SeaQuery.

## Headed browser checks

| Scenario | Evidence |
| --- | --- |
| Real large PDF | `2303.18223v16.pdf`: 144 pages, 8.2 MB input; native result has no page errors |
| Source preview | Original PDF is read from the server; page 1 and page 8 render with matching region overlays |
| Selection and refresh | Job `54c0e1c8-eacf-484a-a253-7ba0cd0ecc9c`, page 1, block `p1:b:m9:s0` remained selected after reload |
| Structured tables | Page 8 displays the actual 58-row, 13-column table, including merged cells |
| Responsive inspection | At 375 × 812, original/result views switch correctly and root scroll width equals viewport width |
| Queued refresh | Job `d623895b-6dfc-43a9-8e4b-8ad899cb6703` survives reload while the real server runs API-only |
| Offline/reconnect | Offline network emulation displays the reconnect state; restoring connectivity removes it and reloads failed PDF range requests |
| API replacement | The same persisted job transitions through SSE snapshots `queued` (version 1), `running/analyzing` (version 4), `succeeded/complete` (version 6) after replacing the API instance with the configured CUDA worker role |
| JSON download | `2609.04184v1.json`, 5,729,385 bytes, 8 pages; its SHA-256 matches the server's persisted result byte-for-byte |

## Large-result handling

The 144-page document produces a 117 MB JSON result. A dedicated browser worker
owns fetching, decoding and retaining that document, and sends only the requested
page to the UI. User-initiated export reads the persisted JSON directly from the
server. Worker memory and the initial result download still scale with the full
result size.

The entry bundle is approximately 352 KB before gzip; the PDF/inspection route
is loaded separately. PDF.js assets and both worker scripts are self-hosted.
The production build was also checked at port 4173, including worker-backed page
changes and PDF rendering after reconnecting to the persisted job. PDF.js ICC
profiles are included. Asset preparation is idempotent for the installed package
version; repeated builds preserve the development server's public-asset index.

## Local evidence

The ignored `target/http-web-e2e/` directory contains the isolated server
configuration and persisted inputs/results. The browser download is at
`~/Downloads/2609.04184v1.json`. The user's configured application database and
their independent WASM logging changes were not modified for this validation.
