# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-chat-v0.1.0) - 2026-05-05

### Added

- *(chat)* @auto_review tests scaffolds unit tests for new items
- *(chat)* @auto_review docstring generates docstrings as inline patches
- *(chat)* @auto_review autofix posts inline suggestion patches
- *(chat)* freeform @auto_review questions answered by the cheap-tier LLM
- *(orchestrator,chat,gateway)* @auto_review re-review actually re-reviews
- *(forgejo,chat)* post_issue_comment + ChatHandler with help/remember/forget
- *(chat)* @auto_review chat command parser (Milestone 4 groundwork)
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(gateway)* gate semantic reviews behind CI ([#72](https://git.johnwilger.com/jwilger/auto_review/pulls/72))
- *(review)* rescope normal sandbox runtime ([#67](https://git.johnwilger.com/jwilger/auto_review/pulls/67))
- *(chat)* paths_in_diff handles git's quoted-path form
- *(llm)* set temperature=0 on every deterministic LLM call
- *(chat)* test scaffolds distinguishes "no coverage needed" from malformed
- *(chat)* autofix distinguishes "no patches" from "all hallucinated"
- *(chat)* skip autofix/docstrings/tests on closed PRs (parity with re_review)
- *(chat)* skip re-review on closed/merged PRs
- *(chat)* case-insensitive mention parsing matches Forgejo's UI
- *(chat)* cap test-scaffold source at 4 KiB before posting
- *(chat)* cap autofix patch reason (1 KiB) and replacement (4 KiB)
- *(chat)* cap freeform question text at 4 KiB before LLM call
- *(chat)* cap remember text at 4 KiB before embedding/storing
- *(chat)* grow test-scaffold fence past internal backtick runs
- *(chat)* grow autofix suggestion fence past internal backtick runs
- *(chat)* cap freeform LLM reply size before posting to Forgejo
- *(chat)* autofix/docstrings drops patches outside the PR's diff

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- *(chat)* round-trip HELP_TEXT keywords through the parser
- *(chat)* contract test linking HELP_TEXT to ChatCommand variants
- per-crate READMEs for all 11 workspace crates
