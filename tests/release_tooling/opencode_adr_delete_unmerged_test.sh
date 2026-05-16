#!/usr/bin/env bash
set -u

RELEASE_TOOLING_SUITE_NAME="opencode ADR delete-unmerged tool"
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

test_adr_delete_unmerged_removes_only_adrs_absent_from_main() {
	local output status
	output="$(node "$ROOT/tests/release_tooling/opencode_adr_delete_unmerged_behavior_test.mjs" 2>&1)"
	status=$?

	printf '%s\n' "$output"
	if [[ $status -eq 0 ]]; then
		pass "opencode ADR delete-unmerged behavior test passes"
	else
		fail "opencode ADR delete-unmerged behavior test passes"
	fi
}

run_tests \
	test_adr_delete_unmerged_removes_only_adrs_absent_from_main
