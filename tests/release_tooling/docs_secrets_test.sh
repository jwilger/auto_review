#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: docs secrets"

test_changelog_uses_release_marker_without_unreleased_section() {
  local output status

  output="$(python3 - "$ROOT/CHANGELOG.md" <<'PY'
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text()
marker = "<!-- release-prepare inserts generated release sections below this line -->"
if "## [Unreleased]" in text:
    print("CHANGELOG should not contain an Unreleased section")
    sys.exit(1)
if marker not in text:
    print("missing release-prepare insertion marker")
    sys.exit(1)

sys.exit(0)
PY
)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "CHANGELOG uses release marker without Unreleased section"
  else
    fail "CHANGELOG uses release marker without Unreleased section ($output)"
  fi
}

test_release_workflows_use_prepare_secret_and_protected_publish_token() {
  local prepare_workflow publish_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not use unsupported forgejo.token expression for tea"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not use unsupported forgejo.token expression for git push"
  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow exposes the prepare-scoped Actions secret to release tooling"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PREPARE_TOKEN"' "release PR preparation workflow passes the prepare-scoped token to PR management tea calls"
  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PUBLISH_TOKEN"' "release PR preparation workflow does not use the publish-scoped token before merge"
  assert_file_not_contains "$prepare_workflow" 'repo_token=' "release PR preparation workflow does not derive shared helper tokens"
  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow does not expose tea token at step scope"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN' "release PR preparation workflow does not expose manual git push token environment"
  assert_file_not_contains "$prepare_workflow" 'TEA_TOKEN:' "release PR preparation workflow does not use tea's legacy token env var"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_TOKEN' "release PR preparation workflow does not use the legacy shared Actions secret"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_RELEASE_PREPARE_TOKEN' "release PR preparation workflow does not reference the old disallowed prepare secret name"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_RELEASE_PUBLISH_TOKEN' "release PR preparation workflow does not reference the old disallowed publish secret name"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_RELEASE_PREPARE_TOKEN' "release PR preparation workflow does not reference the old disallowed prepare token name anywhere"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "release PR preparation workflow does not expose the old publish-scoped Actions secret"

  assert_file_contains "$publish_workflow" 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow uses the publish-scoped Actions secret"
  assert_file_not_contains "$publish_workflow" 'release-plz' "publish workflow does not use release-plz"
  assert_file_contains "$publish_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway' "publish workflow uses the publish-scoped token for registry image publication"
  assert_file_contains "$publish_workflow" 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow intentionally broadens the publish-scoped token to Forgejo Release creation"
  assert_file_contains "$publish_workflow" 'GITEA_SERVER_URL: https://git.johnwilger.com' "publish workflow points Forgejo Release API calls at the Forgejo server"
  assert_file_not_contains "$publish_workflow" 'FORGEJO_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow does not expose legacy FORGEJO_TOKEN to publish tooling"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_TOKEN' "publish workflow does not use the legacy shared Actions secret"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_RELEASE_PREPARE_TOKEN' "publish workflow does not reference the old disallowed prepare secret name"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_RELEASE_PUBLISH_TOKEN' "publish workflow does not reference the old disallowed publish secret name"
  assert_file_not_contains "$publish_workflow" 'FORGEJO_RELEASE_PREPARE_TOKEN' "publish workflow does not expose the prepare-scoped Actions secret"
}

run_tests \
  test_changelog_uses_release_marker_without_unreleased_section \
  test_release_workflows_use_prepare_secret_and_protected_publish_token
