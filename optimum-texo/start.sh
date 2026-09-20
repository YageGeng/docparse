#!/usr/bin/env bash
# Start one CUDA model owner; concurrent uploads are batched inside the service.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
export CUDA_VISIBLE_DEVICES="${CUDA_VISIBLE_DEVICES:-0}"
export PYTHONUNBUFFERED=1
exec uv run --locked uvicorn service:app --host "${HOST:-0.0.0.0}" --port "${PORT:-6008}" --workers 1
