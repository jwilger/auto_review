#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr commit checkpoint"

test_rgr_mark_refactor_reminds_orchestrator_to_commit_before_next_red() {
	local plugin output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"

	# This executable guardrail contract keeps completed RGR cycles reviewable: after
	# GREEN/refactor approval, the orchestrator must be reminded to commit that state
	# before opening a new RED cycle. Dirty-worktree blocking is covered separately.
	if output="$(python3 - "$plugin" 2>&1 <<'PY'
import re
import sys
from pathlib import Path

plugin = Path(sys.argv[1]).read_text()

rgr_mark_refactor = re.search(
    r'rgr_mark_refactor:\s*tool\(\{(?P<body>.*?)\n\s*\}\),\n\s*rgr_status:',
    plugin,
    re.S,
)
if not rgr_mark_refactor:
    raise SystemExit("rgr_mark_refactor tool definition was not found")

return_match = re.search(r'return\s+(["`])(?P<message>.*?)\1\s*;', rgr_mark_refactor.group("body"), re.S)
if not return_match:
    raise SystemExit("rgr_mark_refactor.execute return message was not found")

message = return_match.group("message")
expected = "Commit the approved GREEN/refactor state before starting the next RED."
if expected not in message:
    raise SystemExit(f"rgr_mark_refactor.execute return message missing commit checkpoint: {expected}")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "rgr_mark_refactor surfaces a commit checkpoint before the next RED"
	else
		fail "rgr_mark_refactor surfaces a commit checkpoint before the next RED ($output)"
	fi
}

run_tests test_rgr_mark_refactor_reminds_orchestrator_to_commit_before_next_red
