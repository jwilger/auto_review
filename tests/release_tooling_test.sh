#!/usr/bin/env bash
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE_TOOL="$ROOT/scripts/release"

failures=0

fail() {
  printf 'not ok - %s\n' "$1"
  failures=$((failures + 1))
}

pass() {
  printf 'ok - %s\n' "$1"
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local description="$3"

  if [[ "$haystack" == *"$needle"* ]]; then
    pass "$description"
  else
    fail "$description (missing: $needle)"
  fi
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  local description="$3"

  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$description (unexpected: $needle)"
  else
    pass "$description"
  fi
}

assert_file_exists() {
  local path="$1"
  local description="$2"

  if [[ -f "$path" ]]; then
    pass "$description"
  else
    fail "$description (missing file: $path)"
  fi
}

assert_file_contains() {
  local path="$1"
  local needle="$2"
  local description="$3"

  if [[ ! -f "$path" ]]; then
    fail "$description (missing file: $path)"
    return
  fi

  assert_contains "$(<"$path")" "$needle" "$description"
}

assert_file_not_contains() {
  local path="$1"
  local needle="$2"
  local description="$3"

  if [[ ! -f "$path" ]]; then
    fail "$description (missing file: $path)"
    return
  fi

  assert_not_contains "$(<"$path")" "$needle" "$description"
}

assert_file_has_line() {
  local path="$1"
  local expected_line="$2"
  local description="$3"

  if [[ ! -f "$path" ]]; then
    fail "$description (missing file: $path)"
    return
  fi

  while IFS= read -r line; do
    if [[ "$line" == "$expected_line" ]]; then
      pass "$description"
      return
    fi
  done <"$path"

  fail "$description (missing line: $expected_line)"
}

assert_file_lacks_line() {
  local path="$1"
  local forbidden_line="$2"
  local description="$3"

  if [[ ! -f "$path" ]]; then
    fail "$description (missing file: $path)"
    return
  fi

  while IFS= read -r line; do
    if [[ "$line" == "$forbidden_line" ]]; then
      fail "$description (unexpected line: $forbidden_line)"
      return
    fi
  done <"$path"

  pass "$description"
}

workspace_version() {
  python3 - "$1" <<'PY'
import pathlib
import re
import sys

match = re.search(r'(?m)^version\s*=\s*"(?P<version>[^"]+)"', pathlib.Path(sys.argv[1]).read_text())
if not match:
    raise SystemExit("workspace package version not found")
print(match.group("version"))
PY
}

assert_file_has_line_containing_all() {
  local path="$1"
  local description="$2"
  shift 2

  if [[ ! -f "$path" ]]; then
    fail "$description (missing file: $path)"
    return
  fi

  local line needle matched
  while IFS= read -r line; do
    matched=true
    for needle in "$@"; do
      if [[ "$line" != *"$needle"* ]]; then
        matched=false
        break
      fi
    done
    if [[ "$matched" == true ]]; then
      pass "$description"
      return
    fi
  done <"$path"

  fail "$description (missing line containing: $*)"
}

assert_file_contains_before() {
  local path="$1"
  local earlier="$2"
  local later="$3"
  local description="$4"

  if [[ ! -f "$path" ]]; then
    fail "$description (missing file: $path)"
    return
  fi

  local content before_later
  content="$(<"$path")"
  if [[ "$content" != *"$earlier"* ]]; then
    fail "$description (missing earlier marker: $earlier)"
    return
  fi
  if [[ "$content" != *"$later"* ]]; then
    fail "$description (missing later marker: $later)"
    return
  fi

  before_later="${content%%"$later"*}"
  if [[ "$before_later" == *"$earlier"* ]]; then
    pass "$description"
  else
    fail "$description (marker appears after: $later)"
  fi
}

make_workspace() {
  local workspace="$1"
  mkdir -p "$workspace"
  cp "$ROOT/Cargo.toml" "$workspace/Cargo.toml"
  cp "$ROOT/CHANGELOG.md" "$workspace/CHANGELOG.md"
}

git_commit_all() {
  local workspace="$1"
  local message="$2"

  git -C "$workspace" add Cargo.toml Cargo.lock CHANGELOG.md
  git -C "$workspace" commit --allow-empty -m "$message" >/dev/null
}

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
  assert_file_contains "$publish_workflow" 'pull_request' "publish workflow listens for pull request events"
  assert_file_contains "$publish_workflow" 'closed' "publish workflow runs when release PRs close"
  assert_file_contains "$publish_workflow" 'nix develop' "publish workflow enters the Nix development environment before project tooling"
  assert_file_not_contains "$publish_workflow" 'nix build .#ar-gateway-image' "publish workflow promotes the release candidate image instead of rebuilding it"
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

test_prepare_workflow_builds_and_publishes_release_candidate_images() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "release PR preparation workflow exposes the publish registry token for candidate publication"
  assert_file_contains "$prepare_workflow" 'RELEASE_BOT_NAME: ${{ vars.RELEASE_BOT_NAME }}' "release PR preparation workflow uses the release bot name for candidate registry publication"
  assert_file_not_contains "$prepare_workflow" 'RELEASE_CANDIDATE_TOKEN' "release PR preparation workflow does not use a separate candidate registry token"
  assert_file_contains "$prepare_workflow" 'missing RELEASE_PUBLISH_TOKEN' "release PR preparation workflow fails clearly when the publish registry token is missing"
  assert_file_contains "$prepare_workflow" 'nix build .#ar-gateway-image' "release PR preparation workflow builds the ar-gateway image candidate"
  assert_file_contains "$prepare_workflow" 'RELEASE_CANDIDATE_SHA' "release PR preparation workflow derives a candidate SHA tag"
  assert_file_contains "$prepare_workflow" 'RELEASE_CANDIDATE_TAG' "release PR preparation workflow derives a stable release-candidate tag variable"
  assert_file_contains "$prepare_workflow" 'docker-archive:./result' "release PR preparation workflow publishes the Nix-built archive as the candidate image"
  assert_file_contains "$prepare_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_SHA' "release PR preparation workflow publishes the candidate SHA image tag"
  assert_file_contains "$prepare_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_TAG' "release PR preparation workflow publishes the release-candidate image tag"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

def require_ordered(markers, label):
    cursor = 0
    for marker in markers:
        found = workflow.find(marker, cursor)
        if found == -1:
            errors.append(f'{label} missing ordered marker: {marker}')
            return
        cursor = found + len(marker)

require_ordered(
    [
        'git commit -m "chore: release v$RELEASE_VERSION"',
        'RELEASE_CANDIDATE_SHA',
        'nix build .#ar-gateway-image',
        'RELEASE_PUBLISH_TOKEN',
        'docker-archive:./result',
    ],
    'release candidate publication flow',
)

candidate_tag_derivation_patterns = [
    r'RELEASE_CANDIDATE_TAG\s*=\s*[^\n]*\$\{?RELEASE_VERSION\}?[^\n]*rc[^\n]*(?:GITHUB_RUN_NUMBER|RELEASE_CANDIDATE_SHA)',
    r'printf\s+-v\s+RELEASE_CANDIDATE_TAG[\s\S]{0,300}\$\{?RELEASE_VERSION\}?[\s\S]{0,300}rc[\s\S]{0,300}(?:GITHUB_RUN_NUMBER|RELEASE_CANDIDATE_SHA)',
    r'RELEASE_CANDIDATE_TAG[\s\S]{0,300}\$\{?RELEASE_VERSION\}?[\s\S]{0,300}rc[\s\S]{0,300}(?:GITHUB_RUN_NUMBER|RELEASE_CANDIDATE_SHA)',
]
if not any(re.search(pattern, workflow) for pattern in candidate_tag_derivation_patterns):
    errors.append('RELEASE_CANDIDATE_TAG must be a SemVer pre-release candidate derived from RELEASE_VERSION with rc provenance tied to GITHUB_RUN_NUMBER or RELEASE_CANDIDATE_SHA')

publish_steps = []
for step_match in re.finditer(r'- name: (?P<name>[^\n]+)(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow):
    body = step_match.group('body')
    if 'skopeo copy' in body and 'docker-archive:./result' in body:
        publish_steps.append(body)

if not publish_steps:
    errors.append('prepare workflow must use skopeo to publish docker-archive:./result as candidate images')
else:
    publish_text = '\n'.join(publish_steps)
    if 'RELEASE_PUBLISH_TOKEN' not in publish_text:
        errors.append('candidate image publication must authenticate with RELEASE_PUBLISH_TOKEN')
    if 'RELEASE_BOT_NAME' not in publish_text:
        errors.append('candidate image publication must authenticate as RELEASE_BOT_NAME')
    if 'docker-archive:./result' not in publish_text:
        errors.append('candidate image publication must use docker-archive:./result as the source')
    if 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_SHA' not in publish_text and 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_CANDIDATE_SHA}' not in publish_text:
        errors.append('prepare workflow must publish docker-archive:./result to the RELEASE_CANDIDATE_SHA tag')
    if 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_TAG' not in publish_text and 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_CANDIDATE_TAG}' not in publish_text:
        errors.append('prepare workflow must publish docker-archive:./result to the RELEASE_CANDIDATE_TAG tag')

for step_match in re.finditer(r'- name: (?P<name>[^\n]+)(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow):
    body = step_match.group('body')
    if 'RELEASE_PUBLISH_TOKEN' in body and 'nix build .#ar-gateway-image' in body:
        errors.append('publish-token-bearing step must publish only after a separate no-token Nix image build')

if 'tea pr create' not in workflow:
    errors.append('prepare workflow must create the release PR')
else:
    pr_section = workflow[workflow.find('tea pr create'):]
    if 'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_SHA' not in pr_section:
        errors.append('release PR create/update description must expose the candidate SHA image ref')
    if 'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_TAG' not in pr_section and 'git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_CANDIDATE_TAG}' not in pr_section:
        errors.append('release PR create/update description must expose the release-candidate image ref')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow builds and publishes release candidate images after the metadata commit"
  else
    fail "release PR preparation workflow builds and publishes release candidate images after the metadata commit ($output)"
  fi
}

test_prepare_workflow_creates_prerelease_entry_for_release_candidate() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
if 'tea release create' not in workflow:
    print('prepare workflow must create a Forgejo Release entry for RELEASE_CANDIDATE_TAG with tea release create --prerelease')
    sys.exit(1)

start = workflow.find('release_publish_login=')
if start == -1:
    print('prepare workflow must use a publish-token tea login for release candidate prereleases')
    sys.exit(1)
release_block = workflow[start:]

required_markers = [
    'RELEASE_CANDIDATE_TAG',
    'GITEA_SERVER_TOKEN',
    'tea login add',
    '--token "$GITEA_SERVER_TOKEN"',
    'tea release edit',
    'tea release create',
    '--login "$release_publish_login"',
    '--repo jwilger/auto_review',
    '--tag',
    '"$RELEASE_CANDIDATE_TAG"',
    '--target "$RELEASE_CANDIDATE_SHA"',
    '--prerelease',
    '--prerelease true',
    '|| nix develop --command tea release create',
]
missing = [marker for marker in required_markers if marker not in release_block]
if missing:
    print('prepare workflow must create a prerelease Forgejo Release for each release candidate; missing: ' + ', '.join(missing))
    sys.exit(1)

edit_start = release_block.find('tea release edit')
create_start = release_block.find('tea release create')
if edit_start == -1 or create_start == -1 or edit_start > create_start:
    print('prepare workflow must update an existing prerelease before falling back to create')
    sys.exit(1)
edit_block = release_block[edit_start:create_start]
create_block = release_block[create_start:]
shared_release_markers = [
    '--login "$release_publish_login"',
    '--repo jwilger/auto_review',
    '--target "$RELEASE_CANDIDATE_SHA"',
    '--title "$RELEASE_CANDIDATE_TAG"',
    '--note "$release_note"',
]
for block_name, block in [('edit', edit_block), ('create', create_block)]:
    missing_from_block = [marker for marker in shared_release_markers if marker not in block]
    if missing_from_block:
        print(f'prepare workflow {block_name} prerelease command is missing: ' + ', '.join(missing_from_block))
        sys.exit(1)
if '--prerelease true' not in edit_block:
    print('prepare workflow edit prerelease command must preserve prerelease=true')
    sys.exit(1)
if '--tag "$RELEASE_CANDIDATE_TAG"' not in create_block or '--prerelease' not in create_block:
    print('prepare workflow create prerelease command must create the release-candidate tag as prerelease')
    sys.exit(1)

ordered_markers = [
    'RELEASE_CANDIDATE_TAG="$RELEASE_VERSION-rc.${GITHUB_RUN_NUMBER:-0}"',
    'nix develop --command skopeo copy',
    'tea release edit',
    'tea release create',
]
cursor = 0
for marker in ordered_markers:
    found = workflow.find(marker, cursor)
    if found == -1:
        print('prepare workflow must publish candidate images before creating or updating the prerelease; missing ordered marker: ' + marker)
        sys.exit(1)
    cursor = found + len(marker)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow creates a prerelease Forgejo Release for each release candidate"
  else
    fail "release PR preparation workflow creates a prerelease Forgejo Release for each release candidate ($output)"
  fi
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

test_prepare_workflow_runs_tea_and_jq_inside_nix_develop() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
bare_tool_pattern = re.compile(r'(^|[|;&]\s*)(tea|jq)\b')
bare_tool_lines = []
has_nix_tea = False
has_nix_jq = False
for line_number, line in enumerate(workflow, 1):
    stripped = line.strip()
    if not stripped or stripped.startswith('#'):
        continue
    if 'nix develop --command tea' in stripped:
        has_nix_tea = True
    if 'nix develop --command jq' in stripped:
        has_nix_jq = True
    if bare_tool_pattern.search(stripped):
        bare_tool_lines.append(f'{line_number}: {stripped}')

if bare_tool_lines:
    print('prepare workflow invokes runner-provided tea/jq instead of Nix dev shell tools:')
    print('\n'.join(bare_tool_lines))
    sys.exit(1)
if not has_nix_tea:
    print('prepare workflow does not invoke tea through nix develop --command')
    sys.exit(1)
if not has_nix_jq:
    print('prepare workflow does not invoke jq through nix develop --command')
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow gets tea and jq from the Nix dev shell"
  else
    fail "release PR preparation workflow gets tea and jq from the Nix dev shell ($output)"
  fi
}

test_publish_workflow_requires_release_pr_base_branch_main() {
  local publish_workflow
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" "github.event.pull_request.base.ref == 'main'" "publish workflow only runs for release PRs merged into main"
  assert_file_contains "$publish_workflow" 'FORGEJO_PULL_REQUEST_BASE_BRANCH: ${{ github.event.pull_request.base.ref }}' "publish workflow exposes base branch to release tooling"
}

test_release_workflows_use_prepare_secret_and_protected_publish_token() {
  local prepare_workflow publish_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not use unsupported forgejo.token expression for tea"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not use unsupported forgejo.token expression for git push"
  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow exposes the prepare-scoped Actions secret to release tooling"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PREPARE_TOKEN"' "release PR preparation workflow passes the prepare-scoped token to PR management tea calls"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PUBLISH_TOKEN"' "release PR preparation workflow passes the publish-scoped token to release creation tea calls"
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

test_publish_workflow_validates_provenance_and_changed_files_before_publish_token() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" 'Validate release provenance and changed files' "publish workflow has a no-token provenance validation step"
  assert_file_contains "$publish_workflow" 'RELEASE_BASE_SHA: ${{ github.event.pull_request.base.sha }}' "publish workflow records the release PR base SHA for provenance checks"
  assert_file_contains "$publish_workflow" 'RELEASE_MERGE_SHA: ${{ inputs.release_merge_sha || github.event.pull_request.merge_commit_sha }}' "publish workflow records the release merge SHA for provenance checks"
  assert_file_contains "$publish_workflow" 'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"' "publish workflow derives changed files from the merged release PR"
  assert_file_contains "$publish_workflow" 'case "$changed_file" in' "publish workflow evaluates each changed file before publishing"
  assert_file_contains "$publish_workflow" 'Cargo.toml|Cargo.lock|CHANGELOG.md)' "publish workflow allows release metadata files before publishing"
  output="$(python3 - "$publish_workflow" <<'PY'
import fnmatch
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    print('missing Docker image registry publish marker')
    sys.exit(1)
validation_section = workflow.split(publish_marker, 1)[0]
case_match = re.search(r'case "\$changed_file" in(?P<body>.*?)\n\s*esac', validation_section, re.S)
if not case_match:
    print('missing changed-file case allowlist before publish')
    sys.exit(1)

patterns = []
case_lines = case_match.group('body').splitlines()
index = 0
while index < len(case_lines):
    stripped = case_lines[index].strip()
    index += 1
    if not stripped or stripped.startswith('#') or ')' not in stripped:
        continue

    head = stripped.split(')', 1)[0]
    arm_body = []
    while index < len(case_lines):
        body_line = case_lines[index].strip()
        arm_body.append(body_line)
        index += 1
        if body_line == ';;':
            break

    meaningful_body = [line for line in arm_body if line and not line.startswith('#')]
    is_pass_through = meaningful_body == [';;']
    is_reject = any('exit' in line or 'refusing token-bearing publish' in line for line in meaningful_body)
    is_default = head.strip() == '*'
    if not is_pass_through or is_reject or is_default:
        continue

    if any(token in head for token in ['*', '/', '.toml', '.md', '.lock']):
        patterns.extend(part for part in head.split('|') if part)

required = {
    'Cargo.toml': False,
    'Cargo.lock': False,
    'CHANGELOG.md': False,
}
for sample in required:
    required[sample] = any(fnmatch.fnmatchcase(sample, pattern) for pattern in patterns)

missing = [sample for sample, allowed in required.items() if not allowed]
if missing:
    print('publish allowlist does not permit root release metadata needed for image publication: ' + ', '.join(missing))
    print('observed allowlist patterns: ' + ', '.join(patterns))
    sys.exit(1)
forbidden = ['crates/ar-cli/Cargo.toml', 'crates/ar-cli/CHANGELOG.md']
permitted_forbidden = [sample for sample in forbidden if any(fnmatch.fnmatchcase(sample, pattern) for pattern in patterns)]
if permitted_forbidden:
    print('publish allowlist permits crate package metadata even though publishing is image-based: ' + ', '.join(permitted_forbidden))
    print('observed allowlist patterns: ' + ', '.join(patterns))
    sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow allows only root release metadata before image publishing"
  else
    fail "publish workflow allows only root release metadata before image publishing ($output)"
  fi
  assert_file_contains "$publish_workflow" '.forgejo/workflows/*|scripts/*)' "publish workflow explicitly rejects script and workflow changes before publishing"
  assert_file_contains "$publish_workflow" 'refusing token-bearing publish for release PR file:' "publish workflow fails closed for unexpected release PR files"
  assert_file_contains_before "$publish_workflow" 'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"' 'Publish Docker image to Forgejo package registry' "publish workflow validates changed files before publishing the image with the publish token"
  assert_file_contains_before "$publish_workflow" '.forgejo/workflows/*|scripts/*)' 'Publish Docker image to Forgejo package registry' "publish workflow rejects script and workflow changes before publishing the image with the publish token"
}

test_publish_workflow_uses_release_pr_merge_sha_not_a_recomputed_version() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_not_contains "$publish_workflow" 'RELEASE_VERSION="${FORGEJO_PULL_REQUEST_HEAD_BRANCH#release/v}"' "publish workflow does not derive a release version from a hand-managed branch"
  assert_file_not_contains "$publish_workflow" '[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]' "publish workflow does not recompute release versions"
  assert_file_not_contains "$publish_workflow" 'release-plz' "publish workflow does not use release-plz"
  assert_file_contains "$publish_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway' "publish workflow publishes the application Docker image rather than workspace crates"
  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []
for forbidden in [
    'FORGEJO_PULL_REQUEST_HEAD_BRANCH#release/v',
    'scripts/release plan',
    'cargo semver-checks',
]:
    if forbidden in workflow:
        errors.append(f'publish workflow must not derive RELEASE_VERSION from branch names or planning: {forbidden}')

trusted_metadata_patterns = [
    r'RELEASE_VERSION=.*Cargo\.toml',
    r'Cargo\.toml.*RELEASE_VERSION=',
    r'workspace_version\s+Cargo\.toml',
    r'pathlib\.Path\(["\']Cargo\.toml["\']\).*version',
]
if not any(re.search(pattern, workflow) for pattern in trusted_metadata_patterns):
    errors.append('publish workflow must derive RELEASE_VERSION from trusted Cargo.toml at the checked-out merge commit')

publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker in workflow:
    before_publish = workflow.split(publish_marker, 1)[0]
    if 'Cargo.toml' not in before_publish or 'RELEASE_VERSION' not in before_publish:
        errors.append('publish workflow must derive RELEASE_VERSION before token-bearing publication begins')
else:
    errors.append('publish workflow is missing the token-bearing publication step marker')

if errors:
    print('; '.join(errors))
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow derives release version from checked-out Cargo.toml metadata"
  else
    fail "publish workflow derives release version from checked-out Cargo.toml metadata ($output)"
  fi
}

test_publish_workflow_executes_from_merge_commit_sha_before_publish_token() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" 'ref: ${{ inputs.release_merge_sha || github.event.pull_request.merge_commit_sha }}' "publish workflow checks out the release merge commit"
  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
for index, line in enumerate(workflow):
    if line.strip() == "- uses: actions/checkout@v4":
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
        required_checkout_settings = {
            "fetch-depth: 0": False,
            "persist-credentials: false": False,
        }
        for nested in workflow[with_index + 1:]:
            stripped = nested.strip()
            if not stripped:
                continue
            nested_indent = len(nested) - len(nested.lstrip())
            if nested_indent <= with_indent:
                break
            if stripped in required_checkout_settings:
                required_checkout_settings[stripped] = True
        missing = [setting for setting, present in required_checkout_settings.items() if not present]
        if not missing:
                sys.exit(0)
        print(f"actions/checkout@v4 with mapping is missing: {', '.join(missing)}")
sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow checkout fetches full history without persisting credentials"
  else
    fail "publish workflow checkout fetches full history without persisting credentials ($output)"
  fi
  assert_file_contains "$publish_workflow" '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]' "publish workflow asserts HEAD is the merged release PR commit"
  assert_file_contains_before "$publish_workflow" '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]' 'Publish Docker image to Forgejo package registry' "publish workflow verifies checked-out merge commit before publishing the image with the publish token"
}

test_publish_workflow_attaches_merge_commit_to_main_with_upstream_before_image_publish() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    print('missing Docker image registry publication step')
    sys.exit(1)

before_publish = workflow.split(publish_marker, 1)[0]
lines = before_publish.splitlines()
attach_head_patterns = [
    r'\bgit\s+switch\b.*(?:-C|--force-create|-c|--create)\s+main\b.*\$RELEASE_MERGE_SHA',
    r'\bgit\s+checkout\b.*(?:-B|-b)\s+main\b.*\$RELEASE_MERGE_SHA',
]
move_main_patterns = [
    r'\bgit\s+branch\b.*(?:-f|--force)\s+main\b.*\$RELEASE_MERGE_SHA',
    r'\bgit\s+update-ref\s+refs/heads/main\s+\$RELEASE_MERGE_SHA\b',
]
checkout_main_patterns = [
    r'\bgit\s+switch\b(?!.*\$RELEASE_MERGE_SHA).*\bmain\b',
    r'\bgit\s+checkout\b(?!.*\$RELEASE_MERGE_SHA).*\bmain\b',
]
set_upstream_patterns = [
    r'\bgit\s+branch\b.*(?:--set-upstream-to|-u)(?:=|\s+)origin/main(?:\s+main)?\b',
]
upstream_remote_patterns = [r'\bgit\s+config\s+branch\.main\.remote\s+origin\b']
upstream_merge_patterns = [r'\bgit\s+config\s+branch\.main\.merge\s+refs/heads/main\b']

def first_matching_line(patterns, start=0):
    for index, line in enumerate(lines[start:], start):
        if any(re.search(pattern, line) for pattern in patterns):
            return index
    return None

attach_line = first_matching_line(attach_head_patterns)
if attach_line is None:
    move_main_line = first_matching_line(move_main_patterns)
    if move_main_line is not None:
        checkout_main_line = first_matching_line(checkout_main_patterns, move_main_line + 1)
        if checkout_main_line is not None:
            attach_line = checkout_main_line

if attach_line is None:
    print('missing attached HEAD on local main at $RELEASE_MERGE_SHA before registry image publication')
    sys.exit(1)

set_upstream_line = first_matching_line(set_upstream_patterns, attach_line + 1)
remote_line = first_matching_line(upstream_remote_patterns, attach_line + 1)
merge_line = first_matching_line(upstream_merge_patterns, attach_line + 1)
if set_upstream_line is None and (remote_line is None or merge_line is None):
    print('missing complete local main upstream tracking of origin/main after HEAD attaches to main and before registry image publication')
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow attaches the merge commit to main with upstream before registry image publication"
  else
    fail "publish workflow attaches the merge commit to main with upstream before registry image publication ($output)"
  fi
}

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

test_prepare_workflow_updates_existing_release_pr_body_with_candidate_images() {
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
missing = []
if not has_update_command:
    missing.append('tea pr edit or tea api update in existing_pr branch')
for marker in [
    'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_SHA',
    'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_TAG',
]:
    if marker not in existing_pr_branch:
        missing.append(marker)

if missing:
    print('existing release PR branch must update the PR body with candidate image refs: ' + ', '.join(missing))
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "prepare workflow updates existing release PR body with release candidate image refs"
  else
    fail "prepare workflow updates existing release PR body with release candidate image refs ($output)"
  fi
}

test_prepare_workflow_stages_only_root_release_metadata() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'scripts/release prepare --workspace . --version "$RELEASE_VERSION"' "release PR preparation workflow updates root release metadata"
  assert_file_contains "$prepare_workflow" 'git add Cargo.toml Cargo.lock CHANGELOG.md' "release PR preparation workflow stages root release metadata explicitly"
  assert_file_not_contains "$prepare_workflow" 'crates/*/CHANGELOG.md' "release PR preparation workflow does not stage per-crate changelogs"
}

test_release_tooling_uses_local_prepare_and_image_registry_for_publish() {
  local prepare_workflow publish_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$prepare_workflow" 'scripts/release plan --workspace .' "release PR preparation workflow invokes local release planning"
  assert_file_contains "$prepare_workflow" 'scripts/release prepare --workspace . --version "$RELEASE_VERSION"' "release PR preparation workflow invokes local release metadata preparation"
  assert_file_not_contains "$prepare_workflow" 'release-plz' "release PR preparation workflow does not invoke release-plz"
  assert_file_not_contains "$publish_workflow" 'release-plz' "publish workflow does not invoke release-plz"
  assert_file_contains "$publish_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway' "publish workflow targets the application image package registry"
  assert_file_contains "$prepare_workflow" 'tea login add' "release PR preparation workflow configures tea login"
  assert_file_contains "$prepare_workflow" 'tea pr create' "release PR preparation workflow uses tea for PR management"
  assert_file_contains "$publish_workflow" 'tea release create' "publish workflow creates a Forgejo Release entry"
}

test_publish_workflow_promotes_release_image_tags_and_generates_release_notes() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

image = 'git.johnwilger.com/jwilger/auto_review/ar-gateway'
required_image_tags = {
    'candidate SHA source image tag': [f'{image}:$RELEASE_CANDIDATE_SHA', f'{image}:${{RELEASE_CANDIDATE_SHA}}'],
    'raw release version image tag': [f'{image}:$RELEASE_VERSION', f'{image}:${{RELEASE_VERSION}}'],
    'latest image tag': [f'{image}:latest'],
}
for description, candidates in required_image_tags.items():
    if not any(candidate in workflow for candidate in candidates):
        errors.append(f'missing {description}')

release_markers = [
    'tea login add',
    'tea release create',
    '--repo jwilger/auto_review',
    '--tag v$RELEASE_VERSION',
    '--target "$RELEASE_MERGE_SHA"',
    '--note-file',
    'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}',
    'GITEA_SERVER_URL: https://git.johnwilger.com',
]
missing_release_markers = [marker for marker in release_markers if marker not in workflow]
if missing_release_markers:
    errors.append('missing authenticated Forgejo Release creation markers: ' + ', '.join(missing_release_markers))

note_file_candidates = ['release_notes_file', 'RELEASE_NOTES_FILE', 'notes_file']
if not any(candidate in workflow for candidate in note_file_candidates):
    errors.append('publish workflow must write a dedicated release notes file before tea release create')

finalized_changelog_patterns = [
    r'CHANGELOG\.md[\s\S]{0,500}## \[\$RELEASE_VERSION\]',
    r'## \[\$RELEASE_VERSION\][\s\S]{0,500}CHANGELOG\.md',
    r'CHANGELOG\.md[\s\S]{0,500}## \[v?\$\{?RELEASE_VERSION\}?\]',
]
if not any(re.search(pattern, workflow) for pattern in finalized_changelog_patterns):
    errors.append('release notes file must be generated from the finalized CHANGELOG.md section for RELEASE_VERSION')

docker_link_candidates = [
    'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION',
    'git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}',
]
if not any(candidate in workflow for candidate in docker_link_candidates):
    errors.append('release notes file must include the raw-version release Docker image link git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION')

if '--note-file' in workflow and 'tea release create' in workflow:
    note_file_index = min((workflow.find(candidate) for candidate in note_file_candidates if candidate in workflow), default=-1)
    release_create_index = workflow.find('tea release create')
    if note_file_index == -1 or note_file_index > release_create_index:
        errors.append('release notes file must be generated before tea release create consumes --note-file')

release_step_match = re.search(r'- name: Create Forgejo Release(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not release_step_match:
    errors.append('publish workflow must have a dedicated Create Forgejo Release step')
else:
    release_step = release_step_match.group('body')
    if 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' not in release_step:
        errors.append('Create Forgejo Release step must receive the publish token as GITEA_SERVER_TOKEN')
    if 'tea login add' not in release_step:
        errors.append('Create Forgejo Release step must authenticate tea explicitly with tea login add')
    if 'tea release create' not in release_step:
        errors.append('Create Forgejo Release step must create the Forgejo Release with tea release create')
    if '--target "$RELEASE_MERGE_SHA"' not in release_step:
        errors.append('tea release create must pin the release tag target to $RELEASE_MERGE_SHA')

if errors:
    print('; '.join(errors))
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow promotes release image tags and generates Forgejo release notes"
  else
    fail "publish workflow promotes release image tags and generates Forgejo release notes ($output)"
  fi
}

test_publish_workflow_publishes_nix_docker_image_to_forgejo_registry() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []
if 'release-plz release --forge gitea --git-token "$RELEASE_PUBLISH_TOKEN"' in workflow:
    errors.append('publish workflow still delegates publication to release-plz cargo release')
if 'RELEASE_REGISTRY_USER' in workflow:
    errors.append('publish workflow should reuse RELEASE_BOT_NAME instead of a separate RELEASE_REGISTRY_USER')

if 'nix build .#ar-gateway-image' in workflow:
    errors.append('publish workflow must promote the prepared candidate image instead of rebuilding .#ar-gateway-image')
if 'docker-archive:./result' in workflow:
    errors.append('publish workflow must not publish release tags from docker-archive:./result')

required_markers = [
    'git.johnwilger.com/jwilger/auto_review/ar-gateway',
    'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}',
    'RELEASE_BOT_NAME: ${{ vars.RELEASE_BOT_NAME }}',
    'RELEASE_CANDIDATE_SHA',
]
missing = [marker for marker in required_markers if marker not in workflow]
if missing:
    errors.append('publish workflow does not promote the release candidate Docker image to the Forgejo package registry: ' + ', '.join(missing))

lines = workflow.splitlines()
def workflow_steps():
    steps = []
    for index, line in enumerate(lines):
        if line.startswith('      - '):
            step_lines = [line]
            for nested in lines[index + 1:]:
                if nested.startswith('      - '):
                    break
                step_lines.append(nested)
            steps.append('\n'.join(step_lines))
    return steps

for index, line in enumerate(lines):
    if line.startswith('      - '):
        step_lines = [line]
        for nested in lines[index + 1:]:
            if nested.startswith('      - '):
                break
            step_lines.append(nested)
        step = '\n'.join(step_lines)
        if 'RELEASE_PUBLISH_TOKEN' in step and ('nix build .#ar-gateway-image' in step or 'docker-archive:./result' in step):
            errors.append('token-bearing publish step must promote the candidate image, not rebuild or publish a docker archive')
            break

promotion_steps = []
for step in workflow_steps():
    if 'Publish Docker image to Forgejo package registry' in step or ('skopeo copy' in step and 'RELEASE_CANDIDATE_SHA' in step):
        promotion_steps.append(step)
if not promotion_steps:
    errors.append('publish workflow is missing concrete skopeo promotion from docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_SHA')
    before_publish = workflow
    publish_text = ''
else:
    first_publish = min(workflow.find(step) for step in promotion_steps)
    before_publish = workflow[:first_publish]
    publish_text = '\n'.join(promotion_steps)
    if 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_SHA' not in publish_text and 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_CANDIDATE_SHA}' not in publish_text:
        errors.append('publish workflow must use the release candidate SHA image as the promotion source')
    version_destinations = [
        'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION',
        'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}',
    ]
    if not any(destination in publish_text for destination in version_destinations):
        errors.append('publish workflow must promote the release candidate image to RELEASE_VERSION')
    if 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:latest' not in publish_text:
        errors.append('publish workflow must promote the release candidate image to latest')
auth_patterns = [
    r'\b(?:docker|podman)\s+login\b[^\n]*git\.johnwilger\.com[\s\S]{0,400}\$RELEASE_PUBLISH_TOKEN',
    r'\$RELEASE_PUBLISH_TOKEN[\s\S]{0,400}\b(?:docker|podman)\s+login\b[^\n]*git\.johnwilger\.com',
]
has_login_before_publish = any(re.search(pattern, before_publish) for pattern in auth_patterns)
has_skopeo_creds_on_copy = re.search(r'\bskopeo\s+copy\b[\s\S]{0,1000}(?:--src-creds|--dest-creds|--src-authfile|--dest-authfile)\b[\s\S]{0,300}\$RELEASE_PUBLISH_TOKEN', publish_text)
if not has_login_before_publish and not has_skopeo_creds_on_copy:
    errors.append('publish workflow must authenticate to git.johnwilger.com with RELEASE_PUBLISH_TOKEN before pushing or copying the image')

if re.search(r'git\.johnwilger\.com/jwilger/auto_review/ar-gateway:dev\b', workflow):
    errors.append('publish workflow must not publish the flake image default :dev tag as the release artifact')

if 'git.johnwilger.com/jwilger/auto_review/ar-gateway:latest' not in workflow:
    errors.append('publish workflow must promote the release candidate image to latest')
if 'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION' not in workflow and 'git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}' not in workflow:
    errors.append('publish workflow must promote the release candidate image to RELEASE_VERSION')

if errors:
    print('; '.join(errors))
    sys.exit(1)

sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow publishes the Nix-built Docker image to the Forgejo package registry"
  else
    fail "publish workflow publishes the Nix-built Docker image to the Forgejo package registry ($output)"
  fi
}

test_publish_workflow_requires_trusted_release_environment() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
job_line = None
for index, line in enumerate(workflow):
    if line == "  release-publish:":
        job_line = index
        break
if job_line is None:
    print("release-publish job is missing")
    sys.exit(1)

job_indent = len(workflow[job_line]) - len(workflow[job_line].lstrip())
environment_line = None
steps_line = None
for index in range(job_line + 1, len(workflow)):
    line = workflow[index]
    if not line.strip():
        continue
    indent = len(line) - len(line.lstrip())
    if indent <= job_indent:
        break
    if indent == job_indent + 2 and line.strip() == "environment: release-publish":
        environment_line = index
    if indent == job_indent + 2 and line.strip() == "steps:":
        steps_line = index
        break

if steps_line is None:
    print("release-publish job is missing steps")
    sys.exit(1)
if environment_line is None:
    print("release-publish job is missing job-level environment: release-publish before steps")
    sys.exit(1)
if environment_line > steps_line:
    print("release-publish job environment: release-publish appears after steps")
    sys.exit(1)
sys.exit(0)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow requires the protected release-publish environment at job level before steps"
  else
    fail "publish workflow requires the protected release-publish environment at job level before steps ($output)"
  fi
}

test_publish_workflow_supports_manual_dispatch_from_release_merge_sha() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
if "workflow_dispatch:" not in workflow:
    print("publish workflow is missing workflow_dispatch trigger")
    sys.exit(1)
if "release_merge_sha:" not in workflow:
    print("publish workflow is missing release_merge_sha dispatch input")
    sys.exit(1)

dispatch_section = workflow.split("workflow_dispatch:", 1)[1].split("jobs:", 1)[0]
for required in ["inputs:", "release_merge_sha:", "required: true"]:
    if required not in dispatch_section:
        print(f"publish workflow dispatch input contract is missing: {required}")
        sys.exit(1)
dispatch_lines = dispatch_section.splitlines()
inputs_line = None
for index, line in enumerate(dispatch_lines):
    if line.strip() == "inputs:":
        inputs_line = index
        break
if inputs_line is None:
    print("publish workflow dispatch input contract is missing: inputs:")
    sys.exit(1)
input_names = []
for line in dispatch_lines[inputs_line + 1:]:
    if not line.strip():
        continue
    indent = len(line) - len(line.lstrip())
    if indent <= 4:
        break
    stripped = line.strip()
    if indent == 6 and stripped.endswith(":"):
        input_names.append(stripped[:-1])
if input_names != ["release_merge_sha"]:
    print(f"publish workflow dispatch should expose only release_merge_sha input, got: {input_names}")
    sys.exit(1)
release_input_lines = dispatch_section.split("release_merge_sha:", 1)[1].splitlines()
release_input = "\n".join(
    line
    for line in release_input_lines
    if not line.strip() or len(line) - len(line.lstrip()) > 6
)
if "required: true" not in release_input:
    print("publish workflow release_merge_sha dispatch input must be required")
    sys.exit(1)
if "type: string" not in release_input:
    print("publish workflow release_merge_sha dispatch input must render as a Forgejo UI text field")
    sys.exit(1)

job_header = "  release-publish:"
if job_header not in workflow:
    print("release-publish job is missing")
    sys.exit(1)
job_section = workflow.split(job_header, 1)[1]
job_if = None
for line in job_section.splitlines():
    if line.startswith("    steps:"):
        break
    if line.startswith("    if:"):
        job_if = line
        break
if job_if is None:
    print("release-publish job is missing an if condition")
    sys.exit(1)
condition = job_if.split("if:", 1)[1].strip()
if condition.startswith("${{") and condition.endswith("}}"):
    condition = condition[3:-2].strip()
paths = [path.strip(" ()") for path in condition.split("||")]
if len(paths) < 2:
    print("release-publish job condition must admit manual dispatch through a separate || path")
    sys.exit(1)
trusted_pr_paths = [
    path for path in paths
    if "github.event.pull_request.merged == true" in path
    and "github.event.pull_request.base.ref == 'main'" in path
    and "startsWith(github.event.pull_request.head.ref, 'release/v')" in path
]
manual_paths = [
    path for path in paths
    if "github.event_name == 'workflow_dispatch'" in path
    and "inputs.release_merge_sha" in path
    and "pull_request" not in path
]
if not trusted_pr_paths:
    print("release-publish job condition is missing trusted merged release PR path")
    sys.exit(1)
if not manual_paths:
    print("release-publish job condition is missing separate workflow_dispatch path gated by non-empty inputs.release_merge_sha")
    sys.exit(1)
expected_manual_paths = {
    "github.event_name == 'workflow_dispatch' && inputs.release_merge_sha != ''",
    'github.event_name == \'workflow_dispatch\' && inputs.release_merge_sha != ""',
}
if not any(path in expected_manual_paths for path in manual_paths):
    print("release-publish job workflow_dispatch path must be exactly github.event_name == 'workflow_dispatch' && inputs.release_merge_sha != ''")
    sys.exit(1)

release_sha = "${{ inputs.release_merge_sha || github.event.pull_request.merge_commit_sha }}"
required_markers = [
    f"ref: {release_sha}",
    f"RELEASE_MERGE_SHA: {release_sha}",
    "environment: release-publish",
    'git switch -C main "$RELEASE_MERGE_SHA"',
    'Publish Docker image to Forgejo package registry',
    'tea release create',
    'RELEASE_CANDIDATE_SHA',
    'git.johnwilger.com/jwilger/auto_review/ar-gateway',
]
missing = [marker for marker in required_markers if marker not in workflow]
if missing:
    print("publish workflow manual dispatch does not reuse release merge SHA for checkout/provenance/branch attachment/publication: " + ", ".join(missing))
    sys.exit(1)

publish_marker = 'Publish Docker image to Forgejo package registry'
before_publish = workflow.split(publish_marker, 1)[0]
for required in [
    "Validate release provenance and changed files",
    '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]',
    'git merge-base --is-ancestor "$RELEASE_MERGE_SHA" origin/main',
    'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"',
]:
    if required not in before_publish:
        print(f"publish workflow manual dispatch is missing no-token provenance validation before publish token exposure: {required}")
        sys.exit(1)
token_index = workflow.find("RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}")
validation_index = workflow.find("Validate release provenance and changed files")
if token_index == -1:
    print("publish workflow is missing protected release publish token exposure")
    sys.exit(1)
if validation_index == -1 or token_index < validation_index:
    print("publish workflow exposes publish token before no-token provenance validation")
    sys.exit(1)
validation_step = None
lines = workflow.splitlines()
for index, line in enumerate(lines):
    if line.strip() == "- name: Validate release provenance and changed files":
        step_lines = [line]
        for nested in lines[index + 1:]:
            if nested.startswith("      - "):
                break
            step_lines.append(nested)
        validation_step = "\n".join(step_lines)
        break
if validation_step is None:
    print("publish workflow is missing validation step")
    sys.exit(1)
if "RELEASE_PUBLISH_TOKEN" in validation_step:
    print("publish workflow validation step must not expose RELEASE_PUBLISH_TOKEN")
    sys.exit(1)
publish_step_index = workflow.find("Publish Docker image to Forgejo package registry")
release_step_index = workflow.find("tea release create")
if publish_step_index == -1:
    print("publish workflow is missing publish step")
    sys.exit(1)
if publish_step_index < validation_index or token_index < publish_step_index:
    print("publish workflow must expose RELEASE_PUBLISH_TOKEN only in token-bearing publish/release steps after validation completes")
    sys.exit(1)
allowed_token_lines = set()
for index, line in enumerate(lines):
    if line.strip() in {
        "- name: Publish Docker image to Forgejo package registry",
        "- name: Create Forgejo Release",
    }:
        step_lines = [line]
        for nested in lines[index + 1:]:
            if nested.startswith("      - "):
                break
            step_lines.append(nested)
        allowed_token_lines.update(range(index, index + len(step_lines)))
if not allowed_token_lines:
    print("publish workflow is missing publish step")
    sys.exit(1)
for index, line in enumerate(lines):
    if "RELEASE_PUBLISH_TOKEN" in line and index not in allowed_token_lines:
        print("publish workflow must confine every RELEASE_PUBLISH_TOKEN reference to token-bearing image publish or Forgejo Release steps")
        sys.exit(1)
for forbidden in ["git clone", "tea pr checkout", "gh pr checkout"]:
    if forbidden in workflow:
        print(f"publish workflow should not require local manual checkout recovery commands, found: {forbidden}")
        sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow supports protected manual dispatch from a release merge SHA"
  else
    fail "publish workflow supports protected manual dispatch from a release merge SHA ($output)"
  fi
}

test_publish_workflow_derives_and_promotes_release_candidate_sha() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_not_contains "$publish_workflow" 'nix build .#ar-gateway-image' "publish workflow does not rebuild the ar-gateway image during final publication"
  assert_file_not_contains "$publish_workflow" 'docker-archive:./result' "publish workflow does not publish final release tags from a local docker archive"
  assert_file_contains "$publish_workflow" 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow uses the publish token for promotion"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    errors.append('publish workflow is missing the token-bearing image promotion step')
    before_publish = workflow
else:
    before_publish = workflow.split(publish_marker, 1)[0]

if 'github.event.pull_request.head.sha' not in before_publish:
    errors.append('publish workflow must derive RELEASE_CANDIDATE_SHA from the merged release PR head SHA before token exposure')
manual_fallback_patterns = [
    r'git\s+rev-parse\s+"?\$RELEASE_MERGE_SHA\^2"?',
    r'git\s+rev-parse\s+"?\$\{RELEASE_MERGE_SHA\}\^2"?',
]
if not any(re.search(pattern, before_publish) for pattern in manual_fallback_patterns):
    errors.append('publish workflow must fall back to git rev-parse "$RELEASE_MERGE_SHA^2" for manual dispatch')
if 'RELEASE_CANDIDATE_SHA' not in before_publish:
    errors.append('publish workflow must derive RELEASE_CANDIDATE_SHA before token-bearing promotion')

promotion_step_match = re.search(r'- name: Publish Docker image to Forgejo package registry(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not promotion_step_match:
    errors.append('publish workflow must have a dedicated image promotion step')
else:
    step = promotion_step_match.group('body')
    if 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' not in step:
        errors.append('image promotion step must receive RELEASE_PUBLISH_TOKEN')
    if 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_CANDIDATE_SHA' not in step and 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_CANDIDATE_SHA}' not in step:
        errors.append('image promotion step must copy from the release candidate SHA image ref')
    for destination in [
        ('docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION', 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}'),
        'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:latest',
    ]:
        if isinstance(destination, tuple):
            if not any(candidate in step for candidate in destination):
                errors.append('image promotion step must copy the candidate to RELEASE_VERSION')
        elif destination not in step:
            errors.append(f'image promotion step must copy the candidate to {destination}')
    if 'docker-archive:./result' in step or 'nix build .#ar-gateway-image' in step:
        errors.append('image promotion step must not rebuild or publish from docker-archive:./result')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow derives the release candidate SHA and promotes that image to release tags"
  else
    fail "publish workflow derives the release candidate SHA and promotes that image to release tags ($output)"
  fi
}

test_publish_workflow_attaches_binary_archives_checksums_signatures_and_provenance() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

release_step_match = re.search(r'- name: Create Forgejo Release(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not release_step_match:
    errors.append('publish workflow must have a dedicated Create Forgejo Release step')
    release_step = ''
    asset_upload_section = workflow
else:
    release_step = release_step_match.group('body')
    asset_upload_section = workflow[release_step_match.start():]

required_release_assets = {
    'Linux x86_64 auto-review binary archive': [
        'auto-review-$RELEASE_VERSION-linux-x86_64.tar.gz',
        'auto-review-${RELEASE_VERSION}-linux-x86_64.tar.gz',
        'x86_64-unknown-linux',
    ],
    'Linux aarch64 auto-review binary archive': [
        'auto-review-$RELEASE_VERSION-linux-aarch64.tar.gz',
        'auto-review-${RELEASE_VERSION}-linux-aarch64.tar.gz',
        'aarch64-unknown-linux',
    ],
    'SHA-256 checksum manifest': ['SHA256SUMS', 'sha256sum'],
    'signature files': ['.sig', 'sign-blob', 'minisign', 'cosign sign-blob'],
    'SBOM metadata': ['sbom', 'SBOM', 'cyclonedx', 'spdx', 'syft'],
    'provenance metadata': ['provenance', 'attestation', 'slsa'],
}
for description, candidates in required_release_assets.items():
    if not any(candidate in workflow for candidate in candidates):
        errors.append(f'missing {description}')

asset_attachment_markers = ['--asset', '--attachment', 'tea release assets create', 'tea release create']
if not any(marker in asset_upload_section for marker in asset_attachment_markers):
    errors.append('Forgejo release creation or following asset-upload step must attach binary archives, checksums, signatures, SBOM, and provenance metadata')

for required in [
    'auto-review',
    'linux-x86_64',
    'linux-aarch64',
    'SHA256SUMS',
    '.sig',
    'sbom',
    'provenance',
]:
    if required not in asset_upload_section:
        errors.append(f'Forgejo release asset upload flow is missing marker after release creation: {required}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow attaches Linux binary archives, checksums, signatures, SBOM, and provenance metadata"
  else
    fail "publish workflow attaches Linux binary archives, checksums, signatures, SBOM, and provenance metadata ($output)"
  fi
}

test_publish_workflow_verifies_generated_binary_artifacts_before_release_upload() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

release_marker = 'Create Forgejo Release'
before_release = workflow.split(release_marker, 1)[0] if release_marker in workflow else workflow
required_before_release = [
    'sha256sum -c SHA256SUMS',
    'linux-x86_64',
    'linux-aarch64',
    '.sig',
    'sbom',
    'provenance',
]
for marker in required_before_release:
    if marker not in before_release:
        errors.append(f'generated release artifacts are not verified before Forgejo upload: {marker}')

signature_verify_patterns = [
    r'cosign\s+verify-blob[\s\S]{0,300}\.sig',
    r'minisign\s+-V[\s\S]{0,300}\.sig',
    r'gpg\s+--verify[\s\S]{0,300}\.sig',
    r'ssh-keygen\s+-Y\s+verify[\s\S]{0,300}\.sig',
]
if not any(re.search(pattern, before_release) for pattern in signature_verify_patterns):
    errors.append('generated binary signatures must be verified before Forgejo upload')

artifact_build_index = min(
    (index for index in [before_release.find('linux-x86_64'), before_release.find('linux-aarch64')] if index != -1),
    default=-1,
)
checksum_verify_index = before_release.find('sha256sum -c SHA256SUMS')
if artifact_build_index == -1 or checksum_verify_index == -1 or checksum_verify_index < artifact_build_index:
    errors.append('checksum verification must run after binary artifact generation and before release creation')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow verifies generated binary artifacts before Forgejo upload"
  else
    fail "publish workflow verifies generated binary artifacts before Forgejo upload ($output)"
  fi
}

test_publish_workflow_handles_release_signing_key_in_private_tempdir() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

step_match = re.search(r'- name: Build and verify Linux binary release artifacts(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not step_match:
    print('publish workflow is missing the binary artifact build/signing step')
    sys.exit(1)

step = step_match.group('body')
if 'umask 077' not in step:
    errors.append('binary signing step must set umask 077 before writing signing material')

tempdir_match = re.search(r'(?P<var>[A-Za-z_][A-Za-z0-9_]*)="?\$\(mktemp -d\)"?', step)
tempdir_var = tempdir_match.group('var') if tempdir_match else None
if tempdir_var is None:
    errors.append('binary signing step must create a private temporary directory with mktemp -d')
else:
    trap_patterns = [
        rf"trap\s+'rm -rf \"\${tempdir_var}\"'\s+(?P<signals>[A-Z ]+)",
        rf'trap\s+"rm -rf \\\"\${tempdir_var}\\\""\s+(?P<signals>[A-Z ]+)',
        rf"trap\s+'rm -rf \"\${{{tempdir_var}}}\"'\s+(?P<signals>[A-Z ]+)",
        rf'trap\s+"rm -rf \\\"\${{{tempdir_var}}}\\\""\s+(?P<signals>[A-Z ]+)',
    ]
    trap_matches = [match for pattern in trap_patterns for match in re.finditer(pattern, step)]
    sign_indices = [index for marker in ['ssh-keygen -Y sign', 'ssh-keygen -y -f "$signing_key"'] if (index := step.find(marker)) != -1]
    first_sign_index = min(sign_indices, default=-1)
    if not trap_matches:
        errors.append('binary signing step must install a trap that runs rm -rf "$signing_dir" on EXIT TERM INT')
    elif first_sign_index == -1:
        errors.append('binary signing step must sign with the private signing key')
    else:
        pre_sign_trap_signals = {
            signal
            for match in trap_matches
            if match.start() < first_sign_index
            for signal in match.group('signals').split()
        }
        missing_signals = sorted({'EXIT', 'TERM', 'INT'} - pre_sign_trap_signals)
        if missing_signals:
            errors.append('binary signing step must install signing directory cleanup traps before signing commands for: ' + ', '.join(missing_signals))
    key_path_patterns = [
        rf'signing_key="\${tempdir_var}/[^"\n]+"',
        rf'signing_key="\${{{tempdir_var}}}/[^"\n]+"',
    ]
    if not any(re.search(pattern, step) for pattern in key_path_patterns):
        errors.append('binary signing step must store the private key inside the private temporary directory')

if 'chmod 600 "$signing_key"' not in step:
    errors.append('binary signing step must chmod 600 the private signing key file')
if 'printf \'%s\\n\' "$RELEASE_SIGNING_KEY" > "$signing_key"' not in step and 'printf "%s\\n" "$RELEASE_SIGNING_KEY" > "$signing_key"' not in step:
    errors.append('binary signing step must write RELEASE_SIGNING_KEY only to the private signing key file')

artifact_leak_patterns = [
    r'RELEASE_SIGNING_KEY[^\n]*release-artifacts',
    r'release-artifacts[^\n]*RELEASE_SIGNING_KEY',
    r'private[-_]key',
    r'id_(?:ed25519|rsa)',
]
for pattern in artifact_leak_patterns:
    if re.search(pattern, step, re.I):
        errors.append('binary signing step must not place RELEASE_SIGNING_KEY/private key material under release-artifacts')
        break

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow handles release signing key in a private temporary directory"
  else
    fail "publish workflow handles release signing key in a private temporary directory ($output)"
  fi
}

test_publish_workflow_allows_intentional_release_tooling_changes_before_token_publish() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import fnmatch
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    print('missing token-bearing image publication step')
    sys.exit(1)

validation_section = workflow.split(publish_marker, 1)[0]
case_match = re.search(r'case "\$changed_file" in(?P<body>.*?)\n\s*esac', validation_section, re.S)
if not case_match:
    print('missing changed-file case allowlist before publish')
    sys.exit(1)

patterns = []
reject_patterns = []
case_lines = case_match.group('body').splitlines()
index = 0
while index < len(case_lines):
    stripped = case_lines[index].strip()
    index += 1
    if not stripped or stripped.startswith('#') or ')' not in stripped:
        continue
    head = stripped.split(')', 1)[0]
    arm_body = []
    while index < len(case_lines):
        body_line = case_lines[index].strip()
        arm_body.append(body_line)
        index += 1
        if body_line == ';;':
            break
    meaningful_body = [line for line in arm_body if line and not line.startswith('#')]
    is_pass_through = meaningful_body == [';;']
    is_reject = any('exit' in line or 'refusing token-bearing publish' in line for line in meaningful_body)
    parts = [part for part in head.split('|') if part]
    if is_pass_through:
        patterns.extend(parts)
    if is_reject:
        reject_patterns.extend(parts)

required_allowed = [
    '.forgejo/workflows/release-prepare.yml',
    '.forgejo/workflows/release-publish.yml',
    'scripts/release',
    'tests/release_tooling_test.sh',
]
missing_allowed = [sample for sample in required_allowed if not any(fnmatch.fnmatchcase(sample, pattern) for pattern in patterns)]
if missing_allowed:
    print('publish allowlist does not permit intentional release workflow/script/test changes: ' + ', '.join(missing_allowed))
    print('observed allowlist patterns: ' + ', '.join(patterns))
    sys.exit(1)

unexpected_samples = [
    '.forgejo/workflows/ci.yml',
    'scripts/unrelated-token-helper',
    '.forgejo/workflows/untrusted.yml',
]
missing_rejections = [sample for sample in unexpected_samples if not any(fnmatch.fnmatchcase(sample, pattern) for pattern in reject_patterns)]
if missing_rejections:
    print('publish allowlist does not explicitly refuse unexpected token-bearing workflow/script changes: ' + ', '.join(missing_rejections))
    print('observed reject patterns: ' + ', '.join(reject_patterns))
    sys.exit(1)

if 'refusing token-bearing publish for release PR file:' not in validation_section:
    print('publish workflow must fail closed with a clear refusal for unexpected token-bearing changes')
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow allows intentional release tooling changes while rejecting unexpected token-bearing files"
  else
    fail "publish workflow allows intentional release tooling changes while rejecting unexpected token-bearing files ($output)"
  fi
}

test_release_docs_account_for_final_binary_assets_and_publish_token_scope() {
  local output status

  output="$(python3 - "$ROOT/docs/THREAT-MODEL.md" "$ROOT/docs/OPERATIONS.md" <<'PY'
import pathlib
import re
import sys

threat = pathlib.Path(sys.argv[1]).read_text()
operations = pathlib.Path(sys.argv[2]).read_text()
combined = threat + '\n' + operations
errors = []

concept_patterns = {
    'Linux x86_64 binary archive': r'(?is)(?:Linux[^\n]{0,80}x86_64|x86_64[^\n]{0,80}Linux)[^\n]{0,120}(?:archive|tarball|download|asset)',
    'Linux aarch64 binary archive': r'(?is)(?:Linux[^\n]{0,80}aarch64|aarch64[^\n]{0,80}Linux)[^\n]{0,120}(?:archive|tarball|download|asset)',
    'auto-review binary release asset': r'(?is)auto-review[^\n]{0,120}(?:binary|archive|tarball|download|asset)',
    'SHA-256 checksum concept': r'(?is)(?:SHA-256|sha256|SHA256SUMS)',
    'signature concept': r'(?is)(?:signature|\.sig|sign-blob|minisign|gpg --verify)',
    'SBOM concept': r'(?is)(?:SBOM|software bill of materials|cyclonedx|spdx|syft)',
    'provenance concept': r'(?is)(?:provenance|attestation|slsa)',
    'release publish token scope': r'(?is)(?:RELEASE_PUBLISH_TOKEN|Release publishing PAT)[^\n]{0,240}(?:binary|archive|asset|checksum|signature|SBOM|provenance)',
}
for description, pattern in concept_patterns.items():
    if not re.search(pattern, combined):
        errors.append(f'docs missing binary release concept: {description}')

for forbidden in ['future binary', 'future Linux binary', 'after issue #121 lands', 'Issue #121 must publish']:
    if forbidden in combined:
        errors.append(f'docs still describe binary assets as future work: {forbidden}')

scope_patterns = [
    r'Release publishing PAT[^\n]*(?:Linux binary archives|binary release assets)',
    r'RELEASE_PUBLISH_TOKEN[^\n]*(?:Linux binary archives|binary release assets)',
]
if not any(re.search(pattern, combined, re.I) for pattern in scope_patterns):
    errors.append('docs do not tie release publishing token scope to binary archives and metadata')

verification_patterns = [
    r'sha256sum\s+-c\s+SHA256SUMS',
    r'(?:cosign\s+verify-blob|minisign\s+-V|gpg\s+--verify|ssh-keygen\s+-Y\s+verify)',
]
for pattern in verification_patterns:
    if not re.search(pattern, combined):
        errors.append(f'docs missing operator verification command matching: {pattern}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "docs and threat model account for binary assets and publish token scope"
  else
    fail "docs and threat model account for binary assets and publish token scope ($output)"
  fi
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

test_release_token_blast_radius_is_documented() {
  local t5a
  t5a="$(python3 - "$ROOT/docs/THREAT-MODEL.md" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'(?ms)^### T5a\. Release preparation and publishing PAT compromise\n(?P<section>.*?)(?=^### |\Z)', text)
if not match:
    raise SystemExit("T5a release PAT threat section not found")
print(match.group("section"))
PY
)"

  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release preparation PAT' "threat model names the operator-created release preparation PAT asset"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release publishing PAT' "threat model names the release publishing PAT asset"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release preparation PAT blast radius' "threat model documents the release preparation PAT blast radius"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release publishing PAT blast radius' "threat model documents the release publishing PAT blast radius"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'prepare release PR branches and release PRs only in `jwilger/auto_review`' "threat model documents the release preparation PAT scope"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'publish container images to `git.johnwilger.com/jwilger/auto_review/ar-gateway` and create Forgejo Releases only in `jwilger/auto_review`' "threat model documents the release publishing PAT package registry and release API scope"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'Release candidate publishing PAT' "threat model does not document a separate candidate publishing PAT"
  assert_contains "$t5a" 'builds the release candidate Docker image with `nix build .#ar-gateway-image` after the release metadata commit' "T5a mitigation documents Nix-built candidate image provenance"
  assert_contains "$t5a" 'publishes candidate image tags for the release PR head SHA and release-candidate tag' "T5a mitigation documents release candidate image tags"
  assert_contains "$t5a" 'promotes the candidate image to the release version and `latest` tags' "T5a mitigation documents final release image promotion instead of rebuilding"
  assert_contains "$t5a" 'publishes only `git.johnwilger.com/jwilger/auto_review/ar-gateway` to the Forgejo package registry and creates the matching Forgejo Release entry' "T5a mitigation documents package registry and Forgejo Release publication instead of cargo publishing"
  assert_not_contains "$t5a" 'Forgejo release selection to `release-plz`' "T5a mitigation does not describe stale release-plz cargo publication"
  assert_contains "$t5a" '`Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`' "T5a mitigation publish allowlist includes Cargo.toml, Cargo.lock, and CHANGELOG.md"
  assert_contains "$t5a" 'root release metadata' "T5a mitigation documents root release metadata is permitted"
  assert_not_contains "$t5a" 'root and package release metadata' "T5a mitigation does not permit package-crate release metadata for Docker image publication"
  assert_not_contains "$t5a" 'Prepare validates dispatch inputs' "T5a mitigation does not describe stale manual dispatch input validation"
  assert_not_contains "$t5a" 'validates the derived semantic version' "T5a mitigation does not describe stale derived semantic-version validation"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release preparation PAT blast radius' "operations docs summarize the release preparation PAT blast radius"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release publishing PAT blast radius' "operations docs summarize the release publishing PAT blast radius"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'prepare release PR branches and release PRs only in `jwilger/auto_review`' "operations docs constrain the release preparation PAT scope"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'publish container images to `git.johnwilger.com/jwilger/auto_review/ar-gateway` and create Forgejo Releases only in `jwilger/auto_review`' "operations docs constrain the release publishing PAT package registry and release API scope"
}

test_release_secrets_are_documented_for_operators() {
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_PREPARE_TOKEN`' "operations docs require an operator-created release preparation Actions secret"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Forgejo Actions secret `RELEASE_PREPARE_TOKEN`' "threat model documents the operator-created release preparation Actions secret"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_CANDIDATE_TOKEN`' "operations docs do not require a separate candidate image publishing Actions secret"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'Forgejo Actions secret `RELEASE_CANDIDATE_TOKEN`' "threat model does not document a separate candidate image publishing Actions secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release candidate image' "operations docs document release candidate image provenance"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release candidate image tag' "operations docs document the release candidate image tag"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release publishing credential' "operations docs identify the release publishing credential purpose"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_PUBLISH_TOKEN`' "operations docs document release publishing credential as an Actions secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release bot Forgejo user' "operations docs require a dedicated release bot user"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_SIGNING_KEY`' "operations docs document the release signing key secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'repository variables `RELEASE_BOT_NAME` and `RELEASE_BOT_EMAIL`' "operations docs document release bot identity variables"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'owned by the same release bot named in `RELEASE_BOT_NAME`' "operations docs reuse release bot identity for registry publishing"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'RELEASE_REGISTRY_USER' "operations docs do not require a separate registry user variable"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'release bot identity in repository variable `RELEASE_BOT_NAME`' "threat model reuses release bot identity for registry publishing"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'RELEASE_REGISTRY_USER' "threat model does not require a separate registry user variable"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release signing key' "threat model names the release signing key asset"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'manual approval gate' "operations docs do not require a manual approval gate for release publishing credentials"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Configure the release publishing credential as Forgejo Actions secret `RELEASE_PUBLISH_TOKEN`' "operations docs describe the publish token as an ordinary Actions secret"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'FORGEJO_RELEASE_PREPARE_TOKEN' "operations docs do not expose the old operator-facing prepare secret name"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "operations docs do not expose the old operator-facing publish secret name"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'FORGEJO_RELEASE_PREPARE_TOKEN' "threat model does not expose the old operator-facing prepare secret name"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "threat model does not expose the old operator-facing publish secret name"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `FORGEJO_TOKEN`' "operations docs do not document the legacy shared release Actions secret"
  assert_file_lacks_line "$ROOT/deploy/systemd/auto_review.env.example" 'FORGEJO_TOKEN=' "systemd env example does not declare the release publishing Actions secret"
  assert_file_not_contains "$ROOT/deploy/systemd/auto_review.env.example" 'Release publishing credential' "systemd env example does not describe the Actions-only release publishing credential"
}

test_release_workflows_exist_for_prepare_pr_and_publish_on_merge
test_release_workflows_install_or_reuse_nix_like_ci_before_nix_develop
test_prepare_workflow_builds_and_publishes_release_candidate_images
test_prepare_workflow_creates_prerelease_entry_for_release_candidate
test_prepare_workflow_skips_release_pr_merge_pushes
test_prepare_workflow_runs_release_infra_fix_pushes
test_prepare_workflow_plans_and_checks_semver_before_release_metadata_commit
test_prepare_workflow_closes_superseded_release_prs_before_creating_current_pr
test_prepare_workflow_selects_maximum_of_semver_minimum_and_conventional_bump
test_prepare_workflow_runs_tea_and_jq_inside_nix_develop
test_publish_workflow_requires_release_pr_base_branch_main
test_release_workflows_use_prepare_secret_and_protected_publish_token
test_publish_workflow_validates_provenance_and_changed_files_before_publish_token
test_publish_workflow_uses_release_pr_merge_sha_not_a_recomputed_version
test_publish_workflow_executes_from_merge_commit_sha_before_publish_token
test_publish_workflow_attaches_merge_commit_to_main_with_upstream_before_image_publish
test_prepare_workflow_checkout_does_not_persist_credentials
test_prepare_workflow_authenticates_git_push_without_checkout_credentials
test_prepare_workflow_manages_release_branch_with_prepare_token
test_prepare_workflow_updates_existing_release_pr_body_with_candidate_images
test_prepare_workflow_stages_only_root_release_metadata
test_release_tooling_uses_local_prepare_and_image_registry_for_publish
test_publish_workflow_promotes_release_image_tags_and_generates_release_notes
test_publish_workflow_publishes_nix_docker_image_to_forgejo_registry
test_publish_workflow_requires_trusted_release_environment
test_publish_workflow_supports_manual_dispatch_from_release_merge_sha
test_publish_workflow_derives_and_promotes_release_candidate_sha
test_publish_workflow_attaches_binary_archives_checksums_signatures_and_provenance
test_publish_workflow_verifies_generated_binary_artifacts_before_release_upload
test_publish_workflow_handles_release_signing_key_in_private_tempdir
test_publish_workflow_allows_intentional_release_tooling_changes_before_token_publish
test_release_docs_account_for_final_binary_assets_and_publish_token_scope
test_release_tooling_tests_are_wired_into_nix_flake_check
test_release_plz_config_is_removed_and_workspace_crates_stay_private
test_release_secrets_are_documented_for_operators
test_release_token_blast_radius_is_documented

if [[ $failures -eq 0 ]]; then
  printf 'release tooling dry-run tests passed\n'
  exit 0
fi

printf 'release tooling dry-run tests failed: %s\n' "$failures"
exit 1
