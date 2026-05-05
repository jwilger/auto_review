---
name: rust-workspace-engineering
description: auto_review Rust workspace conventions, Nix toolchain use, error handling, env parsing, tests, and docs coupling.
---

# Rust Workspace Engineering

## Toolchain

Use Nix and the pinned Rust toolchain. Prefer focused checks during RGR and `nix flake check` for full parity.

## Code Conventions

- No `unwrap()` or `expect()` outside `#[cfg(test)]`.
- Use `?`, `anyhow::Context`, or explicit error variants.
- Use `read_non_empty_env(name)` and `parse_env::<T>(name)` in `ar-gateway/src/main.rs`.
- Cap provider error bodies with `ar_llm::cap_for_error` or equivalent helpers.
- Keep crate boundaries aligned with `AGENTS.md` and crate `README.md` files.

## Tests

Pure helpers get adjacent unit tests. HTTP integrations use `wiremock`. LLM behavior uses `CannedProvider` or `ScriptedProvider` fakes.

## User-Facing Changes

Use conventional commit titles for user-facing or operator-facing behavior; the release PR generates changelog notes from conventional commits.
