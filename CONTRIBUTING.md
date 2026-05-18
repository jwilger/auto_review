# Contributing

Thanks for considering a contribution to `auto_review`.

## Development setup

### Prerequisites

- A Rust toolchain plus the project tools on `PATH`: `just`,
  `cargo-deny`, `cargo-nextest`, `git`, `pkg-config`, `openssl`,
  and the rest of the normal build prerequisites.
- Optional: **[Nix](https://nixos.org/download.html) with flakes
  enabled.** This is the supported provisioning path: it pins the
  Rust toolchain (nightly, resolved by `flake.lock`'s rust-overlay
  revision) and supplies the project tools used by CI.
- Optional: [direnv](https://direnv.net/) for automatic shell
  setup — `direnv allow` from this directory loads the flake's
  dev shell on every `cd`.

When using `nix develop`, the dev shell does NOT use any system rustup,
cargo, or rust binaries. Project-local `CARGO_HOME` / `RUSTUP_HOME`
directories under `.dependencies/` keep that shell reproducible.

### First build

```sh
# Optional: enter the dev shell (or `direnv allow` once).
nix develop

# Run every routine CI check locally.
just ci

# Run individual checks:
just fmt
just clippy
just test
just deny
just build
```

`just ci` runs rustfmt, clippy (with `-D warnings`), the full
nextest test suite, cargo-deny (licenses + bans + sources), and a
workspace build. Advisory checks require network access, so run
`cargo deny check advisories` separately when bumping a dep. Land no
commit that fails any required check.

### Bumping the toolchain or a dep

```sh
# Bump the resolved nightly:
nix flake update rust-overlay

# Bump every flake input (nixpkgs, crane, rust-overlay):
nix flake update

# Bump a Rust dep (use cargo as usual):
cargo update -p some-crate
```

Commit `flake.lock` and `Cargo.lock` after any update so
everyone (and CI) picks up the same versions.

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
  or real LLMs is exercised via `auto-review review once` and is
  not currently covered by automated tests.

## Architecture overview

See `docs/ARCHITECTURE.md` for the current architecture projection,
`docs/ADR-0001-hybrid-review-pipeline.md` for the initial high-level decision,
`docs/ADR-0016-adr-event-stream-architecture-projection.md` for the ADR process,
and `docs/CRATES.md` for the crate map.

ADR files are managed as an append-only event stream. Create architecture changes
as proposed ADRs through the project ADR workflow tools, update proposed ADRs
through the same typed workflow, and transition them with the accept/reject
tools. Once accepted or rejected, an ADR body is immutable; later changes require
a new ADR and may only add brief supersession metadata to the older record. Keep
`docs/ARCHITECTURE.md` in sync as the current projection rather than rewriting
historical ADR rationale.

Crate-level navigation is centralized in `docs/CRATES.md`. The summary table:

| Crate | Responsibility |
|---|---|
| `ar-gateway` | HTTP server, HMAC verification, webhook intake, chat poller |
| `ar-orchestrator` | JobDispatcher trait + production SpawningDispatcher |
| `ar-forgejo` | REST client + InitClient for HTTP-Basic bootstrap |
| `ar-llm` | Provider trait + tier-based Router |
| `ar-prompts` | Prompt templates + JSON schemas + validation |
| `ar-review` | Pipeline activities (review, verify, self-heal, RAG context, repo config) |
| `ar-cli` | Operator CLI (`auto-review`; see `docs/CLI.md`) |
| `ar-chat` | Agentic `@auto_review` chat handler (7 specific commands + freeform fallback) |
| `ar-index` | Tree-sitter symbols, embeddings, co-change graph, learnings store |

## Adding a new CLI subcommand

The bot's operator CLI lives in `crates/ar-cli/`. Each subcommand
follows a five-step shape; the existing subcommands are
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

Document the new subcommand in `docs/CLI.md`, and in `docs/OPERATIONS.md` if it
changes operator workflows. The CLI contract test fails when a top-level command
is missing from `docs/CLI.md`; that test is intentionally limited to the public
CLI surface and must not expand into checking prose wording.

For docs-only changes, do not add deterministic tests that assert exact wording
or required phrases. Use human/operator review for prose. Keep tests for
executable behavior, generated docs, public CLI/contracts, schemas, deployment
artifacts, and security red-team boundaries, and explain any docs-reading
contract at the test site.

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

Document the new command in `docs/USER-GUIDE.md` (the table of chat commands
with one-line descriptions) and update `docs/CRATES.md` if the public surface
summary changes.

## Commit messages

The history follows `feat(scope): summary` / `fix(scope): summary` /
`docs: summary` / `chore: summary` shape. Keep summaries imperative
(`add X`, `fix Y`) and include a body that explains **why** (problem,
risk, or user need), not just what changed.

Use this preferred body shape:

```text
Why:
- <reason / problem / risk addressed>

What:
- <specific change made>

Validation:
- <focused checks run>
```

Sign-off trailers and `Co-Authored-By:` lines are welcome.

## License & CLA

The project is published under AGPL-3.0-or-later (see
[LICENSE](./LICENSE)). Every contribution is also subject to the
[Contributor License Agreement](./CLA.md), which grants the
copyright holder broader-rights so future relicensing remains
possible. Read CLA.md once; afterwards a `Signed-off-by:` trailer
on every commit (set automatically with `git commit -s`) carries
both the DCO certification and CLA acceptance.
