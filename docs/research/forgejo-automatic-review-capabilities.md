# Forgejo capabilities for automatic PR review

This is a source inspection, not a live deployment test. The compatibility
baseline is Forgejo v11; [ADR-0026](../ADR-0026-probe-host-capabilities-and-degrade-on-older-forgejo.md)
explains why we do not assume the newest Forgejo API.

## Webhook lifecycle

The [v11](https://codeberg.org/forgejo/forgejo/src/tag/v11.0.0/services/webhook/notifier.go)
and [v17](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/services/webhook/notifier.go)
notifiers send PR deliveries on open, reopen, and synchronization. Both also
send an `edited` PR payload with `changes.title.from` when the title changes.
Forgejo derives draft state from a title prefix (see `IsWorkInProgress` in
[v11](https://codeberg.org/forgejo/forgejo/src/tag/v11.0.0/models/issues/pull.go)
and [v17](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/models/issues/pull.go)).
A title edit can therefore be the draft-to-ready notification. Treat it as a
candidate only; re-fetch the live PR to verify draft, open state, and head.
Do not require GitHub's `ready_for_review` delivery or guess an instance's
configured draft prefixes.

## Check results

The published OpenAPI templates for
[v11](https://codeberg.org/forgejo/forgejo/src/tag/v11.0.0/templates/swagger/v1_json.tmpl),
[v12](https://codeberg.org/forgejo/forgejo/src/tag/v12.0.0/templates/swagger/v1_json.tmpl),
[v13](https://codeberg.org/forgejo/forgejo/src/tag/v13.0.0/templates/swagger/v1_json.tmpl),
[v14](https://codeberg.org/forgejo/forgejo/src/tag/v14.0.0/templates/swagger/v1_json.tmpl),
[v15](https://codeberg.org/forgejo/forgejo/src/tag/v15.0.0/templates/swagger/v1_json.tmpl),
[v16](https://codeberg.org/forgejo/forgejo/src/tag/v16.0.0/templates/swagger/v1_json.tmpl),
and [v17](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/templates/swagger/v1_json.tmpl)
all expose `GET /api/v1/repos/{owner}/{repo}/statuses/{sha}`. The Actions run
listing is absent in v11 and the run-jobs listing is absent through v15.
The gateway requires both endpoints for native named gates. No released
v18.0.0 tag was available for this comparison.

The Actions status writers in
[v11](https://codeberg.org/forgejo/forgejo/src/tag/v11.0.0/services/actions/commit_status.go)
and [v17](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/services/actions/commit_status.go)
use the PR payload's **head SHA** for PR-event jobs, but write to the run's
repository (the base repository for the base repository's PR workflows).
This works for fork PRs without trusting statuses written to the fork. The
context is `Workflow / Job (pull_request)` for PR workflows, with different
suffixes for push and `pull_request_target`; use the exact exposed context,
not an inferred job name. External CI may post its own context. Waiting,
blocked, and running jobs publish pending; v11 maps skipped to success, while
v17 exposes skipped separately. Both should satisfy `pass` in auto_review.

Read the base repository's statuses for the confirmed head and take the
latest entry per exact context, with `sort=leastindex` (descending index) and pagination (see
[v11](https://codeberg.org/forgejo/forgejo/src/tag/v11.0.0/models/git/commit_status.go)
and [v17](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/models/git/commit_status.go)).
A newer pending publication for a rerun supersedes the earlier success. A
commit status does not identify the PR or job attempt and cannot distinguish
jobs that share a context. More importantly, the native Actions status writer
uses the payload head even though the [synchronize notifier](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/services/actions/notifier.go#L738)
passes the captured push commit to the run separately. The [payload converter](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/services/convert/pull.go#L182)
can read the live head ref. If the branch advances between those reads, a
status on the current head is not proof that the Actions run used that head.
Never substitute a fork's own statuses for a missing base-repository signal.

v16+ can instead inspect [the current jobs of an identified run](https://codeberg.org/forgejo/forgejo/src/tag/v16.0.0/routers/api/v1/repo/action.go#L1268)
using the base repo, exact run `commit_sha`, and PR identity from the event
payload. Reruns reset job state and increment the
[attempt](https://codeberg.org/forgejo/forgejo/src/tag/v17.0.0/models/actions/run_job.go#L153)
while keeping run and job IDs. v11–v15 lack that job-level evidence in the
public API. On those versions, keep the existing CI callback when review
must wait for CI, or enable webhook mode without named gates for immediate
review. Commit statuses are not substituted for verified run/job evidence.
