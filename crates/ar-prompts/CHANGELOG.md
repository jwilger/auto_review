# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-prompts-v0.1.0) - 2026-05-05

### Added

- *(review)* suggest missing CI linters ([#65](https://git.johnwilger.com/jwilger/auto_review/pulls/65))
- *(review)* retire bundled linter execution ([#54](https://git.johnwilger.com/jwilger/auto_review/pulls/54))
- *(review)* custom natural-language pre-merge checks
- *(prompts,review)* verification agent (Milestone 3 piece)
- *(prompts,review,cli)* repo_context field for RAG-retrieved context
- *(prompts)* LLM triage prompt + JSON schema + validation
- *(prompts,review,orchestrator)* inject .auto_review.yaml guidelines into LLM prompt
- *(prompts,review)* optional walkthrough + Mermaid diagram in review body
- *(prompts)* inject static-analysis findings section into review prompt
- *(review)* single-pass review pipeline with self-heal validation
- *(prompts)* review JSON schema, validation, and prompt rendering
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(review)* drop pre-merge checks ([#64](https://git.johnwilger.com/jwilger/auto_review/pulls/64))
- *(pre-merge)* scan every marker occurrence in contains_todo_marker ([#4](https://git.johnwilger.com/jwilger/auto_review/pulls/4))
- *(prompts)* cap rendered linter findings at 100 with overflow summary
- *(prompts)* cap guidelines (8 KiB) and repo_context (16 KiB) too
- *(prompts)* cap PR title (512 B) and body (8 KiB) before LLM call

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- cargo fmt sweep for rustfmt 1.8.0 stable style drift
- per-crate READMEs for all 11 workspace crates
