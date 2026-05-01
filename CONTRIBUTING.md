# Contributing

Thanks for considering a contribution to `auto_review`.

## Development setup

### Prerequisites

- Rust toolchain. The repo pins the channel via `rust-toolchain.toml`
  to `stable`; rustup will install it on first build.
- `git`, plus any of the linter binaries you want exercised end-to-end
  (`ruff`, `eslint`, `shellcheck`, `hadolint`, `markdownlint`,
  `gitleaks`, `actionlint`). Missing linters are silently skipped at
  runtime, so absence isn't a build blocker.

### First build

```sh
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs the same four checks (see `.forgejo/workflows/ci.yml`). Land
no commit that fails any of them.

## Workflow

1. Open an issue first for anything beyond a trivial fix. Architectural
   choices benefit from discussion in the open.
2. Write tests first. The repo follows TDD discipline (red → green →
   refactor) and `tdd:test-driven-development` is the preferred
   approach for new features and bugfixes.
3. Keep commits small and focused. Each commit should leave the tree
   green (tests pass, clippy clean, fmt clean).
4. Mention rationale in the commit body, not just the what. The
   project's history doubles as design documentation; future
   contributors (including future you) will be grateful.

## Testing approach

- **Unit tests** for pure functions live alongside the code in
  `#[cfg(test)] mod tests` blocks. Prefer `#[test]` for sync code and
  `#[tokio::test]` for async.
- **HTTP integration tests** mock Forgejo via `wiremock`. See
  `crates/ar-forgejo/src/client.rs` for the canonical pattern.
- **LLM integration tests** use a `CannedProvider` (or
  `ScriptedProvider`) that implements `LlmProvider` with a vec of
  pre-recorded responses. See `crates/ar-review/src/heal.rs` and
  `crates/ar-review/src/pipeline.rs` for examples.
- **End-to-end behaviour** that depends on real `git`, real Forgejo,
  or real LLMs is exercised via `auto_review review-once` and is
  not currently covered by automated tests.

## Architecture overview

See `docs/ADR-0001-architecture.md` for the high-level decision and
`docs/FEASIBILITY.md` for the longer reasoning. The crate layout:

| Crate | Responsibility |
|---|---|
| `ar-gateway` | HTTP server, HMAC verification, webhook intake |
| `ar-orchestrator` | JobDispatcher trait + production SpawningDispatcher |
| `ar-forgejo` | REST client + InitClient for HTTP-Basic bootstrap |
| `ar-llm` | Provider trait + tier-based Router |
| `ar-prompts` | Prompt templates + JSON schemas + validation |
| `ar-review` | Pipeline activities (clone, lint, review, self-heal) |
| `ar-tools` | Static-analysis runners + result parsers |
| `ar-cli` | Operator CLI (`init`, `register-webhook`, `review-once`) |
| `ar-sandbox` | OCI sandbox launcher (Milestone 3, currently stub) |
| `ar-chat` | Agentic `@auto_review` chat handler (Milestone 4, currently stub) |
| `ar-index` | RAG index (Milestone 2, currently stub) |

## Adding a new linter

1. Pick a JSON output format the binary supports (most do).
2. Create `crates/ar-tools/src/<tool>.rs`:
   - `parse_<tool>_output(json: &str) -> Result<Vec<Finding>, RunnerError>`
   - `<Tool>Runner` implementing `LinterRunner`
3. Add the module to `crates/ar-tools/src/lib.rs`.
4. Wire routing in `crates/ar-review/src/routing.rs::select_runners`.
5. Add tests covering parser happy-path and at least one error/edge case.

The existing six-and-counting linters in `crates/ar-tools/src/` are the
template.

## Commit messages

The history follows `feat(scope): summary` / `fix(scope): summary` /
`docs: summary` / `chore: summary` shape. Keep summaries imperative
("add X", "fix Y") and include a body explaining the why. Sign-off
trailers and `Co-Authored-By:` lines are welcome.

## License

By submitting a contribution you agree to license it under the
AGPL-3.0-or-later (see [LICENSE](./LICENSE)). The intent of the
copyleft is documented there.
