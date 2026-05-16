#!/usr/bin/env bash
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$ROOT/tests/release_tooling"
failures=0

scripts=(
  "$TEST_DIR/prepare_workflow_test.sh"
  "$TEST_DIR/publish_workflow_test.sh"
  "$TEST_DIR/ci_pr_artifacts_test.sh"
  "$TEST_DIR/workflow_runner_labels_test.sh"
  "$TEST_DIR/docs_secrets_test.sh"
  "$TEST_DIR/release_script_flake_test.sh"
  "$TEST_DIR/apply_patch_changed_paths_test.sh"
  "$TEST_DIR/apply_patch_edit_gate_changed_paths_test.sh"
  "$TEST_DIR/assert_clean_worktree_runtime_test.sh"
  "$TEST_DIR/implementation_reviewer_behavior_gap_test.sh"
  "$TEST_DIR/rgr_commit_checkpoint_test.sh"
  "$TEST_DIR/rgr_diagnostic_task_prompt_gate_test.sh"
  "$TEST_DIR/rgr_record_red_multiple_failures_test.sh"
  "$TEST_DIR/rgr_record_red_requires_review_text_test.sh"
  "$TEST_DIR/rgr_record_red_resets_review_approval_test.sh"
  "$TEST_DIR/rgr_record_red_validates_evidence_test.sh"
  "$TEST_DIR/rgr_red_review_gate_test.sh"
  "$TEST_DIR/rgr_single_edit_rerun_gate_test.sh"
  "$TEST_DIR/rgr_start_clean_worktree_test.sh"
)

for script in "${scripts[@]}"; do
  if bash "$script"; then
    :
  else
    failures=$((failures + 1))
  fi
done

if [[ $failures -eq 0 ]]; then
  printf 'release tooling dry-run tests passed\n'
  exit 0
fi

printf 'release tooling dry-run tests failed: %s category script(s) failed\n' "$failures"
exit 1
