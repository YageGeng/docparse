#!/usr/bin/env bash
# Start one API process; config.toml controls the independent CUDA consumers.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
export CUDA_VISIBLE_DEVICES="${CUDA_VISIBLE_DEVICES:-0}"
export PYTHONUNBUFFERED=1
exec uv run --locked uvicorn service:app --host "${HOST:-0.0.0.0}" --port "${PORT:-6009}" --workers 1
