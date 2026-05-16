#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: aggregate runner suite members"

test_aggregate_runner_includes_new_rgr_and_opencode_guardrail_scripts() {
	local runner runner_content script script_name
	runner="$ROOT/tests/release_tooling_test.sh"
	runner_content="$(<"$runner")"

	for script in "$SCRIPT_DIR"/*.sh; do
		script_name="$(basename "$script")"
		case "$script_name" in
			lib.sh|aggregate_runner_suite_members_test.sh)
				continue
				;;
		esac

		case "$script_name" in
			rgr_*_test.sh|apply_patch*_test.sh|assert_clean_worktree_runtime_test.sh|implementation_reviewer_behavior_gap_test.sh)
				if [[ "$runner_content" == *"\$TEST_DIR/$script_name"* ]]; then
					:
				else
					fail "aggregate runner includes first missing new guardrail script: $script_name"
					return
				fi
				;;
		esac
	done

	pass "aggregate runner includes all new RGR/opencode guardrail scripts"
}

test_aggregate_runner_includes_new_rgr_and_opencode_guardrail_scripts

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1
