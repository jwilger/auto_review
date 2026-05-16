#!/usr/bin/env bash
set -u

RELEASE_TOOLING_SUITE_NAME="opencode ADR guardrail"
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

test_normal_edit_tools_are_directed_to_adr_workflow_for_architecture_docs() {
	local output status
	output="$(node "$ROOT/tests/release_tooling/opencode_adr_guard_behavior_test.mjs" 2>&1)"
	status=$?

	printf '%s\n' "$output"
	if [[ $status -eq 0 ]]; then
		pass "opencode ADR guard behavior test passes"
	else
		fail "opencode ADR guard behavior test passes"
	fi
}

run_tests \
	test_normal_edit_tools_are_directed_to_adr_workflow_for_architecture_docs
