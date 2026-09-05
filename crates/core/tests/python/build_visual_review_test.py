"""Behavior tests for deterministic overlay selection consumption."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
from pathlib import Path


def load_builder():
    """Import the visual builder without invoking its command line."""
    root = Path(__file__).resolve().parents[4]
    path = root / "scripts/build_visual_review.py"
    spec = importlib.util.spec_from_file_location("build_visual_review", path)
    if spec is None or spec.loader is None:
        raise AssertionError("visual builder module spec could not be created")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def main() -> None:
    """Verify first/middle/last coverage, deduplication, artifacts, and escaping."""
    builder = load_builder()
    with tempfile.TemporaryDirectory(prefix="docparse-visual-") as temporary:
        run_dir = Path(temporary)
        overlay_dir = run_dir / "overlays" / "document<&>"
        overlay_dir.mkdir(parents=True)
        pages = []
        for page_number, reasons in [
            (1, ["first"]),
            (2, ["middle", "max_removed_edges"]),
            (3, ["last"]),
        ]:
            (overlay_dir / f"page-{page_number:04}.png").write_bytes(b"png")
            (overlay_dir / f"page-{page_number:04}.svg").write_text(
                "<svg/>", encoding="utf-8"
            )
            pages.append(
                {
                    "logical_id": "document<&>",
                    "page_number": page_number,
                    "reasons": reasons,
                    "fallback_ratio": 0.0,
                    "assignment_conflicts": 0,
                    "removed_edges": page_number,
                }
            )
        (run_dir / "overlay-selection.json").write_text(
            json.dumps({"schema_version": 1, "pages": pages}),
            encoding="utf-8",
        )
        output = builder.build_review(run_dir)
        html = (output / "index.html").read_text(encoding="utf-8")
        assert "document&lt;&amp;&gt;" in html
        assert "needs-investigation" in (output / "review.md").read_text(
            encoding="utf-8"
        )

        pages.append(dict(pages[0]))
        (run_dir / "overlay-selection.json").write_text(
            json.dumps({"schema_version": 1, "pages": pages}),
            encoding="utf-8",
        )
        try:
            builder.load_selection(run_dir)
        except builder.VisualReviewError as error:
            assert "duplicate selected page" in str(error)
        else:
            raise AssertionError("duplicate selection must fail")
    print("build_visual_review tests passed")


if __name__ == "__main__":
    main()
