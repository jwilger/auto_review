#!/usr/bin/env bash
set -u

RELEASE_TOOLING_SUITE_NAME="opencode ADR transition tools"
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

test_adr_accept_and_reject_transition_only_proposed_adrs() {
	local output status
	output="$(node "$ROOT/tests/release_tooling/opencode_adr_transition_behavior_test.mjs" 2>&1)"
	status=$?

	printf '%s\n' "$output"
	if [[ $status -eq 0 ]]; then
		pass "opencode adr_accept and adr_reject behavior test passes"
	else
		fail "opencode adr_accept and adr_reject behavior test passes"
	fi
}

run_tests \
	test_adr_accept_and_reject_transition_only_proposed_adrs
