#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr record red resets review approval"

test_rgr_record_red_resets_review_approval_when_failing_output_changes() {
	local plugin output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"

	if output="$(python3 - "$plugin" 2>&1 <<'PY'
import re
import sys
from pathlib import Path

plugin = Path(sys.argv[1]).read_text()

rgr_record_red = re.search(r'rgr_record_red:\s*tool\(\{(?P<body>.*?)\n\s*\}\),\n\s*rgr_approve_red:', plugin, re.S)
if not rgr_record_red:
    raise SystemExit("rgr_record_red tool definition was not found")

execute = re.search(r'async\s+execute\([^)]*\)\s*\{(?P<body>.*?)\n\s*\}', rgr_record_red.group("body"), re.S)
if not execute:
    raise SystemExit("rgr_record_red.execute definition was not found")

body = execute.group("body")
set_cycle = re.search(r'setCycle\(context\.sessionID,\s*\{(?P<cycle>.*?)\}\s*\);', body, re.S)
if not set_cycle:
    raise SystemExit("rgr_record_red.execute does not update the active cycle with setCycle")

cycle_update = set_cycle.group("cycle")
if "failingOutput: args.output" not in cycle_update:
    raise SystemExit("rgr_record_red.setCycle must record the new failingOutput")
if not re.search(r'\breviewedRed\s*:\s*false\b', cycle_update):
    raise SystemExit("rgr_record_red.setCycle must reset reviewedRed: false when recording new failingOutput")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "rgr_record_red resets RED approval when failing output changes"
	else
		fail "rgr_record_red resets RED approval when failing output changes ($output)"
	fi
}

test_rgr_record_red_resets_review_approval_when_failing_output_changes

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1
