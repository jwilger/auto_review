#!/usr/bin/env bash
set -u

RELEASE_TOOLING_SUITE_NAME="opencode ADR create tool"
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

test_adr_create_allocates_next_id_and_updates_architecture_projection() {
	local output status
	output="$(node "$ROOT/tests/release_tooling/opencode_adr_create_behavior_test.mjs" 2>&1)"
	status=$?

	printf '%s\n' "$output"
	if [[ $status -eq 0 ]]; then
		pass "opencode adr_create behavior test passes"
	else
		fail "opencode adr_create behavior test passes"
	fi
}

run_tests \
	test_adr_create_allocates_next_id_and_updates_architecture_projection
