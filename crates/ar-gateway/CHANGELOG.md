# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-gateway-v0.1.0) - 2026-05-05

### Added

- *(actions)* wire CI semantic review job ([#70](https://git.johnwilger.com/jwilger/auto_review/pulls/70))
- *(actions)* add CI review gateway action ([#68](https://git.johnwilger.com/jwilger/auto_review/pulls/68))
- *(review)* retire bundled linter execution ([#54](https://git.johnwilger.com/jwilger/auto_review/pulls/54))
- *(gateway)* add CI-triggered review dispatch ([#48](https://git.johnwilger.com/jwilger/auto_review/pulls/48))
- persist runtime state across restarts (closes #19) ([#25](https://git.johnwilger.com/jwilger/auto_review/pulls/25))
- *(sandbox)* support docker as OCI runtime alongside podman
- *(gateway)* probe git at startup, log result
- *(gateway)* warn at startup when WEBHOOK_SECRET is too short
- *(metrics)* track queue waits on the concurrency cap
- *(orchestrator)* AR_REVIEW_CONCURRENCY caps in-flight reviews
- *(metrics)* track verifier-dropped findings as a counter
- *(gateway)* graceful shutdown on SIGTERM/SIGINT
- *(orchestrator)* SqliteReviewHistory for restart-survival
- *(gateway)* webhook delivery dedup via X-Forgejo-Delivery
- *(gateway)* webhook rate limiter (T7 mitigation)
- *(gateway)* GET /info runtime-config snapshot
- *(metrics)* poller observability counters
- *(gateway)* /readyz endpoint distinct from /healthz
- *(metrics)* proper Prometheus histogram for review duration
- *(metrics)* review-pipeline outcome counters via ReviewObserver
- *(gateway)* /metrics endpoint with Prometheus-format counters
- *(gateway,forgejo,orchestrator)* polling fallback for review-thread mentions
- *(orchestrator,gateway)* production sandbox via AR_SANDBOX_IMAGE
- *(gateway)* persist learnings to SQLite when AR_LEARNINGS_DB is set
- *(orchestrator,chat,gateway)* @auto_review re-review actually re-reviews
- *(orchestrator,gateway)* share LearningsStore between chat and RAG
- *(gateway)* wire ChatHandler end-to-end (Milestone 4 chat live)
- *(forgejo,gateway,chat)* wire issue_comment events through chat command parser
- *(gateway)* expose Embedding + Cheap tier env vars; enables RAG end-to-end
- *(forgejo,gateway)* /version endpoint + Forgejo get_server_version
- *(orchestrator,review)* wire clone+lint into the review pipeline
- *(orchestrator,gateway)* wire webhook intake to review pipeline
- *(forgejo,gateway)* implement Forgejo client and webhook intake
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(gateway)* gate semantic reviews behind CI ([#72](https://git.johnwilger.com/jwilger/auto_review/pulls/72))
- *(review)* rescope normal sandbox runtime ([#67](https://git.johnwilger.com/jwilger/auto_review/pulls/67))
- *(gateway)* deduplicate chat mentions ([#62](https://git.johnwilger.com/jwilger/auto_review/pulls/62))
- *(gateway)* handle bot review requests ([#60](https://git.johnwilger.com/jwilger/auto_review/pulls/60))
- *(gateway)* separate Forgejo token env ([#61](https://git.johnwilger.com/jwilger/auto_review/pulls/61))
- *(embed)* size embedding pass for local Ollama ([#20](https://git.johnwilger.com/jwilger/auto_review/pulls/20)) ([#21](https://git.johnwilger.com/jwilger/auto_review/pulls/21))
- *(pre-merge)* scan every marker occurrence in contains_todo_marker ([#4](https://git.johnwilger.com/jwilger/auto_review/pulls/4))
- *(poller)* Delay missed-tick behaviour avoids catch-up bursts
- *(poller)* prune cursor entries for purged PRs each cycle
- *(webhook)* skip non-Created issue_comment actions
- *(poller)* case-insensitive mention pre-filter (parity with parser)
- *(gateway)* warn when env-var integer parses fail (don't silently default)
- *(gateway)* warn when only one half of rate-limit env vars is set
- *(orchestrator,metrics)* bucket review-task panics so counts add up
- *(poller)* case-insensitive self-loop match (matches webhook handler)
- *(gateway)* apply read_non_empty_env to remaining sandbox/db checks
- *(gateway)* treat empty LLM_*_MODEL env vars as unset
- *(gateway)* validate AR_BOT_LOGIN/NAME at startup, dedupe reads
- *(gateway)* bail on startup when LLM_REASONING_MODEL is empty
- *(poller)* first-sight seeding must not dispatch historical mentions
- *(metrics)* unknown failure classes no longer misbucket as unhealable
- *(metrics)* unknown skip reasons no longer misbucket as disabled
- *(gateway)* honour AR_BOT_LOGIN/AR_BOT_NAME in webhook chat path

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- *(metrics)* contract test pinning every counter exposed at /metrics
- cargo fmt sweep for rustfmt 1.8.0 stable style drift
- per-crate READMEs for all 11 workspace crates
- drop-in Grafana dashboard
- drop-in Prometheus rules pack
