#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: release script flake"

test_prepare_dry_run_plans_release_pr_changes_without_publish() {
  local current_version workdir output status
  workdir="$(mktemp -d)"
  make_workspace "$workdir"
  current_version="$(workspace_version "$workdir/Cargo.toml")"

  output="$(
    FORGEJO_TOKEN= "$RELEASE_TOOL" prepare \
      --workspace "$workdir" \
      --version 0.1.0 \
      --date 2026-05-04 \
      --dry-run 2>&1
  )"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "prepare dry-run exits successfully"
  else
    fail "prepare dry-run exits successfully (status $status, output: $output)"
  fi

  assert_contains "$output" '+version = "0.1.0"' "prepare dry-run plans workspace version bump"
  assert_contains "$output" '+## [0.1.0] - 2026-05-04' "prepare dry-run plans changelog finalization"
  assert_not_contains "$output" 'tea release create' "prepare dry-run does not publish a Forgejo release"
  assert_contains "$(<"$workdir/Cargo.toml")" "version = \"$current_version\"" "prepare dry-run leaves Cargo.toml unchanged"
  assert_contains "$(<"$workdir/CHANGELOG.md")" '<!-- release-prepare inserts generated release sections below this line -->' "prepare dry-run leaves CHANGELOG.md unchanged"
}

test_prepare_non_dry_run_updates_release_files() {
  local workdir output status
  workdir="$(mktemp -d)"
  make_workspace "$workdir"

  output="$({
    FORGEJO_TOKEN= "$RELEASE_TOOL" prepare \
      --workspace "$workdir" \
      --version 0.1.0 \
      --date 2026-05-04
  } 2>&1)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "prepare non-dry-run exits successfully"
  else
    fail "prepare non-dry-run exits successfully (status $status, output: $output)"
  fi

  assert_contains "$(<"$workdir/Cargo.toml")" 'version = "0.1.0"' "prepare non-dry-run updates Cargo.toml workspace version"
  assert_contains "$(<"$workdir/CHANGELOG.md")" '## [0.1.0] - 2026-05-04' "prepare non-dry-run finalizes CHANGELOG release heading"
  assert_not_contains "$(<"$workdir/CHANGELOG.md")" '## [Unreleased]' "prepare non-dry-run does not create an Unreleased section"
}

test_prepare_non_dry_run_updates_arbitrary_current_workspace_version() {
  local workdir output status
  workdir="$(mktemp -d)"
  make_workspace "$workdir"
  python3 - "$workdir/Cargo.toml" <<'PY'
import pathlib
import re
import sys

cargo_toml = pathlib.Path(sys.argv[1])
cargo_toml.write_text(re.sub(r'(?m)^(version\s*=\s*")[^"]+("\s*)$', r'\g<1>2.3.4\2', cargo_toml.read_text(), count=1))
PY

  output="$({
    FORGEJO_TOKEN= "$RELEASE_TOOL" prepare \
      --workspace "$workdir" \
      --version 2.3.5 \
      --date 2026-05-04
  } 2>&1)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "prepare non-dry-run accepts arbitrary current workspace version"
  else
    fail "prepare non-dry-run accepts arbitrary current workspace version (status $status, output: $output)"
  fi

  assert_contains "$(<"$workdir/Cargo.toml")" 'version = "2.3.5"' "prepare non-dry-run updates an arbitrary existing workspace version"
  assert_not_contains "$(<"$workdir/Cargo.toml")" 'version = "2.3.4"' "prepare non-dry-run removes the previous arbitrary workspace version"
}

test_prepare_non_dry_run_updates_cargo_lock_workspace_package_versions() {
  local workdir output status lock_status lock_output
  workdir="$(mktemp -d)"
  make_workspace "$workdir"
  cp "$ROOT/Cargo.lock" "$workdir/Cargo.lock"

  output="$({
    FORGEJO_TOKEN= "$RELEASE_TOOL" prepare \
      --workspace "$workdir" \
      --version 0.1.0 \
      --date 2026-05-04
  } 2>&1)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "prepare non-dry-run exits successfully before Cargo.lock verification"
  else
    fail "prepare non-dry-run exits successfully before Cargo.lock verification (status $status, output: $output)"
  fi

  lock_output="$(python3 - "$workdir/Cargo.lock" <<'PY'
import pathlib
import sys

lock = pathlib.Path(sys.argv[1]).read_text()
bad_versions = []
for package in lock.split('[[package]]'):
    name = None
    version = None
    for line in package.splitlines():
        if line.startswith('name = ') and '"' in line:
            name = line.split('"', 2)[1]
        if line.startswith('version = ') and '"' in line:
            version = line.split('"', 2)[1]
    if name and name.startswith('ar-') and version != '0.1.0':
        bad_versions.append(f'{name} {version}')

if bad_versions:
    print('workspace Cargo.lock package versions not updated to 0.1.0: ' + ', '.join(bad_versions))
    sys.exit(1)
PY
  )"
  lock_status=$?

  if [[ $lock_status -eq 0 ]]; then
    pass "prepare non-dry-run updates Cargo.lock workspace package versions"
  else
    fail "prepare non-dry-run updates Cargo.lock workspace package versions ($lock_output)"
  fi
}

test_prepare_generates_release_notes_from_conventional_commits_since_previous_tag() {
  local workdir output status changelog
  workdir="$(mktemp -d)"
  make_workspace "$workdir"
  cp "$ROOT/Cargo.lock" "$workdir/Cargo.lock"
  cat >"$workdir/CHANGELOG.md" <<'CHANGELOG'
# Changelog

All notable changes to this project will be documented in this file.

<!-- release-prepare inserts generated release sections below this line -->
CHANGELOG

  git -C "$workdir" init >/dev/null
  git -C "$workdir" config user.name "release tooling test"
  git -C "$workdir" config user.email "release-tooling-test@example.invalid"
  git_commit_all "$workdir" "feat(core): pre-tag change must stay out (#100)"
  git -c tag.gpgSign=false -C "$workdir" tag -a v0.0.1 -m "Release v0.0.1"

  git_commit_all "$workdir" "feat(cli): add status output (#101)"
  git_commit_all "$workdir" "fix(gateway): reject stale CI review SHAs (#102)"
  git_commit_all "$workdir" "docs: update operator release notes (#103)"
  git_commit_all "$workdir" "security!: rotate release publish token (#104)"

  output="$({
    FORGEJO_TOKEN= "$RELEASE_TOOL" prepare \
      --workspace "$workdir" \
      --version 0.1.0 \
      --date 2026-05-04
  } 2>&1)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "prepare non-dry-run exits successfully before generated changelog verification"
  else
    fail "prepare non-dry-run exits successfully before generated changelog verification (status $status, output: $output)"
  fi

  changelog="$(<"$workdir/CHANGELOG.md")"
  assert_not_contains "$changelog" '## [Unreleased]' "prepare does not create an Unreleased section before generated release notes"
  assert_file_contains_before "$workdir/CHANGELOG.md" '<!-- release-prepare inserts generated release sections below this line -->' '## [0.1.0] - 2026-05-04' "prepare writes generated release notes below the release marker"
  assert_contains "$changelog" '### Added' "prepare groups feat commits under Added"
  assert_contains "$changelog" '- *(cli)* add status output (#101)' "prepare formats scoped feat commit like release-plz"
  assert_contains "$changelog" '### Fixed' "prepare groups fix commits under Fixed"
  assert_contains "$changelog" '- *(gateway)* reject stale CI review SHAs (#102)' "prepare formats scoped fix commit like release-plz"
  assert_contains "$changelog" '### Security' "prepare groups security commits under Security"
  assert_contains "$changelog" '- [**breaking**] rotate release publish token (#104)' "prepare marks breaking unscoped security commit like release-plz"
  assert_contains "$changelog" '### Other' "prepare groups non-default conventional commits under Other"
  assert_contains "$changelog" '- update operator release notes (#103)' "prepare includes docs commit under Other"
  assert_not_contains "$changelog" 'pre-tag change must stay out (#100)' "prepare excludes commits before previous v tag"
}

test_prepare_does_not_duplicate_existing_release_section_when_previous_tag_is_missing() {
  local workdir output status changelog duplicate_count new_count
  workdir="$(mktemp -d)"
  make_workspace "$workdir"
  cp "$ROOT/Cargo.lock" "$workdir/Cargo.lock"
  cat >"$workdir/CHANGELOG.md" <<'CHANGELOG'
# Changelog

All notable changes to this project will be documented in this file.

<!-- release-prepare inserts generated release sections below this line -->

## [0.1.0] - 2026-05-04

### Added

- *(cli)* add status output (#101)
CHANGELOG

  git -C "$workdir" init >/dev/null
  git -C "$workdir" config user.name "release tooling test"
  git -C "$workdir" config user.email "release-tooling-test@example.invalid"
  git_commit_all "$workdir" "feat(cli): add status output (#101)"
  git_commit_all "$workdir" "chore(release): v0.1.0"
  git_commit_all "$workdir" "fix(gateway): reject stale CI review SHAs (#102)"

  output="$({
    FORGEJO_TOKEN= "$RELEASE_TOOL" prepare \
      --workspace "$workdir" \
      --version 0.1.1 \
      --date 2026-05-05
  } 2>&1)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "prepare non-dry-run exits successfully before missing-tag changelog verification"
  else
    fail "prepare non-dry-run exits successfully before missing-tag changelog verification (status $status, output: $output)"
  fi

  changelog="$(<"$workdir/CHANGELOG.md")"
  assert_file_contains_before "$workdir/CHANGELOG.md" '## [0.1.1] - 2026-05-05' '## [0.1.0] - 2026-05-04' "prepare inserts the new release section above existing release sections"
  duplicate_count="$(grep -F -c -- '- *(cli)* add status output (#101)' "$workdir/CHANGELOG.md")"
  new_count="$(grep -F -c -- '- *(gateway)* reject stale CI review SHAs (#102)' "$workdir/CHANGELOG.md")"
  if [[ "$duplicate_count" == 1 ]]; then
    pass "prepare does not duplicate notes already present in an existing release section"
  else
    fail "prepare does not duplicate notes already present in an existing release section (count: $duplicate_count)"
  fi
  if [[ "$new_count" == 1 ]]; then
    pass "prepare includes only new commits after the previous release section when tag is missing"
  else
    fail "prepare includes only new commits after the previous release section when tag is missing (count: $new_count, changelog: $changelog)"
  fi
}

test_pr_guidance_delegates_changelog_notes_to_release_prepare() {
  local pr_template agents skill prepare_command
  pr_template="$ROOT/.forgejo/pull_request_template.md"
  agents="$ROOT/AGENTS.md"
  skill="$ROOT/.kilo/skills/rust-workspace-engineering/SKILL.md"
  prepare_command="$ROOT/.kilo/command/prepare-forgejo-pr.md"

  assert_file_not_contains "$pr_template" 'CHANGELOG.md updated (under `[Unreleased]`)' "PR template no longer requires per-PR Unreleased changelog edits"
  assert_file_not_contains "$agents" 'CHANGELOG.md` under `[Unreleased]`' "AGENTS no longer requires per-PR Unreleased changelog edits"
  assert_file_not_contains "$skill" 'CHANGELOG.md` under `[Unreleased]`' "Rust workspace skill no longer requires per-PR Unreleased changelog edits"
  assert_file_not_contains "$prepare_command" 'CHANGELOG.md` needs an `[Unreleased]` entry' "prepare-forgejo-pr command no longer checks for per-PR Unreleased changelog edits"

  assert_file_has_line_containing_all "$pr_template" "PR template says release notes come from conventional commits" 'release PR' 'conventional commits'
  assert_file_has_line_containing_all "$agents" "AGENTS says release PR generates changelog notes from conventional commits" 'release PR' 'conventional commits'
  assert_file_has_line_containing_all "$skill" "Rust workspace skill says release PR generates changelog notes from conventional commits" 'release PR' 'conventional commits'
  assert_file_has_line_containing_all "$prepare_command" "prepare-forgejo-pr command says release PR generates changelog notes from conventional commits" 'release PR' 'conventional commits'
}

test_publish_dry_run_requires_merged_release_pr_signal() {
  local workdir unmerged_output unmerged_status merged_output merged_status
  workdir="$(mktemp -d)"
  make_workspace "$workdir"

  unmerged_output="$(
    FORGEJO_EVENT_NAME=pull_request \
    FORGEJO_EVENT_ACTION=closed \
    FORGEJO_PULL_REQUEST_MERGED=false \
    FORGEJO_PULL_REQUEST_HEAD_BRANCH=release/v0.1.0 \
      "$RELEASE_TOOL" publish --workspace "$workdir" --version 0.1.0 --dry-run 2>&1
  )"
  unmerged_status=$?

  if [[ $unmerged_status -ne 0 ]]; then
    pass "publish dry-run refuses unmerged release PR signal"
  else
    fail "publish dry-run refuses unmerged release PR signal"
  fi
  assert_contains "$unmerged_output" 'release PR has not been merged' "publish dry-run explains unmerged refusal"

  merged_output="$(
    FORGEJO_EVENT_NAME=pull_request \
    FORGEJO_EVENT_ACTION=closed \
    FORGEJO_PULL_REQUEST_MERGED=true \
    FORGEJO_PULL_REQUEST_HEAD_BRANCH=release/v0.1.0 \
      FORGEJO_TOKEN= "$RELEASE_TOOL" publish --workspace "$workdir" --version 0.1.0 --dry-run 2>&1
  )"
  merged_status=$?

  if [[ $merged_status -eq 0 ]]; then
    pass "publish dry-run accepts merged release PR signal"
  else
    fail "publish dry-run accepts merged release PR signal (status $merged_status, output: $merged_output)"
  fi
  assert_contains "$merged_output" 'git tag -a v0.1.0' "publish dry-run plans annotated release tag"
  assert_contains "$merged_output" 'tea release create --tag v0.1.0' "publish dry-run plans Forgejo release command"
  assert_not_contains "$merged_output" 'missing FORGEJO_TOKEN' "publish dry-run does not require network credentials"
}

test_publish_non_dry_run_uses_scoped_forgejo_commands_with_fakes() {
  local workdir fakebin command_log output status
  workdir="$(mktemp -d)"
  fakebin="$workdir/bin"
  command_log="$workdir/commands.log"
  make_workspace "$workdir"
  mkdir -p "$fakebin"

  cat >"$fakebin/git" <<FAKE_GIT
#!$BASH
printf 'git %s\n' "\$*" >>"\$RELEASE_TEST_COMMAND_LOG"
FAKE_GIT
  cat >"$fakebin/tea" <<FAKE_TEA
#!$BASH
printf 'tea %s\n' "\$*" >>"\$RELEASE_TEST_COMMAND_LOG"
FAKE_TEA
  cat >"$fakebin/gh" <<FAKE_GH
#!$BASH
printf 'gh %s\n' "\$*" >>"\$RELEASE_TEST_COMMAND_LOG"
FAKE_GH
  chmod +x "$fakebin/git" "$fakebin/tea" "$fakebin/gh"

  output="$({
    PATH="$fakebin:$PATH" \
    RELEASE_TEST_COMMAND_LOG="$command_log" \
    FORGEJO_TOKEN=fake-token \
    FORGEJO_EVENT_NAME=pull_request \
    FORGEJO_EVENT_ACTION=closed \
    FORGEJO_PULL_REQUEST_MERGED=true \
    FORGEJO_PULL_REQUEST_HEAD_BRANCH=release/v0.1.0 \
      "$RELEASE_TOOL" publish --workspace "$workdir" --version 0.1.0
  } 2>&1)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "publish non-dry-run succeeds with faked git and tea"
  else
    fail "publish non-dry-run succeeds with faked git and tea (status $status, output: $output)"
  fi

  assert_file_contains "$command_log" "git -C $workdir tag -a v0.1.0 -m Release v0.1.0" "publish non-dry-run creates an annotated tag in the workspace"
  assert_file_contains "$command_log" 'tea release create --repo jwilger/auto_review --tag v0.1.0' "publish non-dry-run creates a scoped Forgejo release"
  assert_file_contains "$command_log" 'tea release create' "publish non-dry-run uses tea rather than GitHub tooling"
  assert_file_not_contains "$command_log" 'gh ' "publish non-dry-run does not invoke GitHub tooling"
}

test_publish_non_dry_run_pushes_tag_and_sends_changelog_notes() {
  local workdir fakebin command_log output status
  workdir="$(mktemp -d)"
  fakebin="$workdir/bin"
  command_log="$workdir/commands.log"
  make_workspace "$workdir"
  cat >"$workdir/CHANGELOG.md" <<'CHANGELOG'
# Changelog

## [0.1.0] - 2026-05-04

### Fixed

- Fixed production release automation.

## [0.0.1] - 2026-04-01

- Prior release notes must not be included.
CHANGELOG
  mkdir -p "$fakebin"

  cat >"$fakebin/git" <<FAKE_GIT
#!$BASH
printf 'git %s\n' "\$*" >>"\$RELEASE_TEST_COMMAND_LOG"
FAKE_GIT
  cat >"$fakebin/tea" <<FAKE_TEA
#!$BASH
printf 'tea' >>"\$RELEASE_TEST_COMMAND_LOG"
next_notes_file=false
for arg in "\$@"; do
  printf ' [%s]' "\$arg" >>"\$RELEASE_TEST_COMMAND_LOG"
  if [[ "\$next_notes_file" == true ]]; then
    printf '\ntea-release-notes-file:%s\n' "\$arg" >>"\$RELEASE_TEST_COMMAND_LOG"
    if [[ -f "\$arg" ]]; then
      cat "\$arg" >>"\$RELEASE_TEST_COMMAND_LOG"
    fi
    next_notes_file=false
  elif [[ "\$arg" == "--notes-file" || "\$arg" == "--note-file" ]]; then
    next_notes_file=true
  fi
done
printf '\n' >>"\$RELEASE_TEST_COMMAND_LOG"
FAKE_TEA
  cat >"$fakebin/gh" <<FAKE_GH
#!$BASH
printf 'gh %s\n' "\$*" >>"\$RELEASE_TEST_COMMAND_LOG"
FAKE_GH
  chmod +x "$fakebin/git" "$fakebin/tea" "$fakebin/gh"

  output="$({
    PATH="$fakebin:$PATH" \
    RELEASE_TEST_COMMAND_LOG="$command_log" \
    FORGEJO_TOKEN=fake-token \
    FORGEJO_EVENT_NAME=pull_request \
    FORGEJO_EVENT_ACTION=closed \
    FORGEJO_PULL_REQUEST_MERGED=true \
    FORGEJO_PULL_REQUEST_HEAD_BRANCH=release/v0.1.0 \
      "$RELEASE_TOOL" publish --workspace "$workdir" --version 0.1.0
  } 2>&1)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "publish non-dry-run succeeds while passing release notes"
  else
    fail "publish non-dry-run succeeds while passing release notes (status $status, output: $output)"
  fi

  assert_file_has_line_containing_all "$command_log" "publish non-dry-run pushes the release tag with an explicit publish token credential path" 'push origin v0.1.0' 'credential.helper' 'FORGEJO_TOKEN'
  assert_file_contains "$command_log" '--note-file' "publish non-dry-run uses tea release create note-file option"
  assert_file_not_contains "$command_log" '--notes-file' "publish non-dry-run avoids unsupported tea notes-file option"
  assert_file_contains "$command_log" 'Fixed production release automation.' "publish non-dry-run passes release notes from CHANGELOG to tea"
  assert_file_not_contains "$command_log" 'Prior release notes must not be included.' "publish non-dry-run only passes notes for the requested version"
  assert_file_not_contains "$command_log" 'gh ' "publish non-dry-run release notes path does not invoke GitHub tooling"
}

test_prepare_workflow_requires_explicit_prepare_secret_runtime_env() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow receives the explicit prepare secret"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PREPARE_TOKEN"' "release PR preparation workflow gives tea the prepare token directly"
  assert_file_not_contains "$prepare_workflow" 'repo_token=' "release PR preparation workflow does not copy the prepare secret into a shared helper token"
  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow does not expose tea token at step scope"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN' "release PR preparation workflow does not configure manual git push token env"
  assert_file_not_contains "$prepare_workflow" 'GITHUB_TOKEN:-' "release PR preparation workflow does not fall back to GitHub-compatible auto token aliases"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_URL="https://git.johnwilger.com"' "release PR preparation workflow configures tea for Forgejo"
}

test_release_tooling_tests_are_wired_into_nix_flake_check() {
  local flake
  flake="$ROOT/flake.nix"

  assert_file_contains "$flake" 'release-tooling' "nix flake check exposes the release tooling shell test"
  assert_file_contains "$flake" 'bash tests/release_tooling_test.sh' "nix flake check runs release tooling tests"
  assert_file_not_contains "$flake" 'release-plz' "nix flake/dev shell/check does not expose release-plz"
  assert_file_contains "$flake" 'skopeo' "nix flake/dev shell/check exposes skopeo for registry image publication"
  assert_file_contains "$flake" 'cargo-semver-checks' "nix flake/dev shell/check exposes cargo-semver-checks for release planning"
  assert_file_contains "$flake" '/tests/' "nix flake source includes release tooling tests"
  assert_file_contains "$flake" 'AGENTS.md' "nix flake source includes contributor guidance checked by release tooling tests"
  assert_file_contains "$flake" '.forgejo/pull_request_template.md' "nix flake source includes PR template checked by release tooling tests"
  assert_file_contains "$flake" '.kilo/command/prepare-forgejo-pr.md' "nix flake source includes PR command guidance checked by release tooling tests"
  assert_file_contains "$flake" '.kilo/skills/rust-workspace-engineering/SKILL.md' "nix flake source includes Rust workspace skill checked by release tooling tests"
}

test_release_plz_config_is_removed_and_workspace_crates_stay_private() {
  local config cargo
  config="$ROOT/release-plz.toml"
  cargo="$ROOT/Cargo.toml"

  if [[ ! -f "$config" ]]; then
    pass "release-plz configuration is removed"
  else
    fail "release-plz configuration is removed (unexpected file: $config)"
  fi
  assert_file_contains "$cargo" 'publish = false' "workspace crates remain private/non-publish"
}

run_tests \
  test_prepare_dry_run_plans_release_pr_changes_without_publish \
  test_prepare_non_dry_run_updates_release_files \
  test_prepare_non_dry_run_updates_arbitrary_current_workspace_version \
  test_prepare_non_dry_run_updates_cargo_lock_workspace_package_versions \
  test_prepare_generates_release_notes_from_conventional_commits_since_previous_tag \
  test_prepare_does_not_duplicate_existing_release_section_when_previous_tag_is_missing \
  test_pr_guidance_delegates_changelog_notes_to_release_prepare \
  test_publish_dry_run_requires_merged_release_pr_signal \
  test_publish_non_dry_run_uses_scoped_forgejo_commands_with_fakes \
  test_publish_non_dry_run_pushes_tag_and_sends_changelog_notes \
  test_prepare_workflow_requires_explicit_prepare_secret_runtime_env \
  test_release_tooling_tests_are_wired_into_nix_flake_check \
  test_release_plz_config_is_removed_and_workspace_crates_stay_private
