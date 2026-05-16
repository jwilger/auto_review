#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr single edit rerun gate"

test_rgr_single_behavioral_edit_requires_focused_rerun_before_next_edit() {
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

token_pattern = r'\b(?:implementation|behavioral|production)?\s*EditToken\??\s*:'
if not re.search(token_pattern, cycle_type.group("body"), re.I):
    raise SystemExit("RgrCycle must track an implementation/edit token so each RED/GREEN permits only one behavioral production edit")

record_red_tool = re.search(r'rgr_record_red:\s*tool\(\{(?P<body>.*?)\n\s*\}\),\n\s*rgr_approve_red:', plugin, re.S)
if not record_red_tool:
    raise SystemExit("rgr_record_red tool definition was not found")
if not re.search(token_pattern, record_red_tool.group("body"), re.I):
    raise SystemExit("rgr_record_red must clear the implementation/edit token after a focused command records new RED")

mark_green_tool = re.search(r'rgr_mark_green:\s*tool\(\{(?P<body>.*?)\n\s*\}\),\n\s*rgr_mark_refactor:', plugin, re.S)
if not mark_green_tool:
    raise SystemExit("rgr_mark_green tool definition was not found")
if not re.search(token_pattern, mark_green_tool.group("body"), re.I):
    raise SystemExit("rgr_mark_green must clear the implementation/edit token after the focused command records GREEN")

edit_gate = re.search(r'for \(const path of changedPathsFromArgs\(output\.args\)\).*?\n\s*\}\n\s*\}', plugin, re.S)
if not edit_gate:
    raise SystemExit("production Rust edit gate was not found")
gate_body = edit_gate.group(0)
if not re.search(token_pattern, gate_body, re.I):
    raise SystemExit("production Rust edit gate must set and check the implementation/edit token")
if not re.search(r'throw new Error\([^)]*(?:focused command|RED|GREEN|rerun|re-run)', gate_body, re.S | re.I):
    raise SystemExit("production Rust edit gate must explain that another behavioral edit requires rerunning the focused command and recording RED or GREEN")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "behavioral production edits require a focused rerun before the next edit"
	else
		fail "behavioral production edits require a focused rerun before the next edit ($output)"
	fi
}

test_rgr_single_behavioral_edit_requires_focused_rerun_before_next_edit

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1
