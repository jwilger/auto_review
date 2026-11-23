# ADR-0028: Trigger GitHub App reviews from webhooks

## Status

Accepted. Updates the GitHub trigger in
[ADR-0022](./ADR-0022-github-app-support-in-the-gateway.md); retains the cost
rationale in [ADR-0012](./ADR-0012-gate-semantic-review-behind-ci.md).

## Decision

The GitHub App's signed `pull_request` deliveries start automatic reviews.
The gateway waits for visible GitHub check runs and commit statuses on the
PR head and, when available, its synthetic merge commit. Pending or failing
checks and failed reads block dispatch; a complete empty result allows it.
Ignore the App's own review status so prior review outcomes cannot block CI
readiness. Explicit chat re-review remains an override.

Use a short settling period and a bounded in-memory retry loop. A PR delivery
can precede check registration, and polling does not depend on GitHub linking
check deliveries to the PR (notably for forks). No finite wait can distinguish
a repository with no checks from a check that has not registered yet. A
restart loses pending watches, and checks that finish outside the watch window
need a new PR event or an explicit re-review.

Legacy authenticated GitHub CI requests are acknowledged without dispatch.
Forgejo CI and AgentCore invocation paths do not change. See the
[GitHub setup and migration guide](./GITHUB.md) for App configuration.
