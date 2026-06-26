#!/usr/bin/env python3
import json
import os
import subprocess
import sys
from pathlib import Path

from scripts.codex import policy


def _command_from_payload(payload: dict) -> str:
    args = payload.get("args") or payload.get("input") or {}
    if isinstance(args, dict):
        value = args.get("command") or args.get("cmd") or args.get("script")
        return value if isinstance(value, str) else ""
    return ""


def _paths_from_payload(payload: dict) -> list[str]:
    args = payload.get("args") or payload.get("input") or {}
    if not isinstance(args, dict):
        return []
    for key in ("filePath", "file_path", "path"):
        value = args.get(key)
        if isinstance(value, str):
            return [value]
    patch = args.get("patchText") or args.get("patch")
    if isinstance(patch, str):
        paths = []
        for line in patch.splitlines():
            for prefix in ("*** Add File: ", "*** Update File: ", "*** Delete File: ", "*** Move to: "):
                if line.startswith(prefix):
                    paths.append(line[len(prefix) :].strip())
        return paths
    return []


def _branch(root: Path) -> str:
    result = subprocess.run(
        ["git", "-C", str(root), "branch", "--show-current"],
        check=False,
        text=True,
        capture_output=True,
    )
    return result.stdout.strip()


def main() -> int:
    payload = json.loads(sys.stdin.read() or "{}")
    tool_name = str(payload.get("tool") or payload.get("tool_name") or "")
    root = Path(os.environ.get("CODEX_REPO_ROOT", ".")).resolve()
    session_id = str(payload.get("session_id") or payload.get("sessionID") or os.environ.get("CODEX_SESSION_ID", ""))

    try:
        if "bash" in tool_name.lower():
            policy.check_bash_command(_command_from_payload(payload))
        if tool_name.lower() in {"apply_patch", "edit", "write"} or "apply_patch" in tool_name.lower():
            policy.check_edit_paths(_paths_from_payload(payload), session_id=session_id, branch=_branch(root), root=root)
    except RuntimeError as error:
        print(str(error), file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
