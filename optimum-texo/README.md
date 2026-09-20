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
worker: each process loads its own CUDA model. `TEXO_BATCH_SIZE` controls the
server's maximum tensor batch size (default 16, range 1–32).

```sh
CUDA_VISIBLE_DEVICES=0 PORT=6008 TEXO_BATCH_SIZE=16 ./start.sh
```

The server gathers ready uploads for 3 ms, groups equal generation limits, and
processes them through one model owner. Its pending queue holds 128 images.
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
empty/oversized uploads 413, full queues or failed inference 503, and requests
exceeding 120 seconds 504. Uploads are limited to 16 MiB and 16 megapixels.
Responses contain `text`, `output_tokens`, `batch_size`, `queue_ms`, and
`inference_time_ms`; the latter is shared batch wall time. CORS and authentication
are not configured by this service; browser deployments can provide them at a proxy.

## DocParse configuration

```toml
[[formula.engine]]
type = "http"
server_url = "http://127.0.0.1:6008"
worker_size = 32
```

Omit `prompt` and `batch_size` for this HTTP consumer. DocParse's `worker_size`
controls concurrent uploads; `TEXO_BATCH_SIZE` independently controls GPU batches
inside this Python service.

## Verification

With the service running, the check uses the repository's three real reference
images, compares exact LaTeX, sends 18 requests at concurrency 16, and verifies
invalid-image rejection, incomplete-generation rejection, and recovery:

```sh
uv run --locked python tests/smoke.py --url http://127.0.0.1:6008
```

When copying this folder without the rest of the repository, supply
`--fixtures /path/to/formula-texo/tests/fixtures` explicitly.
