# HTTP formula recognition

`HttpEngine` implements `docparse_formula::FormulaEngine` for native and WASM
parsers using one shared `FormulaQueue`. No local formula weights are loaded by HTTP groups. Other configured local groups
are preloaded normally and consume the same shared queue.
The crate is `docparse-formula-http`; migrate the former `mineru` engine type to
`http` and explicitly set a prompt for chat models.

## Image-only services

For Texo Optimum deployed on port 6008:

```toml
[formula]
queue_size = 32
inline_enabled = true
display_enabled = true
timeout_ms = 120000

[[formula.engine]]
type = "http"
server_url = "http://127.0.0.1:6008"
worker_size = 32
```

Omitting `prompt` selects `POST /v1/predictions/upload`, using multipart fields
`image` (PNG) and `task=formula`. The response must be a JSON object with a
non-empty `text` string. Crop dimensions are preserved; the service owns model
preprocessing. Incomplete generations must return an unsuccessful HTTP status,
as the Texo service does with 422. This is a specific upload contract, not a
universal protocol supported by every image-to-text server.

## Prompted chat models

Replace the engine section for a MinerU or other OpenAI-compatible vision model:

```toml
[[formula.engine]]
type = "http"
server_url = "http://127.0.0.1:8000"
worker_size = 32
prompt = "\nFormula Recognition:"
model = "MinerU2.5-2509-1.2B"
```

A configured, non-blank `prompt` selects `POST /v1/chat/completions`. The user
message contains the PNG data URL and the exact configured prompt. `model`
defaults to `MinerU2.5-2509-1.2B` and is ignored in image-only mode. The adapter
sends deterministic, non-streaming requests with `temperature=0`; it does not
require vLLM-specific logits processors or sampling extensions.

The prompted path retains the former MinerU image preparation: centered white
padding limits aspect ratios to 50, then bicubic resizing raises edges shorter
than 28 pixels. The service must return exactly one choice with
`finish_reason="stop"`; partial generations are rejected.

## Configuration and scheduling

`server_url` accepts an HTTP(S) service root or `/v1` base, including reverse-proxy
prefixes, without credentials, query parameters, or fragments. Its default is
`http://127.0.0.1:6008`. Worker count defaults to 1 and accepts 1 through 1024.
`DOCPARSE_FORMULA__ENGINE` replaces the complete array of consumer settings.

`worker_size` is the number of asynchronous HTTP consumers sharing the bounded
formula queue, not the number of local ONNX sessions or remote model instances.
Each consumer processes one crop and immediately takes the next ready crop.
All pages and documents using the same parser share this limit. The parser's
pre-crop admission budget is `formula.queue_size + sum(active crops across all groups)`;
Local entries accept `batch_size`; HTTP entries reject it. Remote services decide
how concurrent uploads form GPU batches.

Backpressure propagates through pending HTTP responses: when the remote Texo queue
is full, the upload waits for a free slot while retaining its HTTP worker. Once
all workers are occupied, the shared formula queue fills and its bounded sender
awaits capacity. The parser's pre-crop permits remain held until recognition ends,
so further crops wait before allocating pixels. This uses asynchronous suspension;
no retry loop or extra client-side request queue is needed. Existing request
deadlines and cancellation still terminate waiting work. The optional
`formula.backpressure` policy separately controls inline-formula shedding and is
disabled by default; bounded queue waiting works independently of that policy.

Results preserve caller order. Deadlines include queue admission, PNG encoding,
and the HTTP round trip. Cancellation releases HTTP slots; an already-running
PNG encoder retains its permit until it finishes. The adapter does not retry or
fall back to a local model. Both protocols reject malformed, empty, oversized
(over 1 MiB), and unsuccessful responses. Matching outer math delimiters and
trailing `<|im_end|>` are removed. Logs never include prompts, pixels, or LaTeX.

WASM uses the same settings: `[{ type: "http", server_url: "https://example.com",
worker_size: 32 }]`, optionally with `prompt` and `model`. No `formulaArtifacts`
are needed. The service must permit the browser page's origin through CORS.
Both recognition switches being off bypasses unused service validation.

## Verification

```bash
rtk cargo test -p docparse-config --test http
rtk cargo test -p docparse-formula-http
rtk cargo run -p docparse-formula-http --example recognize -- \
  http://127.0.0.1:6008 32 formula.png
```

The direct example also accepts optional `FORMULA_PROMPT` and `FORMULA_MODEL`
environment variables for chat services. Tests exercise both wire protocols,
custom prompts and model IDs, shared worker_size, continuous queue refill,
ordered results, cancellation, deadlines, and upstream error recovery.
