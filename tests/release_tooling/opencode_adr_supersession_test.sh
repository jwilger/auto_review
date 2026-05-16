#!/usr/bin/env bash
set -u

RELEASE_TOOLING_SUITE_NAME="opencode ADR supersession tools"
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

test_adr_create_and_update_record_typed_supersession_metadata() {
	local output status
	output="$(node "$ROOT/tests/release_tooling/opencode_adr_supersession_behavior_test.mjs" 2>&1)"
	status=$?

	printf '%s\n' "$output"
	if [[ $status -eq 0 ]]; then
		pass "opencode ADR supersession behavior test passes"
	else
		fail "opencode ADR supersession behavior test passes"
	fi
}

run_tests \
	test_adr_create_and_update_record_typed_supersession_metadata
