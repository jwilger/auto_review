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
# FAKE_GH_SLEEP makes every call hang, like an unresponsive API.
cat >"$work/bin/gh" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
fix=${FAKE_GH_FIXTURES:?}
if [[ -n "${FAKE_GH_SLEEP:-}" ]]; then sleep "$FAKE_GH_SLEEP"; fi
case "$1 ${2:-}" in
  "pr view")
    # pr-<number>.json, when a case records one, else pr.json.
    if [[ -f "$fix/pr-${3:-}.json" ]]; then cat "$fix/pr-$3.json"; else cat "$fix/pr.json"; fi
    ;;
  "api "*)
    shift
    [[ "$1" == --paginate ]] && shift
    path=$1
    case "$path" in
      */contents/factory/policy.md\?ref=main) cat "$fix/policy.md" ;;
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

# Fake git: `merge-tree --write-tree <p1> <p2>` prints the tree recorded for
# that pair, and exits 1 (a conflict) when none is recorded.
cat >"$work/bin/git" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
fix=${FAKE_GH_FIXTURES:?}
[[ "$1" == -C ]] && shift 2
[[ "$1 $2" == "merge-tree --write-tree" ]] || exit 2
merge="$fix/merges/$3-$4"
[[ -f "$merge" ]] || exit 1
cat "$merge"
FAKE
chmod +x "$work/bin/git"

failures=0
case_no=0

# fixture <name>: start a fresh fixture directory for one case.
fixture() {
  case_no=$((case_no + 1))
  fix="$work/case$case_no"
  mkdir -p "$fix/commits" "$fix/compare" "$fix/merges"
  echo '[]' >"$fix/reviews.json"
  echo '[]' >"$fix/events.json"
  # The trusted Roles, as `main` has them.
  printf '%s\n' '# Autonomy policy' '' '## Roles' '- engineer: @jwilger' '' '## Change classes' >"$fix/policy.md"
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

# commit <sha> <committer date> <parent...>; its tree is t-<sha>.
commit() {
  local sha=$1 date=$2
  shift 2
  local parents
  parents=$(printf '%s\n' "$@" | jq -R '{sha: .}' | jq -s .)
  jq -n --arg sha "$sha" --arg date "$date" --argjson parents "$parents" \
    '{sha: $sha, parents: $parents, commit: {committer: {date: $date}, tree: {sha: ("t-" + $sha)}}}' \
    >"$fix/commits/$sha.json"
}

# clean_merge <merge sha> <p1> <p2>: merging p2 into p1 needs no hand edits,
# so it yields exactly the merge commit's tree.
clean_merge() {
  echo "t-$1" >"$fix/merges/$2-$3"
}

# evil_merge <p1> <p2>: the automatic merge of p2 into p1 gives another tree
# than the merge commit records (a hand-resolved conflict or extra edits).
evil_merge() {
  echo "t-automatic" >"$fix/merges/$1-$2"
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
    | PATH="$work/bin:$PATH" FAKE_GH_FIXTURES="$fix" CLAUDE_PROJECT_DIR="${PROJECT_DIR:-$repo_root}" \
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

# CI gate scripts decide whether a change may merge, so they are factory
# paths too (factory/policy.md, `## Change classes`).
fixture
pr h1 scripts/check-hermetic-git
commit h1 2026-11-17T10:00:00Z p0
expect 2 "denies a pull request changing a CI gate script with no approval" "$merge"

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
clean_merge h1 r1 m1
commit r1 2026-11-17T10:00:00Z p0
review jwilger APPROVED r1 2026-11-17T11:00:00Z
expect 0 "allows an approval followed only by merges of main" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T12:00:00Z r1 m1
on_main m1
evil_merge r1 m1
commit r1 2026-11-17T10:00:00Z p0
review jwilger APPROVED r1 2026-11-17T11:00:00Z
expect 2 "denies an approval followed by a merge of main with hand edits" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T12:00:00Z r1 m1
on_main m1
commit r1 2026-11-17T10:00:00Z p0
review jwilger APPROVED r1 2026-11-17T11:00:00Z
expect 2 "denies an approval followed by a merge of main that conflicts" "$merge"

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
clean_merge h1 r1 m1
commit r1 2026-11-17T10:00:00Z p0
label_event labeled jwilger factory:approved 2026-11-17T11:00:00Z
expect 0 "allows a Roles label newer than the last branch-own commit" "$merge"

fixture
LABELS='["Factory:Approved"]' pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
label_event labeled jwilger Factory:Approved 2026-11-17T11:00:00Z
expect 0 "reads the label case-insensitively, as GitHub names it" "$merge"

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

# The pull request's own policy.md names another reviewer; only main's Roles
# count, so the change under review cannot pick who approves it.
fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review someone-else APPROVED h1 2026-11-17T11:00:00Z
mkdir -p "$work/pr-checkout/factory"
printf '%s\n' '## Roles' '- engineer: @jwilger' '- engineer: @someone-else' >"$work/pr-checkout/factory/policy.md"
PROJECT_DIR="$work/pr-checkout" expect 2 "reads Roles from main, not the checked-out branch" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review jwilger APPROVED h1 2026-11-17T11:00:00Z
rm "$fix/policy.md"
expect 2 "fails closed when main's policy cannot be read" "$merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review jwilger APPROVED h1 2026-11-17T11:00:00Z
FAKE_GH_SLEEP=5 FACTORY_MERGE_GUARD_CALL_TIMEOUT=1 expect 2 "fails closed when a gh call hangs" "$merge"

# Every merge in a command is checked on its own, with the pin from its own
# segment, so one approved merge cannot carry another along.
fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review jwilger APPROVED h1 2026-11-17T11:00:00Z
expect 2 "denies an unpinned merge after an approved one" "$merge ; gh pr merge 9 --squash --admin"
expect 2 "denies a REST merge after an approved one" "$merge && gh api -X PUT repos/o/r/pulls/9/merge"
expect 2 "denies a GraphQL merge after an approved one" "$merge ; gh api graphql -f query='mutation { mergePullRequest(input: {pullRequestId: \"x\"}) { clientMutationId } }'"
expect 2 "reads the pin from the merge's own segment" 'gh pr merge 7 --squash ; echo --match-head-commit h1'
expect 0 "allows two pinned, approved merges" "$merge ; $merge"

fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
expect 2 "checks a merge run under nix develop" 'nix develop .#ci --command gh pr merge 7 --squash'
expect 2 "checks a merge run under bash -c" "bash -c 'gh pr merge 7 --squash'"
expect 2 "checks a merge after a shell keyword" 'if true; then gh pr merge 7 --squash; fi'
# Bash splits words on tabs as well as spaces.
expect 2 "checks a tab-separated merge" $'gh\tpr\tmerge 7 --squash'
expect 2 "blocks a tab-separated label add" $'gh\tpr\tedit 5 --add-label factory:approved'

# The pull request is the merge's argument, not a flag's value: here #7
# touches no factory paths, and #218 is the one gh would merge.
fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
jq -n '{headRefOid: "h7", files: [{path: "crates/ar-llm/src/lib.rs"}], labels: []}' >"$fix/pr-7.json"
expect 2 "skips a flag's numeric value when reading the pull request" 'gh pr merge --subject 7 218 --squash'
expect 2 "skips a quoted flag value holding a number" "gh pr merge -b 'fixes 7' 218 --squash"
expect 2 "skips a --flag=value holding a number" 'gh pr merge --subject=7 218 --squash'
expect 0 "still reads a pull request named after its flags" 'gh pr merge --squash --subject x 7'

fixture
pr h1 factory/policy.md
expect 0 "keeps the in-session FACTORY_MERGE_APPROVED override" 'FACTORY_MERGE_APPROVED=7 gh pr merge 7 --squash'

# A recorded approval covers one head, but a scheduled auto-merge outlives
# later pushes, so it never schedules auto-merge of a factory pull request.
fixture
pr h1 factory/policy.md
commit h1 2026-11-17T10:00:00Z p0
review jwilger APPROVED h1 2026-11-17T11:00:00Z
expect 2 "denies auto-merge of an approved factory pull request" 'gh pr merge 7 --auto --squash --match-head-commit h1'
expect 2 "denies the --auto=true form" 'gh pr merge 7 --auto=true --squash --match-head-commit h1'
expect 0 "allows auto-merge of a factory pull request on the in-session override" 'FACTORY_MERGE_APPROVED=7 gh pr merge 7 --auto --squash'

fixture
pr h1 crates/ar-llm/src/lib.rs
expect 0 "allows auto-merge of a pull request with no factory paths" 'gh pr merge 7 --auto --squash --match-head-commit h1'

fixture
expect 2 "blocks gh pr edit adding factory:approved" 'gh pr edit 5 --add-label factory:approved'
expect 2 "blocks gh issue edit adding factory:approved in a list" 'gh issue edit 5 --add-label "bug,factory:approved"'
expect 2 "blocks a gh api label POST naming factory:approved" "gh api repos/o/r/issues/5/labels -f 'labels[]=factory:approved'"
expect 2 "blocks an explicit PUT naming factory:approved" 'gh api --method PUT repos/o/r/issues/5/labels -F labels[]=factory:approved'
expect 2 "blocks gh -R <repo> pr edit adding factory:approved" 'gh -R jwilger/auto_review pr edit 5 --add-label factory:approved'
expect 2 "blocks gh pr --repo <repo> edit adding factory:approved" 'gh pr --repo jwilger/auto_review edit 5 --add-label factory:approved'
expect 2 "blocks --repo=<repo> and --add-label=<label>" 'gh pr edit --repo=o/r 5 --add-label=factory:approved'
expect 2 "blocks an env-prefixed gh pr edit" 'env gh pr edit 5 --add-label factory:approved'
expect 2 "blocks a command-prefixed gh pr edit" 'command gh pr edit 5 --add-label factory:approved'
expect 2 "blocks a gh api label POST after -R" "gh -R o/r api repos/o/r/issues/5/labels -f 'labels[]=factory:approved'"
expect 2 "blocks gh pr create labelling factory:approved" 'gh pr create --title x --body y --label factory,factory:approved'
expect 2 "blocks gh pr create --label=factory:approved" 'gh pr create --title x --body y --label=factory:approved'
expect 2 "blocks gh issue create -l factory:approved" 'gh issue create --title x --body y -l factory:approved'
# shellcheck disable=SC2016 # the command text carries a literal $label
expect 2 "blocks a label held in a shell variable" 'gh pr edit 5 --add-label "$label"'
expect 2 "blocks a GraphQL addLabelsToLabelable mutation" "gh api graphql -f query='mutation { addLabelsToLabelable(input: {labelableId: \"x\", labelIds: [\"y\"]}) { clientMutationId } }'"
expect 2 "blocks a label write from an --input file" 'gh api repos/o/r/issues/5/labels --input labels.json'
expect 2 "blocks a label add under nix develop" 'nix develop .#ci --command gh pr edit 5 --add-label factory:approved'
expect 2 "blocks a label add after a shell keyword" 'if true; then gh pr edit 5 --add-label factory:approved; fi'
expect 2 "blocks a label add inside a brace group" '{ gh pr edit 5 --add-label factory:approved; }'
expect 2 "blocks a label add under bash -c" "bash -c 'gh pr edit 5 --add-label factory:approved'"
expect 2 "blocks a label add under sh -c" 'sh -c "gh pr edit 5 --add-label factory:approved"'
expect 2 "blocks a label add under bash -lc after another command" "bash -lc 'cd x; gh pr edit 5 --add-label factory:approved'"
expect 2 "blocks a label add under xargs" 'echo 5 | xargs -n1 gh pr edit --add-label factory:approved'
expect 2 "blocks an issue PATCH from an --input file" 'gh api -X PATCH repos/o/r/issues/5 --input body.json'
expect 2 "blocks a label write read from a file" "gh api repos/o/r/issues/5/labels -F 'labels[]=@lbl.txt'"
# GitHub label names are case-insensitive: gh and the REST API apply the
# existing factory:approved label for any spelling of it.
expect 2 "blocks an upper-case factory:approved" 'gh pr edit 5 --add-label FACTORY:APPROVED'
expect 2 "blocks a mixed-case api label POST" 'gh api -X POST repos/o/r/issues/5/labels -f labels[]=Factory:Approved'
expect 2 "blocks a backslash-escaped label" 'gh pr edit 5 --add-label factory\:approved'
expect 2 "blocks a label split across adjacent quotes" "gh pr edit 5 --add-label 'factory'':approved'"
expect 2 "blocks an api label write split across adjacent quotes" "gh api repos/o/r/issues/5/labels -f labels[]='factory'\":approved\""
expect 0 "allows an issue PATCH with readable fields" 'gh api -X PATCH repos/o/r/issues/5 -f title=x'
expect 0 "allows an issue comment body read from a file" 'gh api repos/o/r/issues/5/comments -F body=@c.md'
expect 0 "allows other labels after a shell keyword" 'if true; then gh pr edit 5 --add-label needs-rework; fi'
expect 0 "allows removing factory:approved while adding another label" 'gh pr edit 5 --remove-label factory:approved --add-label needs-rework'
expect 0 "allows gh pr create with other labels and prose naming factory:approved" 'gh pr create --title t --body "waits for factory:approved" --label factory'
expect 0 "allows removing factory:approved" 'gh pr edit 5 --remove-label factory:approved'
expect 0 "allows a gh api DELETE of factory:approved" 'gh api -X DELETE repos/o/r/issues/5/labels/factory:approved'
expect 0 "allows prose that mentions the label" 'gh pr create --title t --body "waits for factory:approved"'
expect 0 "allows creating the factory:approved label itself" "gh label create factory:approved --description 'maintainer approval for the factory to merge'"

# Agents act as a Roles account, so an approving review they submit would
# read as the maintainer's recorded approval on a pull request someone else
# (such as a bot) authored. The guard blocks every way of submitting one.
fixture
expect 2 "blocks gh pr review --approve" 'gh pr review 7 --approve'
expect 2 "blocks gh pr review -a" 'gh pr review 7 -a -b lgtm'
expect 2 "blocks -a inside a short-flag cluster" 'gh pr review 7 -ab lgtm'
expect 2 "blocks gh pr review --approve under nix develop" 'nix develop .#ci --command gh pr review 7 --approve'
expect 2 "blocks a REST review with event=APPROVE" 'gh api repos/o/r/pulls/7/reviews -f event=APPROVE'
expect 2 "blocks a REST review with a lower-case approve event" "gh api repos/o/r/pulls/7/reviews -f 'event=approve'"
expect 2 "blocks submitting a pending review as APPROVE" 'gh api -X POST repos/o/r/pulls/7/reviews/9/events -f event=APPROVE'
expect 2 "blocks a REST review from an --input file" 'gh api repos/o/r/pulls/7/reviews --input review.json'
expect 2 "blocks a REST review whose fields come from a file" 'gh api repos/o/r/pulls/7/reviews -F event=@event.txt'
# shellcheck disable=SC2016 # the command text carries a literal $event
expect 2 "blocks a REST review whose event is a shell variable" 'gh api repos/o/r/pulls/7/reviews -f event="$event"'
expect 2 "blocks a GraphQL addPullRequestReview mutation" "gh api graphql -f query='mutation { addPullRequestReview(input: {pullRequestId: \"x\", event: APPROVE}) { clientMutationId } }'"
expect 2 "blocks a GraphQL submitPullRequestReview mutation" "gh api graphql -f query='mutation { submitPullRequestReview(input: {pullRequestReviewId: \"x\", event: APPROVE}) { clientMutationId } }'"
expect 2 "blocks a GraphQL query read from a file" 'gh api graphql -F query=@mutation.graphql'
expect 0 "allows a commenting review" 'gh pr review 7 --comment -b "please approve once CI is green"'
expect 0 "allows a review requesting changes" 'gh pr review 7 --request-changes -b "fix the test"'
expect 0 "allows reading reviews" 'gh api repos/o/r/pulls/7/reviews'
expect 0 "allows a REST review with event=COMMENT" 'gh api repos/o/r/pulls/7/reviews -f event=COMMENT -f body=x'

if ((failures > 0)); then
  echo "$failures case(s) failed" >&2
  exit 1
fi
echo "all merge guard cases passed"
