# ADR-0022: GitHub App Support in the Gateway Service

## Status

Accepted

## Date

2026-11-16

## Context

ADR-0020 introduced the provider-neutral `ReviewHost` boundary and a GitHub App
adapter, but wired GitHub only into the AgentCore runtime and deferred GitHub
webhook ingress. Operators who run the always-on gateway service, the primary
runtime, could not review GitHub repositories with it. They also could not use
GitHub chat commands without building their own invocation plumbing.

The gateway is long-lived and multi-repository. A GitHub App JWT is valid for at
most ten minutes, and installation tokens are scoped per installation and
repository. The gateway therefore cannot reuse the AgentCore pattern of minting
one token per invocation up front.

## Decision

Serve GitHub App repositories from the gateway alongside, or instead of,
Forgejo.

- Add `AppReviewHost`, a long-lived GitHub `ReviewHost` that signs a fresh app
  JWT per app-level call, resolves and caches the installation for each
  repository (seeded from webhook deliveries when available), and mints
  repository-scoped installation tokens on demand.
- Add `POST /webhooks/github`, verified with `X-Hub-Signature-256` and a
  dedicated `GITHUB_WEBHOOK_SECRET` before parsing. It handles `ping`, and
  routes `@<bot>` mentions in `issue_comment` and `pull_request_review_comment`
  deliveries to the shared chat handler. Bot senders are ignored.
- Keep semantic review CI-gated (ADR-0012). `POST /reviews/ci` gains an
  optional `provider` field. `github` requests route to the GitHub host and
  dispatcher; an omitted provider keeps the Forgejo contract. A composite
  GitHub Action in `deploy/github-action` posts the request.
- Make Forgejo configuration optional when a GitHub App is configured. A gateway
  without a Forgejo webhook secret answers `404` on `/webhooks/forgejo`.
- Give each provider its own review dispatcher over shared history, learnings,
  vector, and metrics stores.

## Consequences

- GitHub repositories get the same review pipeline, chat commands, repo config,
  and learnings as Forgejo from one deployable service.
- GitHub credentials stay short-lived and repository-scoped. The long-lived
  secret is the App private key, which must be protected like the Forgejo bot
  token.
- The ADR-0020 deferral of GitHub webhook ingress no longer applies to the
  gateway. AgentCore still has no webhook ingress.
- Review history keys do not include the provider. A gateway serving identically
  named repositories on both forges could compare incremental diffs across
  forges. Operators should avoid that layout until history is provider-keyed.
- The threat model and red-team tests cover the GitHub webhook signature,
  GitHub-only Forgejo route refusal, and bot-sender loop prevention.
