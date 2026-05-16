#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: assertCleanWorktree runtime guardrail"

test_assert_clean_worktree_rejects_untracked_files_with_next_red_guidance() {
	local output status workspace
	workspace="$(mktemp -d)"
	git -C "$workspace" init >/dev/null 2>&1
	touch "$workspace/untracked-red-demand.txt"

	if output="$(node --experimental-strip-types --input-type=module - "$ROOT/.opencode/plugins/lib/shared.ts" "$workspace" 2>&1 <<'JS'
const { assertCleanWorktree } = await import(process.argv[2]);

const worktree = process.argv[3];

try {
  assertCleanWorktree(worktree);
  throw new Error("assertCleanWorktree accepted an untracked dirty worktree");
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  if (message === "assertCleanWorktree accepted an untracked dirty worktree") {
    throw error;
  }
  if (!message.includes("Commit the approved GREEN/refactor state before starting the next RED")) {
    throw new Error(`dirty worktree rejection omitted next-RED checkpoint guidance: ${message}`);
  }
}
JS
	)"; then
		status=0
	else
		status=$?
	fi

	rm -rf "$workspace"

	if [[ $status -eq 0 ]]; then
		pass "assertCleanWorktree rejects untracked files with next-RED checkpoint guidance"
	else
		fail "assertCleanWorktree rejects untracked files with next-RED checkpoint guidance ($output)"
	fi
}

run_tests test_assert_clean_worktree_rejects_untracked_files_with_next_red_guidance
