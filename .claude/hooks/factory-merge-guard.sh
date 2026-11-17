#!/usr/bin/env bash
# PreToolUse guard: agents never merge, or schedule the merge of, a pull
# request that touches a `factory` path from factory/policy.md. The maintainer
# merges those. Agents run as the maintainer's own GitHub account, so GitHub
# cannot enforce this; this hook is a guard against mistakes, not a security
# boundary.
#
# One exception: the maintainer's recorded approval. A merge of a factory
# pull request is allowed when the command pins the head with
# `--match-head-commit <sha>`, <sha> is the pull request's current head, and
# one of these signals, read here through `gh api`, holds:
#   - a person listed under `## Roles` in factory/policy.md has an approving
#     review (their latest approve/request-changes decision) on a commit R of
#     the pull request, and every commit after R on the branch is a merge of
#     `main`; or
#   - the `factory:approved` label is on the pull request, and its latest
#     application was by a Roles person after the branch's last own commit
#     (merges of `main` after it do not count).
# That approval is the maintainer's instruction for that pull request and that
# head only, not a blanket bypass. Anything the hook cannot read denies.
#
# Because agents act as the maintainer's account, they must never add the
# `factory:approved` label themselves: this hook also blocks commands that add
# it (`gh pr edit`/`gh issue edit --add-label`, and `gh api` writes naming it).
#
# Override, only on the maintainer's explicit instruction in the session:
# prefix the command with FACTORY_MERGE_APPROVED=<pr number>.
set -euo pipefail

cmd=$(jq -r '.tool_input.command // ""')

deny() {
  echo "Blocked by factory merge guard: $1" >&2
  exit 2
}

approved_label='factory:approved'

# Label guard. Each shell segment is checked on its own, from command
# position, so prose such as a pull request body that names the label passes.
while IFS= read -r segment; do
  segment=$(sed -E 's/^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*//' <<<"$segment")
  grep -qF "$approved_label" <<<"$segment" || continue
  if grep -Eq '^gh +(pr|issue) +edit( |$)' <<<"$segment" && grep -Eq -- '--add-label' <<<"$segment"; then
    deny "only the maintainer adds the $approved_label label."
  fi
  if grep -Eq '^gh +api( |$)' <<<"$segment"; then
    method=$(grep -Eo -- '(-X|--method)[[:space:]=]*[A-Za-z]+' <<<"$segment" | head -n1 \
      | sed -E 's/^(-X|--method)[[:space:]=]*//' | tr '[:lower:]' '[:upper:]' || true)
    if [[ -z "$method" ]]; then
      if grep -Eq -- '(^| )(-f|-F|--field|--raw-field|--input)( |=|$)' <<<"$segment"; then
        method=POST
      else
        method=GET
      fi
    fi
    if [[ "$method" != GET && "$method" != DELETE ]]; then
      deny "only the maintainer adds the $approved_label label."
    fi
  fi
done < <(tr ';&|' '\n' <<<"$cmd")

# Only merge-shaped commands are checked: `gh pr merge` in command position
# (line start or after ; & | ( , optionally behind VAR=value assignments), so
# prose that mentions it, such as a pull request body, does not trip the
# guard; and the GraphQL or REST merge calls anywhere.
cmd_pos='(^|[;&|(])[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*'
if ! grep -Eq "${cmd_pos}gh +pr +merge|enablePullRequestAutoMerge|mergePullRequest|pulls/[0-9]+/merge" <<<"$cmd"; then
  exit 0
fi

# The first bare number (or pull URL) among `gh pr merge`'s arguments.
pr=$(grep -Eo 'gh +pr +merge[^;&|]*' <<<"$cmd" | head -n1 | tr ' ' '\n' \
  | sed -E 's#^https://github\.com/[^/]+/[^/]+/pull/##; s/^#//' | grep -Ex '[0-9]+' | head -n1 || true)
if [[ -z "$pr" ]]; then
  pr=$(grep -Eo 'pulls/[0-9]+/merge' <<<"$cmd" | grep -Eo '[0-9]+' | head -n1 || true)
fi
[[ -n "$pr" ]] || deny "name the pull request number explicitly so its files can be checked."

if grep -Eq "(^|[;&|] *)FACTORY_MERGE_APPROVED=${pr}( |$)" <<<"$cmd"; then
  exit 0
fi

view=$(gh pr view "$pr" --json files,headRefOid,labels) \
  || deny "could not read pull request #$pr."
files=$(jq -r '.files[].path' <<<"$view") \
  || deny "could not read the files of pull request #$pr."

# Keep in step with the `factory` class in factory/policy.md.
factory_re='^(\.claude/|factory/|\.github/|AGENTS\.md$|Justfile$|lefthook\.yml$|deny\.toml$|rust-toolchain\.toml$|flake\.nix$|flake\.lock$|Cargo\.toml$)'
hits=$(grep -E "$factory_re" <<<"$files" || true)
[[ -n "$hits" ]] || exit 0

held="pull request #$pr changes factory paths, which the maintainer merges:
$hits"

# --- Recorded approval ------------------------------------------------------

pinned=$(grep -Eo -- '--match-head-commit[= ]+[0-9A-Za-z]+' <<<"$cmd" | head -n1 \
  | sed -E 's/^--match-head-commit[= ]+//' || true)
[[ -n "$pinned" ]] || deny "$held
(a recorded approval only counts on a merge pinned with --match-head-commit <sha>)"

head=$(jq -r '.headRefOid // empty' <<<"$view")
[[ -n "$head" ]] || deny "could not read the head of pull request #$pr."
[[ "$pinned" == "$head" ]] || deny "$held
(--match-head-commit $pinned is not the current head $head)"

policy="${CLAUDE_PROJECT_DIR:-.}/factory/policy.md"
roles=$(awk '/^## /{r = ($0 == "## Roles")} r && /^- /' "$policy" 2>/dev/null \
  | grep -Eo '@[A-Za-z0-9-]+' | tr -d '@' | sort -u || true)
[[ -n "$roles" ]] || deny "$held
(could not read the Roles in $policy)"
roles_json=$(jq -R . <<<"$roles" | jq -sc .)

# Walk first parents from the head. Merges of `main` (two parents, the second
# an ancestor of main) are collected; the first other commit is the branch's
# last own commit, which ends the walk.
after=()
own=
own_date=
c=$head
for _ in $(seq 1 50); do
  info=$(gh api "repos/{owner}/{repo}/commits/$c") || deny "$held
(could not read commit $c)"
  parents=$(jq -r '.parents[].sha' <<<"$info") || deny "$held
(could not read the parents of $c)"
  if [[ $(wc -l <<<"$parents") -eq 2 ]]; then
    second=$(sed -n 2p <<<"$parents")
    status=$(gh api "repos/{owner}/{repo}/compare/$second...main" | jq -r '.status') \
      || deny "$held
(could not compare $second with main)"
    if [[ "$status" == ahead || "$status" == identical ]]; then
      after+=("$c")
      c=$(sed -n 1p <<<"$parents")
      continue
    fi
  fi
  own=$c
  own_date=$(jq -r '.commit.committer.date // empty' <<<"$info")
  break
done
[[ -n "$own" && -n "$own_date" ]] || deny "$held
(could not find the branch's last own commit)"
candidates=$(printf '%s\n' "$own" "${after[@]}" | jq -R . | jq -sc .)

# Signal 1: a Roles person's latest decisive review approves a candidate.
reviews=$(gh api --paginate "repos/{owner}/{repo}/pulls/$pr/reviews") \
  || deny "$held
(could not read the reviews of #$pr)"
review_ok=$(jq -s --argjson roles "$roles_json" --argjson cands "$candidates" '
  add // []
  | map(select((.user.login as $u | $roles | index($u))
               and (.state == "APPROVED" or .state == "CHANGES_REQUESTED")))
  | group_by(.user.login)
  | map(sort_by(.submitted_at) | last)
  | any(.state == "APPROVED" and (.commit_id as $c | $cands | index($c)))
' <<<"$reviews") || deny "$held
(could not parse the reviews of #$pr)"
[[ "$review_ok" == true ]] && exit 0

# Signal 2: the label is present, last applied by a Roles person after the
# branch's last own commit.
if jq -e --arg l "$approved_label" 'any(.labels[]?; .name == $l)' <<<"$view" >/dev/null; then
  events=$(gh api --paginate "repos/{owner}/{repo}/issues/$pr/events") \
    || deny "$held
(could not read the events of #$pr)"
  label_ok=$(jq -s --argjson roles "$roles_json" --arg l "$approved_label" --arg own "$own_date" '
    add // []
    | map(select(.event == "labeled" and .label.name == $l))
    | sort_by(.created_at) | last
    | if . == null then false
      else ((.actor.login as $u | $roles | index($u)) != null)
           and ((.created_at | fromdateiso8601) > ($own | fromdateiso8601))
      end
  ' <<<"$events") || deny "$held
(could not parse the events of #$pr)"
  [[ "$label_ok" == true ]] && exit 0
fi

deny "$held
(no recorded maintainer approval covers head $head)"
