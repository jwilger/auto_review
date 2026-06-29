# AGENTS.md

Codex uses this file as the always-loaded project guidance for `auto_review`.

## Toolchain

`just` is the canonical command interface for routine development and CI checks. Nix is optional for developers who already have the required tools on `PATH`, and remains the supported way to provision the pinned Rust toolchain in `rust-toolchain.toml` plus `cargo-nextest`, `cargo-deny`, `forgejo-mcp`, `git`, `pkg-config`, `openssl`, and other project tools. Do not call system `rustup` from inside `nix develop` or bypass project-local `CARGO_HOME` / `RUSTUP_HOME` under `.dependencies/`.

```sh
nix develop
just ci
```

Focused gates:

```sh
just fmt
just clippy
just test
just deny
just build
```

`cargo deny check advisories` requires network and is excluded from the default `just deny`/CI policy gate; run it manually when bumping dependencies.

Use `bacon`, `bacon clippy`, or `bacon test` for focused Rust check loops.

## Forgejo, Not GitHub

The remote is `git.johnwilger.com` (Forgejo). `gh` does not work for this repo.

Prefer Forgejo MCP (`forgejo_*` tools) for issue, PR, and repository operations when available. Use `tea` only as a fallback when MCP is unavailable:

```sh
# MCP-first path
forgejo_list_repo_issues --owner Slipstream --repo auto_review
forgejo_create_pull_request --owner Slipstream --repo auto_review --base main --head <branch> --title "..." --body "..."

# CLI fallback
tea issue view <N> --repo Slipstream/auto_review
tea pr create --repo Slipstream/auto_review --head <branch> --base main --title "..." --description "..."
```

Forgejo MCP may be available from the surrounding Codex environment. If it is not available, use `tea` as the fallback. `FORGEJO_TOKEN` is the expected credential for Forgejo tooling; never hardcode or commit the token.

Branch protection requires a PR for every merge to `main`. CI in `.forgejo/workflows/ci.yml` runs the Rust verification gates for application changes.

## Architecture

A Forgejo webhook lands at `ar-gateway`, which HMAC-verifies the payload and handles low-cost PR intake plus chat commands. Normal semantic review dispatch comes from the CI action path after repository-selected prerequisites pass; explicit chat commands such as `@auto-review re-review` can force a review. The review pipeline is:

```text
clone (workspace.rs)
  -> deterministic triage (triage.rs)
  -> context curation (context_builder.rs)
  -> review generation (pipeline.rs)
  -> self-heal validation (heal.rs)
  -> pre-verifier severity-floor filter (pipeline.rs)
  -> verification (verify.rs, agentic_verify.rs)
  -> post-verifier severity/path filter (pipeline.rs)
  -> post inline review + commit status (mapping.rs)
```

The `@auto-review` chat handler in `ar-chat` runs a poller plus webhook path and supports `help`, `remember <text>`, `forget <id>`, `re-review`, `autofix`, `docstring`, `tests`, and free-form questions. `@auto_review` remains a compatibility alias. Polling exists because Forgejo does not reliably fire inline-thread reply webhooks.

Deterministic linters/tests/builds run in CI before semantic review. Runtime workspace tools are read-only and constrained to the clone root; the retired linter sandbox/runtime-tool execution code was removed in the issue #46 rescope.

`ar-llm::Router` maps `ModelTier::{Reasoning, Cheap, Embedding}` to provider implementations. `OpenAiProvider` speaks OpenAI-compatible backends and tier-specific env vars select models, base URLs, and API keys.

## Crates

| Crate | Purpose |
|---|---|
| `ar-agentcore` | AWS Bedrock AgentCore-compatible HTTP runtime surface |
| `ar-gateway` | HTTP server, HMAC verification, webhook intake, CI-triggered dispatch, chat poller |
| `ar-orchestrator` | `JobDispatcher`, `SpawningDispatcher`, review history, changed-file triage, status calls |
| `ar-forge` | Provider-neutral repository-host DTOs, error type, and `ReviewHost` trait |
| `ar-forgejo` | Forgejo REST client, webhook DTOs, review/comment/status APIs, Basic-auth bootstrap client |
| `ar-github` | GitHub App REST client foundation and `ReviewHost` wrapper |
| `ar-llm` | provider trait and tier-based router |
| `ar-prompts` | prompt templates and JSON schemas |
| `ar-review` | review, verify, self-heal, RAG context, repo config |
| `ar-cli` | `auto-review` operator command, gateway entrypoint, and AgentCore entrypoint |
| `ar-chat` | chat command handling |
| `ar-index` | tree-sitter symbols, embeddings, vector stores, co-change graph, learnings store |

Crate-level documentation is centralized in `docs/CRATES.md`; open it before
changing public behavior. The CLI command reference lives in `docs/CLI.md`.

## Development Discipline

- TDD is mandatory for behavior changes: create one focused RED, confirm it fails for the intended reason, make the smallest production edit, and rerun the focused command until it passes.
- Multi-failure RED output is invalid; split or narrow tests until one failure drives one edit.
- Plans and todo lists for behavior work should be shaped around focused red/green/refactor checkpoints, not component waterfalls.
- Pure parsing and formatting helpers get adjacent `#[cfg(test)] mod tests`; HTTP integration tests use `wiremock`; LLM tests use `CannedProvider` or `ScriptedProvider` fakes.
- Do not add deterministic tests that assert documentation wording for docs-only content. Keep tests for executable behavior, generated docs, public CLI/contracts, schemas, deployment artifacts, and security red-team boundaries; justify any docs-reading contract test near the test.
- Commits must stay green and use `feat(scope):`, `fix(scope):`, `docs:`, `chore:`, `refactor:`, or `test:`.
  Include a short body that explains **why** the change is needed (risk solved, user need, or regression fixed), not only **what** changed.
  PR titles should remain a concise conventional-commit-style summary of the PR as a whole, and PR bodies should capture **all** work on the branch (not only the last commit), including any follow-up docs/process updates.
  Prefer this lightweight template:

  ```
   Why:
   - <reason / problem / risk addressed>
   - If this PR resolves an issue, `See issue #<issue-number>` is acceptable.

  What:
  - <specific change made>

  Validation:
  - <focused checks run>
  ```
- Use `read_non_empty_env(name)` and `parse_env::<T>(name)` in `ar-gateway/src/startup.rs` instead of raw env parsing.
- Cap provider error bodies with `ar_llm::cap_for_error` or equivalent helpers.
- No `unwrap()` or `expect()` outside `#[cfg(test)]`.
- If a change touches a documented threat in `docs/THREAT-MODEL.md`, update the matching red-team test when needed.
- Metrics changes may require updates to `deploy/prometheus/auto_review.rules.yaml`, `deploy/grafana/auto_review.dashboard.json`, and contract tests.
- User-facing and operator-facing release notes are generated by the release PR from merged conventional commits.

## Project Layout

- `scripts/release` contains release helper logic.
- `tests/features/` contains end-to-end feature specifications.
- `.forgejo/workflows/` contains Forgejo Actions workflows.
- `.forgejo/pull_request_template.md` contains the pull request template.

## Reference Docs

- `docs/ADR-0001-hybrid-review-pipeline.md`
- `docs/ADR-0016-adr-event-stream-architecture-projection.md`
- `docs/ARCHITECTURE.md`
- `docs/THREAT-MODEL.md`
- `docs/OPERATIONS.md`
- `docs/USER-GUIDE.md`
- `docs/QUICKSTART.md`
- `docs/DEPLOYMENT.md`
- `deploy/systemd/auto_review.env.example`
