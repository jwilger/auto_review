#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr diagnostic task prompt gate"

test_rgr_diagnostic_implementer_tasks_must_name_one_diagnostic_and_allowed_change() {
	local plugin output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"

	if output="$(python3 - "$plugin" 2>&1 <<'PY'
import re
import sys
from pathlib import Path

plugin = Path(sys.argv[1]).read_text()

helper = re.search(
    r'(?:export\s+)?function\s+rejectsBroadDiagnosticTask\s*\(\s*args\s*:\s*unknown\s*\)\s*:\s*boolean\s*\{(?P<body>.*?)\n\}',
    plugin,
    re.S,
)
if not helper:
    raise SystemExit("rejectsBroadDiagnosticTask(args) helper is required to make rgr-diagnostic-implementer prompt validation testable")

body = helper.group("body")
if not re.search(r'rgr-diagnostic-implementer', body, re.I):
    raise SystemExit("rejectsBroadDiagnosticTask must specifically guard rgr-diagnostic-implementer task calls")
if not re.search(r'(current\s+)?diagnostic', body, re.I):
    raise SystemExit("rejectsBroadDiagnosticTask must require naming one current diagnostic")
if not re.search(r'allowed\s+(immediate\s+)?change', body, re.I):
    raise SystemExit("rejectsBroadDiagnosticTask must require naming the allowed immediate change")
if not re.search(r'fix\s+all\s+failures|all\s+failures|fix\s+everything', body, re.I):
    raise SystemExit("rejectsBroadDiagnosticTask must reject broad prompts such as 'fix all failures'")

hook = re.search(r'"tool\.execute\.before":\s*async\s*\(input,\s*output\)\s*=>\s*\{(?P<body>.*?)\n\s*\},\n\s*"experimental\.session\.compacting"', plugin, re.S)
if not hook:
    raise SystemExit("tool.execute.before hook was not found")
if not re.search(r'rejectsBroadDiagnosticTask\s*\(\s*output\.args\s*\)', hook.group("body")):
    raise SystemExit("tool.execute.before must block broad rgr-diagnostic-implementer task prompts through rejectsBroadDiagnosticTask(output.args)")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "rgr-diagnostic-implementer task prompts name one diagnostic and allowed change"
	else
		fail "rgr-diagnostic-implementer task prompts name one diagnostic and allowed change ($output)"
	fi
}

test_rgr_diagnostic_implementer_tasks_must_name_one_diagnostic_and_allowed_change

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1
