#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr red review gate"

test_rgr_red_review_gate_requires_reviewed_red_before_production_edits() {
	local plugin shared output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"
	shared="$ROOT/.opencode/plugins/lib/shared.ts"

	if output="$(python3 - "$plugin" "$shared" 2>&1 <<'PY'
import re
import sys
from pathlib import Path

plugin = Path(sys.argv[1]).read_text()
shared = Path(sys.argv[2]).read_text()

cycle_type = re.search(r'export\s+type\s+RgrCycle\s*=\s*\{(?P<body>.*?)\n\};', shared, re.S)
if not cycle_type:
    raise SystemExit("RgrCycle type was not found")
if not re.search(r'\breviewedRed\??\s*:', cycle_type.group("body")):
    raise SystemExit("RgrCycle must track reviewedRed state separate from failingOutput")

if "rgr_approve_red" not in plugin:
    raise SystemExit("auto-review-discipline.ts must expose a distinct rgr_approve_red tool")

approve_tool = re.search(r'rgr_approve_red:\s*tool\(\{(?P<body>.*?)\n\s*\}\),\n\s*rgr_mark_green:', plugin, re.S)
if not approve_tool:
    raise SystemExit("rgr_approve_red tool definition must sit between recording RED and marking GREEN")

approve_body = approve_tool.group("body")
if "failingOutput" not in approve_body:
    raise SystemExit("rgr_approve_red must require recorded failingOutput before approval")
if not re.search(r'setCycle\([^\n]+reviewedRed\s*:\s*true', approve_body, re.S):
    raise SystemExit("rgr_approve_red must set reviewedRed: true on the active cycle")

edit_gate = re.search(r'for \(const path of changedPathsFromArgs\(output\.args\)\).*?\n\s*\}\n\s*\}', plugin, re.S)
if not edit_gate:
    raise SystemExit("production Rust edit gate was not found")
gate_body = edit_gate.group(0)
if "reviewedRed" not in gate_body:
    raise SystemExit("production Rust edit gate must check reviewedRed, not only failingOutput")
if re.search(r'if \(!current\?\.failingOutput\)', gate_body):
    raise SystemExit("production Rust edit gate still allows edits based only on failingOutput")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "production Rust edits require reviewed RED approval"
	else
		fail "production Rust edits require reviewed RED approval ($output)"
	fi
}

test_rgr_red_review_gate_requires_reviewed_red_before_production_edits

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1
