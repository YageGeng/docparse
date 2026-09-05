#!/usr/bin/env python3
"""Copy canonical repository legal texts into every publishable new crate."""

from __future__ import annotations

from pathlib import Path


def sync(root: Path) -> None:
    """Synchronize LICENSE, NOTICE, and third-party notices byte-for-byte."""
    packages = ("config", "layout", "core", "cli")
    legal_files = ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md")
    for package in packages:
        directory = root / "crates" / package
        for basename in legal_files:
            (directory / basename).write_bytes((root / basename).read_bytes())


def main() -> None:
    """Resolve the repository root and synchronize package legal files."""
    sync(Path(__file__).resolve().parent.parent)


if __name__ == "__main__":
    main()
