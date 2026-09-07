#!/usr/bin/env python3
"""Copy LICENSE and NOTICE into crates; keep third-party notices at the root."""

from __future__ import annotations

from pathlib import Path


def sync(root: Path) -> None:
    """Synchronize LICENSE and NOTICE byte-for-byte without duplicating third-party notices."""
    packages = ("config", "layout", "core", "cli", "web")
    legal_files = ("LICENSE", "NOTICE")
    for package in packages:
        directory = root / "crates" / package
        for basename in legal_files:
            (directory / basename).write_bytes((root / basename).read_bytes())


def main() -> None:
    """Resolve the repository root and synchronize package legal files."""
    sync(Path(__file__).resolve().parent.parent)


if __name__ == "__main__":
    main()
