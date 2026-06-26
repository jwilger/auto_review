#!/usr/bin/env python3
import argparse
import json
import pathlib
from typing import Any


STATE_DIR = pathlib.Path(".codex/state")
STATE_FILE = STATE_DIR / "rgr.json"


def _state_path(root: pathlib.Path) -> pathlib.Path:
    return root / STATE_FILE


def _load(root: pathlib.Path) -> dict[str, Any]:
    path = _state_path(root)
    if not path.exists():
        return {"sessions": {}}
    return json.loads(path.read_text())


def _save(root: pathlib.Path, state: dict[str, Any]) -> None:
    path = _state_path(root)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")


def _session(root: pathlib.Path, session_id: str) -> dict[str, Any] | None:
    return _load(root).get("sessions", {}).get(session_id)


def start(root: pathlib.Path, session_id: str, behavior: str, test: str) -> None:
    state = _load(root)
    state.setdefault("sessions", {})[session_id] = {
        "behavior": behavior,
        "test": test,
        "stage": "red_started",
        "reviewed_red": False,
        "implementation_edit_token": False,
    }
    _save(root, state)


def record_red(root: pathlib.Path, session_id: str, command: str, output: str) -> None:
    state = _load(root)
    current = state.setdefault("sessions", {}).get(session_id)
    if current is None:
        raise RuntimeError("Start an RGR cycle before recording RED.")
    if "test result: FAILED. 2 failed" in output:
        raise RuntimeError("RED evidence must contain exactly one failing test.")
    current.update(
        {
            "command": command,
            "failing_output": output,
            "current_diagnostic": output,
            "reviewed_red": False,
            "implementation_edit_token": False,
            "allowed_paths": None,
            "stage": "red_observed",
        }
    )
    _save(root, state)


def approve_red(root: pathlib.Path, session_id: str) -> None:
    state = _load(root)
    current = state.setdefault("sessions", {}).get(session_id)
    if not current or not current.get("failing_output"):
        raise RuntimeError("Cannot approve RED before observed RED is recorded.")
    current.update({"reviewed_red": True, "stage": "red_approved"})
    _save(root, state)


def record_changed_diagnostic(
    root: pathlib.Path,
    session_id: str,
    command: str,
    output: str,
    diagnostic: str,
) -> None:
    state = _load(root)
    current = state.setdefault("sessions", {}).get(session_id)
    if not current or not current.get("command"):
        raise RuntimeError("Cannot record changed diagnostic before RED command is recorded.")
    if current["command"] != command:
        raise RuntimeError("Changed diagnostics must use the same focused command as the approved RED.")
    current.update(
        {
            "failing_output": output,
            "current_diagnostic": diagnostic,
            "reviewed_red": False,
            "implementation_edit_token": False,
            "allowed_paths": None,
            "stage": "changed_diagnostic_observed",
        }
    )
    _save(root, state)


def approve_changed_diagnostic(
    root: pathlib.Path,
    session_id: str,
    allowed_immediate_change: str,
    allowed_paths: list[str] | None = None,
) -> None:
    state = _load(root)
    current = state.setdefault("sessions", {}).get(session_id)
    if not current or current.get("stage") != "changed_diagnostic_observed":
        raise RuntimeError("Cannot approve changed diagnostic before recording changed failing output.")
    current.update(
        {
            "reviewed_red": True,
            "allowed_immediate_change": allowed_immediate_change,
            "allowed_paths": allowed_paths,
            "stage": "changed_diagnostic_approved",
        }
    )
    _save(root, state)


def record_proof(root: pathlib.Path, session_id: str, output: str) -> None:
    state = _load(root)
    current = state.setdefault("sessions", {}).get(session_id)
    if current is None:
        raise RuntimeError("Cannot record proof before starting an RGR cycle.")
    current["pending_proof_of_work"] = False
    current["verification"] = output
    _save(root, state)


def mark_green(root: pathlib.Path, session_id: str, output: str) -> None:
    state = _load(root)
    current = state.setdefault("sessions", {}).get(session_id)
    if not current or not current.get("failing_output"):
        raise RuntimeError("Cannot mark GREEN before observed RED is recorded.")
    if current.get("pending_proof_of_work"):
        raise RuntimeError("Record proof-of-work verification before marking GREEN.")
    current.update({"implementation_edit_token": False, "stage": "green", "verification": output})
    _save(root, state)


def is_production_rust_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    return normalized.startswith("crates/") and "/src/" in normalized and normalized.endswith(".rs")


def _paths_within_allowed(paths: list[str], allowed_paths: list[str] | None) -> bool:
    return not allowed_paths or all(path in allowed_paths for path in paths)


def assert_edit_allowed(root: pathlib.Path, session_id: str, paths: list[str], branch: str) -> None:
    production_paths = [path for path in paths if is_production_rust_path(path)]
    if not production_paths:
        return

    current = _session(root, session_id)
    if not current or not current.get("reviewed_red"):
        if current and current.get("stage") == "changed_diagnostic_observed":
            raise RuntimeError("RGR gate: changed diagnostic requires approval before the next production edit.")
        raise RuntimeError("RGR gate: production Rust edit requires RED review approval.")
    if branch == "main":
        raise RuntimeError("Branch gate: production Rust edits require leaving main.")
    if not _paths_within_allowed(production_paths, current.get("allowed_paths")):
        raise RuntimeError("RGR gate: production Rust edit paths are outside the approved diagnostic scope.")
    if current.get("implementation_edit_token"):
        raise RuntimeError("RGR gate: rerun the focused command before another behavioral production edit.")

    state = _load(root)
    state["sessions"][session_id]["implementation_edit_token"] = True
    _save(root, state)


def _main() -> int:
    parser = argparse.ArgumentParser(description="Codex RGR state helper")
    parser.add_argument("--root", default=".")
    parser.add_argument("--session", required=True)
    sub = parser.add_subparsers(dest="command", required=True)

    start_parser = sub.add_parser("start")
    start_parser.add_argument("--behavior", required=True)
    start_parser.add_argument("--test", required=True)

    red_parser = sub.add_parser("record-red")
    red_parser.add_argument("--cmd", required=True)
    red_parser.add_argument("--output", required=True)

    sub.add_parser("approve-red")

    changed_parser = sub.add_parser("record-changed-diagnostic")
    changed_parser.add_argument("--cmd", required=True)
    changed_parser.add_argument("--output", required=True)
    changed_parser.add_argument("--diagnostic", required=True)

    approve_changed = sub.add_parser("approve-changed-diagnostic")
    approve_changed.add_argument("--allowed-immediate-change", required=True)
    approve_changed.add_argument("--allowed-path", action="append", default=[])

    proof_parser = sub.add_parser("record-proof")
    proof_parser.add_argument("--output", required=True)

    green_parser = sub.add_parser("mark-green")
    green_parser.add_argument("--output", required=True)

    args = parser.parse_args()
    root = pathlib.Path(args.root)
    if args.command == "start":
        start(root, args.session, args.behavior, args.test)
    elif args.command == "record-red":
        record_red(root, args.session, args.cmd, args.output)
    elif args.command == "approve-red":
        approve_red(root, args.session)
    elif args.command == "record-changed-diagnostic":
        record_changed_diagnostic(root, args.session, args.cmd, args.output, args.diagnostic)
    elif args.command == "approve-changed-diagnostic":
        approve_changed_diagnostic(root, args.session, args.allowed_immediate_change, args.allowed_path)
    elif args.command == "record-proof":
        record_proof(root, args.session, args.output)
    elif args.command == "mark-green":
        mark_green(root, args.session, args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
