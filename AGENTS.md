# AGENTS.md

Kilo Code uses this file as the always-loaded project guidance for `auto_review`.

## Toolchain

Everything goes through Nix. The dev shell pins the Rust toolchain in `rust-toolchain.toml` plus `cargo-nextest`, `cargo-deny`, `forgejo-mcp`, `git`, `pkg-config`, and `openssl`. Do not call system `rustup` or bypass project-local `CARGO_HOME` / `RUSTUP_HOME` under `.dependencies/`.

```sh
nix develop
nix flake check
```

Faster focused gates:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-tests=pass
cargo deny check licenses bans sources
```

`cargo deny check advisories` requires network and is excluded from `nix flake check`'s sandbox; run it manually when bumping dependencies.

Use `bacon`, `bacon clippy`, or `bacon test` for watch loops. Job definitions live in `bacon.toml`.

## Forgejo, Not GitHub

The remote is `git.johnwilger.com` (Forgejo). `gh` does not work for this repo. Use `tea`:

```sh
tea issue view <N> --repo jwilger/auto_review
tea pr create --repo jwilger/auto_review --head <branch> --base main --title "..." --description "..."
```

Kilo also configures a local `forgejo` MCP server in `kilo.json`. It runs `forgejo-mcp` from the Nix dev shell against `https://git.johnwilger.com` and expects `FORGEJO_TOKEN` in the environment; never hardcode or commit the token. Use the Forgejo MCP tools when available for Forgejo issue/PR/repository operations, with `tea` as the CLI fallback.

Branch protection requires a PR for every merge to `main`. CI in `.forgejo/workflows/ci.yml` runs `nix flake check` on every PR.

## Architecture

A Forgejo webhook lands at `ar-gateway`, which HMAC-verifies the payload and enqueues a job for `ar-orchestrator`. The review pipeline is:

```text
clone (workspace.rs)
  -> triage (triage.rs, llm_triage.rs)
  -> context curation (context_builder.rs)
  -> review generation (pipeline.rs)
  -> self-heal validation (heal.rs)
  -> verification (verify.rs, agentic_verify.rs)
  -> severity-floor filter (dispatcher.rs)
  -> post inline review + commit status (mapping.rs)
```

The `@auto_review` chat handler in `ar-chat` runs a poller plus webhook path and supports `help`, `remember <text>`, `forget <id>`, `re-review`, `autofix`, `docstring`, `tests`, and free-form questions. Polling exists because Forgejo does not reliably fire inline-thread reply webhooks.

Deterministic linters/tests/builds run in CI before semantic review. Runtime workspace tools are read-only and constrained to the clone root; `ar-sandbox` remains for the issue #46 rescope.

`ar-llm::Router` maps `ModelTier::{Reasoning, Cheap, Embedding}` to provider implementations. `OpenAiProvider` speaks OpenAI-compatible backends and tier-specific env vars select models, base URLs, and API keys.

## Crates

| Crate | Purpose |
|---|---|
| `ar-gateway` | HTTP server, HMAC verification, webhook intake, chat poller |
| `ar-orchestrator` | `JobDispatcher`, `SpawningDispatcher`, per-PR state machine, review history |
| `ar-forgejo` | REST client and HTTP-Basic bootstrap client |
| `ar-llm` | provider trait and tier-based router |
| `ar-prompts` | prompt templates and JSON schemas |
| `ar-review` | review, verify, self-heal, RAG context, repo config |
| `ar-tools` | legacy static-analysis runners retained outside normal runtime |
| `ar-cli` | operator subcommands |
| `ar-sandbox` | sandbox trait plus direct and Podman backends |
| `ar-chat` | chat command handling |
| `ar-index` | tree-sitter symbols, embeddings, co-change graph, learnings store |

Each crate has its own `README.md`; open the crate docs before changing public behavior.

## Development Discipline

- TDD is mandatory. For behavior changes, use the specialist RGR agents: `rgr-test-author` for RED, `rgr-test-reviewer` before production edits, `rgr-diagnostic-implementer` for each minimum GREEN change, and `rgr-implementation-reviewer` before REFACTOR or broader verification.
- Plans and todo lists for behavior work must be RGR-shaped, not component waterfalls.
- Pure parsing and formatting helpers get adjacent `#[cfg(test)] mod tests`; HTTP integration tests use `wiremock`; LLM tests use `CannedProvider` or `ScriptedProvider` fakes.
- Commits must stay green and use `feat(scope):`, `fix(scope):`, `docs:`, `chore:`, `refactor:`, or `test:`. Bodies explain why.
- Use `read_non_empty_env(name)` and `parse_env::<T>(name)` in `ar-gateway/src/main.rs` instead of raw env parsing.
- Cap provider error bodies with `ar_llm::cap_for_error` or equivalent helpers.
- No `unwrap()` or `expect()` outside `#[cfg(test)]`.
- If a change touches a documented threat in `docs/THREAT-MODEL.md`, update the matching red-team test when needed.
- Metrics changes may require updates to `deploy/prometheus/auto_review.rules.yaml`, `deploy/grafana/auto_review.dashboard.json`, and contract tests.
- User-facing and operator-facing changes belong in `CHANGELOG.md` under `[Unreleased]`.

## Kilo Project Layout

- `kilo.json` registers project instructions, permissions, and Kilo discovery paths.
- `.kilo/rules/` contains short always-loaded guardrails only.
- `.kilo/skills/` contains longer on-demand procedures.
- `.kilo/agent/` contains specialist primary and subagents.
- `.kilo/command/` contains slash-command workflows.
- `.kilo/plugin/` contains enforceable project-local behavior.

## Reference Docs

- `docs/ADR-0001-architecture.md`
- `docs/ADR-0002-sandbox.md`
- `docs/ADR-0003-observability.md`
- `docs/ADR-0004-vector-store.md`
- `docs/THREAT-MODEL.md`
- `docs/OPERATIONS.md`
- `docs/USER-GUIDE.md`
- `QUICKSTART.md`
- `deploy/systemd/auto_review.env.example`
