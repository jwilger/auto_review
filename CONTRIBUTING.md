# Contributing

Thanks for considering a contribution to `auto_review`.

## Development setup

### Prerequisites

- Rust toolchain. The repo pins the channel via `rust-toolchain.toml`
  to `stable`; rustup will install it on first build.
- `git`, plus any of the 17 bundled linter binaries you want exercised
  end-to-end (`actionlint`, `ast-grep`, `biome`, `eslint`, `gitleaks`,
  `golangci-lint`, `hadolint`, `markdownlint`, `osv-scanner`,
  `phpstan`, `rubocop`, `ruff`, `semgrep`, `shellcheck`, `sqlfluff`,
  `trivy`, `yamllint`). Missing linters are silently skipped at
  runtime, so absence isn't a build blocker.

### First build

```sh
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs the same four checks (see `.forgejo/workflows/ci.yml`). Land
no commit that fails any of them. CI additionally runs
`cargo deny check` against the supply-chain config in
[`deny.toml`](./deny.toml) — license compatibility, RUSTSEC
advisories, source allowlist. Run it locally before bumping a
dep:

```sh
cargo install --locked cargo-deny
cargo deny check
```

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

See `docs/ADR-0001-architecture.md` for the high-level decision,
`docs/ADR-0002-sandbox.md` for why every linter spawn is sandboxed,
`docs/ADR-0003-observability.md` for the metrics / readiness /
runtime-introspection design, and `docs/FEASIBILITY.md` for the
longer reasoning. The crate layout:

Each crate has its own `README.md` documenting public surface,
module breakdown, and key tests — open the crate directory for
the navigation aid. The summary table:

| Crate | Responsibility |
|---|---|
| `ar-gateway` | HTTP server, HMAC verification, webhook intake, sandbox selection |
| `ar-orchestrator` | JobDispatcher trait + production SpawningDispatcher |
| `ar-forgejo` | REST client + InitClient for HTTP-Basic bootstrap |
| `ar-llm` | Provider trait + tier-based Router |
| `ar-prompts` | Prompt templates + JSON schemas + validation |
| `ar-review` | Pipeline activities (clone, lint, review, verify, self-heal) |
| `ar-tools` | Static-analysis runners + result parsers (44 linters) |
| `ar-cli` | Operator CLI (init / register-webhook / review-once / bench / doctor / status / 16 more — see crate README) |
| `ar-sandbox` | Sandbox trait + DirectSandbox + PodmanSandbox (see ADR-0002) |
| `ar-chat` | Agentic `@auto_review` chat handler (8 commands + freeform) |
| `ar-index` | Tree-sitter symbols, embeddings, co-change graph, learnings store |

## Adding a new linter

1. Pick a JSON output format the binary supports (most do); for
   text-only tools, wrap a small parser around the line format
   (`yamllint` does this).
2. Create `crates/ar-tools/src/<tool>.rs`:
   - `parse_<tool>_output(...) -> Result<Vec<Finding>, RunnerError>`
     — pure function, no I/O. Test it directly against captured tool
     output.
   - `<Tool>Runner` implementing `LinterRunner`. Build a
     `SandboxCommand` and dispatch via the `run_in_sandbox` helper —
     never spawn `tokio::process::Command` directly. The helper
     swallows "binary not installed" / "sandbox runtime missing" as
     `Ok(empty)` so a missing optional linter can't fail the batch.
3. Add the module to `crates/ar-tools/src/lib.rs`.
4. Wire routing in `crates/ar-review/src/routing.rs::select_runners`
   and update the affected test assertions (the routing tests sort
   the runner names alphabetically, so insertion order in
   `select_runners` doesn't matter — the tests do).
5. Add the binary to `deploy/Dockerfile.sandbox` so production
   deployments using `AR_SANDBOX_IMAGE` get it.
6. Update the linter table in `README.md` and `CHANGELOG.md`.

The existing 44 linters in `crates/ar-tools/src/` are the template.

## Adding a new CLI subcommand

The bot's operator CLI lives in `crates/ar-cli/`. Each subcommand
follows a five-step shape; the existing 22 subcommands are
templates.

1. **Define the args struct** in `crates/ar-cli/src/cli.rs`:
   ```rust
   #[derive(clap::Args, Debug)]
   pub struct FrobnicateArgs {
       #[arg(long, env = "AR_SOMETHING")]
       pub something: String,
       #[arg(long)]
       pub json: bool,
   }
   ```
   Use `env = "AR_*"` for any field that has a sensible env
   default — the existing pattern is to share env vars with the
   gateway so operators on the gateway host can run without
   flags.

2. **Add the variant** to the `Command` enum in the same file:
   ```rust
   Frobnicate(FrobnicateArgs),
   ```
   Include a doc-comment one-liner; clap surfaces it as the
   subcommand's `--help` text.

3. **Implement the handler** in `crates/ar-cli/src/commands.rs`:
   ```rust
   pub async fn frobnicate(args: FrobnicateArgs) -> Result<()> {
       // ...
   }
   ```
   Use `anyhow::Context` to wrap errors with operator-meaningful
   strings ("open history db at <path>", not raw sqlx errors).

4. **Wire the dispatch** in `crates/ar-cli/src/main.rs`:
   ```rust
   Command::Frobnicate(args) => commands::frobnicate(args).await,
   ```

5. **Add tests** in both files:
   - `cli.rs` — clap parse tests covering required args and any
     mutually-exclusive flags (`#[arg(conflicts_with = "...")]`)
   - `commands.rs` — behavioural tests using `wiremock` for HTTP
     paths, `tempfile::tempdir()` for filesystem paths, in-memory
     DB stores for storage paths.

Document the new subcommand in `OPERATIONS.md` (if operator-
facing) or `CONTRIBUTING.md` (if developer-facing) and add a row
to `ar-cli/README.md`'s subcommand inventory.

## Adding a new chat command

Chat commands live in `crates/ar-chat/`. The pattern matches the
existing 8 commands.

1. **Add the variant** to `ChatCommand` in
   `crates/ar-chat/src/command.rs`:
   ```rust
   pub enum ChatCommand {
       // ... existing
       Frobnicate(String),
   }
   ```

2. **Extend the parser** in `parse_chat_command` to recognise
   `@<bot> frobnicate <args>`. Mention parsing strips
   case-sensitivity; the keyword should be lowercase ASCII.

3. **Implement the handler branch** in
   `crates/ar-chat/src/handler.rs::ChatHandler::handle`:
   ```rust
   ChatCommand::Frobnicate(text) => self.handle_frobnicate(ctx, text).await,
   ```
   Helpers like `post_issue_comment(ctx, body)` are already
   available; reach for them rather than touching the Forgejo
   client directly.

4. **Update the help text** in the `ChatCommand::Help` branch so
   `@<bot> help` lists the new command.

5. **Add tests**:
   - Parser tests: positive (recognised), negative (similar-
     looking-but-different commands don't match).
   - Handler tests with `wiremock`-stubbed Forgejo and a
     `CannedProvider` for any LLM call.

Document the new command in `docs/USER-GUIDE.md` (the table of
chat commands with one-line descriptions) and `ar-chat/README.md`
(the public-surface table).

## Commit messages

The history follows `feat(scope): summary` / `fix(scope): summary` /
`docs: summary` / `chore: summary` shape. Keep summaries imperative
("add X", "fix Y") and include a body explaining the why. Sign-off
trailers and `Co-Authored-By:` lines are welcome.

## License

By submitting a contribution you agree to license it under the
AGPL-3.0-or-later (see [LICENSE](./LICENSE)). The intent of the
copyleft is documented there.
