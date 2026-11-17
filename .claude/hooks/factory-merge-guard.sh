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
#   - the `factory:approved` label is on the pull request, its latest
#     application was by a Roles person after the committer date of the
#     branch's last own commit (clean merges of `main` after it do not count),
#     and the branch head at that moment, read from the push times in the
#     repository activity API, was that commit or a clean merge after it.
# A merge of `main` is clean when its tree is exactly what `git merge-tree`
# makes of its parents; one with hand-resolved conflicts or extra edits is an
# own commit and voids an earlier approval. The Roles are read from `main`,
# never from the checkout, so a pull request cannot name its own approvers.
# That approval is the maintainer's instruction for that pull request and that
# head only, not a blanket bypass, so it never allows `gh pr merge --auto` on a
# factory pull request: a scheduled auto-merge stays enabled across later
# pushes, so it could merge a head nobody approved. Every merge in a command is checked on its
# own, with the pin read from that merge's own command segment; GraphQL merge
# mutations, which name no pull request number, are denied. Anything the hook
# cannot read, within its time budget, denies.
#
# Why the push time: the committer sets a commit's date, and a commit made
# before the label was applied can be pushed after it (a rework committed
# before the maintainer labels the old head and pushed later, or a label
# re-applied between the factory's label removal and its push). By committer
# date alone such a commit would inherit the label, which would then approve
# a head the maintainer never saw. Comparing with the head the branch had when
# the label was applied closes that race; the factory also removes the label
# before every push other than a clean merge of main. Known residual risks:
# the activity API keeps a limited history, so an old branch with no recorded
# push before the label denies; and a review that GitHub dismissed on push
# (`dismiss_stale_reviews_on_push`) no longer counts, so the label is the
# approval that survives a refresh.
#
# Because agents act as the maintainer's account, they must never add the
# `factory:approved` label themselves: this hook also blocks commands that add
# it (`gh pr|issue edit --add-label`, `gh pr|issue create --label`, `gh api`
# writes naming it, GraphQL label mutations, issue and label writes whose
# values come from `--input` or a `-F field=@file`, and label names held in
# shell variables, which it cannot check). Labels are read after removing
# shell quoting and compared case-insensitively, as GitHub names labels;
# words may be split by spaces or tabs; commands are read from command
# position behind wrappers such as `nix develop .#ci --command`, `sh -c`,
# `env`, `xargs`, `timeout`, `nice`, `stdbuf`, `setsid`, `ionice`, `nohup`
# (with their flags) and shell keywords; and the command inside `$( )`,
# `( )` or backticks is read as a command of its own, wherever it stands.
#
# For the same reason agents never submit an approving review, which would
# count as a Roles approval on a pull request another account (such as a bot)
# authored: the hook blocks `gh pr review --approve` (or `-a`, alone or in a
# cluster of short flags), `gh api` writes to `pulls/<n>/reviews` (and their
# `/events`) with `event=APPROVE` or fields it cannot read, the GraphQL
# `addPullRequestReview` and `submitPullRequestReview` mutations, and GraphQL
# documents read from a file or stdin.
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

# segments: the command split into the pieces that can each start a command:
# at `;`, `&`, `|` and newlines, and at `(`, `)` and backticks, so the command
# inside `$( )`, `( )` or backticks, wherever it stands (`x=$(gh ...)`,
# `echo $(gh ...)`), reads from command position. A parenthesis inside quoted
# text splits too; that can only make the guard read more segments, so a
# merge whose pin follows such a quote denies rather than slips through.
segments() {
  tr ';&|()`' '\n' <<<"$cmd"
}

# normalize <segment>: drop what can stand before a command (assignments,
# env, command, exec, eval, time, sudo, wrappers that run their arguments
# such as nohup, timeout, nice, stdbuf, setsid, ionice and xargs, with their
# flags, shell keywords such as `then` and `do`, a brace opener, `sh -c` or
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
      s/^(\$|\{|!)//
      s/^(then|do|else|elif|if|while|until|command|exec|eval|time|sudo)([[:space:]]+|$)//
      s/^(nohup|setsid)([[:space:]]+-[^[:space:]]*)*([[:space:]]+|$)//
      s/^env([[:space:]]+(-[uCS][[:space:]]*[^-[:space:]][^[:space:]]*|--[a-z-]+(=[^[:space:]]*)?|-[A-Za-z0]*))*([[:space:]]+|$)//
      s/^timeout([[:space:]]+(-[sk][[:space:]]*[^-[:space:]][^[:space:]]*|--[a-z-]+(=[^[:space:]]*)?|-[A-Za-z]+))*[[:space:]]+[0-9.]+[smhd]?([[:space:]]+|$)//
      s/^nice([[:space:]]+(-n[[:space:]]*-?[0-9]+|--adjustment=-?[0-9]+|-[0-9]+))*([[:space:]]+|$)//
      s/^stdbuf([[:space:]]+(-[ioe][[:space:]]*[^-[:space:]][^[:space:]]*|--(input|output|error)=[^[:space:]]*))*([[:space:]]+|$)//
      s/^ionice([[:space:]]+(-[cnp][[:space:]]*[^-[:space:]][^[:space:]]*|-[tu]|--[a-z-]+(=[^[:space:]]*)?))*([[:space:]]+|$)//
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
  # Bash splits words on tabs too, so every run of blanks reads as one space.
  sed -E 's/[[:space:]]+/ /g; s#^[^ ]*/gh( |$)#gh\1#; s/ (-R|--repo)( +|=)[^ ]+//g' <<<"$s"
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

# merge_target <segment>: the pull request a `gh pr merge` segment names,
# read from its shell words after the first three, skipping the values of the
# flags that take one; empty when none is named.
merge_target() {
  local w skip=0 n
  while IFS= read -r w; do
    if ((skip)); then
      skip=0
      continue
    fi
    case $w in
      -t | --subject | -b | --body | -F | --body-file | -A | --author-email | --match-head-commit)
        skip=1
        continue
        ;;
    esac
    n=$(sed -E 's#^https://github\.com/[^/]+/[^/]+/pull/##; s/^#//' <<<"$w")
    if [[ $n =~ ^[0-9]+$ ]]; then
      echo "$n"
      return
    fi
  done < <(grep -Eo "$word" <<<"$1" | tail -n +4)
}

# A label name the guard cannot read (a variable or a command substitution)
# could be the approval label.
unreadable='[$`]'

# GraphQL label writes name labels by node id, which the guard cannot read.
if grep -Eq '(^|[^A-Za-z])gh[[:space:]]' <<<"$cmd" \
  && grep -Eq 'addLabelsToLabelable|labelIds' <<<"$cmd"; then
  deny "add labels with gh pr edit --add-label, which this guard can check."
fi

# Approval guard: an approving review from the account agents act as would
# read as the maintainer's recorded approval, so agents never submit one.
if grep -Eq '(^|[^A-Za-z])gh[[:space:]]' <<<"$cmd" \
  && grep -Eq '(addPullRequestReview|submitPullRequestReview)([^A-Za-z]|$)' <<<"$cmd"; then
  deny "only a person approves a pull request; agents never submit reviews through GraphQL."
fi

# review_approves <segment>: whether a `gh pr review` segment approves, by
# --approve or by -a alone or inside a cluster of short flags.
review_approves() {
  local w
  while IFS= read -r w; do
    w=$(dequote <<<"$w")
    if [[ $w =~ ^--approve(=.*)?$ || $w =~ ^-[A-Za-z]*a[A-Za-z]*$ ]]; then
      return 0
    fi
  done < <(grep -Eo "$word" <<<"$1" | tail -n +4)
  return 1
}

# api_method <segment>: the HTTP method a `gh api` segment sends: its -X or
# --method, else POST when it sends fields or a body, as gh does, else GET.
api_method() {
  local method
  method=$(grep -Eo -- '(-X|--method)[[:space:]=]*[A-Za-z]+' <<<"$1" | head -n1 \
    | sed -E 's/^(-X|--method)[[:space:]=]*//' | tr '[:lower:]' '[:upper:]' || true)
  if [[ -z "$method" ]]; then
    if grep -Eq -- '(^| )(-f|-F|--field|--raw-field|--input)( |=|$)' <<<"$1"; then
      method=POST
    else
      method=GET
    fi
  fi
  echo "$method"
}

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
    if grep -qiF "$approved_label" <<<"$values"; then
      deny "only the maintainer adds the $approved_label label."
    fi
    if grep -Eq "$unreadable" <<<"$values"; then
      deny "name labels literally, so this guard can check them for $approved_label."
    fi
  fi
  if grep -Eq '^gh +pr +review( |$)' <<<"$segment" && review_approves "$segment"; then
    deny "only a person approves a pull request; agents never submit an approving review."
  fi
  if grep -Eq '^gh +api( |$)' <<<"$segment"; then
    method=$(api_method "$segment")
    if [[ "$method" != GET && "$method" != DELETE ]]; then
      if dequote <<<"$segment" | grep -qiF "$approved_label"; then
        deny "only the maintainer adds the $approved_label label."
      fi
      # An issue write (which can set `labels`) or a label write whose values
      # come from a file, stdin or the shell cannot be checked.
      if grep -Eq "issues/[^[:space:]/]+(/labels)?([[:space:]\"']|\\?|$)" <<<"$segment" \
        && grep -Eq -- "(^| )--input( |=|$)|(^| )(-F|--field)([[:space:]]+|=)[^[:space:]]*=@|$unreadable" <<<"$segment"; then
        deny "name labels literally in issue and label writes, so this guard can check them for $approved_label."
      fi
      # A review write that approves, or whose fields the guard cannot read.
      if grep -Eq 'pulls/[^[:space:]/]+/reviews' <<<"$segment"; then
        if dequote <<<"$segment" | grep -qiE '(^|[ =])event=approve'; then
          deny "only a person approves a pull request; agents never submit an approving review."
        fi
        if grep -Eq -- "(^| )--input( |=|$)|=@|$unreadable" <<<"$segment"; then
          deny "give review fields literally, so this guard can check that they do not approve."
        fi
      fi
      # A GraphQL document read from a file or stdin could hold any mutation.
      if grep -Eq '(^| )graphql( |$)' <<<"$segment" \
        && grep -Eq -- '(^| )--input( |=|$)|query=@' <<<"$segment"; then
        deny "give GraphQL queries inline, so this guard can check them."
      fi
    fi
  fi
done < <(segments)

# Only merge-shaped commands are checked: `gh pr merge` in command position
# (each shell segment, read from command position as the label guard reads
# it), so prose that mentions it, such as a pull request body, does not trip
# the guard; and the GraphQL or REST merge calls anywhere, except a `gh api`
# GET of `pulls/<n>/merge`, which only reads merge status. Every merge in the
# command is checked on its own, with the pin read from its own segment, so
# one approved merge never carries another along.
if grep -Eq 'enablePullRequestAutoMerge|mergePullRequest' <<<"$cmd"; then
  deny "merge with gh pr merge <number> --match-head-commit <sha>, which this guard can check."
fi

merges=()
while IFS= read -r raw; do
  segment=$(normalize "$raw")
  if grep -Eq '^gh +pr +merge( |$)' <<<"$segment"; then
    # The first bare number (or pull URL) among `gh pr merge`'s arguments,
    # skipping the values of its value-taking flags, so the number in
    # `--subject 7 218` is 218, the pull request gh merges.
    pr=$(merge_target "$segment")
    [[ -n "$pr" ]] || deny "name the pull request number explicitly so its files can be checked."
    pin=$(flag_values "$segment" '--match-head-commit' | head -n1 | tr -d "\"'")
    override=0
    if grep -Eq "(^|[[:space:]])FACTORY_MERGE_APPROVED=${pr}([[:space:]]|$)" <<<"${raw%%pr merge*}"; then
      override=1
    fi
    # `--auto` schedules the merge for whenever checks pass, which outlives
    # the head an approval covers.
    auto=0
    if grep -Eq '(^| )--auto(=[^ ]*)?( |$)' <<<"$segment"; then
      auto=1
    fi
    merges+=("$pr $override $auto $pin")
  fi
  # A REST merge, by gh or any other client. A `gh api` GET of the same path
  # only reads whether the pull request was merged.
  if grep -Eq '^gh +api( |$)' <<<"$segment" && [[ $(api_method "$segment") == GET ]]; then
    continue
  fi
  for pr in $(grep -Eo 'pulls/[0-9]+/merge' <<<"$raw" | grep -Eo '[0-9]+' || true); do
    merges+=("$pr 0 0")
  done
done < <(segments)

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

# check_merge <pr> <override> <auto> [<pinned sha>]: exits 0 when the merge
# may run and denies otherwise. Run it in a subshell, once per merge.
check_merge() {
  local pr=$1 override=$2 auto=$3 pinned=${4:-}
  ((override == 0)) || exit 0

  local view files hits held
  view=$(bounded gh pr view "$pr" --json files,headRefOid,headRefName,labels) \
    || deny "could not read pull request #$pr."
  files=$(jq -r '.files[].path' <<<"$view") \
    || deny "could not read the files of pull request #$pr."

  # Keep in step with the `factory` class in factory/policy.md.
  local factory_re='^(\.claude/|factory/|\.github/|AGENTS\.md$|Justfile$|lefthook\.yml$|deny\.toml$|rust-toolchain\.toml$|flake\.nix$|flake\.lock$|Cargo\.toml$|scripts/check-)'
  hits=$(grep -E "$factory_re" <<<"$files" || true)
  [[ -n "$hits" ]] || exit 0

  held="pull request #$pr changes factory paths, which the maintainer merges:
$hits"

  # A scheduled auto-merge stays enabled when someone pushes, and GitHub
  # checks --match-head-commit only when it schedules, so a later head nobody
  # approved could merge. The approval merge sequence waits for the checks and
  # merges at once instead, so it never needs --auto.
  ((auto == 0)) || deny "$held
(a recorded approval covers one head, so it never schedules auto-merge; merge once the required checks pass)"

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
  # branch's last own commit, and the branch head when it was applied was the
  # last own commit or a clean merge of main after it. The committer sets a
  # commit's date, and a commit made before the label can be pushed after it,
  # so the date alone could let the label cover a head the maintainer never
  # saw; the push times GitHub records (the repository activity API) say which
  # head the branch had when the label was applied.
  local events label_at branch activity label_ok
  if jq -e --arg l "$approved_label" 'any(.labels[]?; (.name | ascii_downcase) == $l)' <<<"$view" >/dev/null; then
    events=$(bounded gh api --paginate "repos/{owner}/{repo}/issues/$pr/events") \
      || deny "$held
(could not read the events of #$pr)"
    label_at=$(jq -rs --argjson roles "$roles_json" --arg l "$approved_label" --arg own "$own_date" '
      def t: sub("\\.[0-9]+"; "") | fromdateiso8601;
      add // []
      | map(select(.event == "labeled" and (.label.name | ascii_downcase) == $l))
      | sort_by(.created_at | t) | last
      | if . == null then empty
        elif ((.actor.login as $u | $roles | index($u)) != null) and ((.created_at | t) > ($own | t))
        then .created_at
        else empty
        end
    ' <<<"$events") || deny "$held
(could not parse the events of #$pr)"
    if [[ -n "$label_at" ]]; then
      branch=$(jq -r '.headRefName // empty' <<<"$view")
      [[ -n "$branch" ]] || deny "$held
(could not read the branch of #$pr)"
      activity=$(bounded gh api --paginate \
        "repos/{owner}/{repo}/activity?ref=$(jq -rn --arg r "refs/heads/$branch" '$r | @uri')&per_page=100") \
        || deny "$held
(could not read the push activity of $branch)"
      label_ok=$(jq -s --arg at "$label_at" --argjson cands "$candidates" '
        def t: sub("\\.[0-9]+"; "") | fromdateiso8601;
        add // []
        | map(select((.timestamp | t) < ($at | t)))
        | sort_by(.timestamp | t) | last
        | if . == null then false else (.after as $a | $cands | index($a)) != null end
      ' <<<"$activity") || deny "$held
(could not parse the push activity of $branch)"
      [[ "$label_ok" == true ]] && exit 0
    fi
  fi

  deny "$held
(no recorded maintainer approval covers head $head)"
}

for m in "${merges[@]}"; do
  # shellcheck disable=SC2086 # the entry is "<pr> <override> <auto> [<pin>]"
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
