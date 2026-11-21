# Engineering standards

**Must** marks a requirement; **prefer** marks a default with room for a
better fit. Explain meaningful exceptions in the PR and, where useful to
future readers, next to the code. See [Contributing](../CONTRIBUTING.md) for
commands, toolchain setup, and required checks.

## Behavioral evidence

- **Must:** For a user-visible behavior change, add a regression through the
  affected public interface (for example, a CLI command, gateway route, or
  published action). For any behavior change, confirm one focused test fails
  for the intended reason, make the smallest production edit, then confirm the
  test passes. Split multiple failures into separate cycles.
- **Must:** When a threat mitigation or its expected behavior changes, update
  the matching regression listed in the
  [threat model's test map](THREAT-MODEL.md#test-coverage-of-these-threats).
  Assert the relevant observable result: for example, a denied request must
  not dispatch a review. Don't assert exact wording for prose-only changes.
- **Prefer:** Exercise the real interface, fake external forges and model
  providers, and use small unit tests for pure decisions. Choose the lowest
  level that still proves the behavior. Use a process-level test when startup,
  packaging, or environment wiring is what changed.

## Architecture and trust boundaries

- **Must:** Validate untrusted input before it reaches decisions or side
  effects. Keep authorization separate from parsing. Load repository review
  instructions from the base branch, not the PR head; validate structured
  model review output against the closed schemas before mapping it to findings.
- **Must:** Review workspace tools stay read-only and confined to the clone.
  The review runtime never executes repository-controlled tools, scripts, or
  commands; the reviewed repository's CI owns its linters, builds, and tests.
- **Must:** Keep shared review orchestration forge-neutral. Put provider-specific
  behavior behind `ReviewHost` or at the adapter boundary. See the
  [crate map](CRATES.md) before changing an interface. For an architectural
  decision, add or supersede an ADR and update the
  [architecture projection](ARCHITECTURE.md); don't rewrite a decided ADR.
- **Prefer:** Use types that distinguish roles when swapping two values would
  be a bug. Keep decisions pure where practical, put I/O at the edges, and use
  explicit error paths that stop dependent work on failure.

## Checks and review

- **Must:** Use `just ci` for routine verification and keep the required
  hosted checks green. Follow the existing signed-commit and PR process.
- **Must:** New Rust crates inherit the [workspace lint policy](../Cargo.toml).
  Set `workspace = true` under `[lints]` in the crate manifest. Fix warnings
  rather than broadly suppressing them; scope each exception narrowly and
  record why it is needed.
- **Prefer:** Enforce objective rules with focused, reproducible checks that
  include a passing case and a representative failure. Measure warm-cache CI
  time before adding expensive required checks; aim for ten minutes.
- Semantic review complements deterministic checks and is not a required
  check for this repository. The [self-review rubric](../.auto_review.yaml) is
  a short summary for the reviewer, not a second policy. A finding should
  point to a changed location, name the applicable rule, and explain the
  concrete consequence. A preference alone is not a defect; missing evidence
  should be described as missing evidence, not as proof of failure.
