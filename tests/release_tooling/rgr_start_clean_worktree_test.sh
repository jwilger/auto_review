#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr start clean worktree guardrail"

test_rgr_start_checks_clean_worktree_before_recording_cycle() {
	local plugin output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"

	# Starting a new RED cycle on top of unrelated dirty work hides which edits belong
	# to the cycle. This static contract demands a mechanical guardrail in the
	# plugin entrypoint before rgr_start records the new cycle.
	if output="$(python3 - "$plugin" 2>&1 <<'PY'
import re
import sys
from pathlib import Path

plugin = Path(sys.argv[1]).read_text()

rgr_start = re.search(
    r'rgr_start:\s*tool\(\{(?P<body>.*?)\n\s*\}\),\n\s*rgr_record_red:',
    plugin,
    re.S,
)
if not rgr_start:
    raise SystemExit("rgr_start tool definition was not found")

body = rgr_start.group("body")
check = body.find("assertCleanWorktree(")
record = body.find("setCycle(context.sessionID")

if record < 0:
    raise SystemExit("rgr_start.execute does not record the cycle with setCycle")
if check < 0:
    raise SystemExit("rgr_start.execute must call assertCleanWorktree before setCycle")
if check > record:
    raise SystemExit("rgr_start.execute must check worktree cleanliness before setCycle")

shared_import = re.search(r'import\s*\{(?P<names>[^}]+)\}\s*from\s*"\./lib/shared\.ts";', plugin, re.S)
imports_helper = shared_import and "assertCleanWorktree" in shared_import.group("names")
defines_helper = re.search(r'function\s+assertCleanWorktree\b', plugin)
if not (imports_helper or defines_helper):
    raise SystemExit("auto-review-discipline.ts must import or define assertCleanWorktree")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "rgr_start checks worktree cleanliness before recording a cycle"
	else
		fail "rgr_start checks worktree cleanliness before recording a cycle ($output)"
	fi
}

run_tests test_rgr_start_checks_clean_worktree_before_recording_cycle
