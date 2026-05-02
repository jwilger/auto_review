#!/usr/bin/env bash
# PreToolUse hook for Edit / Write / MultiEdit on Rust source files
# under crates/*/src/. Fires the Kent Beck TDD checklist to the
# agent before it modifies implementation code, so test-first
# discipline can't quietly drift into "I'll mechanically reason
# about it" rationalisations.
#
# Advisory only (exit 0). The reminder lands in the agent's
# context as a system message.

set -uo pipefail

# Hook input: JSON on stdin with at least { tool_name, tool_input }
input="$(cat)"

# Extract file_path without taking a hard jq dependency. Falls
# through to no-op if the shape isn't what we expect — better
# than failing the tool call on a hook bug.
file_path="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    tool_input = data.get("tool_input") or {}
    print(tool_input.get("file_path", ""))
except Exception:
    pass
' 2>/dev/null)"

# Only fire for Rust source files inside a crate's src/ tree.
# Test modules are inside #[cfg(test)] blocks within these same
# files, so we can't filter on path alone — but the message is
# written to handle both cases.
case "$file_path" in
    */crates/*/src/*.rs) ;;
    *) exit 0 ;;
esac

cat >&2 <<'EOF'
[TDD GUARDRAIL — ask "What would Kent Beck do?"]

Before editing implementation code in crates/*/src/, the next
turn of work MUST be:

  1. RED: write the smallest failing test that captures the
     change. Run `cargo nextest run -p <crate> <substring>` and
     PASTE the failing output. If the test passes already, the
     test is wrong — fix the test.
  2. GREEN: minimum code to make that one test pass. Not three
     improvements rolled in. Run the test. Watch it pass.
  3. REFACTOR: with tests green, improve. Each refactor is its
     own micro-cycle.

If the test is hard to write (e.g. "tracing output is awkward"),
that is a DESIGN signal, not a TDD waiver. Extract a pure
helper that IS testable, drive the helper test-first, then
plug it into the call site.

Test-after is allowed only for pure renames / moves where
existing tests would catch a regression. Anything that changes
observable behaviour needs a fresh failing test first.
EOF

exit 0
