# ADR-0028: Trigger GitHub App reviews from webhooks

## Status

Accepted. Updates the GitHub trigger in
[ADR-0022](./ADR-0022-github-app-support-in-the-gateway.md). Repositories can
opt into CI gating for the cost rationale in
[ADR-0012](./ADR-0012-gate-semantic-review-behind-ci.md).

## Decision

The GitHub App's signed `pull_request` deliveries start automatic reviews.
Review open non-draft PRs without waiting for CI by default. A repository may
opt into draft reviews or require named checks to complete or pass in the
trusted base-branch config. Only the configured names on the current head or
synthetic merge commit count; a missing name keeps the gate closed. The App's
own status never qualifies. Explicit chat re-review bypasses automatic admission.

For configured waits, use a short settling period and a bounded in-memory
retry loop. A PR delivery can precede check registration, and polling does not
depend on GitHub linking check deliveries to the PR (notably for forks).
Requiring names avoids having to guess whether an empty result means checks
have not registered or the repository has no CI. A restart loses pending
watches, and checks that finish outside the watch window need a new PR event
or an explicit re-review.

Legacy authenticated GitHub CI requests are acknowledged without dispatch.
Forgejo CI and AgentCore invocation paths do not change. See the
[GitHub setup and migration guide](./GITHUB.md) for App configuration.
