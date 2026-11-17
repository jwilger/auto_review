# ADR-0026: Probe Host Capabilities and Degrade on Older Forgejo

## Status

Accepted

## Date

2026-11-17

## Context

Planned review features need four review-thread operations from the repository
host: multi-line review comments, suggestion blocks, replies to review threads,
and resolving review threads. GitHub supports all four. Forgejo added
multi-line review comments in v16.0.0 (Forgejo PR #12582). Suggestion blocks,
thread replies and thread resolve are merged for v18.0.0 (Forgejo PRs #13023,
#14850 and #14873) and are missing from v17 and earlier. Operators run a range
of Forgejo releases, so features cannot assume the newest API.

Those features also need the bot's own login inside the review pipeline, to
find the bot's earlier reviews and comments. Before this decision the login was
resolved only by the gateway and chat code.

ADR-0020 keeps forge-specific details behind the provider-neutral `ReviewHost`
boundary, so pipeline and orchestrator code must not branch on host type or
version strings.

## Decision

Probe what the connected host supports instead of assuming it, and expose the
result through `ReviewHost`.

- `ReviewHost::capabilities()` returns `HostCapabilities { multi_line_comments,
  suggestion_blocks, thread_replies, resolve_threads }`. The trait default
  reports none of them, so test fakes and new adapters opt in explicitly.
- GitHub hosts report all four.
- The Forgejo adapter reads `GET /api/v1/version` once per client and caches
  the result. The leading numeric major component decides: ≥16 enables
  multi-line comments; ≥18 enables the other three. A failed probe or an
  unparseable version reports none of them and logs a warning.
- `ReviewHost::bot_login()` returns the bot's login, cached per host: Forgejo
  through `GET /api/v1/user`, GitHub as `<app slug>[bot]` through `GET /app`.
  Hosts built by the gateway are seeded with the identity resolved at startup,
  so `AR_BOT_LOGIN` and `GITHUB_BOT_LOGIN` still win.
- Pipeline and orchestrator code see only `HostCapabilities` and the login,
  never the host version.

Features degrade on Forgejo v11–v17 rather than being refused:

- Suggested changes render as a plain fenced code block instead of a
  suggestion block.
- A finding that has been addressed is marked by editing the original bot
  comment instead of replying to and resolving its thread.
- Without multi-line comments (Forgejo before v16), a finding spanning several
  lines is anchored to a single line.

## Consequences

- One code path serves every supported Forgejo release and GitHub; reviewers on
  older Forgejo get the same findings with less interactive presentation.
- Each Forgejo client makes at most one version request and at most one user
  request for its lifetime. Upgrading Forgejo takes effect when auto_review
  restarts or builds a new client.
- A Forgejo instance whose version endpoint is unreachable or reports a
  non-standard string loses the optional features until the probe succeeds on a
  new client; the warning log tells operators why.
- New capability flags extend `HostCapabilities` and its per-version table in
  `docs/CRATES.md` and `docs/DEPLOYMENT.md`.
