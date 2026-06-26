#!/usr/bin/env python3
import argparse
import json
import pathlib
import re
import subprocess
from typing import Any


def _slug(title: str) -> str:
    return re.sub(r"(^-+|-+$)", "", re.sub(r"[^a-z0-9]+", "-", title.lower()))


def _section(text: str, name: str) -> str | None:
    match = re.search(rf"^## {re.escape(name)}\n\n([\s\S]*?)(?=\n## |\Z)", text, re.MULTILINE)
    return match.group(1).strip() if match else None


def _replace_section(text: str, name: str, value: str) -> str:
    return re.sub(
        rf"(^## {re.escape(name)}\n\n)([\s\S]*?)(?=\n## |\Z)",
        lambda match: f"{match.group(1)}{value.strip()}\n",
        text,
        flags=re.MULTILINE,
    )


def _next_number(docs: pathlib.Path) -> int:
    highest = 0
    for path in docs.glob("ADR-*.md"):
        match = re.match(r"ADR-(\d+)-", path.name)
        if match:
            highest = max(highest, int(match.group(1)))
    return highest + 1


def _patch_section(patch: dict[str, str]) -> str:
    _require_architecture_patch(patch)
    return f"## Proposed Architecture Patch\n\nPatch:\n{json.dumps(patch, sort_keys=True)}\n"


def _recorded_patch(text: str) -> dict[str, str] | None:
    match = re.search(r"^## Proposed Architecture Patch\n\nPatch:\n([^\n]+)", text, re.MULTILINE)
    return json.loads(match.group(1)) if match else None


def _remove_patch_section(text: str) -> str:
    return re.sub(r"\n?## Proposed Architecture Patch\n\n[\s\S]*?(?=\n## |\Z)", "\n", text).strip() + "\n"


def _recorded_supersedes(text: str) -> list[dict[str, str]]:
    value = _section(text, "Supersedes")
    if not value:
        return []
    entries = []
    for line in value.splitlines():
        match = re.match(r"- ([^:]+): (.+)", line)
        if match:
            entries.append({"path": match.group(1), "reason": match.group(2)})
    return entries


def _require_adr_path(path: str) -> None:
    if not re.match(r"docs/ADR-[^/]+\.md$", path.replace("\\", "/")):
        raise RuntimeError("ADR workflow paths must be docs/ADR-*.md")


def _require_architecture_patch(patch: dict[str, str]) -> None:
    for key in ("path", "find", "replace"):
        if not patch.get(key):
            raise RuntimeError(f"architecture patch requires {key}")
    if patch["path"].replace("\\", "/") != "docs/ARCHITECTURE.md":
        raise RuntimeError("architecture patch path must be docs/ARCHITECTURE.md")


def _require_accepted_supersedes(root: pathlib.Path, supersedes: list[dict[str, str]]) -> None:
    for entry in supersedes:
        prior_path = entry.get("path", "")
        reason = entry.get("reason", "")
        if not prior_path or not reason:
            raise RuntimeError("supersedes entries require path and reason")
        _require_adr_path(prior_path)
        full_path = root / prior_path
        if not full_path.exists():
            raise RuntimeError("supersedes entries must reference existing ADR files")
        status = _section(full_path.read_text(), "Status")
        if status not in {"Accepted", "Superseded", "Partially superseded"}:
            raise RuntimeError("supersedes entries must reference Accepted ADRs")


def _replace_or_append_patch_section(text: str, patch: dict[str, str]) -> str:
    section = _patch_section(patch).strip()
    if re.search(r"^## Proposed Architecture Patch\n\n", text, re.MULTILINE):
        return re.sub(
            r"(^|\n)## Proposed Architecture Patch\n\n[\s\S]*?(?=\n## |\Z)",
            lambda match: f"{match.group(1)}{section}\n",
            text,
            flags=re.MULTILINE,
        )
    return text.rstrip() + f"\n\n{section}\n"


def _replace_supersedes_section(text: str, supersedes: list[dict[str, str]]) -> str:
    without = re.sub(r"\n?## Supersedes\n\n[\s\S]*?(?=\n## |\Z)", "\n", text).strip() + "\n"
    if not supersedes:
        return without
    body = "\n".join(f"- {entry['path']}: {entry['reason']}" for entry in supersedes)
    return without.rstrip() + f"\n\n## Supersedes\n\n{body}\n"


def _adr_id(path: str) -> str:
    return re.match(r"ADR-\d+", pathlib.Path(path).name).group(0)


def _append_superseded_by(prior: str, superseding_adr_path: str, reason: str) -> str:
    note = f"{_adr_id(superseding_adr_path)}: {reason}"
    status = _section(prior, "Status")
    updated = _replace_section(prior, "Status", "Superseded") if status == "Accepted" else prior
    existing = _section(updated, "Superseded By")
    if existing:
        return _replace_section(updated, "Superseded By", f"{existing}\n{note}")
    return updated.rstrip() + f"\n\n## Superseded By\n\n{note}\n"


def _git_path_exists_on_main(root: pathlib.Path, path: str) -> bool:
    verify = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--verify", "main^{commit}"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if verify.returncode != 0:
        raise RuntimeError("adr_delete_unmerged cannot verify main; refusing to delete ADR")
    result = subprocess.run(
        ["git", "-C", str(root), "ls-tree", "--name-only", "main", "--", path.replace("\\", "/")],
        check=False,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        raise RuntimeError("adr_delete_unmerged cannot check whether ADR exists on main")
    return bool(result.stdout.strip())


def _remove_lines_referencing(text: str, references: list[str]) -> str:
    return "\n".join(line for line in text.splitlines() if not any(ref in line for ref in references)) + "\n"


def _clean_references(root: pathlib.Path, deleted_adr_path: str) -> None:
    references = [deleted_adr_path.replace("\\", "/"), pathlib.Path(deleted_adr_path).name]
    for path in [root / "docs" / "ARCHITECTURE.md", *sorted((root / "docs").glob("ADR-*.md"))]:
        if not path.exists():
            continue
        current = path.read_text()
        updated = _remove_lines_referencing(current, references)
        if updated != current:
            path.write_text(updated)


def create(
    root: pathlib.Path,
    title: str,
    date: str,
    context: str,
    decision: str,
    consequences: str,
    architecture_patch: dict[str, str],
    supersedes: list[dict[str, str]] | None = None,
) -> str:
    docs = root / "docs"
    _require_architecture_patch(architecture_patch)
    _require_accepted_supersedes(root, supersedes or [])
    number = _next_number(docs)
    adr_id = f"ADR-{number:04d}"
    path = pathlib.Path("docs") / f"{adr_id}-{_slug(title)}.md"
    supersedes = supersedes or []
    supersedes_section = ""
    if supersedes:
        supersedes_section = "\n## Supersedes\n\n" + "\n".join(
            f"- {entry['path']}: {entry['reason']}" for entry in supersedes
        ) + "\n"
    body = (
        f"# {adr_id}: {title}\n\n"
        "## Status\n\nProposed\n\n"
        f"## Date\n\n{date}\n\n"
        f"## Context\n\n{context}\n\n"
        f"## Decision\n\n{decision}\n\n"
        f"## Consequences\n\n{consequences}\n\n"
        f"{_patch_section(architecture_patch)}"
        f"{supersedes_section}"
    )
    (root / path).write_text(body)
    return path.as_posix()


def update(
    root: pathlib.Path,
    adr_path: str,
    title: str,
    date: str,
    context: str,
    decision: str,
    consequences: str,
    sections_to_update: list[str],
    architecture_patch: dict[str, str],
    supersedes: list[dict[str, str]] | None = None,
) -> None:
    path = root / adr_path
    text = path.read_text()
    if _section(text, "Status") != "Proposed":
        raise RuntimeError("adr_update only updates Proposed ADRs")
    replacements = {
        "date": ("Date", date),
        "context": ("Context", context),
        "decision": ("Decision", decision),
        "consequences": ("Consequences", consequences),
    }
    if "title" in sections_to_update:
        text = re.sub(r"^# ADR-\d+: .+$", f"# {_adr_id(adr_path)}: {title}", text, flags=re.MULTILINE)
    for section in sections_to_update:
        if section in replacements:
            heading, value = replacements[section]
            text = _replace_section(text, heading, value)
    supersedes = supersedes or []
    _require_architecture_patch(architecture_patch)
    _require_accepted_supersedes(root, supersedes)
    text = _replace_or_append_patch_section(text, architecture_patch)
    text = _replace_supersedes_section(text, supersedes)
    path.write_text(text)


def accept(root: pathlib.Path, adr_path: str) -> None:
    path = root / adr_path
    text = path.read_text()
    if _section(text, "Status") != "Proposed":
        raise RuntimeError("adr_accept only transitions Proposed ADRs")
    _require_accepted_supersedes(root, _recorded_supersedes(text))
    patch = _recorded_patch(text)
    if patch:
        projection = root / patch["path"]
        current = projection.read_text()
        if patch["find"] not in current:
            raise RuntimeError("architecture patch find text was not found")
        projection.write_text(current.replace(patch["find"], patch["replace"]))
        text = _remove_patch_section(text)
    for entry in _recorded_supersedes(text):
        prior_path = root / entry["path"]
        prior_path.write_text(_append_superseded_by(prior_path.read_text(), adr_path, entry["reason"]))
    path.write_text(text.replace("## Status\n\nProposed", "## Status\n\nAccepted"))


def reject(root: pathlib.Path, adr_path: str, rationale: str) -> None:
    path = root / adr_path
    text = path.read_text()
    if _section(text, "Status") != "Proposed":
        raise RuntimeError("adr_reject only transitions Proposed ADRs")
    path.write_text(_replace_section(text, "Status", f"Rejected\n\n## Rejection Rationale\n\n{rationale}"))


def delete_unmerged(root: pathlib.Path, adr_path: str) -> None:
    _require_adr_path(adr_path)
    if _git_path_exists_on_main(root, adr_path):
        raise RuntimeError(f"adr_delete_unmerged refuses to delete {adr_path} because it exists on main")
    path = root / adr_path
    path.unlink()
    _clean_references(root, adr_path)


def _main() -> int:
    parser = argparse.ArgumentParser(description="Codex ADR workflow helper")
    parser.add_argument("--root", default=".")
    sub = parser.add_subparsers(dest="command", required=True)
    create_parser = sub.add_parser("create")
    create_parser.add_argument("--title", required=True)
    create_parser.add_argument("--date", required=True)
    create_parser.add_argument("--context", required=True)
    create_parser.add_argument("--decision", required=True)
    create_parser.add_argument("--consequences", required=True)
    create_parser.add_argument("--architecture-patch-json", required=True)
    create_parser.add_argument("--supersedes-json", default="[]")
    accept_parser = sub.add_parser("accept")
    accept_parser.add_argument("--path", required=True)
    update_parser = sub.add_parser("update")
    update_parser.add_argument("--path", required=True)
    update_parser.add_argument("--title", required=True)
    update_parser.add_argument("--date", required=True)
    update_parser.add_argument("--context", required=True)
    update_parser.add_argument("--decision", required=True)
    update_parser.add_argument("--consequences", required=True)
    update_parser.add_argument("--section", action="append", dest="sections", default=[])
    update_parser.add_argument("--architecture-patch-json", required=True)
    update_parser.add_argument("--supersedes-json", default="[]")
    reject_parser = sub.add_parser("reject")
    reject_parser.add_argument("--path", required=True)
    reject_parser.add_argument("--rationale", required=True)
    delete_parser = sub.add_parser("delete-unmerged", aliases=["delete"])
    delete_parser.add_argument("--path", required=True)
    args = parser.parse_args()
    root = pathlib.Path(args.root)
    if args.command == "create":
        print(
            create(
                root,
                args.title,
                args.date,
                args.context,
                args.decision,
                args.consequences,
                json.loads(args.architecture_patch_json),
                json.loads(args.supersedes_json),
            )
        )
    elif args.command == "accept":
        accept(root, args.path)
    elif args.command == "update":
        update(
            root,
            args.path,
            args.title,
            args.date,
            args.context,
            args.decision,
            args.consequences,
            args.sections,
            json.loads(args.architecture_patch_json),
            json.loads(args.supersedes_json),
        )
    elif args.command == "reject":
        reject(root, args.path, args.rationale)
    elif args.command == "delete-unmerged":
        delete_unmerged(root, args.path)
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
