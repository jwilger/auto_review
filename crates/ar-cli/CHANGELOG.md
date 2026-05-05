# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-cli-v0.1.0) - 2026-05-05

### Added

- *(review)* retire bundled linter execution ([#54](https://git.johnwilger.com/jwilger/auto_review/pulls/54))
- persist runtime state across restarts (closes #19) ([#25](https://git.johnwilger.com/jwilger/auto_review/pulls/25))
- *(doctor)* probe for git in PATH
- *(bench)* expand_fixture_paths now recurses into subdirectories
- *(cli)* explain-routing for understanding linter dispatch
- *(cli,orchestrator)* purge-history for long-running deploys
- *(cli)* list-learnings + forget-learning admin commands
- *(cli)* reset-pr subcommand for ad-hoc history clears
- *(orchestrator)* SqliteReviewHistory for restart-survival
- *(cli,review)* validate-config --strict catches typo'd keys
- *(cli)* status subcommand for live gateway snapshot
- *(bench)* --baseline FILE comparison + --fail-on-regression
- *(cli,forgejo)* list-webhooks and unregister-webhook
- *(cli)* doctor verifies configured LLM model names are loaded
- *(cli)* doctor subcommand for deployment health checks
- *(cli)* test-webhook subcommand for deploy smoke-tests
- *(cli,tools)* list-linters subcommand + canonical catalogue
- *(metrics)* review-pipeline outcome counters via ReviewObserver
- *(cli)* validate-config subcommand for .auto_review.yaml files
- *(cli)* bench harness scores precision/recall against labelled fixtures
- *(cli)* bench subcommand for fixture-replay regression tracking
- *(orchestrator,gateway)* production sandbox via AR_SANDBOX_IMAGE
- *(orchestrator,chat,gateway)* @auto_review re-review actually re-reviews
- *(orchestrator,gateway)* share LearningsStore between chat and RAG
- *(orchestrator)* SpawningDispatcher tracks per-PR review history
- *(prompts,review,cli)* repo_context field for RAG-retrieved context
- *(cli)* review-once --dry-run prints the rendered LLM prompt
- *(cli)* auto_review review-once for one-shot demos / debugging
- *(cli)* auto_review init + register-webhook subcommands
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(gateway)* gate semantic reviews behind CI ([#72](https://git.johnwilger.com/jwilger/auto_review/pulls/72))
- *(review)* rescope normal sandbox runtime ([#67](https://git.johnwilger.com/jwilger/auto_review/pulls/67))
- *(bench)* bail on empty LLM_BASE_URL or LLM_REASONING_MODEL
- *(cli)* reject empty/weak webhook secrets at registration time
- *(bench)* success-rate drop now triggers --fail-on-regression

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- *(cli)* init's next-step hint suggests `openssl rand -hex 32`
- cargo fmt sweep for rustfmt 1.8.0 stable style drift
- *(cli)* README contract test catches subcommand drift
- per-crate READMEs for all 11 workspace crates
- expand labelled corpus from 1 to 5 fixtures
- *(bench)* assert shipped bench/fixtures parse against the Fixture struct
- *(changelog,cli)* freshen CHANGELOG; correct ar-cli crate description
