#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: prepare workflow"

test_release_workflows_exist_for_prepare_pr_and_publish_on_merge() {
  local prepare_workflow publish_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_exists "$prepare_workflow" "release PR preparation workflow exists"
  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
on_line = None
for index, line in enumerate(workflow):
    if line == "on:":
        on_line = index
        break
if on_line is None:
    print("workflow is missing top-level on mapping")
    sys.exit(1)

push_line = None
for index in range(on_line + 1, len(workflow)):
    line = workflow[index]
    if not line.strip():
        continue
    indent = len(line) - len(line.lstrip())
    if indent == 0:
        break
    if indent == 2 and line.strip() == "push:":
        push_line = index
        break
if push_line is None:
    print("on mapping is missing push trigger")
    sys.exit(1)

branches_line = None
for index in range(push_line + 1, len(workflow)):
    line = workflow[index]
    if not line.strip():
        continue
    indent = len(line) - len(line.lstrip())
    if indent <= 2:
        break
    if indent == 4 and line.strip() == "branches:":
        branches_line = index
        break
if branches_line is None:
    print("on.push is missing branches list")
    sys.exit(1)

branches = []
for line in workflow[branches_line + 1:]:
    if not line.strip():
        continue
    indent = len(line) - len(line.lstrip())
    if indent <= 4:
        break
    if indent == 6 and line.strip().startswith("- "):
        branches.append(line.strip()[2:])
if branches != ["main"]:
    print(f"on.push.branches must contain only main, got: {branches}")
    sys.exit(1)
sys.exit(0)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow runs automatically after pushes and merges to main"
  else
    fail "release PR preparation workflow runs automatically after pushes and merges to main ($output)"
  fi
  assert_file_contains "$prepare_workflow" 'scripts/release plan --workspace .' "release PR preparation workflow computes the next release version locally"
  assert_file_contains "$prepare_workflow" 'for candidate_type in patch minor major' "release PR preparation workflow selects the release type by trying patch, minor, then major"
  assert_file_contains "$prepare_workflow" 'cargo semver-checks --workspace --baseline-rev "$BASELINE_TAG" --release-type "$candidate_type"' "release PR preparation workflow validates each candidate bump with cargo-semver-checks"
  assert_file_contains "$prepare_workflow" 'scripts/release plan --workspace . --release-type "$candidate_type"' "release PR preparation workflow recomputes the version from the semver-checks-selected release type"
  assert_file_contains "$prepare_workflow" 'scripts/release prepare --workspace . --version "$RELEASE_VERSION"' "release PR preparation workflow updates root release metadata with project tooling"
  assert_file_not_contains "$prepare_workflow" 'release-plz' "release PR preparation workflow does not invoke release-plz"
  assert_file_contains "$prepare_workflow" 'fetch-depth: 0' "release PR preparation workflow checks out full history for changelog generation"
  assert_file_contains "$prepare_workflow" 'git fetch --tags' "release PR preparation workflow fetches tags for changelog generation"
  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not map tea token from unsupported forgejo.token expression"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not map git push token from unsupported forgejo.token expression"
  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow exposes the explicit prepare-scoped Actions secret to release tooling"
  assert_file_contains "$prepare_workflow" 'RELEASE_SIGNING_KEY: ${{ secrets.RELEASE_SIGNING_KEY }}' "release PR preparation workflow exposes the release bot signing key only to release preparation"
  assert_file_contains "$prepare_workflow" 'RELEASE_BOT_NAME: ${{ vars.RELEASE_BOT_NAME }}' "release PR preparation workflow uses the configured release bot name"
  assert_file_contains "$prepare_workflow" 'RELEASE_BOT_EMAIL: ${{ vars.RELEASE_BOT_EMAIL }}' "release PR preparation workflow uses the configured release bot email"
  assert_file_contains "$prepare_workflow" 'GIT_AUTHOR_NAME="$RELEASE_BOT_NAME"' "release PR preparation workflow gives git the release bot author name"
  assert_file_contains "$prepare_workflow" 'GIT_AUTHOR_EMAIL="$RELEASE_BOT_EMAIL"' "release PR preparation workflow gives git the release bot author email"
  assert_file_contains "$prepare_workflow" 'GIT_COMMITTER_NAME="$RELEASE_BOT_NAME"' "release PR preparation workflow gives git the release bot committer name"
  assert_file_contains "$prepare_workflow" 'GIT_COMMITTER_EMAIL="$RELEASE_BOT_EMAIL"' "release PR preparation workflow gives git the release bot committer email"
  assert_file_contains "$prepare_workflow" 'GIT_CONFIG_KEY_0=gpg.format' "release PR preparation workflow configures git SSH signing format"
  assert_file_contains "$prepare_workflow" 'GIT_CONFIG_VALUE_0=ssh' "release PR preparation workflow configures git SSH signing format value"
  assert_file_contains "$prepare_workflow" 'GIT_CONFIG_KEY_1=user.signingkey' "release PR preparation workflow points git at the release bot signing key"
  assert_file_contains "$prepare_workflow" 'GIT_CONFIG_KEY_2=commit.gpgsign' "release PR preparation workflow requires release commits to be signed"
  assert_file_contains "$prepare_workflow" 'GIT_CONFIG_VALUE_2=true' "release PR preparation workflow enables release commit signing"
  assert_file_not_contains "$prepare_workflow" 'Auto Review Bot' "release PR preparation workflow does not attribute release commits to the review bot"
  assert_file_not_contains "$prepare_workflow" 'repo_token=' "release PR preparation workflow does not derive shared helper tokens"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PREPARE_TOKEN"' "release PR preparation workflow configures tea with only the prepare token"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN' "release PR preparation workflow does not configure manual git push helper tokens"
  assert_file_contains "$prepare_workflow" 'git switch -C "$branch"' "release PR preparation workflow switches to the release branch"
  assert_file_contains "$prepare_workflow" 'git push --force-with-lease origin "$branch"' "release PR preparation workflow updates the release branch"
  assert_file_contains "$prepare_workflow" 'git commit -m "chore: release v$RELEASE_VERSION"' "release PR preparation workflow creates a signed release metadata commit"
  assert_file_contains "$prepare_workflow" 'git add Cargo.toml Cargo.lock CHANGELOG.md' "release PR preparation workflow stages only root release metadata"
  assert_file_contains "$prepare_workflow" 'tea login add' "release PR preparation workflow configures tea with the prepare token"
  assert_file_contains "$prepare_workflow" 'tea pr create' "release PR preparation workflow creates the release PR"
  assert_file_contains "$prepare_workflow" 'nix develop' "release PR preparation workflow enters the Nix development environment before project tooling"
  assert_file_not_contains "$prepare_workflow" 'gh ' "release PR preparation workflow does not invoke GitHub tooling"

  assert_file_exists "$publish_workflow" "publish-on-merge workflow exists"
  assert_file_contains "$publish_workflow" 'push:' "publish workflow listens for pushes to main"
  assert_file_contains "$publish_workflow" 'branches: [main]' "publish workflow runs after release metadata lands on main"
  assert_file_contains "$publish_workflow" 'workflow_dispatch:' "publish workflow supports manual dispatch with an explicit release merge SHA"
  assert_file_not_contains "$publish_workflow" 'pull_request' "publish workflow no longer waits for release PR close events"
  assert_file_contains "$publish_workflow" 'nix develop' "publish workflow enters the Nix development environment before project tooling"
  assert_file_contains "$publish_workflow" 'nix build .#ar-gateway-image' "publish workflow builds the release Docker image only after the release PR merges to main"
  assert_file_contains "$publish_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway' "publish workflow targets the Forgejo package registry image repository"
  assert_file_not_contains "$publish_workflow" 'release-plz' "publish workflow does not use release-plz"
  assert_file_not_contains "$publish_workflow" 'scripts/release publish' "publish workflow does not call the hand-rolled publish script"
  assert_file_not_contains "$publish_workflow" 'RELEASE_VERSION="${FORGEJO_PULL_REQUEST_HEAD_BRANCH#release/v}"' "publish workflow does not derive a release version from a hand-managed branch"
  assert_file_not_contains "$publish_workflow" '[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]' "publish workflow does not duplicate release-plz version selection"
  assert_file_not_contains "$publish_workflow" 'gh ' "publish workflow does not invoke GitHub tooling"
}

test_release_workflows_install_or_reuse_nix_like_ci_before_nix_develop() {
  local prepare_workflow publish_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$prepare_workflow" 'Install or reuse Nix' "release PR preparation workflow installs or reuses Nix like CI"
  assert_file_contains "$prepare_workflow" 'https://install.determinate.systems/nix' "release PR preparation workflow uses the CI Nix installer path"
  assert_file_contains "$prepare_workflow" 'echo "$NIX_BIN_DIR" >> "$GITHUB_PATH"' "release PR preparation workflow persists the Nix path for later steps"
  assert_file_contains_before "$prepare_workflow" 'Install or reuse Nix' 'nix develop' "release PR preparation workflow installs Nix before nix develop"

  assert_file_contains "$publish_workflow" 'Install or reuse Nix' "publish workflow installs or reuses Nix like CI"
  assert_file_contains "$publish_workflow" 'https://install.determinate.systems/nix' "publish workflow uses the CI Nix installer path"
  assert_file_contains "$publish_workflow" 'echo "$NIX_BIN_DIR" >> "$GITHUB_PATH"' "publish workflow persists the Nix path for later steps"
  assert_file_contains_before "$publish_workflow" 'Install or reuse Nix' 'nix develop' "publish workflow installs Nix before nix develop"
}

test_prepare_workflow_delegates_pr_artifact_publication_to_ci() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_not_contains "$prepare_workflow" 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "release PR preparation workflow does not expose the final-release publish token"
  assert_file_not_contains "$prepare_workflow" 'tea release create' "release PR preparation workflow does not create Forgejo Release entries for PR builds"
  assert_file_not_contains "$prepare_workflow" '--prerelease' "release PR preparation workflow does not model PR artifacts as Forgejo prereleases"
  assert_file_not_contains "$prepare_workflow" 'Build pre-release versions from source' "release PR body no longer tells users to build PR artifacts from source"
}

test_prepare_workflow_does_not_create_prerelease_entry_before_merge() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_not_contains "$prepare_workflow" 'tea release edit' "release PR preparation workflow does not edit Forgejo prerelease entries before merge"
  assert_file_not_contains "$prepare_workflow" 'tea release create' "release PR preparation workflow does not create Forgejo prerelease entries before merge"
  assert_file_not_contains "$prepare_workflow" '--prerelease' "release PR preparation workflow does not mark Forgejo prerelease entries before merge"
  assert_file_not_contains "$prepare_workflow" 'Build pre-release versions from source' "release PR body does not replace package-hosted PR artifacts with source-build instructions"
}

test_prepare_workflow_skips_release_pr_merge_pushes() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_has_line_containing_all "$prepare_workflow" "release PR preparation workflow skips release PR merge pushes by title" 'github.event_name' 'push' 'github.event.head_commit.message' 'chore: release'
  assert_file_contains "$prepare_workflow" 'workflow_dispatch' "release PR preparation workflow still supports manual dispatch"
}

test_prepare_workflow_runs_release_infra_fix_pushes() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_not_contains "$prepare_workflow" "fix(release)" "release PR preparation workflow runs after release infrastructure fixes"
  assert_file_contains "$prepare_workflow" 'workflow_dispatch' "release PR preparation workflow still supports manual dispatch after release infrastructure fixes"
}

test_prepare_workflow_plans_and_checks_semver_before_release_metadata_commit() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()

def require_ordered(section, markers, label):
    cursor = 0
    for marker in markers:
        found = section.find(marker, cursor)
        if found == -1:
            print(f'{label} missing ordered marker: {marker}')
            sys.exit(1)
        cursor = found + len(marker)

require_ordered(
    workflow,
    [
        'scripts/release plan --workspace .',
        'for candidate_type in patch minor major',
        'cargo semver-checks --workspace --baseline-rev "$BASELINE_TAG" --release-type "$candidate_type"',
        'scripts/release prepare --workspace . --version "$RELEASE_VERSION"',
        'git add Cargo.toml Cargo.lock CHANGELOG.md',
        'git commit -m "chore: release v$RELEASE_VERSION"',
        'git push --force-with-lease origin "$branch"',
        'tea pr create',
    ],
    'release preparation flow',
)
for forbidden in ['release-plz', 'crates/*/CHANGELOG.md']:
    if forbidden in workflow:
        print(f'prepare workflow should use local root-release tooling, found: {forbidden}')
        sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow plans, semver-checks, commits, pushes, and opens the release PR"
  else
    fail "release PR preparation workflow plans, semver-checks, commits, pushes, and opens the release PR ($output)"
  fi
}

test_prepare_workflow_closes_superseded_release_prs_before_creating_current_pr() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
create_marker = 'tea pr create'
if create_marker not in workflow:
    print('prepare workflow is missing release PR creation')
    sys.exit(1)

before_create = workflow.split(create_marker, 1)[0]
required_markers = [
    'pulls?state=open',
    'release/v',
    'superseded',
]
missing = [marker for marker in required_markers if marker not in before_create]
if missing:
    print('prepare workflow does not identify superseded open release/v* PRs before creating the current release PR: ' + ', '.join(missing))
    sys.exit(1)

close_markers = [
    'tea pr close',
    'state=closed',
    '"state":"closed"',
    "'state':'closed'",
]
if not any(marker in before_create for marker in close_markers):
    print('prepare workflow does not close superseded open release/v* PRs before creating the current release PR')
    sys.exit(1)

if '.head.ref != $branch' not in before_create and '.head.ref != $current_branch' not in before_create and '!= "$branch"' not in before_create:
    print('prepare workflow does not preserve the current release branch while closing older release/v* PRs')
    sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow closes superseded open release PRs before creating the current PR"
  else
    fail "release PR preparation workflow closes superseded open release PRs before creating the current PR ($output)"
  fi
}

test_prepare_workflow_selects_maximum_of_semver_minimum_and_conventional_bump() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()

prepare_index = workflow.find('scripts/release prepare --workspace . --version "$RELEASE_VERSION"')
if prepare_index == -1:
    print('prepare workflow is missing release metadata preparation')
    sys.exit(1)

semver_marker = 'cargo semver-checks --workspace --baseline-rev "$BASELINE_TAG" --release-type "$candidate_type"'
semver_index = workflow.find(semver_marker)
if semver_index == -1 or semver_index > prepare_index:
    print('prepare workflow does not run cargo semver-checks before preparing release metadata')
    sys.exit(1)

before_semver = workflow[:semver_index]
selection_section = workflow[semver_index:prepare_index]

if 'scripts/release plan --workspace .' not in before_semver:
    print('prepare workflow does not compute a conventional-commit release plan before semver-checks')
    sys.exit(1)

if not re.search(r'\$\([^)]*release_plan[^)]*release_type', before_semver, re.S):
    print('prepare workflow does not retain the conventional-commit release type from the initial release plan')
    sys.exit(1)

if not re.search(r'=\s*"?\$candidate_type"?', selection_section):
    print('prepare workflow does not retain the first semver-checks-compatible release type as the minimum allowed bump')
    sys.exit(1)

rank_patterns = [
    r'patch[^\n]*(?:0|1)[\s\S]*minor[^\n]*(?:1|2)[\s\S]*major[^\n]*(?:2|3)',
    r'(?:patch\s+minor\s+major|patch\|minor\|major|patch,\s*minor,\s*major)',
    r'case[\s\S]*patch[\s\S]*minor[\s\S]*major[\s\S]*esac',
]
if not any(re.search(pattern, selection_section) for pattern in rank_patterns):
    print('prepare workflow does not map/rank release bumps in patch < minor < major order')
    sys.exit(1)

comparison_patterns = [
    r'\bmax\b',
    r'\[\[[^\]]*(?:-gt|-ge|>|>=)[^\]]*\]\]',
    r'\bif\b[\s\S]{0,160}(?:-gt|-ge|>|>=)[\s\S]{0,160}\bthen\b',
]
if not any(re.search(pattern, selection_section) for pattern in comparison_patterns):
    print('prepare workflow does not compare conventional and semver bump ranks to choose the maximum')
    sys.exit(1)

replan = re.search(r'scripts/release plan --workspace \. --release-type "?\$[A-Za-z_][A-Za-z0-9_]*"?', selection_section)
if not replan:
    print('prepare workflow does not replan the release version from the selected maximum bump before preparing metadata')
    sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow selects the higher of semver minimum and conventional-commit bump"
  else
    fail "release PR preparation workflow selects the higher of semver minimum and conventional-commit bump ($output)"
  fi
}

test_prepare_workflow_uses_pinned_tea_and_json_parser_tooling() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
bare_tool_pattern = re.compile(r'(^|[|;&]\s*)(tea|jq)\b')
bare_tool_lines = []
has_pinned_tea = False
has_pinned_json_parser = False
for line_number, line in enumerate(workflow, 1):
    stripped = line.strip()
    if not stripped or stripped.startswith('#'):
        continue
    if any(marker in stripped for marker in [
        'nix develop --command tea',
        '/nix/var/nix/profiles/default/bin/tea',
        'TRUSTED_RELEASE_TOOLS',
        '$TEA',
        '${TEA}',
    ]):
        has_pinned_tea = True
    if any(marker in stripped for marker in [
        'nix develop --command jq',
        '/nix/var/nix/profiles/default/bin/jq',
        'TRUSTED_RELEASE_TOOLS',
        '$JQ',
        '${JQ}',
        'python3 -',
    ]):
        has_pinned_json_parser = True
    if bare_tool_pattern.search(stripped):
        bare_tool_lines.append(f'{line_number}: {stripped}')

if bare_tool_lines:
    print('prepare workflow invokes runner-provided tea/jq instead of pinned trusted tools:')
    print('\n'.join(bare_tool_lines))
    sys.exit(1)
if not has_pinned_tea:
    print('prepare workflow does not invoke tea through a pinned trusted tool path or Nix dev shell')
    sys.exit(1)
if not has_pinned_json_parser:
    print('prepare workflow does not parse Forgejo API JSON through a pinned trusted parser or Nix dev shell')
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow gets tea and JSON parsing from pinned trusted tooling"
  else
    fail "release PR preparation workflow gets tea and JSON parsing from pinned trusted tooling ($output)"
  fi
}

test_prepare_workflow_uses_release_bot_identity_for_signed_commits() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'GIT_AUTHOR_NAME="$RELEASE_BOT_NAME"' "release PR preparation workflow sets release bot author name"
  assert_file_contains "$prepare_workflow" 'GIT_AUTHOR_EMAIL="$RELEASE_BOT_EMAIL"' "release PR preparation workflow sets release bot author email"
  assert_file_contains "$prepare_workflow" 'git commit -m "chore: release v$RELEASE_VERSION"' "release PR preparation workflow creates the signed release commit"
}

test_prepare_workflow_checkout_does_not_persist_credentials() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
checkout_steps = 0
for index, line in enumerate(workflow):
    if line.strip() == "- uses: actions/checkout@v4":
        checkout_steps += 1
        with_index = None
        for nested in workflow[index + 1:]:
            if nested.startswith("      - "):
                break
            if nested.strip() == "with:":
                with_index = workflow.index(nested, index + 1)
                break
        if with_index is None:
            print("actions/checkout@v4 step is missing a with mapping")
            sys.exit(1)

        with_indent = len(workflow[with_index]) - len(workflow[with_index].lstrip())
        has_persist_credentials_false = False
        for nested in workflow[with_index + 1:]:
            stripped = nested.strip()
            if not stripped:
                continue
            nested_indent = len(nested) - len(nested.lstrip())
            if nested_indent <= with_indent:
                break
            if stripped == "persist-credentials: false":
                has_persist_credentials_false = True
                break
        if not has_persist_credentials_false:
            print(f"actions/checkout@v4 step at line {index + 1} with mapping is missing persist-credentials: false")
            sys.exit(1)
if checkout_steps == 0:
    print("workflow is missing actions/checkout@v4")
    sys.exit(1)
sys.exit(0)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "every release PR preparation workflow checkout does not persist checkout credentials"
  else
    fail "every release PR preparation workflow checkout does not persist checkout credentials ($output)"
  fi
}

test_prepare_workflow_authenticates_git_push_without_checkout_credentials() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'persist-credentials: false' "release PR preparation workflow keeps checkout credentials disabled"
  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
push_marker = 'git push --force-with-lease origin "$branch"'
if push_marker not in workflow:
    print('missing release branch push')
    sys.exit(1)

before_push = workflow.split(push_marker, 1)[0]
required = [
    'export GIT_CONFIG_COUNT=4',
    'export GIT_CONFIG_KEY_3=url.https://release:${RELEASE_PREPARE_TOKEN}@git.johnwilger.com/.insteadOf',
    'export GIT_CONFIG_VALUE_3=https://git.johnwilger.com/',
]
missing = [marker for marker in required if marker not in before_push]
if missing:
    print('missing exported authenticated Forgejo HTTPS git push config before push: ' + ', '.join(missing))
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow rewrites Forgejo HTTPS git pushes to use the prepare token"
  else
    fail "release PR preparation workflow rewrites Forgejo HTTPS git pushes to use the prepare token ($output)"
  fi
}

test_prepare_workflow_manages_release_branch_with_prepare_token() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not map git push token from unsupported forgejo.token expression"
  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow receives the operator-created prepare secret"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PREPARE_TOKEN"' "release PR preparation workflow gives tea the prepare token for release PR management"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN' "release PR preparation workflow does not expose a manual git push helper token"
  assert_file_not_contains "$prepare_workflow" 'git -c credential.helper=' "release PR preparation workflow does not install manual git credential helpers"
  assert_file_contains "$prepare_workflow" 'git push --force-with-lease origin "$branch"' "release PR preparation workflow pushes release branches with the prepare token"
}

test_prepare_workflow_updates_existing_release_pr_body_without_forgejo_release_refs() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
else_marker = 'else\n            printf \'release PR already open: #%s\\n\' "$existing_pr"'
if else_marker not in workflow:
    print('prepare workflow is missing the existing_pr else branch marker')
    sys.exit(1)

existing_pr_branch = workflow.split(else_marker, 1)[1].split('          fi', 1)[0]
has_update_command = 'tea pr edit' in existing_pr_branch or 'tea api' in existing_pr_branch
errors = []
if not has_update_command:
    errors.append('tea pr edit or tea api update in existing_pr branch')
for forbidden in [
    'Build pre-release versions from source',
    'tea release create',
    '--prerelease',
    '/releases/tag/',
]:
    if forbidden in existing_pr_branch:
        errors.append('forbidden Forgejo Release/source-build reference: ' + forbidden)

if errors:
    print('existing release PR branch must update the PR body without Forgejo Release or source-build refs: ' + ', '.join(errors))
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "prepare workflow updates existing release PR body without Forgejo Release refs"
  else
    fail "prepare workflow updates existing release PR body without Forgejo Release refs ($output)"
  fi
}

test_prepare_workflow_stages_only_root_release_metadata() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'scripts/release prepare --workspace . --version "$RELEASE_VERSION"' "release PR preparation workflow updates root release metadata"
  assert_file_contains "$prepare_workflow" 'git add Cargo.toml Cargo.lock CHANGELOG.md' "release PR preparation workflow stages root release metadata explicitly"
  assert_file_not_contains "$prepare_workflow" 'crates/*/CHANGELOG.md' "release PR preparation workflow does not stage per-crate changelogs"
}

run_tests \
  test_release_workflows_exist_for_prepare_pr_and_publish_on_merge \
  test_release_workflows_install_or_reuse_nix_like_ci_before_nix_develop \
  test_prepare_workflow_delegates_pr_artifact_publication_to_ci \
  test_prepare_workflow_does_not_create_prerelease_entry_before_merge \
  test_prepare_workflow_skips_release_pr_merge_pushes \
  test_prepare_workflow_runs_release_infra_fix_pushes \
  test_prepare_workflow_plans_and_checks_semver_before_release_metadata_commit \
  test_prepare_workflow_closes_superseded_release_prs_before_creating_current_pr \
  test_prepare_workflow_selects_maximum_of_semver_minimum_and_conventional_bump \
  test_prepare_workflow_uses_pinned_tea_and_json_parser_tooling \
  test_prepare_workflow_uses_release_bot_identity_for_signed_commits \
  test_prepare_workflow_checkout_does_not_persist_credentials \
  test_prepare_workflow_authenticates_git_push_without_checkout_credentials \
  test_prepare_workflow_manages_release_branch_with_prepare_token \
  test_prepare_workflow_updates_existing_release_pr_body_without_forgejo_release_refs \
  test_prepare_workflow_stages_only_root_release_metadata
