#!/usr/bin/env bash
# PreToolUse guard: agents never merge, or schedule the merge of, a pull
# request that touches a `factory` path from factory/policy.md. The maintainer
# merges those. Agents run as the maintainer's own GitHub account, so GitHub
# cannot enforce this. This hook is a tripwire against the factory's own
# scripted mistakes, not a security boundary: it does not try to read every
# way a shell command could merge, and anything running as the maintainer's
# account can still merge. The enforceable boundary is a separate factory
# identity that cannot merge factory paths, tracked in its own issue.
#
# Override, only on the maintainer's explicit instruction in the session:
# prefix the command with FACTORY_MERGE_APPROVED=<pr number>.
set -euo pipefail

cmd=$(jq -r '.tool_input.command // ""')

# Only merge-shaped commands are checked: `gh pr merge` in command position
# (line start or after ; & | ( , optionally behind VAR=value assignments), so
# prose that mentions it, such as a pull request body, does not trip the
# guard; and the GraphQL or REST merge calls anywhere.
cmd_pos='(^|[;&|(])[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*'
if ! grep -Eq "${cmd_pos}gh +pr +merge|enablePullRequestAutoMerge|mergePullRequest|pulls/[0-9]+/merge" <<<"$cmd"; then
  exit 0
fi

deny() {
  echo "Blocked by factory merge guard: $1" >&2
  exit 2
}

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

files=$(gh pr view "$pr" --json files --jq '.files[].path') \
  || deny "could not read the files of pull request #$pr."

# Keep in step with the `factory` class in factory/policy.md.
factory_re='^(\.claude/|factory/|\.github/|AGENTS\.md$|Justfile$|lefthook\.yml$|deny\.toml$|rust-toolchain\.toml$|flake\.nix$|flake\.lock$|Cargo\.toml$)'
hits=$(grep -E "$factory_re" <<<"$files" || true)
if [[ -n "$hits" ]]; then
  deny "pull request #$pr changes factory paths, which the maintainer merges:
$hits"
fi
exit 0
