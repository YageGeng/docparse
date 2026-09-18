# MinerU formula recognition

`MineruEngine` implements `docparse_formula::FormulaEngine` for native and WASM parsers
using the external MinerU2.5 vLLM OpenAI-compatible service. It sends one PNG
crop per `/v1/chat/completions` request with the formula-recognition prompt and
sampling parameters used by `mineru-vl-utils`. No Python client or local formula
model files are needed. The server must expose model ID `MinerU2.5-2509-1.2B`
and load `mineru_vl_utils:MinerULogitsProcessor`.

Replace the existing formula engine section in your configuration:

```toml
[formula]
inline_enabled = true
display_enabled = true
batch_size = 16
timeout_ms = 120000

[formula.engine]
type = "mineru"
server_url = "http://127.0.0.1:8000"
concurrency = 8
```

`server_url` accepts an HTTP(S) service root or a base ending in `/v1`, including
reverse-proxy prefixes. Credentials, query parameters, and fragments are rejected.
It defaults to `http://127.0.0.1:8000`; concurrency defaults to 8 and must be
between 1 and 1024. Overrides use `DOCPARSE_FORMULA__ENGINE__SERVER_URL` and
`DOCPARSE_FORMULA__ENGINE__CONCURRENCY`.

Concurrency limits all requests sharing one engine, across pages and documents.
A bounded crop queue feeds HTTP slots continuously: as one request finishes,
ready work can start without waiting for its original caller batch. Core uses a
shared pre-crop admission budget of twice the HTTP concurrency and preserves
per-formula errors. `formula.batch_size` governs local model batching and does
not limit this HTTP queue. `formula.timeout_ms` covers each parser crop, including
admission, PNG encoding, HTTP, and decoding; direct multi-image `recognize` calls
retain their whole-call deadline and ordered result contract.
Canceling a caller drops its requests and releases permits; already-started
blocking PNG encoding retains its permit until it finishes. No automatic retries
or local-engine fallback are performed.

Before PNG encoding, extreme crops receive centered white padding to bound
their aspect ratio to 50, followed by bicubic upscaling when the shorter edge is
below 28 pixels. This preserves the formula pixels and follows MinerU's input
preparation order.

Results retain input order. Empty output, malformed JSON, unsuccessful HTTP
responses, oversized responses, and non-`stop` completions fail explicitly.
The trailing `<|im_end|>` protocol marker is removed before matching outer
`\[...\]`, `\(...\)`, `$$...$$`, or `$...$` delimiters and checking for empty
content; internal LaTeX is preserved. Existing parser error/Markdown handling
continues to apply. Logs contain lifecycle events and error context, never image
payloads or recognized LaTeX. Browser builds use Fetch with the same shared
admission limit and a cancellable Worker deadline; no Tokio timer is required.

In the WASM SDK, select `config.formula.engine = { type: "mineru", server_url:
"https://mineru.example.com/v1", concurrency: 8 }`. Do not supply
`formulaArtifacts`. The example exposes the service URL and concurrency when
MinerU is selected, and rebuilds the parser when either value changes. Only
formula crops are uploaded; the rest of the PDF pipeline remains in the Worker.
The service must allow CORS from the page's origin. HTTPS pages require an
HTTPS-compatible service address under the browser's mixed-content policy.
When both inline and display recognition are disabled, unused MinerU service
settings are ignored and no formula requests or model downloads are made.
Enabling either kind restores service-setting validation.

Recognize cropped images directly:

```bash
rtk cargo run -p docparse-formula-mineru --example recognize -- \
  http://127.0.0.1:8000 8 formula.png
```

For the gpuhub deployment, run `ssh -N -L 18000:127.0.0.1:8000 gpuhub`
locally and use `http://127.0.0.1:18000`. A parser running on gpuhub can use
port 8000 directly.

```bash
rtk cargo test -p docparse-config --test mineru
rtk cargo test -p docparse-formula-mineru
```

The HTTP integration tests cover request serialization, global concurrency,
result order, cancellation, deadlines, and invalid responses without loading
ONNX artifacts.
