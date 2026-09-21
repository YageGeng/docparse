# Texo with Optimum and ONNX CUDA

Standalone formula-image recognition server, extracted from the verified gpuhub
deployment. It runs `alephpi/FormulaNet` through
`ORTModelForVision2Seq.generate()` with CUDA, KV cache, and Optimum I/O Binding.

## Setup

Use Linux x86_64, an NVIDIA CUDA GPU with a compatible driver, and `uv`.
Python 3.12 and the tested dependencies are pinned in `pyproject.toml` and
`uv.lock`. PyTorch comes from the CUDA 12.8 wheel index. This deployment does not
provide a CPU or macOS inference fallback.

```sh
cd optimum-texo
uv sync --locked
uv run --locked python prepare_model.py
./start.sh
```

The startup script works from any directory and defaults to `0.0.0.0:6008`.
Override `HOST`, `PORT`, or `CUDA_VISIBLE_DEVICES` when needed. Keep one Uvicorn
worker: `SessionManager` creates the independent model owners inside that process.
Edit `config.toml` to control concurrency and memory use:

```toml
session_size = 2
queue_size = 128
batch_size = 16
```

The shipped default is one owner. Each owner loads a complete model with three
ONNX sessions (encoder, first-step decoder, cached decoder) on a dedicated thread.
Increasing `session_size` duplicates model and cache memory; throughput depends
on available GPU resources. `queue_size` counts admitted pending images, excluding
uploads waiting for admission and up to `session_size * batch_size` images already
held by consumers. All values must be positive integers, and `batch_size` is
limited to 32. Unknown keys fail startup.
These settings replace the former `TEXO_BATCH_SIZE` environment variable.

```sh
CUDA_VISIBLE_DEVICES=0 PORT=6008 ./start.sh
# Select another configuration file with an absolute path.
TEXO_CONFIG=/path/to/config.toml ./start.sh
```

All owners consume the same bounded request queue. When it is full, HTTP requests
await a free slot without blocking the event loop or returning a queue-full 503.
Waiting uploads retain their decoded images until admission or cancellation.
Each owner drains ready uploads up to its batch limit; sparse uploads get a
3 ms collection window, while full ready
batches start immediately. Equal generation limits are grouped together. Canceled
queued requests are skipped, and one failed batch does not stop other owners.
HTTP disconnects cancel admission waits and the corresponding queued reply;
completed batch images are released before a consumer waits for new work.
Already-running native inference retains its inputs until it completes.
The service becomes ready only after every model has loaded. Partial startup
failure releases earlier owners; shutdown rejects admission waiters and queued
callers and waits for running native inference to finish without blocking the API
event loop.
ORT graph optimization is `all`; intra-op thread settings remain at ORT defaults.
Model files and virtual environments are ignored by Git.

## Model preparation

The downloader pins revision `b2668efe5112082846fde4d446b9bfaab3989533` and fetches
only the ONNX graphs and tokenizer/configuration files. Original assets stay in
`models/FormulaNet/onnx`; adapted assets go to `models/FormulaNet-optimum`.
Run preparation before starting the service, not while it is loading models.

`runtime.py` registers `my_hgnetv2` with Transformers and applies the Optimum
compatibility registration:

```python
NormalizedConfigManager._conf["my-hgnetv2"] = NormalizedTextConfig
```

The encoder's symbolic sequence length cannot be inferred by this Optimum
I/O Binding implementation. Preparation verifies the real CUDA output for a
384×384 input, then sets encoder metadata to `[batch, 3, 384, 384]` input and
`[batch, 144, 2048]` output. Batch remains dynamic; weights and graph operations
are unchanged. `adaptation.json` records the revision, output shape, and hashes.
The decoder uses separate first-step and cached graphs (`use_merged=False`).

## HTTP API

- `GET /v1/health` (also `/health`): readiness, providers, and queue occupancy.
- `GET /v1/models`: served model identity.
- `POST /v1/predictions/upload`: multipart PNG/image input and JSON text output.
- `/docs`: interactive API documentation.

Health responses also include `session_size`, `queue_size`, `batch_size`,
`active_sessions`, and the total `active_images` across consumers.

```sh
curl --fail http://127.0.0.1:6008/v1/predictions/upload \
  -F 'image=@formula.png' -F 'task=formula' -F 'max_tokens=1024'
```

`task` accepts `formula`, `ocr`, or `ocr_plain`; all recognize formulas.
`query` is accepted for compatibility and ignored. There is no text-prompt or
Chat Completions API. Each HTTP request contains one image; the server batches
concurrent uploads internally.

`max_tokens` limits total sequence length including BOS (2–1024). Unfinished
generations return 422 rather than truncated LaTeX. Invalid images return 400,
empty/oversized uploads 413, unavailable sessions or failed inference 503, and
requests exceeding 120 seconds 504. The deadline covers queue admission, queued
waiting, and inference. Uploads are limited to 16 MiB and 16 megapixels.
After margin cropping, preprocessing also limits the intermediate resized image
to 16,777,216 pixels. Crops exceeding this limit return 422 without failing
other images in the same batch.
Responses contain `text`, `output_tokens`, `batch_size`, `queue_ms`, and
`inference_time_ms`. `queue_ms` includes waiting for admission;
`inference_time_ms` is shared batch wall time. CORS and authentication are not
configured by this service; browser deployments can provide them at a proxy.

## DocParse configuration

```toml
[[formula.engine]]
type = "http"
server_url = "http://127.0.0.1:6008"
worker_size = 32
```

Omit `prompt` and `batch_size` for this HTTP consumer. DocParse's `worker_size`
controls concurrent uploads; `config.toml` independently controls the session
count, pending queue capacity, and GPU batch limit inside this Python service.

## Verification

From this directory, install the locked dependencies and check all Python scripts
and tests. Pyright uses the project's Python 3.12 `.venv` via `pyproject.toml`:

```sh
uv sync --locked
uvx pyright --project .
uvx ruff check .
uvx ruff format --check .
```

The CPU-only regression checks cover intermediate allocation limits, isolation
of invalid crops in a batch, unchanged reference preprocessing pixels, TOML
validation, concurrent owners, queue saturation, cancellation, and teardown:

```sh
uv run --locked python -m unittest discover -s tests -p 'test_*.py'
```

With the service running, the check uses the repository's three real reference
images, compares exact LaTeX, sends 18 requests at concurrency 16, and verifies
invalid-image rejection, incomplete-generation rejection, and recovery:

```sh
uv run --locked python tests/smoke.py --url http://127.0.0.1:6008
```

When copying this folder without the rest of the repository, supply
`--fixtures /path/to/formula-texo/tests/fixtures` explicitly.

### Local throughput check

On an RTX 4060 Laptop GPU (8 GiB), the three reference crops repeated over
96 requests at HTTP concurrency 32 gave the following median throughput across
three warmed runs, with `queue_size = 128` and `batch_size = 16`:

| Session owners | Images/second |
| --- | --- |
| 1 | 21.38 |
| 2 | 24.69 |

Every response matched the reference LaTeX. The roughly 15% gain is specific to
this small workload and device; it is not a general PDF benchmark. The default
remains one owner so deployments can choose additional model memory explicitly.
