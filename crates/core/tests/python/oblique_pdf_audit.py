"""Audit a PDF with one repeated oblique overlay per native-text page.

This document-specific acceptance check complements the synthetic geometry tests;
it does not assume that every oblique line in an arbitrary PDF is a watermark.
"""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path


def source_items(page: dict) -> list[dict]:
    """Recover stable source order independently of the resulting block and line order."""
    return sorted(
        (item for block in page["blocks"] for line in block["lines"] for item in line["text_items"]),
        key=lambda item: item["extraction_order"],
    )


def audit(before: dict, after: dict, overlays: Path, expected_text: str) -> list[dict]:
    """Check every page, reporting pages without native text separately from overlay acceptance."""
    assert not after["errors"], "The document contains page-level failures"
    assert len(before["pages"]) == len(after["pages"]), "Page count changed"
    assert [page["page_number"] for page in after["pages"]] == list(range(1, len(after["pages"]) + 1)), "Page sequence is incomplete"
    assert after["context"]["page_count"] == len(after["pages"]), "Document context has the wrong page count"
    rows = []
    for old, page in zip(before["pages"], after["pages"], strict=True):
        number = page["page_number"]
        for field in ("page_number", "width", "height", "rotation"):
            assert old[field] == page[field], f"Page {number}: {field} changed"
        old_regions = sorted((block["model_region_id"], block["label"]) for block in old["blocks"] if block.get("model_region_id"))
        regions = sorted((block["model_region_id"], block["label"]) for block in page["blocks"] if block.get("model_region_id"))
        assert old_regions == regions, f"Page {number}: model regions were removed or relabeled"
        old_items, items = source_items(old), source_items(page)
        assert "".join(item["raw_text"] for item in old_items) == "".join(item["raw_text"] for item in items), f"Page {number}: raw source text changed"
        ordinary = []
        for snapshot in (old_items, items):
            ordinary.append(Counter(
                json.dumps({key: value for key, value in item.items() if key not in ("id", "extraction_order", "final_order")}, sort_keys=True, ensure_ascii=False)
                for item in snapshot if not 2 < item["rotation"] % 90 < 88
            ))
        assert ordinary[0] == ordinary[1], f"Page {number}: ordinary text facts changed"
        oblique = [item for item in items if 2 < item["rotation"] % 90 < 88]
        blocks = [block for block in page["blocks"] if any(2 < item["rotation"] % 90 < 88 for line in block["lines"] for item in line["text_items"])]
        if oblique:
            assert len(blocks) == 1, f"Page {number}: oblique text is split across {len(blocks)} blocks"
            block = blocks[0]
            assert not block.get("model_region_id"), f"Page {number}: overlay still belongs to a model region"
            assert block["label_source"] == "Fallback" and block["label"] == "text", f"Page {number}: overlay has an unrelated semantic label"
            assert len(block["lines"]) == 1, f"Page {number}: overlay is split across lines"
            assert all(2 < item["rotation"] % 90 < 88 for line in block["lines"] for item in line["text_items"]), f"Page {number}: ordinary text is mixed with the overlay"
            # Ignore spacing only when recognizing the phrase; exact source text is checked above.
            assert "".join(block["text"].split()) == "".join(expected_text.split()), f"Page {number}: overlay text is incomplete or reordered"
        for suffix in ("png", "svg"):
            assert (overlays / f"page-{number:04}.{suffix}").is_file(), f"Page {number}: missing {suffix} overlay"
        rows.append({
            "page": number,
            "source_text_preserved": True,
            "ordinary_facts_preserved": True,
            "ordinary_characters": sum(len(item["raw_text"]) for item in items if not 2 < item["rotation"] % 90 < 88),
            "native_items": sum(item["source"] == "Native" for item in items),
            "oblique_items": len(oblique),
            "overlay_blocks": [block["id"] for block in blocks],
            "overlay_text": blocks[0]["text"] if blocks else None,
            "status": "native_overlay_passed" if oblique else "no_native_text" if not items else "no_oblique_text",
            "warnings": [warning["code"] for warning in page["warnings"]],
            "model_order_changed": [block["model_region_id"] for block in old["blocks"] if block.get("model_region_id")] != [block["model_region_id"] for block in page["blocks"] if block.get("model_region_id")],
        })
    return rows


def main() -> None:
    """Load explicit local artifacts and persist the complete page-by-page audit result."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--overlays", type=Path, required=True)
    parser.add_argument("--expected-text", required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    before_bytes, after_bytes = args.before.read_bytes(), args.after.read_bytes()
    pages = audit(json.loads(before_bytes), json.loads(after_bytes), args.overlays, args.expected_text)
    report = {
        "before_sha256": hashlib.sha256(before_bytes).hexdigest(),
        "after_sha256": hashlib.sha256(after_bytes).hexdigest(),
        "status": "passed",
        "pages": pages,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(f"Audited {len(pages)} pages: {dict(Counter(page['status'] for page in pages))}")
    print(f"Report: {args.report}")


if __name__ == "__main__":
    main()
