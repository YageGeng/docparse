"""Verify a running CUDA service against the real chart images prepared for this deployment."""

import argparse
import json
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import requests

# The charts prepared for this deployment live outside the repository.
DEFAULT_IMAGES = Path("/home/isbest/Pictures/屏幕截图")
DEFAULT_REFERENCE = Path(__file__).resolve().parent / "reference.json"
# Concurrency above the shipped batch limit still exercises queueing and cancellation; it does
# not form a model batch, so the batching assertion below only applies when the limit exceeds 1.
CONCURRENCY = 4
# The pinned magnitudes are this deployment's own output, so the HTTP round trip may only add
# float formatting noise. `tests/parity.py` compares the same quantity against PyTorch, where
# the tolerance is larger because that comparison spans two different runtimes.
MAGNITUDE_TOLERANCE = 1e-5


def main():
    """Check reference parity, concurrent batching, invalid inputs, and recovery over real HTTP."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:6009")
    parser.add_argument("--images", type=Path, default=DEFAULT_IMAGES)
    parser.add_argument("--reference", type=Path, default=DEFAULT_REFERENCE)
    args = parser.parse_args()
    base = args.url.rstrip("/")
    response = requests.get(base + "/v1/health", timeout=10)
    response.raise_for_status()
    health = response.json()
    assert health["status"] == "ready"
    assert health["providers"][0] == "CUDAExecutionProvider"
    assert health["use_cache"] and health["use_io_binding"]
    # Verify that the live service exposes the validated consumer and queue settings.
    assert health["session_size"] >= 1 and health["queue_size"] >= 1
    assert 1 <= health["batch_size"] == health["batch_limit"] <= 32
    cases = json.loads(args.reference.read_text())["cases"]

    def extract(case):
        """Send a real chart without changing its pixels or the deployment's budget."""
        with (args.images / case["image"]).open("rb") as image:
            return requests.post(
                base + "/v1/predictions/upload",
                files={"image": (case["image"], image, "image/png")},
                data={"max_new_tokens": str(case["max_new_tokens"])},
                timeout=120,
            )

    for case in cases:
        response = extract(case)
        response.raise_for_status()
        payload = response.json()
        # The pinned text and table are the regression contract; the schema check keeps the
        # service from silently returning something other than a chart dictionary.
        assert payload["text"] == case["text"], case["image"]
        assert payload["table"] == case["table"], case["image"]
        assert payload["text"].startswith("{"), case["image"]
        assert '"title"' in payload["text"], case["image"]
        assert payload["output_tokens"] > 0, case["image"]
        # The auxiliary head's magnitudes feed the reliability check, so they must survive the
        # HTTP round trip; a step-alignment regression would change them without changing text.
        magnitudes = payload["magnitudes"]
        expected = case["magnitudes"]
        assert len(magnitudes) == len(expected), case["image"]
        assert all(
            abs(actual - want) < MAGNITUDE_TOLERANCE
            for actual, want in zip(magnitudes, expected)
        ), case["image"]
    work = cases * 6
    with ThreadPoolExecutor(max_workers=CONCURRENCY) as pool:
        responses = list(pool.map(extract, work))
    for case, response in zip(work, responses):
        response.raise_for_status()
        payload = response.json()
        assert payload["text"] == case["text"], case["image"]
        assert payload["table"] == case["table"], case["image"]
    extracted = sum(
        len(entries) for case in cases for entries in case["table"].values()
    )
    assert extracted > 0, "no chart table rows were extracted from the real charts"
    batches = sorted({response.json()["batch_size"] for response in responses})
    if health["batch_limit"] > 1:
        assert max(batches) > 1, "concurrent requests did not form a model batch"
    invalid = requests.post(
        base + "/v1/predictions/upload",
        files={"image": ("bad.png", b"not an image", "image/png")},
        timeout=10,
    )
    assert invalid.status_code == 400, invalid.status_code
    recovered = extract(cases[0])
    recovered.raise_for_status()
    payload = recovered.json()
    assert payload["text"] == cases[0]["text"]
    assert payload["table"] == cases[0]["table"]
    print(
        json.dumps(
            {
                "reference_cases": len(cases),
                "table_rows": extracted,
                "concurrent_requests": len(work),
                "observed_batches": batches,
                "session_size": health["session_size"],
                "queue_size": health["queue_size"],
                "batch_size": health["batch_size"],
                "invalid_image": 400,
                "recovery": "passed",
            }
        )
    )


if __name__ == "__main__":
    main()
