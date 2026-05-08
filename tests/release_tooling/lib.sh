#!/usr/bin/env bash
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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

run_tests() {
  local test_name

  for test_name in "$@"; do
    "$test_name"
  done

  if [[ $failures -eq 0 ]]; then
    printf '%s tests passed\n' "${RELEASE_TOOLING_SUITE_NAME:-release tooling}"
    return 0
  fi

  printf '%s tests failed: %s\n' "${RELEASE_TOOLING_SUITE_NAME:-release tooling}" "$failures"
  return 1
}
