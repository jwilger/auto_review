#!/usr/bin/env bash
# PreToolUse hook for Bash. Fires when the command looks like a
# top-level PR comment (`tea comment <n>` or `gh pr comment <n>`)
# and reminds the agent that inline review feedback must be
# answered ON THE THREADS first, not via a single top-level
# comment.
#
# Advisory only (exit 0). The reminder lands in the agent's
# context as a system message.

set -uo pipefail

input="$(cat)"

tool_name="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("tool_name", ""))
except Exception:
    pass
' 2>/dev/null)"

if [ "$tool_name" != "Bash" ]; then
    exit 0
fi

command="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    print((json.load(sys.stdin).get("tool_input") or {}).get("command", ""))
except Exception:
    pass
' 2>/dev/null)"

# Match `tea comment <num>` and `gh pr comment <num>` patterns.
case "$command" in
    *"tea comment "[0-9]*|*"gh pr comment "[0-9]*) ;;
    *) exit 0 ;;
esac

cat >&2 <<'EOF'
[REVIEW REPLY PROTOCOL — verified 2026-05-01 against Forgejo 15.0.0]

You are about to post a top-level PR comment. If this is a reply
to inline review feedback (line/file-anchored review comments),
STOP and reply on the threads first.

The recipe (Forgejo / Gitea):

  1. List the bot's review and its inline comments:
         GET  /api/v1/repos/{o}/{r}/pulls/{n}/reviews
         GET  /api/v1/repos/{o}/{r}/pulls/{n}/reviews/{review_id}/comments

  2. For each comment in that list, READ THE `position` FIELD
     from the JSON. Do NOT use the line numbers that appear in
     `comment.body` text ("Lines 146–150" etc.) — those are
     human-display line numbers that the comment's prose
     mentions; the Forgejo data model stores `Line` (called
     `position` in the API response) which may be the diff-time
     position number, NOT the file line. Forgejo threads by
     (review_id, path, line) — mirroring `position` is the
     thread-join key.

  3. POST one reply per comment INTO THE SAME REVIEW:
         POST /api/v1/repos/{o}/{r}/pulls/{n}/reviews/{review_id}/comments
         {
           "body":         "<your reply>",
           "path":         "<comment.path>",
           "new_position": <comment.position>,
           "old_position": 0
         }
     With Authorization: token <PAT>. Each reply states (a)
     what you did or why you're declining, and (b) the
     resolving commit SHA when applicable.

  4. Optionally add a top-level summary comment AFTER the
     per-thread replies. Top-level alone is not a substitute.

What does NOT work:

  ✗ POST /pulls/{n}/reviews — creates a SEPARATE review object
    that shows as a sibling to the original, never threaded.
  ✗ POSTing to the right review at the WRONG `new_position`
    (e.g. the file-display line in the comment prose) — lands
    in the same review but at a different (path, line), which
    the UI shows as an unrelated thread.
  ✗ The Web form route /{o}/{r}/pulls/{n}/files/reviews/comments
    with `reply=<id>` — works for human users via the UI, but
    requires a session cookie. Forgejo's `legacySkipFormAndBearer`
    rejects token auth on this route. Don't probe it; the
    security guardrails will (correctly) flag it as CSRF
    bypass scouting.
EOF

exit 0
