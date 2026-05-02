# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Toolchain

Everything goes through Nix. The dev shell pins the Rust toolchain (nightly per `rust-toolchain.toml`), `cargo-nextest`, `cargo-deny`, `git`, `pkg-config`, `openssl`, etc. **Do not** call a system rustup/cargo — project-local `CARGO_HOME` / `RUSTUP_HOME` live under `.dependencies/` to keep builds reproducible.

```sh
nix develop                  # one-time shell entry (or `direnv allow` once)
nix flake check              # all four CI gates (fmt, clippy, nextest, deny licenses+bans+sources)
```

Faster iteration runs the gates individually:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-tests=pass
cargo deny check licenses bans sources
```

`cargo deny check advisories` requires network and is excluded from `nix flake check`'s sandbox; run it manually when bumping deps.

For an interactive run-and-restart-on-edit dev loop, use **bacon** (also in the dev shell):

```sh
bacon            # default `run` job: builds + restarts ar-gateway on source change
bacon clippy     # lint loop
bacon test       # nextest loop
```

Job definitions live in `bacon.toml` at the repo root.

### Running a single test

```sh
cargo nextest run -p <crate> <substring>           # runs every test whose name contains the substring
cargo test -p <crate> --lib <module>::<test_name>  # exact match
```

### Bumping deps / toolchain

```sh
nix flake update rust-overlay   # nightly bump
nix flake update                # all flake inputs
cargo update -p some-crate      # a single Rust dep
```

Commit `flake.lock` and `Cargo.lock` together.

## Forgejo, not GitHub

The remote is `git.johnwilger.com` (Forgejo). `gh` does **not** work; use `tea` for PRs and issues:

```sh
tea issue view <N> --repo jwilger/auto_review
tea pr create --repo jwilger/auto_review --head <branch> --base main --title "..." --description "..."
```

Branch protection requires a PR for every merge to `main`; CI (`.forgejo/workflows/ci.yml`) runs `nix flake check` on every PR.

## Architecture

A Forgejo webhook lands at the **gateway** (`ar-gateway` binary), which HMAC-verifies the payload and enqueues a job for the **orchestrator** (`SpawningDispatcher` in `ar-orchestrator`). The orchestrator runs a per-PR pipeline, with each stage isolated in its own `ar-review` module:

```
clone (workspace.rs)
  → triage (triage.rs, llm_triage.rs)              # skip lockfile-only PRs, route trivial files away from reasoning model
  → static-analysis fan-out (routing.rs)           # 45 linters via ar-tools, all sandboxed
  → context curation (context_builder.rs)          # tree-sitter symbols + cosine over learnings + RAG
  → review generation (pipeline.rs)                # reasoning-tier LLM, strict-JSON-schema output
  → self-heal validation (heal.rs)                 # one retry on schema-decode failure
  → verification (verify.rs, agentic_verify.rs)    # cheap-tier model drops unfounded findings
  → severity-floor filter (dispatcher.rs)          # AR_SEVERITY_FLOOR; default Warning drops note-only nits
  → post inline review + commit status (mapping.rs)
```

The `@auto_review` chat handler (`ar-chat`) runs a separate poller (`gateway::poller`) plus webhook path; it accepts `help`, `remember <text>`, `forget <id>`, `re-review`, `autofix`, `docstring`, `tests`, and free-form questions. Polling exists because Forgejo doesn't reliably fire `pull_request_review_comment` webhooks for inline-thread replies (gitea#26023).

### Sandbox

Every linter spawn — and every LLM-issued workspace tool — goes through `ar-sandbox`. `AR_SANDBOX_IMAGE` switches from `DirectSandbox` (unsafe; LAN-only) to `PodmanSandbox` (`podman run --network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges …`). Background: docs/ADR-0002, docs/THREAT-MODEL.md. Production deploys without `AR_SANDBOX_IMAGE` log a `sandbox: direct (NO ISOLATION)` warning.

### LLM tier abstraction

`ar-llm::Router` maps `ModelTier::{Reasoning, Cheap, Embedding}` to provider implementations. Today only `OpenAiProvider` ships, but it speaks any OpenAI-compatible backend (hosted OpenAI, Ollama, vLLM, OpenRouter, Together, Groq). Tier-specific env vars (`LLM_REASONING_MODEL`, `LLM_CHEAP_MODEL`, `LLM_EMBEDDING_MODEL`, plus `_BASE_URL` / `_API_KEY` overrides) let one deployment mix providers — e.g. local Ollama for embeddings + cloud for reasoning.

### Crates

| Crate | Purpose |
|---|---|
| `ar-gateway` | HTTP server, HMAC verification, webhook intake, sandbox selection, chat poller |
| `ar-orchestrator` | `JobDispatcher` trait + production `SpawningDispatcher`; per-PR state machine; review history |
| `ar-forgejo` | REST client + `InitClient` for HTTP-Basic bootstrap |
| `ar-llm` | Provider trait + tier-based `Router` (`OpenAiProvider` today) |
| `ar-prompts` | Prompt templates + JSON schemas (consumed via `include_str!`) |
| `ar-review` | Pipeline activities (clone, lint, review, verify, self-heal, RAG context, `.auto_review.yaml` config) |
| `ar-tools` | 45 static-analysis runners + result normalisation |
| `ar-cli` | 16 operator subcommands (`init`, `register-webhook`, `review-once`, `bench`, `doctor`, `status`, …) |
| `ar-sandbox` | Sandbox trait + `DirectSandbox` + `PodmanSandbox` |
| `ar-chat` | `@auto_review` chat handler (8 commands + free-form fallback) |
| `ar-index` | Tree-sitter symbols, embeddings (`EmbedConfig`), co-change graph, learnings store |

Each crate has its own `README.md` with public-surface docs; open the crate dir before adding a feature.

## Development conventions

- **TDD is mandatory.** Red → green → refactor; tests precede implementation. Pure parsing/formatting helpers go in `#[cfg(test)] mod tests` blocks alongside the code; HTTP integration tests use `wiremock`; LLM tests use `CannedProvider` / `ScriptedProvider` fakes (templates in `crates/ar-review/src/heal.rs` and `pipeline.rs`).
- **Commits stay green.** Each commit must pass fmt/clippy/tests on its own. `feat(scope): ...`, `fix(scope): ...`, `docs: ...`, `chore: ...`, `refactor: ...`, `test: ...`. Body explains *why*, not *what* — git history doubles as design documentation.
- **Env-var validation.** Use `read_non_empty_env(name)` and `parse_env::<T>(name)` from `ar-gateway/src/main.rs` instead of raw `env::var(...).ok()` / `.parse().ok()`. Empty / unparseable values warn-and-fall-through to the default rather than silently degrading.
- **Error bodies are capped.** `ar_llm::cap_for_error` and similar helpers truncate provider response bodies before they land in `Error::Provider` / `Error::Decode` — a misbehaving proxy returning a 200 KB HTML page would otherwise pollute logs.
- **No `unwrap()` / `expect()` outside `#[cfg(test)]`.** Use `?`, `anyhow::Context`, or `thiserror` variants.
- **Adding a linter, CLI subcommand, or chat command** has a five-step shape; `CONTRIBUTING.md` walks each one through.
- **Threat-model coupling.** If a change touches a documented threat (T#) in `docs/THREAT-MODEL.md`, the matching red-team test in `crates/ar-review/tests/red_team_*.rs` may need updating. Same for metrics: `deploy/prometheus/auto_review.rules.yaml` and `deploy/grafana/auto_review.dashboard.json` have contract tests against the live `/metrics` surface.
- **CHANGELOG.md** has a `[Unreleased]` section; user-facing or operator-facing changes go there before the PR merges.

## Reference docs

- `docs/ADR-0001-architecture.md` — high-level decision
- `docs/ADR-0002-sandbox.md` — why every linter spawn is sandboxed
- `docs/ADR-0003-observability.md` — metrics / readiness / runtime introspection
- `docs/ADR-0004-vector-store.md` — why SQLite today rather than LanceDB
- `docs/THREAT-MODEL.md` — read before exposing the bot to drive-by PRs
- `docs/OPERATIONS.md` — deploy / rotate / upgrade / alert
- `docs/USER-GUIDE.md` — PR-author-facing: what the bot does, how to talk to it
- `QUICKSTART.md` — env-var inventory + minimal deploy
- `deploy/systemd/auto_review.env.example` — every gateway env var with rationale
