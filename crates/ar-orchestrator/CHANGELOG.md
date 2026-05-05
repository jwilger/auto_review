# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-orchestrator-v0.1.0) - 2026-05-05

### Added

- *(actions)* add Forgejo CI action and quiet compare fallback ([#69](https://git.johnwilger.com/jwilger/auto_review/pulls/69))
- *(review)* retire bundled linter execution ([#54](https://git.johnwilger.com/jwilger/auto_review/pulls/54))
- persist runtime state across restarts (closes #19) ([#25](https://git.johnwilger.com/jwilger/auto_review/pulls/25))
- *(metrics)* track queue waits on the concurrency cap
- *(orchestrator)* AR_REVIEW_CONCURRENCY caps in-flight reviews
- *(metrics)* track verifier-dropped findings as a counter
- *(orchestrator)* severity breakdown in commit-status description
- *(cli,orchestrator)* purge-history for long-running deploys
- *(orchestrator)* SqliteReviewHistory for restart-survival
- *(review)* AR_SEVERITY_FLOOR for signal-to-noise tuning
- *(review)* linter-only review mode
- *(review)* custom natural-language pre-merge checks
- *(metrics)* review-pipeline outcome counters via ReviewObserver
- *(gateway,forgejo,orchestrator)* polling fallback for review-thread mentions
- *(orchestrator,review)* wire agentic verifier behind AR_AGENTIC_VERIFIER
- *(orchestrator,gateway)* production sandbox via AR_SANDBOX_IMAGE
- *(orchestrator,chat,gateway)* @auto_review re-review actually re-reviews
- *(orchestrator,gateway)* share LearningsStore between chat and RAG
- *(orchestrator,review)* incremental diff via Client::get_compare_diff
- *(orchestrator)* SpawningDispatcher tracks per-PR review history
- *(orchestrator)* wire LLM-driven file triage into the lint phase
- *(orchestrator)* wire build_review_context into the review pipeline
- *(orchestrator)* per-PR review history for incremental reviews
- *(prompts,review,orchestrator)* inject .auto_review.yaml guidelines into LLM prompt
- *(review,orchestrator)* wire ignored_paths through review pipeline
- *(orchestrator)* honor .auto_review.yaml enabled + disabled_tools
- *(review,orchestrator)* heuristic triage skips lockfile-only PRs
- *(orchestrator,review)* wire clone+lint into the review pipeline
- *(review)* thread linter findings through review_pull_request
- *(review)* workspace prep with shallow clone + auth-stripping logs
- *(orchestrator,gateway)* wire webhook intake to review pipeline
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(forgejo)* use web compare diff route ([#73](https://git.johnwilger.com/jwilger/auto_review/pulls/73))
- *(gateway)* gate semantic reviews behind CI ([#72](https://git.johnwilger.com/jwilger/auto_review/pulls/72))
- *(review)* drop pre-merge checks ([#64](https://git.johnwilger.com/jwilger/auto_review/pulls/64))
- *(review)* show linter run summaries in reviews ([#40](https://git.johnwilger.com/jwilger/auto_review/pulls/40))
- *(post)* default AR_SEVERITY_FLOOR to warning ([#6](https://git.johnwilger.com/jwilger/auto_review/pulls/6)) ([#17](https://git.johnwilger.com/jwilger/auto_review/pulls/17))
- *(ci)* satisfy rustfmt + clippy gates on M0 deliverables
- *(orchestrator)* only record review history on successful reviews
- *(orchestrator)* cap commit-status description at 240 chars
- *(orchestrator)* defer Started until after early-skip checks pass
- *(orchestrator,metrics)* bucket review-task panics so counts add up
- *(orchestrator)* log when agentic verifier silently downgrades
- *(orchestrator)* post a crash status when the review task panics
- *(orchestrator)* log panics and cancellations from spawned review tasks

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- *(orchestrator)* synthetic e2e drives webhook-to-review in-process
- *(orchestrator)* share raw_diff between lint phase and pipeline
- *(orchestrator)* fetch list_changed_files once, share with lint phase
- cargo fmt sweep for rustfmt 1.8.0 stable style drift
- per-crate READMEs for all 11 workspace crates
- *(review)* bundle review_pull_request inputs into ReviewArgs struct
