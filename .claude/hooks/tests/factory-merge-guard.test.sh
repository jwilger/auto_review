#!/usr/bin/env bash
# Tests for .claude/hooks/factory-merge-guard.sh. A fake `gh` on PATH serves
# fixture JSON from a temporary directory, so no network or token is needed.
# Run: bash .claude/hooks/tests/factory-merge-guard.test.sh
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
hook="$here/../factory-merge-guard.sh"
repo_root=$(cd "$here/../../.." && pwd)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

# Fake gh: `pr view` serves pr.json; `api <path>` serves the fixture named
# after the path's tail. A missing fixture fails, like an unreadable API.
cat >"$work/bin/gh" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
fix=${FAKE_GH_FIXTURES:?}
case "$1 ${2:-}" in
  "pr view") cat "$fix/pr.json" ;;
  "api "*)
    shift
    [[ "$1" == --paginate ]] && shift
    path=$1
    case "$path" in
      */pulls/*/reviews) cat "$fix/reviews.json" ;;
      */issues/*/events) cat "$fix/events.json" ;;
      */commits/*) cat "$fix/commits/${path##*/}.json" ;;
      */compare/*) base=${path##*/compare/}; cat "$fix/compare/${base%%...*}.json" ;;
      *) exit 1 ;;
    esac
    ;;
  *) exit 1 ;;
esac
FAKE
chmod +x "$work/bin/gh"

failures=0
case_no=0

# fixture <name>: start a fresh fixture directory for one case.
fixture() {
  case_no=$((case_no + 1))
  fix="$work/case$case_no"
  mkdir -p "$fix/commits" "$fix/compare"
  echo '[]' >"$fix/reviews.json"
  echo '[]' >"$fix/events.json"
}

# pr <head> <files...> [labels via LABELS env]
pr() {
  local head=$1
  shift
  local files
  files=$(printf '%s\n' "$@" | jq -R '{path: .}' | jq -s .)
  jq -n --arg head "$head" --argjson files "$files" --argjson labels "${LABELS:-[]}" \
    '{headRefOid: $head, files: $files, labels: ($labels | map({name: .}))}' >"$fix/pr.json"
}

# commit <sha> <committer date> <parent...>
commit() {
  local sha=$1 date=$2
  shift 2
  local parents
  parents=$(printf '%s\n' "$@" | jq -R '{sha: .}' | jq -s .)
  jq -n --arg sha "$sha" --arg date "$date" --argjson parents "$parents" \
    '{sha: $sha, parents: $parents, commit: {committer: {date: $date}}}' >"$fix/commits/$sha.json"
}

# on_main <sha>: <sha> is an ancestor of main.
on_main() {
  echo '{"status": "ahead"}' >"$fix/compare/$1.json"
}

# off_main <sha>: <sha> is not an ancestor of main.
off_main() {
  echo '{"status": "diverged"}' >"$fix/compare/$1.json"
}

review() { # review <login> <state> <commit> <submitted_at>
  jq --arg u "$1" --arg s "$2" --arg c "$3" --arg t "$4" \
    '. + [{user: {login: $u}, state: $s, commit_id: $c, submitted_at: $t}]' \
    "$fix/reviews.json" >"$fix/reviews.tmp" && mv "$fix/reviews.tmp" "$fix/reviews.json"
}

label_event() { # label_event <event> <login> <label> <created_at>
  jq --arg e "$1" --arg u "$2" --arg l "$3" --arg t "$4" \
    '. + [{event: $e, actor: {login: $u}, label: {name: $l}, created_at: $t}]' \
    "$fix/events.json" >"$fix/events.tmp" && mv "$fix/events.tmp" "$fix/events.json"
}

# expect <exit code> <description> <command>
expect() {
  local want=$1 name=$2 cmd=$3 got=0
  jq -n --arg c "$cmd" '{tool_input: {command: $c}}' \
    | PATH="$work/bin:$PATH" FAKE_GH_FIXTURES="$fix" CLAUDE_PROJECT_DIR="$repo_root" \
      bash "$hook" >/dev/null 2>"$work/stderr" || got=$?
  if [[ "$got" != "$want" ]]; then
    failures=$((failures + 1))
    echo "FAIL: $name: exit $got, want $want" >&2
    sed 's/^/  /' "$work/stderr" >&2
  else
    echo "ok: $name"
  fi
}

merge='gh pr merge 7 --squash --match-head-commit h1'

fixture
expect 0 "ignores commands that are not merges" 'gh pr view 7'

fixture
pr h1 crates/ar-llm/src/lib.rs
expect 0 "allows merging a pull request with no factory paths" 'gh pr merge 7 --squash'

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
expect 2 "denies a factory pull request with no approval" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review jwilger APPROVED h1 2026-11-17T11:00:00Z
expect 2 "denies a factory merge without --match-head-commit" 'gh pr merge 7 --squash'

fixture
pr h2 factory/policy.md
commit h2 2026-11-17T10:00:00Z p0
review jwilger APPROVED h2 2026-11-17T11:00:00Z
expect 2 "denies when --match-head-commit is not the current head" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review jwilger APPROVED h1 2026-11-17T11:00:00Z
expect 0 "allows a Roles approval on the current head" "$merge"
expect 0 "allows the --match-head-commit=<sha> form" 'gh pr merge 7 --squash --match-head-commit=h1'

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review someone-else APPROVED h1 2026-11-17T11:00:00Z
expect 2 "denies an approval from outside Roles" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review jwilger APPROVED h1 2026-11-17T11:00:00Z
review jwilger CHANGES_REQUESTED h1 2026-11-17T12:00:00Z
expect 2 "denies when the reviewer's latest decision requests changes" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T12:00:00Z r1 m1
on_main m1
commit r1 2026-11-17T10:00:00Z p0
review jwilger APPROVED r1 2026-11-17T11:00:00Z
expect 0 "allows an approval followed only by merges of main" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T12:00:00Z r1 x1
off_main x1
commit r1 2026-11-17T10:00:00Z p0
review jwilger APPROVED r1 2026-11-17T11:00:00Z
expect 2 "denies an approval followed by a merge of another branch" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T12:00:00Z r1
commit r1 2026-11-17T10:00:00Z p0
review jwilger APPROVED r1 2026-11-17T11:00:00Z
expect 2 "denies an approval followed by a branch-own commit" "$merge"

fixture
LABELS='["factory:approved"]' pr h1 factory/policy.md
commit h1 2026-11-17T12:00:00Z r1 m1
on_main m1
commit r1 2026-11-17T10:00:00Z p0
label_event labeled jwilger factory:approved 2026-11-17T11:00:00Z
expect 0 "allows a Roles label newer than the last branch-own commit" "$merge"

fixture
LABELS='["factory:approved"]' pr h1 factory/policy.md
commit h1 2026-11-17T12:00:00Z p0
label_event labeled jwilger factory:approved 2026-11-17T11:00:00Z
expect 2 "denies a label older than the last branch-own commit" "$merge"

fixture
LABELS='["factory:approved"]' pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
label_event labeled someone-else factory:approved 2026-11-17T11:00:00Z
expect 2 "denies a label applied from outside Roles" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
label_event labeled jwilger factory:approved 2026-11-17T11:00:00Z
expect 2 "denies a label that has since been removed" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
rm "$fix/reviews.json"
expect 2 "fails closed when the reviews cannot be read" "$merge"

fixture
pr h1 factory/policy.md
expect 0 "keeps the in-session FACTORY_MERGE_APPROVED override" 'FACTORY_MERGE_APPROVED=7 gh pr merge 7 --squash'

fixture
expect 2 "blocks gh pr edit adding factory:approved" 'gh pr edit 5 --add-label factory:approved'
expect 2 "blocks gh issue edit adding factory:approved in a list" 'gh issue edit 5 --add-label "bug,factory:approved"'
expect 2 "blocks a gh api label POST naming factory:approved" "gh api repos/o/r/issues/5/labels -f 'labels[]=factory:approved'"
expect 2 "blocks an explicit PUT naming factory:approved" 'gh api --method PUT repos/o/r/issues/5/labels -F labels[]=factory:approved'
expect 0 "allows removing factory:approved" 'gh pr edit 5 --remove-label factory:approved'
expect 0 "allows a gh api DELETE of factory:approved" 'gh api -X DELETE repos/o/r/issues/5/labels/factory:approved'
expect 0 "allows prose that mentions the label" 'gh pr create --title t --body "waits for factory:approved"'

if ((failures > 0)); then
  echo "$failures case(s) failed" >&2
  exit 1
fi
echo "all merge guard cases passed"
