"""Verify a running CUDA service against the repository's real Texo reference crops."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path

import requests


def main():
    """Check reference parity, concurrent batching, invalid inputs, and recovery over real HTTP."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:6008")
    parser.add_argument("--fixtures", type=Path, default=Path(__file__).resolve().parents[2] / "crates/formula-texo/tests/fixtures")
    args = parser.parse_args()
    base = args.url.rstrip("/")
    response = requests.get(base + "/v1/health", timeout=10)
    response.raise_for_status()
    health = response.json()
    assert health["status"] == "ready" and health["providers"][0] == "CUDAExecutionProvider"
    assert health["use_cache"] and health["use_io_binding"]
    cases = json.loads((args.fixtures / "reference.json").read_text())["cases"]

    def recognize(case, max_tokens=1024):
        """Send a real crop without changing its pixels or the deployment's generation defaults."""
        with (args.fixtures / case["image"]).open("rb") as image:
            return requests.post(
                base + "/v1/predictions/upload",
                files={"image": (case["image"], image, "image/png")},
                data={"task": "formula", "max_tokens": str(max_tokens)}, timeout=120,
            )

    for case in cases:
        response = recognize(case)
        response.raise_for_status()
        assert response.json()["text"] == case["latex"], case["image"]
    work = cases * 6
    with ThreadPoolExecutor(max_workers=16) as pool:
        responses = list(pool.map(recognize, work))
    for case, response in zip(work, responses):
        response.raise_for_status()
        assert response.json()["text"] == case["latex"], case["image"]
    batches = sorted({response.json()["batch_size"] for response in responses})
    if health["batch_limit"] > 1:
        assert max(batches) > 1, "concurrent requests did not form a model batch"
    invalid = requests.post(base + "/v1/predictions/upload", files={"image": ("bad.png", b"not an image", "image/png")}, timeout=10)
    assert invalid.status_code == 400, invalid.status_code
    assert recognize(cases[0], max_tokens=2).status_code == 422
    recovered = recognize(cases[0])
    recovered.raise_for_status()
    assert recovered.json()["text"] == cases[0]["latex"]
    print(json.dumps({"reference_cases": len(cases), "concurrent_requests": len(work), "observed_batches": batches, "invalid_image": 400, "incomplete_generation": 422, "recovery": "passed"}))


if __name__ == "__main__":
    main()
