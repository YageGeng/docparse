# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Build a static visual-review site from deterministic E2E overlay selection."""

from __future__ import annotations

import argparse
import html
import json
import sys
from collections import defaultdict
from pathlib import Path


class VisualReviewError(RuntimeError):
    """Signals malformed selection data or missing overlay artifacts."""


def load_selection(run_dir: Path) -> list[dict]:
    """Load, validate, deduplicate, and sort selected page records."""
    path = run_dir / "overlay-selection.json"
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise VisualReviewError(f"failed to load {path}: {error}") from error
    if payload.get("schema_version") != 1 or not isinstance(payload.get("pages"), list):
        raise VisualReviewError("overlay selection must use schema_version 1 and pages[]")
    seen: set[tuple[str, int]] = set()
    pages = []
    reasons_by_document: dict[str, set[str]] = defaultdict(set)
    for index, page in enumerate(payload["pages"]):
        try:
            logical_id = page["logical_id"]
            page_number = page["page_number"]
            reasons = page["reasons"]
        except (KeyError, TypeError) as error:
            raise VisualReviewError(f"pages[{index}] is malformed: {error}") from error
        if (
            not isinstance(logical_id, str)
            or not logical_id
            or not isinstance(page_number, int)
            or page_number <= 0
            or not isinstance(reasons, list)
            or not all(isinstance(reason, str) and reason for reason in reasons)
        ):
            raise VisualReviewError(f"pages[{index}] has invalid identity or reasons")
        key = (logical_id, page_number)
        if key in seen:
            raise VisualReviewError(f"duplicate selected page {logical_id}/{page_number}")
        seen.add(key)
        reasons_by_document[logical_id].update(reasons)
        png = run_dir / "overlays" / logical_id / f"page-{page_number:04}.png"
        svg = run_dir / "overlays" / logical_id / f"page-{page_number:04}.svg"
        if not png.is_file() or not svg.is_file():
            raise VisualReviewError(
                f"missing overlay artifacts for {logical_id}/{page_number}"
            )
        pages.append(page)
    for logical_id, reasons in reasons_by_document.items():
        missing = {"first", "middle", "last"} - reasons
        if missing:
            raise VisualReviewError(
                f"selection for {logical_id} lacks required reasons {sorted(missing)}"
            )
    return sorted(pages, key=lambda page: (page["logical_id"], page["page_number"]))


def build_html(run_dir: Path, pages: list[dict]) -> str:
    """Render a self-contained index shell that links existing PNG/SVG artifacts."""
    cards = []
    for page in pages:
        logical_id = page["logical_id"]
        page_number = page["page_number"]
        relative = Path("..") / "overlays" / logical_id
        png = relative / f"page-{page_number:04}.png"
        svg = relative / f"page-{page_number:04}.svg"
        reasons = ", ".join(page["reasons"])
        metrics = (
            f"fallback={page.get('fallback_ratio', 0):.4f}, "
            f"assignment_conflicts={page.get('assignment_conflicts', 0)}, "
            f"removed_edges={page.get('removed_edges', 0)}"
        )
        cards.append(
            f'''<article><h2>{html.escape(logical_id)} · page {page_number}</h2>
<p>{html.escape(reasons)} · {html.escape(metrics)}</p>
<div class="pair"><figure><figcaption>Original render</figcaption><img src="{html.escape(png.as_posix())}"/></figure>
<figure><figcaption>Diagnostic overlay</figcaption><object data="{html.escape(svg.as_posix())}" type="image/svg+xml"></object></figure></div></article>'''
        )
    return f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>DocParse visual review</title><style>
body{{font:14px system-ui,sans-serif;margin:0;background:#111827;color:#e5e7eb}}main{{max-width:1500px;margin:auto;padding:24px}}
article{{background:#1f2937;margin:0 0 24px;padding:16px;border-radius:10px}}h1,h2{{margin-top:0}}.pair{{display:grid;grid-template-columns:1fr 1fr;gap:16px}}
figure{{margin:0}}figcaption{{margin-bottom:8px;color:#93c5fd}}img,object{{width:100%;height:78vh;object-fit:contain;background:white}}
@media(max-width:900px){{.pair{{grid-template-columns:1fr}}}}</style></head>
<body><main><h1>DocParse visual review</h1>{''.join(cards)}</main></body></html>
'''


def build_markdown(pages: list[dict], status: str) -> str:
    """Create the explicit human checklist with one reviewer-selected status."""
    lines = [
        "# DocParse visual review",
        "",
        "Status values: `pass`, `fail`, or `needs-investigation`.",
        "",
        "| Document | Page | Reasons | Status | Note |",
        "|---|---:|---|---|---|",
    ]
    for page in pages:
        reasons = ", ".join(page["reasons"]).replace("|", "\\|")
        lines.append(
            f"| {page['logical_id']} | {page['page_number']} | {reasons} | {status} | Check columns, titles, tables, captions, formulas, chrome, missing/duplicate text, and order. |"
        )
    lines.extend(
        [
            "",
            "## Checklist",
            "",
            "- Cross-column reading order",
            "- Full-width title placement",
            "- Table visual order",
            "- Caption/object locality",
            "- Inline formula position",
            "- Repeated header/footer handling",
            "- Missing or duplicated text",
            "- Block order labels",
            "",
        ]
    )
    return "\n".join(lines)


def build_review(run_dir: Path, status: str = "needs-investigation") -> Path:
    """Validate selected overlays and write deterministic HTML and Markdown review files."""
    pages = load_selection(run_dir)
    output = run_dir / "visual-review"
    output.mkdir(parents=True, exist_ok=True)
    (output / "index.html").write_text(build_html(run_dir, pages), encoding="utf-8")
    (output / "review.md").write_text(
        build_markdown(pages, status), encoding="utf-8"
    )
    return output


def parse_args() -> argparse.Namespace:
    """Parse the completed E2E run directory."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument(
        "--status",
        choices=["pass", "fail", "needs-investigation"],
        default="needs-investigation",
        help="explicit reviewer verdict written to every selected page",
    )
    return parser.parse_args()


def main() -> int:
    """Build the review site or return a concise validation failure."""
    arguments = parse_args()
    try:
        output = build_review(arguments.run_dir.resolve(), arguments.status)
    except VisualReviewError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"visual review written to {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
