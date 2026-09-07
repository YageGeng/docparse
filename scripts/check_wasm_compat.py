#!/usr/bin/env python3
"""Reject conditional compilation outside the explicit platform boundaries."""
from __future__ import annotations

import argparse
import os
import re
from pathlib import Path

ALLOWED = {f"crates/{crate}/src/wasm_compat.rs" for crate in ("config", "layout", "core", "pdfium", "pdfium-sys")}
# Keep the extracted adapters explicit instead of allowing their entire directories.
ALLOWED.update({
    "crates/core/src/wasm_compat/pdf_input.rs",
    "crates/core/src/wasm_compat/task_set.rs",
    "crates/core/src/wasm_compat/pdfium_worker.rs",
    "crates/layout/src/wasm_compat/session_pool.rs",
})
EXCLUDED = {".git", "target", "node_modules"}


def code_only(source: str) -> str:
    """Mask comments and literals while preserving offsets and nested Rust block comments."""
    result = list(source)
    index = 0
    while index < len(source):
        end = index
        if source.startswith("//", index):
            end = source.find("\n", index)
            if end < 0:
                end = len(source)
        elif source.startswith("/*", index):
            depth, end = 1, index + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
        elif match := re.match(r'(?:br|r)(#{0,255})"', source[index:]):
            marker = '"' + match.group(1)
            found = source.find(marker, index + match.end())
            end = len(source) if found < 0 else found + len(marker)
        elif source[index] == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
        elif match := re.match(r"'(?:\\.|[^'\\\n])'", source[index:]):
            end = index + match.end()
        if end > index:
            for position in range(index, min(end, len(source))):
                if result[position] != "\n":
                    result[position] = " "
            index = end
        else:
            index += 1
    return "".join(result)


def violations(path: Path, relative: str) -> list[str]:
    """Locate forbidden attributes and expansion macros in one real Rust file."""
    if relative in ALLOWED or path.name == "build.rs" or relative == "crates/pdfium-sys/bindings.rs":
        return []
    source = code_only(path.read_text())
    errors = []
    for match in re.finditer(r"#\s*!?\s*\[\s*(?:cfg|cfg_attr)\b", source):
        tail = source[match.start():]
        if re.match(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+tests\s*\{", tail):
            continue
        line = source.count("\n", 0, match.start()) + 1
        errors.append(f"{relative}:{line}: cfg belongs in an explicitly allowed compatibility file or an exact test module")
    for match in re.finditer(r"\b(?:cfg|if_wasm|if_not_wasm)\s*!", source):
        line = source.count("\n", 0, match.start()) + 1
        errors.append(f"{relative}:{line}: platform macro expands outside an explicitly allowed compatibility file")
    return errors


def main() -> int:
    """Check first-party sources while excluding only generated or pinned external trees."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    root = parser.parse_args().root.resolve()
    errors = []
    for directory, children, files in os.walk(root):
        directory = Path(directory)
        children[:] = [name for name in children if name not in EXCLUDED and (directory / name).relative_to(root).as_posix() != "vendor/ort-web"]
        for name in files:
            if name.endswith(".rs"):
                path = directory / name
                errors.extend(violations(path, path.relative_to(root).as_posix()))
    for error in errors:
        print(error)
    if not errors:
        print("Platform cfg boundary check passed")
    return int(bool(errors))


if __name__ == "__main__":
    raise SystemExit(main())
