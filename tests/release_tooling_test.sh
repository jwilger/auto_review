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
