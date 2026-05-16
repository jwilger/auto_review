#!/usr/bin/env bash
set -u

RELEASE_TOOLING_SUITE_NAME="opencode ADR update tool"
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

test_adr_update_rewrites_only_requested_proposed_sections() {
	local output status
	output="$(node "$ROOT/tests/release_tooling/opencode_adr_update_behavior_test.mjs" 2>&1)"
	status=$?

	printf '%s\n' "$output"
	if [[ $status -eq 0 ]]; then
		pass "opencode adr_update behavior test passes"
	else
		fail "opencode adr_update behavior test passes"
	fi
}

run_tests \
	test_adr_update_rewrites_only_requested_proposed_sections
