"""Behavior tests for deterministic E2E result comparison."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from copy import deepcopy
from pathlib import Path


def baseline() -> dict:
    """Build one minimal complete canonical hash manifest."""
    return {
        "schema_version": 1,
        "corpus_manifest_sha256": "corpus",
        "model_revision": "revision",
        "model_sha256": "model",
        "config_fingerprint": "config",
        "documents": [
            {
                "logical_id": "document-a",
                "document_sha256": "document-hash",
                "page_count": 1,
                "pages": [
                    {
                        "page_number": 1,
                        "sha256": "page-hash",
                        "blocks": 2,
                        "lines": 3,
                        "text_items": 4,
                    }
                ],
            }
        ],
    }


def run_compare(script: Path, left: dict, right: dict) -> subprocess.CompletedProcess:
    """Write two manifests and invoke the comparison script without a shell."""
    with tempfile.TemporaryDirectory(prefix="docparse-compare-") as temporary:
        directory = Path(temporary)
        left_path = directory / "left.json"
        right_path = directory / "right.json"
        left_path.write_text(json.dumps(left), encoding="utf-8")
        right_path.write_text(json.dumps(right), encoding="utf-8")
        return subprocess.run(
            [sys.executable, str(script), str(left_path), str(right_path)],
            check=False,
            capture_output=True,
            text=True,
        )


def main() -> None:
    """Verify equality and every identity/document/page mismatch category."""
    root = Path(__file__).resolve().parents[4]
    script = root / "scripts/compare_e2e_runs.py"
    source = baseline()
    equal = run_compare(script, source, deepcopy(source))
    assert equal.returncode == 0, equal.stderr
    assert "1 documents, 1 pages" in equal.stdout

    mutations = []
    identity = deepcopy(source)
    identity["model_sha256"] = "different"
    mutations.append((identity, "model_sha256"))
    missing_document = deepcopy(source)
    missing_document["documents"] = []
    mutations.append((missing_document, "documents"))
    document_hash = deepcopy(source)
    document_hash["documents"][0]["document_sha256"] = "different"
    mutations.append((document_hash, "document_sha256"))
    missing_page = deepcopy(source)
    missing_page["documents"][0]["pages"] = []
    mutations.append((missing_page, "pages"))
    page_hash = deepcopy(source)
    page_hash["documents"][0]["pages"][0]["sha256"] = "different"
    mutations.append((page_hash, "pages[1].sha256"))
    for mutated, path_fragment in mutations:
        result = run_compare(script, source, mutated)
        assert result.returncode != 0
        assert path_fragment in result.stderr, result.stderr
    print("compare_e2e_runs tests passed")


if __name__ == "__main__":
    main()
