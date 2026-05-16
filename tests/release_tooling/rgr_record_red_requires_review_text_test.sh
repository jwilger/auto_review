#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr record red requires review text"

test_rgr_record_red_return_text_requires_review_before_production_edits() {
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

return_text = re.search(r'return\s+["\'](?P<text>[^"\']*)["\']\s*;', execute.group("body"))
if not return_text:
    raise SystemExit("rgr_record_red.execute return text was not found")

text = return_text.group("text")
if "Minimum production edits are now allowed" in text:
    raise SystemExit("rgr_record_red.execute must not say production edits are allowed immediately")

required_fragments = ["RED recorded", "review", "approval", "before production edits"]
missing = [fragment for fragment in required_fragments if fragment not in text]
if missing:
    raise SystemExit(f"rgr_record_red.execute return text must require RED review/approval before production edits; missing: {', '.join(missing)}")
PY
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "rgr_record_red return text requires RED review before production edits"
	else
		fail "rgr_record_red return text requires RED review before production edits ($output)"
	fi
}

test_rgr_record_red_return_text_requires_review_before_production_edits

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1
