# ADR-0020: Provider-Neutral Review Hosts and AgentCore Runtime

## Status

Accepted

## Date

2026-06-25

## Context

`auto_review` began as a single-tenant Forgejo review bot. The gateway, chat
handler, orchestrator, and review pipeline currently share that assumption: they
fetch pull request metadata from Forgejo, clone through Forgejo credentials, and
post reviews, comments, and commit statuses through `ar-forgejo`.

The review behavior is no longer inherently Forgejo-specific. The same
semantic review pipeline should support GitHub repositories without copying the
pipeline or weakening the existing Forgejo deployment path. GitHub support also
needs GitHub App authentication, not personal access tokens, so the runtime can
use short-lived installation access tokens scoped to the selected repository and
permissions.

Operators also need a no-dedicated-server path for CI-triggered semantic review.
ADR-0012 already makes CI the normal readiness signal. AWS Bedrock AgentCore can
host a runtime that CI invokes on demand through an invocation URL instead of
requiring an always-on public gateway for CI review dispatch. Current AgentCore
tooling and runtime examples use authenticated runtime invocation URLs, and the
AWS SDK for Rust uses the default credential provider chain for AWS clients such
as DynamoDB. That makes AgentCore a suitable runtime boundary for synchronous CI
review jobs while DynamoDB provides durable state across cold starts.

## Decision

Split repository-host operations from review behavior.

Introduce a provider-neutral host boundary with shared DTOs and a `ReviewHost`
trait for the operations the review system needs: pull request metadata,
changed files and diffs, compare diffs, review posting, commit status posting,
top-level and inline comments, repository config reads, pull request updates,
and clone/auth material.

Keep `ar-forgejo` as the Forgejo implementation and preserve the existing
gateway service, environment variables, webhook behavior, CI-triggered review
endpoint, chat commands, metrics, and operator docs as a supported deployment
path. Forgejo remains the compatibility baseline while the host boundary is
introduced.

Add `ar-github` as a GitHub implementation that authenticates as a GitHub App.
The GitHub adapter will mint app JWTs, resolve or accept installation IDs,
create short-lived installation access tokens, cache those tokens until near
expiry, and use GitHub REST endpoints for pull request details, files/diffs,
reviews, review comments, issue comments, commit statuses, repository contents,
and webhook signature verification.

Add an AgentCore runtime path as an additional runtime, not as a replacement for
the gateway. The CLI will expose `auto-review agentcore serve`, an HTTP runtime
with `/ping` and `/invocations`. Each invocation receives a provider-neutral
payload containing `provider`, `kind`, `owner`, `repo`, `pr_number`,
`head_sha`, optional `installation_id`, optional `force`, and chat command
fields when `kind=chat_command`. The runtime re-fetches the pull request head
from the selected host and rejects stale invocations before running review
work.

For AgentCore deployments, store review history, durable learnings, and
delivery/request idempotency in DynamoDB-backed stores. Use AWS SDK for Rust
default configuration loading so CI OIDC, web identity, environment, container,
and instance-role credentials can be used by normal AWS provider-chain
configuration. Keep the vector cache in memory for AgentCore v1; external
vector persistence requires a measured need and a later decision.

## Consequences

- Review generation, verification, and filtering can evolve once while Forgejo
  and GitHub adapters own host-specific wire behavior.
- Existing Forgejo gateway deployments remain valid and must stay behaviorally
  compatible during the conversion.
- GitHub support has a smaller credential blast radius than PAT-based support
  because repository access comes from short-lived installation tokens with
  explicit app permissions.
- AgentCore gives CI a no-dedicated-server review path, but it introduces AWS
  IAM, runtime packaging, DynamoDB table, and invocation-payload contracts that
  need deployment docs and tests.
- The AgentCore path must not depend on process-local state for review history,
  learnings, or request idempotency because cold starts and horizontal runtime
  concurrency are normal.
- Webhook ingress for GitHub or AgentCore is deferred. CI invocation and chat
  command invocation are the first supported AgentCore behaviors.
- Metrics, threat model, runbooks, and deployment assets must describe both
  repository providers and both runtime modes as the implementation lands.
