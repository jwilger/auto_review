<!--
Pull-request template for `auto_review`. Forgejo / Gitea look for
.forgejo/pull_request_template.md when you open a PR; the body of
this file becomes the default PR description.

Delete the comment block once you've filled the rest in.
-->

## Summary

<!-- One paragraph. What does this change? Why? -->

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

- [ ] `cargo test --workspace --all-targets`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo deny check` (when bumping dependencies)
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

## Related

<!-- Link to issue / RFC / ADR if applicable. -->
