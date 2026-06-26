# GitHub + AWS AgentCore Conversion Plan

## Summary

Complete this plan for auto-review-agent-core: preserve Forgejo behavior, add GitHub App repository support, and add AWS Bedrock AgentCore CI-invoked runtime without requiring a dedicated server; use repo RGR discipline and stop only after focused verification and docs are updated.

## Key Changes

- Add an ADR before behavior work: change the architecture from Forgejo-only gateway coupling to a provider-neutral review core with Forgejo and GitHub adapters, plus AgentCore invocation as a non-server runtime path.
- Add a new common host crate, tentatively `ar-forge`, with shared DTOs and a `ReviewHost` trait for PR fetch, diff/files, compare diff, review posting, statuses, comments, repo config reads, PR updates, and clone URL/auth material.
- Keep `ar-forgejo` as the Forgejo implementation and add `ar-github` using GitHub App auth: app JWT, installation lookup/token creation, token caching, REST PR review/status/comment endpoints, and GitHub webhook signature parsing.
- Refactor `ar-review`, `ar-orchestrator`, `ar-chat`, and `ar-gateway` to depend on `ReviewHost` instead of `ar_forgejo::Client`; preserve existing Forgejo env vars and behavior.
- Add `auto-review agentcore serve`, an AgentCore-compatible HTTP runtime with `/ping` and `/invocations`; each invocation runs one CI review or chat command synchronously and returns structured JSON.
- Add provider-neutral AgentCore invocation payloads:
  `provider`, `kind`, `owner`, `repo`, `pr_number`, `head_sha`, optional `installation_id`, optional `force`, and chat fields when `kind=chat_command`.
- Use DynamoDB-backed implementations for AgentCore review history, learnings, and request/delivery idempotency. Keep vector cache in-memory for AgentCore v1 unless a later measured need justifies external vector storage.
- Add deployment assets under `deploy/agentcore/`: container build, IAM policy notes, AgentCore runtime config, GitHub Actions OIDC invocation example, and Forgejo Actions invocation example.
- Keep the existing dedicated gateway as a supported Forgejo deployment path; AgentCore becomes the no-dedicated-server path.

## RGR Shape

Behavior checklist:

- Existing Forgejo CI-triggered review still works unchanged.
- GitHub App can review PRs, post inline reviews, post commit statuses, read comments/config, and clone safely.
- AgentCore invocation can replace the always-on gateway for CI-triggered semantic review.
- AgentCore state survives cold starts for review history, learnings, and idempotency.
- Docs, threat model, metrics, and operator runbooks reflect both providers and both runtimes.

Active Cycle 1 only:

1. RED: add a focused `ar-review` contract test using a fake `ReviewHost` that proves the review pipeline posts through the trait instead of `ar_forgejo::Client`; run `cargo test -p ar-review <test_name>`.
2. GREEN: introduce the minimum common trait/DTO seam and adapt Forgejo through it until that one test passes.
3. REFACTOR: move only the stable common DTOs into `ar-forge`, then run the focused test again and commit the green checkpoint.

Choose the next smallest behavior from the checklist only after Cycle 1 is green and reviewed.

## Test Plan

- Unit and wiremock tests for Forgejo adapter parity, GitHub App auth/token flow, GitHub diff/files/review/status/comment endpoints, and both webhook signature formats.
- Orchestrator/review/chat tests with fake `ReviewHost` to prevent provider-specific coupling from returning.
- AgentCore HTTP tests for `/ping`, valid `/invocations`, stale head rejection, idempotent duplicate handling, and structured error responses.
- DynamoDB-backed store tests behind local fakes for key shape, conditional put, TTL fields, and trait behavior; keep live AWS tests manual/documented.
- Contract tests for new deploy action snippets and docs that are executable/public contracts.
- Final verification: focused cycle commands as work proceeds, then `just fmt`, `just clippy`, `just test`, `just build`; run `just codex-test` only if `.codex/**`, `.agents/**`, `scripts/codex/**`, or `tests/codex/**` changes.

## Assumptions

- Defaults chosen because no clarification response arrived: CI invokes AgentCore directly; webhook ingress is follow-up, not required for v1.
- GitHub support uses GitHub App auth, not PATs.
- Existing OpenAI-compatible LLM routing remains; Bedrock model/provider support is not required for this conversion.
- Current-doc basis: AWS AgentCore Starter Toolkit/Runtime docs, AWS SDK for Rust default config docs, DynamoDB conditional write/TTL docs, and GitHub REST App/review/status docs.
