# ADR 0029: Share automatic review policy across forges

## Status

Accepted.

## Context

[ADR 0028](./ADR-0028-trigger-github-app-reviews-from-webhooks.md) moved
GitHub App reviews to signed PR webhooks. Forgejo still depended on a CI
callback. Requiring a callback for every Forgejo repository adds setup and
can exclude fork PRs whose workflows cannot access repository secrets.
Changing the default for existing Forgejo repositories would bypass their
chosen CI prerequisites.

## Decision

Use `automatic_review` on the trusted PR base branch for draft admission
and exact named checks on both hosts. Continue accepting
`github_automatic_review` for GitHub when the shared section is absent.
Forgejo webhook admission requires explicit `forgejo_trigger: webhook`;
recommend it for new setups. In that mode an authenticated, otherwise
eligible CI callback is a no-op. Both webhook paths use live PR eligibility
and bounded polling only when named checks are configured.

On Forgejo, native Actions gates require the v16+ run-jobs API. Verify the
run's captured SHA and PR identity in the base repository, then use current
job states. Commit statuses alone do not establish the exact native Actions
run revision and attempt. See the [capability audit](./research/forgejo-automatic-review-capabilities.md).
Older servers can use immediate webhook review or keep CI callbacks for
prerequisites. Missing or unreadable evidence never bypasses a gate.

## Consequences

No automatic migration or change to manual reviews or merge policy. Forgejo
draft removal is a title edit; qualifying title edits are candidates, not
proof of eligibility. The gateway re-fetches the PR and relies on ordinary
same-revision review deduplication after admission. Pending status updates
are best effort. Watches are in memory, so a restart can leave a pending
status until a review replaces it. Status APIs cannot atomically compare and
replace a status across separate gateway instances or external writers.
Likewise, polling current Actions jobs is not an atomic snapshot across
runs. A watch uses the requirements it started with; both forges recheck the
trusted policy at worker admission and discard the job if it changed.

An unavailable Forgejo config must not disable an existing CI setup by
itself. The gateway remembers observed webhook opt-ins in process and
suppresses callbacks for those branches during an outage. If that bounded
history fills, it does not suppress unrelated callbacks: additional opt-ins
are not remembered. Without a remembered observation, the gateway cannot
distinguish an unreadable opt-in from an existing CI setup.

The maintainer chose to preserve legacy callbacks during config outages
rather than fail closed for all repositories or require durable registration.
The supported cutover is to disable the old CI callback and drain any
in-flight callback jobs **before** enabling webhook mode. This prevents old
workflow callbacks from duplicating reviews after cutover. It is not a
server-side guarantee against a callback that remains enabled: after a
restart with unreadable config, the gateway may accept that callback.
