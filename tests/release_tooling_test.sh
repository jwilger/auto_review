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
  assert_contains "$(<"$workdir/CHANGELOG.md")" '## [Unreleased]' "prepare dry-run leaves CHANGELOG.md unchanged"
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
  assert_contains "$(<"$workdir/CHANGELOG.md")" '## [Unreleased]' "prepare non-dry-run keeps an Unreleased section for future changes"
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

## [Unreleased]
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
  assert_contains "$changelog" '## [Unreleased]' "prepare keeps an Unreleased section before generated release notes"
  assert_file_contains_before "$workdir/CHANGELOG.md" '## [Unreleased]' '## [0.1.0] - 2026-05-04' "prepare writes generated release notes below Unreleased"
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

## [Unreleased]

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
  assert_file_contains "$prepare_workflow" 'scripts/release prepare' "release PR preparation workflow calls the prepare command"
  assert_file_contains "$prepare_workflow" 'fetch-depth: 0' "release PR preparation workflow checks out full history for changelog generation"
  assert_file_contains "$prepare_workflow" 'git fetch --tags' "release PR preparation workflow fetches tags for changelog generation"
  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not map tea token from unsupported forgejo.token expression"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not map git push token from unsupported forgejo.token expression"
  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow exposes the explicit prepare-scoped Actions secret to release tooling"
  assert_file_contains "$prepare_workflow" 'repo_token="${RELEASE_PREPARE_TOKEN:-}"' "release PR preparation workflow derives repo token only from the prepare-scoped secret"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$repo_token"' "release PR preparation workflow assigns tea token from derived repo token"
  assert_file_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN="$repo_token"' "release PR preparation workflow assigns git push token from derived repo token"
  assert_file_has_line_containing_all "$prepare_workflow" "release PR preparation workflow derives tea server URL from shell FORGEJO_SERVER_URL with production fallback" 'GITEA_SERVER_URL' 'FORGEJO_SERVER_URL' 'https://git.johnwilger.com'
  assert_file_contains "$prepare_workflow" 'git fetch origin' "release PR preparation workflow checks remote branch state"
  assert_file_contains "$prepare_workflow" 'git switch' "release PR preparation workflow switches to a release branch"
  assert_file_contains "$prepare_workflow" 'origin/main' "release PR preparation workflow reruns start from the current main branch"
  assert_file_not_contains "$prepare_workflow" 'git switch -C "$branch" "origin/$branch"' "release PR preparation workflow does not rerun from the stale remote release branch"
  assert_file_contains "$prepare_workflow" 'git push --force-with-lease origin "$branch"' "release PR preparation workflow updates the remote release branch safely"
  assert_file_contains "$prepare_workflow" 'tea pr' "release PR preparation workflow manages release PRs with tea"
  assert_file_contains "$prepare_workflow" 'tea pr list --repo jwilger/auto_review' "release PR preparation workflow looks up an existing PR before editing"
  assert_file_contains "$prepare_workflow" 'tea pr create --repo jwilger/auto_review' "release PR preparation workflow opens a scoped Forgejo PR"
  assert_file_contains "$prepare_workflow" 'tea pr edit --repo jwilger/auto_review "$pr_index"' "release PR preparation workflow edits an existing scoped Forgejo PR by index"
  assert_file_not_contains "$prepare_workflow" 'tea pr edit --repo jwilger/auto_review "$branch"' "release PR preparation workflow does not pass a branch name to tea pr edit"
  assert_file_contains "$prepare_workflow" 'nix develop' "release PR preparation workflow enters the Nix development environment before project tooling"
  assert_file_not_contains "$prepare_workflow" 'gh ' "release PR preparation workflow does not invoke GitHub tooling"

  assert_file_exists "$publish_workflow" "publish-on-merge workflow exists"
  assert_file_contains "$publish_workflow" 'pull_request' "publish workflow listens for pull request events"
  assert_file_contains "$publish_workflow" 'closed' "publish workflow runs when release PRs close"
  assert_file_contains "$publish_workflow" 'nix develop' "publish workflow enters the Nix development environment before project tooling"
  assert_file_contains "$publish_workflow" 'scripts/release publish' "publish workflow calls the publish command"
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

test_prepare_workflow_validates_dispatch_inputs_before_token_bearing_steps() {
  local prepare_workflow output status
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'RELEASE_VERSION: ${{ inputs.version }}' "release PR preparation workflow moves dispatch version through env"
  assert_file_contains "$prepare_workflow" 'RELEASE_DATE: ${{ inputs.date }}' "release PR preparation workflow moves dispatch date through env"
  output="$(python3 - "$prepare_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
token_marker = 'GITEA_SERVER_TOKEN'
if token_marker not in workflow:
    print('missing token derivation in token-bearing prepare step')
    sys.exit(1)

validation_section, token_section = workflow.split(token_marker, 1)

def require_ordered(section, markers, label):
    cursor = 0
    for marker in markers:
        found = section.find(marker, cursor)
        if found == -1:
            print(f'{label} missing ordered marker: {marker}')
            sys.exit(1)
        cursor = found + len(marker)

require_ordered(
    validation_section,
    [
        'version="${RELEASE_VERSION:-',
        'date="${RELEASE_DATE:-',
        '[[ "$version" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+$ ]]',
        '[[ "$date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]',
    ],
    'no-token validation step',
)
require_ordered(
    token_section,
    [
        'version="${RELEASE_VERSION:-',
        'date="${RELEASE_DATE:-',
        'branch="release/v${version}"',
        'scripts/release prepare --workspace . --version "$version" --date "$date"',
    ],
    'token-bearing prepare step',
)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release PR preparation workflow derives and uses defaulted version/date in both validation and prepare steps"
  else
    fail "release PR preparation workflow derives and uses defaulted version/date in both validation and prepare steps ($output)"
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
  assert_file_contains "$prepare_workflow" 'repo_token="${RELEASE_PREPARE_TOKEN:-}"' "release PR preparation workflow derives repo token from the prepare-scoped secret only"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$repo_token"' "release PR preparation workflow gives tea the prepare-scoped repo token"
  assert_file_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN="$repo_token"' "release PR preparation workflow gives git push the prepare-scoped repo token"
  assert_file_has_line_containing_all "$prepare_workflow" "release PR preparation workflow gives tea the Forgejo server URL with fallback" 'GITEA_SERVER_URL' 'FORGEJO_SERVER_URL' 'https://git.johnwilger.com'
  assert_file_not_contains "$prepare_workflow" 'TEA_TOKEN:' "release PR preparation workflow does not use tea's legacy token env var"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_TOKEN' "release PR preparation workflow does not use the legacy shared Actions secret"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_RELEASE_PREPARE_TOKEN' "release PR preparation workflow does not reference the old disallowed prepare secret name"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_RELEASE_PUBLISH_TOKEN' "release PR preparation workflow does not reference the old disallowed publish secret name"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_RELEASE_PREPARE_TOKEN' "release PR preparation workflow does not reference the old disallowed prepare token name anywhere"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "release PR preparation workflow does not expose the publish-scoped Actions secret"

  assert_file_contains "$publish_workflow" 'FORGEJO_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow uses the publish-scoped Actions secret"
  assert_file_contains "$publish_workflow" 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow gives tea the publish-scoped environment secret"
  assert_file_contains "$publish_workflow" 'GITEA_SERVER_URL: https://git.johnwilger.com' "publish workflow gives tea the Forgejo server URL"
  assert_file_not_contains "$publish_workflow" 'TEA_TOKEN:' "publish workflow does not use tea's legacy token env var"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_TOKEN' "publish workflow does not use the legacy shared Actions secret"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_RELEASE_PREPARE_TOKEN' "publish workflow does not reference the old disallowed prepare secret name"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_RELEASE_PUBLISH_TOKEN' "publish workflow does not reference the old disallowed publish secret name"
  assert_file_not_contains "$publish_workflow" 'FORGEJO_RELEASE_PREPARE_TOKEN' "publish workflow does not expose the prepare-scoped Actions secret"
}

test_prepare_workflow_requires_explicit_prepare_secret_runtime_env() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'repo_token="${RELEASE_PREPARE_TOKEN:-}"' "release PR preparation workflow derives one repo token from the explicit prepare secret"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$repo_token"' "release PR preparation workflow assigns tea token from prepare secret"
  assert_file_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN="$repo_token"' "release PR preparation workflow assigns git push token from prepare secret"
  assert_file_contains "$prepare_workflow" 'missing RELEASE_PREPARE_TOKEN' "release PR preparation workflow clearly explains a missing prepare secret"
  assert_file_not_contains "$prepare_workflow" 'GITHUB_TOKEN:-' "release PR preparation workflow does not fall back to GitHub-compatible auto token aliases"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_URL="${FORGEJO_SERVER_URL:-${GITHUB_SERVER_URL:-https://git.johnwilger.com}}"' "release PR preparation workflow derives tea server URL from Forgejo or GitHub-compatible env"
}

test_publish_workflow_validates_provenance_and_changed_files_before_publish_token() {
  local publish_workflow
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" 'Validate release provenance and changed files' "publish workflow has a no-token provenance validation step"
  assert_file_contains "$publish_workflow" 'RELEASE_BASE_SHA: ${{ github.event.pull_request.base.sha }}' "publish workflow records the release PR base SHA for provenance checks"
  assert_file_contains "$publish_workflow" 'RELEASE_MERGE_SHA: ${{ github.event.pull_request.merge_commit_sha }}' "publish workflow records the release PR merge SHA for provenance checks"
  assert_file_contains "$publish_workflow" 'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"' "publish workflow derives changed files from the merged release PR"
  assert_file_contains "$publish_workflow" 'case "$changed_file" in' "publish workflow evaluates each changed file before publishing"
  assert_file_contains "$publish_workflow" 'Cargo.toml|CHANGELOG.md)' "publish workflow allows only release metadata files before publishing"
  assert_file_contains "$publish_workflow" '.forgejo/workflows/*|scripts/*)' "publish workflow explicitly rejects script and workflow changes before publishing"
  assert_file_contains "$publish_workflow" 'refusing token-bearing publish for release PR file:' "publish workflow fails closed for unexpected release PR files"
  assert_file_contains_before "$publish_workflow" 'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"' 'FORGEJO_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow validates changed files before exposing publish token to release tooling"
  assert_file_contains_before "$publish_workflow" 'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"' 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow validates changed files before exposing publish token to tea"
  assert_file_contains_before "$publish_workflow" '.forgejo/workflows/*|scripts/*)' 'FORGEJO_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow rejects script and workflow changes before exposing publish token to release tooling"
  assert_file_contains_before "$publish_workflow" '.forgejo/workflows/*|scripts/*)' 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow rejects script and workflow changes before exposing publish token to tea"
}

test_publish_workflow_semver_validates_version_before_publish_token() {
  local publish_workflow
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" 'RELEASE_VERSION="${FORGEJO_PULL_REQUEST_HEAD_BRANCH#release/v}"' "publish workflow derives the release version before token-bearing publish"
  assert_file_contains "$publish_workflow" '[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]' "publish workflow semver-validates the publish version"
  assert_file_contains_before "$publish_workflow" '[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]' 'FORGEJO_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow validates publish version before exposing publish token to release tooling"
  assert_file_contains_before "$publish_workflow" '[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]' 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow validates publish version before exposing publish token to tea"
}

test_publish_workflow_executes_from_merge_commit_sha_before_publish_token() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" 'ref: ${{ github.event.pull_request.merge_commit_sha }}' "publish workflow checks out the merged release PR commit"
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
        for nested in workflow[with_index + 1:]:
            stripped = nested.strip()
            if not stripped:
                continue
            nested_indent = len(nested) - len(nested.lstrip())
            if nested_indent <= with_indent:
                break
            if stripped == "persist-credentials: false":
                sys.exit(0)
print("actions/checkout@v4 with mapping is missing persist-credentials: false")
sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow checkout with block does not persist checkout credentials"
  else
    fail "publish workflow checkout with block does not persist checkout credentials ($output)"
  fi
  assert_file_contains "$publish_workflow" '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]' "publish workflow asserts HEAD is the merged release PR commit"
  assert_file_contains_before "$publish_workflow" '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]' 'FORGEJO_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow verifies checked-out merge commit before exposing publish token to release tooling"
  assert_file_contains_before "$publish_workflow" '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]' 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow verifies checked-out merge commit before exposing publish token to tea"
}

test_changelog_mentions_issue_66_release_automation() {
  local output status

  output="$(python3 - "$ROOT/CHANGELOG.md" <<'PY'
import pathlib
import re
import sys

entries = []
current = []
for line in pathlib.Path(sys.argv[1]).read_text().splitlines():
    if line.startswith("- "):
        if current:
            entries.append("\n".join(current))
        current = [line]
    elif current and (line.startswith("  ") or not line.strip()):
        current.append(line)
    elif current:
        entries.append("\n".join(current))
        current = []
if current:
    entries.append("\n".join(current))

for entry in entries:
    if "release automation" in entry.lower() and "Closes #66" in entry:
        sys.exit(0)

print("missing one changelog bullet containing release automation and Closes #66")
sys.exit(1)
PY
)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "CHANGELOG has one release automation entry closing issue 66"
  else
    fail "CHANGELOG has one release automation entry closing issue 66 ($output)"
  fi
}

test_prepare_workflow_configures_git_identity_before_commit() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains "$prepare_workflow" 'git config user.name "auto_review release automation"' "release PR preparation workflow configures git author name"
  assert_file_contains "$prepare_workflow" 'git config user.email "auto_review@git.johnwilger.com"' "release PR preparation workflow configures git author email"
  assert_file_contains_before "$prepare_workflow" 'git config user.name "auto_review release automation"' 'git commit' "release PR preparation workflow configures git author name before commit"
  assert_file_contains_before "$prepare_workflow" 'git config user.email "auto_review@git.johnwilger.com"' 'git commit' "release PR preparation workflow configures git author email before commit"
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

test_prepare_workflow_pushes_release_branch_with_prepare_secret_helper() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not map git push token from unsupported forgejo.token expression"
  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow branch push receives the operator-created prepare secret"
  assert_file_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN="$repo_token"' "release PR preparation workflow exposes the prepare-scoped repo token for git push"
  assert_file_has_line_containing_all "$prepare_workflow" "release PR preparation workflow pushes the branch with the prepare-scoped token credential helper" 'git -c credential.helper=' 'FORGEJO_ACTIONS_TOKEN' 'push --force-with-lease origin "$branch"'
}

test_prepare_workflow_stages_cargo_lock_after_prepare() {
  local prepare_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"

  assert_file_contains_before "$prepare_workflow" 'scripts/release prepare' 'git add Cargo.toml Cargo.lock CHANGELOG.md' "release PR preparation workflow stages Cargo.lock with release metadata after prepare"
}

test_release_tooling_configures_tea_login_before_token_bearing_tea_commands() {
  local prepare_workflow release_tool
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  release_tool="$ROOT/scripts/release"

  assert_file_has_line_containing_all "$prepare_workflow" "release PR preparation workflow configures a non-interactive tea login from Actions token env" 'tea login add' '--url' 'GITEA_SERVER_URL' '--token' 'GITEA_SERVER_TOKEN'
  assert_file_contains_before "$prepare_workflow" 'tea login add' 'tea pr list --repo jwilger/auto_review' "release PR preparation workflow configures tea login before listing release PRs"
  assert_file_contains_before "$prepare_workflow" 'tea login add' 'tea pr edit --repo jwilger/auto_review' "release PR preparation workflow configures tea login before editing release PRs"
  assert_file_contains_before "$prepare_workflow" 'tea login add' 'tea pr create --repo jwilger/auto_review' "release PR preparation workflow configures tea login before creating release PRs"

  assert_file_has_line_containing_all "$release_tool" "release publish tooling configures a non-interactive tea login from release token env" 'tea login add' '--url' 'GITEA_SERVER_URL' '--token' 'GITEA_SERVER_TOKEN'
  assert_file_contains_before "$release_tool" 'tea login add' 'tea release create --repo jwilger/auto_review' "release publish tooling configures tea login before creating the Forgejo release"
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

test_release_tooling_tests_are_wired_into_nix_flake_check() {
  local flake
  flake="$ROOT/flake.nix"

  assert_file_contains "$flake" 'release-tooling' "nix flake check exposes the release tooling shell test"
  assert_file_contains "$flake" 'bash tests/release_tooling_test.sh' "nix flake check runs release tooling tests"
  assert_file_contains "$flake" '/tests/' "nix flake source includes release tooling tests"
  assert_file_contains "$flake" '/scripts/' "nix flake source includes release tooling scripts"
  assert_file_contains "$flake" 'AGENTS.md' "nix flake source includes contributor guidance checked by release tooling tests"
  assert_file_contains "$flake" '.forgejo/pull_request_template.md' "nix flake source includes PR template checked by release tooling tests"
  assert_file_contains "$flake" '.kilo/command/prepare-forgejo-pr.md' "nix flake source includes PR command guidance checked by release tooling tests"
  assert_file_contains "$flake" '.kilo/skills/rust-workspace-engineering/SKILL.md' "nix flake source includes Rust workspace skill checked by release tooling tests"
}

test_release_token_blast_radius_is_documented() {
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release preparation PAT' "threat model names the operator-created release preparation PAT asset"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release publishing PAT' "threat model names the release publishing PAT asset"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release preparation PAT blast radius' "threat model documents the release preparation PAT blast radius"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release publishing PAT blast radius' "threat model documents the release publishing PAT blast radius"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'prepare release PR branches and release PRs only in `jwilger/auto_review`' "threat model documents the release preparation PAT scope"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'push tags and create releases only in `jwilger/auto_review`' "threat model documents the release publishing PAT scope"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release preparation PAT blast radius' "operations docs summarize the release preparation PAT blast radius"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release publishing PAT blast radius' "operations docs summarize the release publishing PAT blast radius"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'prepare release PR branches and release PRs only in `jwilger/auto_review`' "operations docs constrain the release preparation PAT scope"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'push tags and create releases only in `jwilger/auto_review`' "operations docs constrain the release publishing PAT scope"
}

test_release_secrets_are_documented_for_operators() {
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_PREPARE_TOKEN`' "operations docs require an operator-created release preparation Actions secret"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Forgejo Actions secret `RELEASE_PREPARE_TOKEN`' "threat model documents the operator-created release preparation Actions secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release publishing credential' "operations docs identify the release publishing credential purpose"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'protected `release-publish` environment secret `RELEASE_PUBLISH_TOKEN`' "operations docs document release publishing credential as a protected environment secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'manual approval gate' "operations docs require a manual approval gate for release publishing credentials"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'Configure the release publishing credential as Forgejo Actions secret `RELEASE_PUBLISH_TOKEN`' "operations docs do not describe the publish token as an ordinary repo-wide Actions secret"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'FORGEJO_RELEASE_PREPARE_TOKEN' "operations docs do not expose the old operator-facing prepare secret name"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "operations docs do not expose the old operator-facing publish secret name"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'FORGEJO_RELEASE_PREPARE_TOKEN' "threat model does not expose the old operator-facing prepare secret name"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "threat model does not expose the old operator-facing publish secret name"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `FORGEJO_TOKEN`' "operations docs do not document the legacy shared release Actions secret"
  assert_file_lacks_line "$ROOT/deploy/systemd/auto_review.env.example" 'FORGEJO_TOKEN=' "systemd env example does not declare the release publishing Actions secret"
  assert_file_not_contains "$ROOT/deploy/systemd/auto_review.env.example" 'Release publishing credential' "systemd env example does not describe the Actions-only release publishing credential"
}

test_prepare_dry_run_plans_release_pr_changes_without_publish
test_prepare_non_dry_run_updates_release_files
test_prepare_non_dry_run_updates_arbitrary_current_workspace_version
test_prepare_non_dry_run_updates_cargo_lock_workspace_package_versions
test_prepare_generates_release_notes_from_conventional_commits_since_previous_tag
test_pr_guidance_delegates_changelog_notes_to_release_prepare
test_publish_dry_run_requires_merged_release_pr_signal
test_publish_non_dry_run_uses_scoped_forgejo_commands_with_fakes
test_publish_non_dry_run_pushes_tag_and_sends_changelog_notes
test_release_workflows_exist_for_prepare_pr_and_publish_on_merge
test_release_workflows_install_or_reuse_nix_like_ci_before_nix_develop
test_prepare_workflow_validates_dispatch_inputs_before_token_bearing_steps
test_publish_workflow_requires_release_pr_base_branch_main
test_release_workflows_use_prepare_secret_and_protected_publish_token
test_prepare_workflow_requires_explicit_prepare_secret_runtime_env
test_publish_workflow_validates_provenance_and_changed_files_before_publish_token
test_publish_workflow_semver_validates_version_before_publish_token
test_publish_workflow_executes_from_merge_commit_sha_before_publish_token
test_changelog_mentions_issue_66_release_automation
test_prepare_workflow_configures_git_identity_before_commit
test_prepare_workflow_checkout_does_not_persist_credentials
test_prepare_workflow_pushes_release_branch_with_prepare_secret_helper
test_prepare_workflow_stages_cargo_lock_after_prepare
test_release_tooling_configures_tea_login_before_token_bearing_tea_commands
test_publish_workflow_requires_trusted_release_environment
test_release_tooling_tests_are_wired_into_nix_flake_check
test_release_secrets_are_documented_for_operators
test_release_token_blast_radius_is_documented

if [[ $failures -eq 0 ]]; then
  printf 'release tooling dry-run tests passed\n'
  exit 0
fi

printf 'release tooling dry-run tests failed: %s\n' "$failures"
exit 1
