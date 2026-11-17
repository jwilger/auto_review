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
#     the pull request, and every commit after R on the branch is a clean merge
#     of `main`; or
#   - the `factory:approved` label is on the pull request, and its latest
#     application was by a Roles person after the branch's last own commit
#     (clean merges of `main` after it do not count).
# A merge of `main` is clean when its tree is exactly what `git merge-tree`
# makes of its parents; one with hand-resolved conflicts or extra edits is an
# own commit and voids an earlier approval. The Roles are read from `main`,
# never from the checkout, so a pull request cannot name its own approvers.
# That approval is the maintainer's instruction for that pull request and that
# head only, not a blanket bypass. Every merge in a command is checked on its
# own, with the pin read from that merge's own command segment; GraphQL merge
# mutations, which name no pull request number, are denied. Anything the hook
# cannot read, within its time budget, denies.
#
# Known residual risks: the label is compared with the committer date of the
# branch's last own commit, which the committer sets, so a backdated commit
# pushed after the label would inherit it (the factory removes the label
# before every push other than a clean merge of main to close that gap); and
# a review that GitHub dismissed on push (`dismiss_stale_reviews_on_push`)
# no longer counts, so the label is the approval that survives a refresh.
#
# Because agents act as the maintainer's account, they must never add the
# `factory:approved` label themselves: this hook also blocks commands that add
# it (`gh pr|issue edit --add-label`, `gh pr|issue create --label`, `gh api`
# writes naming it, GraphQL label mutations, issue and label writes whose
# values come from `--input` or a `-F field=@file`, and label names held in
# shell variables, which it cannot check). Labels are read after removing
# shell quoting, and commands are read from command position behind wrappers
# such as `nix develop .#ci --command`, `sh -c`, `xargs` and shell keywords.
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

# normalize <segment>: drop what can stand before a command (assignments,
# env, command, exec, eval, nohup, time, sudo, xargs and its flags, shell
# keywords such as `then` and `do`, a subshell or brace opener, `sh -c` or
# `bash -c` and the quote that opens its script, `nix develop <flake>
# --command`, a path to gh) and gh's global repository flag, so
# `nix develop .#ci --command env gh -R o/r pr edit` reads as `gh pr edit`.
normalize() {
  local s=$1 prev='' i rest
  local -a words
  while [[ "$s" != "$prev" ]]; do
    prev=$s
    s=$(sed -E '
      s/^[[:space:]]+//
      s/^(\$\(|\(|`|\{|!)//
      s/^(then|do|else|elif|if|while|until|env|command|exec|eval|nohup|time|sudo)([[:space:]]+|$)//
      s/^[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*([[:space:]]+|$)//
      s/^xargs([[:space:]]+-[^[:space:]]+)*([[:space:]]+|$)//
      s/^([^[:space:]]*\/)?(ba|da|k|z)?sh([[:space:]]+-[A-Za-z]+)*[[:space:]]+-[A-Za-z]*c[A-Za-z]*([[:space:]]+|$)//
      s/^["'"'"']//
    ' <<<"$s")
    # `nix develop|shell ... --command|-c <command>`: keep what follows the
    # first --command or -c; without one, nothing runs.
    if [[ $s =~ ^nix[[:space:]]+(develop|shell)([[:space:]]|$) ]]; then
      read -ra words <<<"$s"
      rest=
      for i in "${!words[@]}"; do
        if [[ ${words[i]} == --command || ${words[i]} == -c ]]; then
          rest="${words[*]:i+1}"
          break
        fi
      done
      s=$rest
    fi
  done
  sed -E 's#^[^[:space:]]*/gh([[:space:]]|$)#gh\1#; s/[[:space:]](-R|--repo)([[:space:]]+|=)[^[:space:]]+//g' <<<"$s"
}

# dequote: remove shell quoting (backslash escapes and quote characters), so
# `factory\:approved` and `'factory'':approved'` read as the label they spell.
dequote() {
  sed -E 's/\\(.)/\1/g; s/["'"'"']//g'
}

# A shell word: quoted runs, backslash escapes and plain characters, adjacent.
word="(\"[^\"]*\"|'[^']*'|\\\\.|[^[:space:]\"'\\\\])+"

# flag_values <segment> <flag regex>: each value given to the flags, unquoted.
flag_values() {
  grep -Eo -- "(^|[[:space:]])($2)([[:space:]]+|=)$word" <<<"$1" \
    | sed -E "s/^[[:space:]]*($2)([[:space:]]+|=)//" | dequote || true
}

# A label name the guard cannot read (a variable or a command substitution)
# could be the approval label.
unreadable='[$`]'

# GraphQL label writes name labels by node id, which the guard cannot read.
if grep -Eq '(^|[^A-Za-z])gh[[:space:]]' <<<"$cmd" \
  && grep -Eq 'addLabelsToLabelable|labelIds' <<<"$cmd"; then
  deny "add labels with gh pr edit --add-label, which this guard can check."
fi

# Label guard. Each shell segment is checked on its own, from command
# position, so prose such as a pull request body that names the label passes.
while IFS= read -r segment; do
  segment=$(normalize "$segment")
  if grep -Eq '^gh +(pr|issue) +(edit|create)( |$)' <<<"$segment"; then
    if grep -Eq '^gh +(pr|issue) +edit' <<<"$segment"; then
      values=$(flag_values "$segment" '--add-label')
    else
      values=$(flag_values "$segment" '-l|--label')
    fi
    if grep -qF "$approved_label" <<<"$values"; then
      deny "only the maintainer adds the $approved_label label."
    fi
    if grep -Eq "$unreadable" <<<"$values"; then
      deny "name labels literally, so this guard can check them for $approved_label."
    fi
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
      if dequote <<<"$segment" | grep -qF "$approved_label"; then
        deny "only the maintainer adds the $approved_label label."
      fi
      # An issue write (which can set `labels`) or a label write whose values
      # come from a file, stdin or the shell cannot be checked.
      if grep -Eq "issues/[^[:space:]/]+(/labels)?([[:space:]\"']|\\?|$)" <<<"$segment" \
        && grep -Eq -- "(^| )--input( |=|$)|(^| )(-F|--field)([[:space:]]+|=)[^[:space:]]*=@|$unreadable" <<<"$segment"; then
        deny "name labels literally in issue and label writes, so this guard can check them for $approved_label."
      fi
    fi
  fi
done < <(tr ';&|' '\n' <<<"$cmd")

# Only merge-shaped commands are checked: `gh pr merge` in command position
# (each shell segment, read from command position as the label guard reads
# it), so prose that mentions it, such as a pull request body, does not trip
# the guard; and the GraphQL or REST merge calls anywhere. Every merge in the
# command is checked on its own, with the pin read from its own segment, so
# one approved merge never carries another along.
if grep -Eq 'enablePullRequestAutoMerge|mergePullRequest' <<<"$cmd"; then
  deny "merge with gh pr merge <number> --match-head-commit <sha>, which this guard can check."
fi

merges=()
while IFS= read -r raw; do
  segment=$(normalize "$raw")
  if grep -Eq '^gh +pr +merge( |$)' <<<"$segment"; then
    # The first bare number (or pull URL) among `gh pr merge`'s arguments.
    pr=$(tr ' ' '\n' <<<"$segment" \
      | sed -E 's#^https://github\.com/[^/]+/[^/]+/pull/##; s/^#//' | grep -Ex '[0-9]+' | head -n1 || true)
    [[ -n "$pr" ]] || deny "name the pull request number explicitly so its files can be checked."
    pin=$(flag_values "$segment" '--match-head-commit' | head -n1 | tr -d "\"'")
    override=0
    if grep -Eq "(^|[[:space:]])FACTORY_MERGE_APPROVED=${pr}([[:space:]]|$)" <<<"${raw%%pr merge*}"; then
      override=1
    fi
    merges+=("$pr $override $pin")
  fi
  for pr in $(grep -Eo 'pulls/[0-9]+/merge' <<<"$raw" | grep -Eo '[0-9]+' || true); do
    merges+=("$pr 0")
  done
done < <(tr ';&|' '\n' <<<"$cmd")

((${#merges[@]} > 0)) || exit 0

# Every gh or git call is bounded, and the whole check has a budget well under
# the hook's own timeout in .claude/settings.json: a timed-out hook lets the
# command run, so the guard denies before that can happen.
call_timeout=${FACTORY_MERGE_GUARD_CALL_TIMEOUT:-10}
((call_timeout >= 1 && call_timeout <= 10)) || call_timeout=10
budget=60
command -v timeout >/dev/null || deny "the timeout command is missing, so remote reads cannot be bounded."
bounded() {
  ((SECONDS < budget)) || return 124
  timeout "$call_timeout" "$@"
}

# check_merge <pr> <override> [<pinned sha>]: exits 0 when the merge may run
# and denies otherwise. Run it in a subshell, once per merge.
check_merge() {
  local pr=$1 override=$2 pinned=${3:-}
  ((override == 0)) || exit 0

  local view files hits held
  view=$(bounded gh pr view "$pr" --json files,headRefOid,labels) \
    || deny "could not read pull request #$pr."
  files=$(jq -r '.files[].path' <<<"$view") \
    || deny "could not read the files of pull request #$pr."

  # Keep in step with the `factory` class in factory/policy.md.
  local factory_re='^(\.claude/|factory/|\.github/|AGENTS\.md$|Justfile$|lefthook\.yml$|deny\.toml$|rust-toolchain\.toml$|flake\.nix$|flake\.lock$|Cargo\.toml$)'
  hits=$(grep -E "$factory_re" <<<"$files" || true)
  [[ -n "$hits" ]] || exit 0

  held="pull request #$pr changes factory paths, which the maintainer merges:
$hits"

  # --- Recorded approval ----------------------------------------------------

  [[ -n "$pinned" ]] || deny "$held
(a recorded approval only counts on a merge pinned with --match-head-commit <sha>)"

  local head
  head=$(jq -r '.headRefOid // empty' <<<"$view")
  [[ -n "$head" ]] || deny "could not read the head of pull request #$pr."
  [[ "$pinned" == "$head" ]] || deny "$held
(--match-head-commit $pinned is not the current head $head)"

  # Roles come from main, not from the checkout, which is usually the pull
  # request's own branch and so could name its own approvers.
  local policy roles roles_json
  policy=$(bounded gh api "repos/{owner}/{repo}/contents/factory/policy.md?ref=main" \
    -H 'Accept: application/vnd.github.raw') || deny "$held
(could not read factory/policy.md on main)"
  roles=$(awk '/^## /{r = ($0 == "## Roles")} r && /^- /' <<<"$policy" \
    | grep -Eo '@[A-Za-z0-9-]+' | tr -d '@' | sort -u || true)
  [[ -n "$roles" ]] || deny "$held
(could not read the Roles in factory/policy.md on main)"
  roles_json=$(jq -R . <<<"$roles" | jq -sc .)

  local repo_dir=${CLAUDE_PROJECT_DIR:-.}

  # Walk first parents from the head. Clean merges of `main` (two parents, the
  # second an ancestor of main, the tree exactly the automatic merge) are
  # collected; the first other commit is the branch's last own commit, which
  # ends the walk.
  local -a after=()
  local own='' own_date='' c=$head info parents first second compare status tree merged automatic
  for _ in $(seq 1 20); do
    info=$(bounded gh api "repos/{owner}/{repo}/commits/$c") || deny "$held
(could not read commit $c)"
    parents=$(jq -r '.parents[].sha' <<<"$info") || deny "$held
(could not read the parents of $c)"
    if [[ $(wc -l <<<"$parents") -eq 2 ]]; then
      first=$(sed -n 1p <<<"$parents")
      second=$(sed -n 2p <<<"$parents")
      compare=$(bounded gh api "repos/{owner}/{repo}/compare/$second...main") || deny "$held
(could not compare $second with main)"
      status=$(jq -r '.status' <<<"$compare") || deny "$held
(could not compare $second with main)"
      if [[ "$status" == ahead || "$status" == identical ]]; then
        tree=$(jq -r '.commit.tree.sha // empty' <<<"$info")
        [[ -n "$tree" ]] || deny "$held
(could not read the tree of $c)"
        merged=0
        automatic=$(bounded git -C "$repo_dir" merge-tree --write-tree "$first" "$second") || merged=$?
        if ((merged > 1)); then
          deny "$held
(could not recompute merge $c locally; fetch the pull request head first)"
        fi
        if ((merged == 0)) && [[ $(head -n1 <<<"$automatic") == "$tree" ]]; then
          after+=("$c")
          c=$first
          continue
        fi
      fi
    fi
    own=$c
    own_date=$(jq -r '.commit.committer.date // empty' <<<"$info")
    break
  done
  [[ -n "$own" && -n "$own_date" ]] || deny "$held
(could not find the branch's last own commit)"
  local candidates
  candidates=$(printf '%s\n' "$own" "${after[@]}" | jq -R . | jq -sc .)

  # Signal 1: a Roles person's latest decisive review approves a candidate.
  # With `dismiss_stale_reviews_on_push`, GitHub may dismiss that review when
  # a merge of main is pushed; a dismissed review no longer counts here.
  local reviews review_ok
  reviews=$(bounded gh api --paginate "repos/{owner}/{repo}/pulls/$pr/reviews") \
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
  # branch's last own commit. Known residual risk: the committer sets that
  # commit's date, so a backdated commit pushed after the label would inherit
  # it; the factory removes the label before every push other than a clean
  # merge of main (references/follow-through.md, step 4) to close that gap.
  local events label_ok
  if jq -e --arg l "$approved_label" 'any(.labels[]?; .name == $l)' <<<"$view" >/dev/null; then
    events=$(bounded gh api --paginate "repos/{owner}/{repo}/issues/$pr/events") \
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
}

for m in "${merges[@]}"; do
  # shellcheck disable=SC2086 # the entry is "<pr> <override> [<pin>]"
  set -- $m
  # errexit is suspended around the subshell only to read its status; the
  # subshell turns it back on for the check itself. Anything but a clean
  # allow denies.
  set +e
  (
    set -e
    check_merge "$@"
  )
  rc=$?
  set -e
  ((rc == 0)) || exit 2
done
exit 0
