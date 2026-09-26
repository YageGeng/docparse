"""Measure the served model on clean, in-distribution charts with known values.

The three charts prepared for this deployment are demanding: one has dual value axes, one is an
annotated histogram, and one combines three jobs with a legend and a statistics box. Bad results
there could mean a hard input rather than a weak model, so this check renders nothing itself and
instead replays three plain matplotlib charts whose every value is known, against the running
service. It reports both what the model labelled and what it read, and it is deliberately a
reporting tool: the numbers are the finding, so it fails only when the service itself misbehaves.

Regenerate the fixtures with matplotlib at 100 dpi:

    chart, values                     title
    six bars, 120/200/150/80/230/90   Quarterly Revenue by Region
    twelve points, 10..35             Monthly Sales in 2023
    five slices, 35/25/20/12/8        Market Share by Product
"""

import argparse
import json
from pathlib import Path

import requests

DEFAULT_URL = "http://127.0.0.1:6009"
FIXTURES = Path(__file__).resolve().parent / "controls"
# Values written into each fixture when it was rendered; labels are the x-axis categories.
TRUTH = {
    "bar.png": {
        "North": 120.0,
        "South": 200.0,
        "East": 150.0,
        "West": 80.0,
        "Central": 230.0,
        "Nordic": 90.0,
    },
    "line.png": {
        "Jan": 10.0,
        "Feb": 14.0,
        "Mar": 12.0,
        "Apr": 18.0,
        "May": 22.0,
        "Jun": 19.0,
        "Jul": 25.0,
        "Aug": 24.0,
        "Sep": 28.0,
        "Oct": 31.0,
        "Nov": 27.0,
        "Dec": 35.0,
    },
    "pie.png": {
        "Alpha": 35.0,
        "Beta": 25.0,
        "Gamma": 20.0,
        "Delta": 12.0,
        "Epsilon": 8.0,
    },
}


def numeric_rows(table: dict) -> list[dict]:
    """Flatten the recovered table into rows whose value parses as a number."""
    rows = []
    for entries in table.values():
        for row in entries:
            try:
                rows.append({"label": row["label"], "value": float(row["value"])})
            except (KeyError, TypeError, ValueError):
                continue
    return rows


def score(name: str, payload: dict) -> dict:
    """Compare one response against the fixture's known labels and values."""
    expected = TRUTH[name]
    rows = numeric_rows(payload["table"])
    # A label hit needs the right number too; a value hit is order-insensitive, because a chart
    # can be read correctly but written back with mangled category names.
    labelled = sum(
        any(
            row["label"].strip().lower() == label.lower()
            and abs(row["value"] - value) < 0.5
            for row in rows
        )
        for label, value in expected.items()
    )
    unclaimed = list(rows)
    valued = 0
    for value in expected.values():
        match = next(
            (row for row in unclaimed if abs(row["value"] - value) < 0.5), None
        )
        if match is not None:
            unclaimed.remove(match)
            valued += 1
    return {
        "tokens": payload["output_tokens"],
        "valid_json": payload["data"] is not None,
        "has_values_key": '"values"' in payload["text"],
        "rows": len(rows),
        "expected": len(expected),
        "labelled_correctly": labelled,
        "values_present": valued,
        "title": payload["text"].splitlines()[1].strip()
        if len(payload["text"].splitlines()) > 1
        else "",
    }


def main() -> None:
    """Send every control fixture and report its label and value agreement."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=DEFAULT_URL)
    parser.add_argument("--max-new-tokens", type=int, default=1024)
    args = parser.parse_args()
    base = args.url.rstrip("/")
    requests.get(base + "/v1/health", timeout=10).raise_for_status()
    report = {}
    for name in TRUTH:
        with (FIXTURES / name).open("rb") as image:
            response = requests.post(
                base + "/v1/predictions/upload",
                files={"image": (name, image, "image/png")},
                data={"max_new_tokens": str(args.max_new_tokens)},
                timeout=120,
            )
        response.raise_for_status()
        payload = response.json()
        result = score(name, payload)
        report[name] = result
        print(
            f"{name:9s} labels {result['labelled_correctly']}/{result['expected']}  "
            f"values {result['values_present']}/{result['expected']}  "
            f"rows={result['rows']:3d}  json={result['valid_json']!s:5s}  "
            f"values_key={result['has_values_key']!s:5s}  title={result['title'][:38]!r}",
            flush=True,
        )
        print(f"          text: {payload['text'][:160]!r}", flush=True)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
