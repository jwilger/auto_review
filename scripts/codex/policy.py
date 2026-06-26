#!/usr/bin/env python3
import re
from pathlib import Path

from . import rgr


UNSAFE_COMMAND_PATTERNS = [
    re.compile(r"(^|\s)rustup(\s|$)"),
    re.compile(r"(^|\s)git\s+add\s+(-A|-u|\.)(\s|$)"),
    re.compile(r"(^|\s)git\s+commit\b(?=[^\n]*\s-(?:a|am|ma)(\s|$))"),
    re.compile(r"--no-verify\b"),
    re.compile(r"--no-gpg-sign\b"),
    re.compile(r"(^|\s)git\s+reset\s+--hard\b"),
    re.compile(r"(^|\s)git\s+checkout\s+--\b"),
    re.compile(r"(^|\s)git\s+push\s+[^\n]*--force\b"),
    re.compile(r"(^|\s)gh\s+"),
    re.compile(r"\btea\s+comment\s+\d+\b"),
    re.compile(r"/issues/\d+/comments\b"),
    re.compile(r"/pulls/\d+/comments\b"),
]

PRODUCTION_RUST_WRITE_PATTERNS = [
    re.compile(r"\.write_text\s*\(", re.IGNORECASE),
    re.compile(r"\bopen\s*\([^)]*['\"](?:w|a|x|w\+|a\+|x\+|r\+|wb|ab|xb|w\+b|a\+b|x\+b|r\+b)['\"]", re.IGNORECASE),
    re.compile(r"\bcat\b[\s\S]*>", re.IGNORECASE),
    re.compile(r"\btee\b", re.IGNORECASE),
]


def check_bash_command(command: str) -> None:
    for pattern in UNSAFE_COMMAND_PATTERNS:
        if pattern.search(command):
            raise RuntimeError("Codex policy blocked an unsafe command for this repository.")
    if re.search(r"crates/[^ \n]+/src/[^ \n]+\.rs", command) and any(
        pattern.search(command) for pattern in PRODUCTION_RUST_WRITE_PATTERNS
    ):
        raise RuntimeError("RGR shell command bypass: production Rust edits via bash are blocked.")


def is_protected_adr_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    return normalized == "docs/ARCHITECTURE.md" or bool(re.match(r"docs/ADR-[^/]+\.md$", normalized))


def check_edit_paths(
    paths: list[str],
    session_id: str = "",
    branch: str = "",
    root: str | Path = ".",
) -> None:
    if any(is_protected_adr_path(path) for path in paths):
        raise RuntimeError("ADR workflow paths must be changed through scripts/codex/adr.py.")
    rgr.assert_edit_allowed(Path(root), session_id, paths, branch)
