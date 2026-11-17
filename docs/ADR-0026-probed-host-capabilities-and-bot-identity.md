# ADR-0026: Probed Host Capabilities and Bot Identity

## Status

Accepted

## Date

2026-11-17

## Context

ADR-0020 put every repository-host operation behind the provider-neutral
`ReviewHost` trait, and ADR-0022 added a long-lived GitHub App host to the
gateway. Both assumed that every supported host offers the same review
features. That no longer holds.

GitHub supports multi-line review comments, `suggestion` blocks, replies in a
review thread, and resolving a review thread. Forgejo added multi-line review
comments in v16.0.0 (Forgejo PR #12582). Suggestion blocks, thread replies,
and thread resolve are merged for v18.0.0 (Forgejo PRs #13023, #14850 and
#14873) and are missing from v17 and earlier. Operators run a range of Forgejo
releases, from v11 upwards, and upgrade on their own schedule.

Planned review-output features (real line ranges, suggestions, marking
findings as addressed in their threads) must therefore learn what the
connected host supports instead of assuming it. They also need the bot's own
login inside the review pipeline to find the bot's own reviews and comments.
Before this decision the login existed only in the gateway and chat
configuration (`AR_BOT_LOGIN`).

## Decision

Probe host capabilities and the bot identity through `ReviewHost`.

- `ReviewHost::capabilities()` returns `HostCapabilities { multi_line_comments,
  suggestion_blocks, thread_replies, resolve_threads }`. The default
  implementation returns all false, so a host that does not report its
  capabilities gets the most conservative output.
- The GitHub hosts (`GitHubReviewHost` and the gateway's `AppReviewHost`)
  report all four capabilities.
- The Forgejo host reads `GET /api/v1/version` once per client and caches the
  result. A pure parser takes the leading major version from strings such as
  `16.0.7` or `17.0.0-100-2254cb0+gitea-1.22.0`. Major version 16 or later
  enables multi-line comments; major version 18 or later also enables
  suggestion blocks, thread replies and thread resolve. A failed request or an
  unparseable version gives all false and logs a warning; it never fails the
  review.
- `ReviewHost::bot_login()` returns the login the host authenticates as:
  Forgejo through `GET /api/v1/user`, GitHub as `<app slug>[bot]` through
  `GET /app`. The value is cached per host. Hosts the gateway builds are
  seeded with the identity resolved at startup, so an explicit `AR_BOT_LOGIN`
  still wins over the probed value.
- Pipeline and orchestrator code sees only `HostCapabilities` and the login
  string. Version strings and host names stay inside `ar-forgejo` and
  `ar-github`, as ADR-0020 requires.

Features that need a missing capability degrade rather than fail. On Forgejo
v11 to v17:

- a suggested change is posted as a plain fenced code block instead of a
  `suggestion` block;
- a finding that has been addressed is marked by editing the original bot
  comment, instead of replying in its thread and resolving the thread;
- on v11 to v15, a comment that spans several lines is anchored to a single
  line.

## Consequences

- One review pipeline serves every supported Forgejo release and GitHub. New
  output features turn on automatically when an operator upgrades Forgejo, with
  no configuration change.
- A Forgejo host costs one extra `GET /api/v1/version` request and one
  `GET /api/v1/user` request per client lifetime. A GitHub host costs one
  `GET /app` request per host lifetime.
- A failed version probe produces the plainest output until the client is
  rebuilt. This trades richer output for never breaking a review on a probe
  error.
- Forgejo forks or builds that report a non-Forgejo version string are treated
  as having no optional capabilities.
- Self-detection and "find my own comments" logic share one source of truth
  per host, while `AR_BOT_LOGIN` remains the operator override for the gateway.
- New `ReviewHost` methods follow the existing pattern of a default
  implementation, so test fakes keep compiling.
