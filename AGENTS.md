# AGENTS.md

opencode uses this file as the always-loaded project guidance for `auto_review`.

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
just opencode-test
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
forgejo_list_repo_issues --owner jwilger --repo auto_review
forgejo_create_pull_request --owner jwilger --repo auto_review --base main --head <branch> --title "..." --body "..."

# CLI fallback
tea issue view <N> --repo jwilger/auto_review
tea pr create --repo jwilger/auto_review --head <branch> --base main --title "..." --description "..."
```

opencode also configures a local `forgejo` MCP server in `opencode.json`. It runs `forgejo-mcp` from the Nix dev shell against `https://git.johnwilger.com` and expects `FORGEJO_TOKEN` in the environment; never hardcode or commit the token.

Branch protection requires a PR for every merge to `main`. CI in `.forgejo/workflows/ci.yml` runs the project verification gates on every PR.

## Architecture

A Forgejo webhook lands at `ar-gateway`, which HMAC-verifies the payload and handles low-cost PR intake plus chat commands. Normal semantic review dispatch comes from the CI action path after repository-selected prerequisites pass; explicit chat commands such as `@auto_review re-review` can force a review. The review pipeline is:

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

Deterministic linters/tests/builds run in CI before semantic review. Runtime workspace tools are read-only and constrained to the clone root; the retired linter sandbox/runtime-tool execution code was removed in the issue #46 rescope.

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
| `ar-cli` | `auto-review` operator command and gateway entrypoint |
| `ar-chat` | chat command handling |
| `ar-index` | tree-sitter symbols, embeddings, co-change graph, learnings store |

Crate-level documentation is centralized in `docs/CRATES.md`; open it before
changing public behavior. The CLI command reference lives in `docs/CLI.md`.

## Development Discipline

- TDD is mandatory. For behavior changes, use the specialist RGR agents: `rgr-test-author` for one focused RED, `rgr-test-reviewer` and `rgr_approve_red` before production edits, `rgr-diagnostic-implementer` for one minimum GREEN edit per current diagnostic, and `rgr-implementation-reviewer` before REFACTOR or broader verification. Multi-failure RED output is invalid; split or narrow tests until one failure drives one edit.
- After one behavioral production edit, rerun the focused command and record the changed RED or GREEN before editing again. Commit each approved GREEN/refactor checkpoint before starting the next RED.
- Plans and todo lists for behavior work must be RGR-shaped, not component waterfalls.
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
- Use `read_non_empty_env(name)` and `parse_env::<T>(name)` in `ar-gateway/src/main.rs` instead of raw env parsing.
- Cap provider error bodies with `ar_llm::cap_for_error` or equivalent helpers.
- No `unwrap()` or `expect()` outside `#[cfg(test)]`.
- If a change touches a documented threat in `docs/THREAT-MODEL.md`, update the matching red-team test when needed.
- Metrics changes may require updates to `deploy/prometheus/auto_review.rules.yaml`, `deploy/grafana/auto_review.dashboard.json`, and contract tests.
- User-facing and operator-facing release notes are generated by the release PR from merged conventional commits.

## opencode Project Layout

- `opencode.json` registers project instructions, permissions, and opencode discovery paths.
- `.opencode/rules/` contains short always-loaded guardrails only.
- `.opencode/skills/` contains longer on-demand procedures.
- `.opencode/agents/` contains specialist primary and subagents.
- `.opencode/commands/` contains slash-command workflows.
- `.opencode/plugins/` contains enforceable project-local behavior and its
  adjacent Node test suite. Run `just opencode-test` for these harness/plugin
  tests; `just test` remains the Rust application test suite.

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
