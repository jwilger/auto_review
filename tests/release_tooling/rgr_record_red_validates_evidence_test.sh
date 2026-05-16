#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr record red validates evidence"

test_rgr_record_red_validates_evidence_before_recording() {
	local plugin output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"

	if output="$(python3 - "$plugin" 2>&1 <<'PY'
import re
import sys
from pathlib import Path

plugin = Path(sys.argv[1]).read_text()

shared_import = re.search(r'import\s*\{(?P<names>[^}]+)\}\s*from\s*"\./lib/shared\.ts";', plugin, re.S)
if not shared_import:
    raise SystemExit("auto-review-discipline.ts does not import from ./lib/shared.ts")

if "validateRgrRedEvidence" not in shared_import.group("names"):
    raise SystemExit("rgr_record_red must import validateRgrRedEvidence from ./lib/shared.ts")

rgr_record_red = re.search(r'rgr_record_red:\s*tool\(\{(?P<body>.*?)\n\s*\}\),\n\s*rgr_mark_green:', plugin, re.S)
if not rgr_record_red:
    raise SystemExit("rgr_record_red tool definition was not found")

body = rgr_record_red.group("body")
call = body.find("validateRgrRedEvidence(args.output)")
record = body.find("setCycle(context.sessionID")

if call < 0:
    raise SystemExit("rgr_record_red.execute must call validateRgrRedEvidence(args.output)")
if record < 0:
    raise SystemExit("rgr_record_red.execute does not record the cycle with setCycle")
if call > record:
    raise SystemExit("rgr_record_red.execute must validate RED evidence before recording the cycle")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "rgr_record_red validates RED evidence before recording"
	else
		fail "rgr_record_red validates RED evidence before recording ($output)"
	fi
}

test_rgr_record_red_validates_evidence_before_recording

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1
