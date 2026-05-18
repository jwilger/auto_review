<!--
Pull-request template for `auto_review`. Forgejo / Gitea look for
.forgejo/pull_request_template.md when you open a PR; the body of
this file becomes the default PR description.

Delete the comment block once you've filled the rest in.
-->

## Why this change

<!-- One paragraph. What risk/need does this solve? -->
<!-- If this PR resolves an issue, `See issue #<issue-number>` is acceptable. -->

## What changed

<!-- Bullet list. Include all work from the branch, not just the most recent change. -->

## Scope of this PR

<!-- Enumerate every behavioral/doc/process area this PR changes. -->

## Consequences

<!-- Risks, tradeoffs, or follow-up work. -->

## Type of change

<!-- Tick all that apply. -->

- [ ] feat — new capability
- [ ] fix — bug fix
- [ ] docs — documentation only
- [ ] refactor — internal cleanup, no behaviour change
- [ ] test — test-only addition
- [ ] chore — tooling, deps, CI

## Verification

<!-- Tick what you did. -->

- [ ] `just test`
- [ ] `just opencode-test` (when changing `.opencode/` harness/plugins)
- [ ] `just clippy`
- [ ] `just fmt`
- [ ] `just deny` (when bumping dependencies)
- [ ] Manual smoke test against a dev gateway (when changing the
      review pipeline or webhook surface)

## Pre-merge checklist

<!-- Human-side release/readiness checklist. Project-enforced checks
belong in CI/static analysis jobs. -->

- [ ] Commit titles follow conventional commits; the release PR generates changelog notes from conventional commits
- [ ] Public surface changes have rustdoc on the new items
- [ ] If the change touches a documented threat (T#) in
      `docs/THREAT-MODEL.md`, the corresponding red-team test
      in `crates/ar-review/tests/red_team_*.rs` has been
      updated or extended
- [ ] If the change touches a metric, the rules pack
      (`deploy/prometheus/auto_review.rules.yaml`) and dashboard
      (`deploy/grafana/auto_review.dashboard.json`) still pass
      their contract tests
- [ ] Architecture changes use the ADR workflow tools: proposed ADRs may be edited, accepted/rejected ADRs are immutable except supersession metadata, and `docs/ARCHITECTURE.md` is updated as the current projection

## Related

<!-- Link to issue / RFC / ADR if applicable. -->
