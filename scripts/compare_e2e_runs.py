#!/usr/bin/env python3
"""Compare two deterministic DocParse E2E canonical-hash manifests."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


class ComparisonError(RuntimeError):
    """Signals malformed input or the first stable canonical mismatch."""


def load_run(path: Path) -> dict[str, Any]:
    """Load and minimally validate one canonical hash manifest."""
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ComparisonError(f"failed to load {path}: {error}") from error
    if not isinstance(payload, dict) or payload.get("schema_version") != 1:
        raise ComparisonError(f"schema_version mismatch at {path}")
    documents = payload.get("documents")
    if not isinstance(documents, list):
        raise ComparisonError(f"documents must be a list at {path}")
    seen_documents: set[str] = set()
    for document_index, document in enumerate(documents):
        if not isinstance(document, dict) or not isinstance(
            document.get("logical_id"), str
        ):
            raise ComparisonError(
                f"documents[{document_index}].logical_id is invalid at {path}"
            )
        logical_id = document["logical_id"]
        if logical_id in seen_documents:
            raise ComparisonError(f"duplicate documents[{logical_id}] at {path}")
        seen_documents.add(logical_id)
        pages = document.get("pages")
        if not isinstance(pages, list):
            raise ComparisonError(f"documents[{logical_id}].pages is invalid at {path}")
        seen_pages: set[int] = set()
        for page in pages:
            if not isinstance(page, dict) or not isinstance(
                page.get("page_number"), int
            ):
                raise ComparisonError(
                    f"documents[{logical_id}].pages[].page_number is invalid at {path}"
                )
            page_number = page["page_number"]
            if page_number in seen_pages:
                raise ComparisonError(
                    f"duplicate documents[{logical_id}].pages[{page_number}] at {path}"
                )
            seen_pages.add(page_number)
    return payload


def compare_runs(left: dict[str, Any], right: dict[str, Any]) -> tuple[int, int]:
    """Compare identity, documents, pages, hashes, and counts in stable key order."""
    for field in [
        "schema_version",
        "corpus_manifest_sha256",
        "model_revision",
        "model_sha256",
        "config_fingerprint",
    ]:
        if left.get(field) != right.get(field):
            raise ComparisonError(
                f"mismatch at {field}: {left.get(field)!r} != {right.get(field)!r}"
            )
    left_documents = {document["logical_id"]: document for document in left["documents"]}
    right_documents = {
        document["logical_id"]: document for document in right["documents"]
    }
    if set(left_documents) != set(right_documents):
        raise ComparisonError(
            "mismatch at documents: "
            f"missing={sorted(set(left_documents) - set(right_documents))}, "
            f"extra={sorted(set(right_documents) - set(left_documents))}"
        )
    page_total = 0
    for logical_id in sorted(left_documents):
        left_document = left_documents[logical_id]
        right_document = right_documents[logical_id]
        for field in ["document_sha256", "page_count"]:
            if left_document.get(field) != right_document.get(field):
                raise ComparisonError(
                    f"mismatch at documents[{logical_id}].{field}: "
                    f"{left_document.get(field)!r} != {right_document.get(field)!r}"
                )
        left_pages = {
            page["page_number"]: page for page in left_document.get("pages", [])
        }
        right_pages = {
            page["page_number"]: page for page in right_document.get("pages", [])
        }
        if set(left_pages) != set(right_pages):
            raise ComparisonError(f"mismatch at documents[{logical_id}].pages")
        for page_number in sorted(left_pages):
            left_page = left_pages[page_number]
            right_page = right_pages[page_number]
            for field in ["sha256", "blocks", "lines", "text_items"]:
                if left_page.get(field) != right_page.get(field):
                    raise ComparisonError(
                        f"mismatch at documents[{logical_id}].pages[{page_number}].{field}: "
                        f"{left_page.get(field)!r} != {right_page.get(field)!r}"
                    )
            page_total += 1
    return len(left_documents), page_total


def parse_args() -> argparse.Namespace:
    """Parse the two canonical manifest paths."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("left", type=Path)
    parser.add_argument("right", type=Path)
    return parser.parse_args()


def main() -> int:
    """Report exact equality or the first stable mismatch path."""
    arguments = parse_args()
    try:
        documents, pages = compare_runs(
            load_run(arguments.left), load_run(arguments.right)
        )
    except ComparisonError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(
        f"canonical E2E results match: {documents} documents, "
        f"{pages} pages, 0 canonical mismatches"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
