# Changelog

All notable changes to `auto_review` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- release-prepare inserts generated release sections below this line -->

## [0.9.0] - 2026-05-12

### Added

- *(review)* add PR metadata pre-merge check (#182)

### Fixed

- *(release)* attach assets during release creation (#173)
- *(release)* package runnable binary archives (#175)
- *(chat)* use hyphenated bot identity (#177)
- *(review)* include prior PR discussion in follow-up reviews (#178)
- *(review)* accept release PR metadata (#184)

### Other

- release v0.8.3 (#170)
- consolidate operator documentation (#172)
- *(docs)* remove prose wording checks (#181)

## [0.8.3] - 2026-05-09

### Fixed

- *(release)* publish from main push (#169)
- *(release)* clean container PR packages (#171)

### Other

- release v0.8.2 (#168)

## [0.8.2] - 2026-05-08

### Fixed

- *(release)* isolate publish dispatch inputs (#167)

### Other

- release v0.8.1 (#165)

## [0.8.1] - 2026-05-08

### Fixed

- *(release)* defer Forgejo releases until merge (#164)
- *(release)* clean PR packages after merge (#166)

## [0.8.0] - 2026-05-08

### Fixed

- *(release)* keep release jobs containerized (#159)

### Other

- release v0.7.0 (#158)
- *(adr)* defer aarch64 binary releases (#161)

## [0.7.0] - 2026-05-08

### Fixed

- *(release)* require nix-native publish runner (#157)

### Other

- release v0.6.0 (#156)

## [0.6.0] - 2026-05-08

### Fixed

- *(release)* build artifacts on native runner (#155)

### Other

- release v0.5.0 (#154)

## [0.5.0] - 2026-05-08

### Fixed

- *(release)* configure artifact build platforms (#153)

### Other

- release v0.4.0 (#152)

## [0.4.0] - 2026-05-08

### Fixed

- *(release)* allow local aarch64 builds (#151)

### Other

- release v0.3.0 (#126)
- *(kilo)* allow Forgejo MCP tools (#146)

## [0.3.0] - 2026-05-07

### Added

- *(cli)* unify operator binary (#125)
- *(gateway)* add fail-closed OCI launcher seam (#127)
- *(gateway)* package embedded OCI rootfs (#129)
- *(gateway)* launch through embedded OCI runtime (#130)
- *(ops)* report runtime isolation posture (#132)
- *(deploy)* add NixOS deployment module (#136)
- *(release)* publish Linux binary artifacts (#137)

### Other

- record single-binary ADRs (#123)
- Run Docker image through unified auto-review binary (#131)
- document single-binary rollout (#133)

## [0.2.0] - 2026-05-06

### Added

- *(release)* promote release candidate images (#110)
- *(release)* create prerelease entries for candidates (#113)

### Fixed

- *(release)* use bot login for registry auth (#112)
- *(release)* avoid multiline rc release note quoting (#114)

## [0.1.3] - 2026-05-06

### Fixed

- *(release)* publish version tags and Forgejo releases (#108)

### Other

- release v0.1.2 (#106)

## [0.1.2] - 2026-05-06

### Fixed

- *(release)* add skopeo trust policy (#105)

### Other

- release v0.1.1 (#103)

## [0.1.1] - 2026-05-06

### Fixed

- *(release)* attach publish checkout to main (#95)
- *(release)* allow publish reruns from UI (#96)
- *(release)* render publish rerun SHA input (#97)
- *(review)* make incremental walkthroughs delta-focused (#98)
- *(release)* publish Nix image from release workflow (#99)
- *(release)* replace release-plz prep automation (#100)
- *(release)* run prep after release fixes (#101)
- *(release)* run release PR tooling through Nix (#102)
- *(release)* supersede stale release PRs (#104)

## [0.1.0] - 2026-05-05

### Added

- bootstrap workspace for Forgejo AI PR reviewer
- *(forgejo,gateway)* implement Forgejo client and webhook intake
- *(llm)* OpenAI-compatible provider and tier-based router
- *(prompts)* review JSON schema, validation, and prompt rendering
- *(review)* single-pass review pipeline with self-heal validation
- *(orchestrator,gateway)* wire webhook intake to review pipeline
- *(tools)* linter runners and parsers for ruff, shellcheck, hadolint, markdownlint
- *(prompts)* inject static-analysis findings section into review prompt
- *(review)* workspace prep with shallow clone + auth-stripping logs
- *(review)* thread linter findings through review_pull_request
- *(review)* route changed files to language-specific linters
- *(orchestrator,review)* wire clone+lint into the review pipeline
- *(forgejo)* InitClient + create_access_token + create_webhook
- *(cli)* auto_review init + register-webhook subcommands
- *(tools,review)* eslint runner + routing for JS/TS extensions
- *(review,orchestrator)* heuristic triage skips lockfile-only PRs
- *(prompts,review)* optional walkthrough + Mermaid diagram in review body
- *(tools,review)* gitleaks runner for committed-secret detection
- *(review)* cap PR diff at 100 KiB before sending to LLM
- *(review)* repo-level .auto_review.yaml config loader
- *(orchestrator)* honor .auto_review.yaml enabled + disabled_tools
- *(review)* glob-based ignored_paths utilities for diff/file filtering
- *(review,orchestrator)* wire ignored_paths through review pipeline
- *(prompts,review,orchestrator)* inject .auto_review.yaml guidelines into LLM prompt
- *(forgejo,gateway)* /version endpoint + Forgejo get_server_version
- *(tools,review)* actionlint runner for workflow YAML
- *(prompts)* LLM triage prompt + JSON schema + validation
- *(forgejo)* get_pull_request returns a PullRequestSummary
- *(cli)* auto_review review-once for one-shot demos / debugging
- *(cli)* review-once --dry-run prints the rendered LLM prompt
- *(tools,review)* yamllint runner for general YAML
- *(index)* tree-sitter symbol extraction for Rust (Milestone 2 RAG groundwork)
- *(index)* tree-sitter symbol extraction for Python
- *(index)* tree-sitter symbol extraction for TypeScript and TSX
- *(index)* extract_symbols_for_path dispatches by file extension
- *(index)* index_workspace walks a clone and emits IndexedSymbols
- *(index)* co-change graph from git history
- *(review)* triage_files_with_llm wires the cheap-tier classifier
- *(index)* symbol-level embedding pass via the LLM router
- *(index)* VectorStore trait + InMemoryVectorStore implementation
- *(index)* persistent learnings store (in-memory implementation)
- *(forgejo)* get_compare_diff for incremental review support
- *(orchestrator)* per-PR review history for incremental reviews
- *(prompts,review,cli)* repo_context field for RAG-retrieved context
- *(review)* format_repo_context renders RAG retrieval as prompt markdown
- *(index)* tree-sitter symbol extraction for Go
- *(review)* build_review_context integrates the RAG layer end-to-end
- *(orchestrator)* wire build_review_context into the review pipeline
- *(gateway)* expose Embedding + Cheap tier env vars; enables RAG end-to-end
- *(orchestrator)* wire LLM-driven file triage into the lint phase
- *(orchestrator)* SpawningDispatcher tracks per-PR review history
- *(orchestrator,review)* incremental diff via Client::get_compare_diff
- *(tools,review)* semgrep runner (multi-language SAST). 9th linter.
- *(prompts,review)* verification agent (Milestone 3 piece)
- *(tools,review)* golangci-lint runner. 10th bundled linter.
- *(tools,review)* rubocop runner. 11th bundled linter.
- *(deploy)* Forgejo Action packaging (Milestone 5 piece)
- *(deploy)* Helm chart for Kubernetes (Milestone 5 piece)
- *(tools,review)* trivy runner. 12th bundled linter.
- *(chat)* @auto_review chat command parser (Milestone 4 groundwork)
- *(forgejo,gateway,chat)* wire issue_comment events through chat command parser
- *(forgejo,chat)* post_issue_comment + ChatHandler with help/remember/forget
- *(gateway)* wire ChatHandler end-to-end (Milestone 4 chat live)
- *(orchestrator,gateway)* share LearningsStore between chat and RAG
- *(orchestrator,chat,gateway)* @auto_review re-review actually re-reviews
- *(chat)* freeform @auto_review questions answered by the cheap-tier LLM
- *(index)* SQLite-backed LearningsStore for cross-restart persistence
- *(gateway)* persist learnings to SQLite when AR_LEARNINGS_DB is set
- *(sandbox)* Sandbox trait + DirectSandbox + PodmanSandbox
- *(orchestrator,gateway)* production sandbox via AR_SANDBOX_IMAGE
- *(deploy)* sandbox image + AR_LEARNINGS_DB / AR_SANDBOX_IMAGE wiring
- *(tools,review)* osv-scanner runner. 13th bundled linter.
- *(cli)* bench subcommand for fixture-replay regression tracking
- *(tools,review)* sqlfluff runner. 14th bundled linter.
- *(tools,review)* ast-grep runner. 15th bundled linter.
- *(tools,review)* biome runner. 16th bundled linter.
- *(tools,review)* phpstan runner. 17th bundled linter.
- *(review)* agentic verifier with sandboxed read_file/search tools
- *(orchestrator,review)* wire agentic verifier behind AR_AGENTIC_VERIFIER
- *(tools,review)* oxlint runner. 18th bundled linter.
- *(tools,review)* taplo runner. 19th bundled linter.
- *(tools,review)* checkov runner. 20th bundled linter.
- *(tools,review)* dotenv-linter runner. 21st bundled linter.
- *(chat)* @auto_review autofix posts inline suggestion patches
- *(chat)* @auto_review docstring generates docstrings as inline patches
- *(chat)* @auto_review tests scaffolds unit tests for new items
- *(gateway,forgejo,orchestrator)* polling fallback for review-thread mentions
- *(tools,review)* kubeconform runner. 22nd bundled linter.
- *(tools,review)* mypy runner. 23rd bundled linter.
- *(tools,review)* bandit runner. 24th bundled linter.
- *(tools,review)* vale runner. 25th bundled linter.
- *(tools,review)* swiftlint runner. 26th bundled linter.
- *(tools,review)* buf runner. 27th bundled linter.
- *(tools,review)* typos runner. 28th bundled linter.
- *(tools,review)* cppcheck runner. 29th bundled linter.
- *(tools,review)* pmd runner. 30th bundled linter.
- *(tools,review)* gosec runner. 31st bundled linter.
- *(tools,review)* ansible-lint runner. 32nd bundled linter.
- *(tools,review)* tflint runner. 33rd bundled linter.
- *(tools,review)* staticcheck runner. 34th bundled linter.
- *(tools,review)* stylelint runner. 35th bundled linter.
- *(tools,review)* ktlint runner. 36th bundled linter.
- *(tools,review)* pylint runner. 37th bundled linter.
- *(tools,review)* htmlhint runner. 38th bundled linter.
- *(tools,review)* prettier runner. 39th bundled linter.
- *(tools,review)* helm runner. 40th bundled linter.
- *(tools,review)* shfmt runner. 41st bundled linter.
- *(tools,review)* vint runner. 42nd bundled linter.
- *(tools,review)* nilaway runner. 43rd bundled linter.
- *(tools,review)* jsonlint runner. 44th bundled linter.
- *(cli)* bench harness scores precision/recall against labelled fixtures
- *(cli)* validate-config subcommand for .auto_review.yaml files
- *(gateway)* /metrics endpoint with Prometheus-format counters
- *(metrics)* review-pipeline outcome counters via ReviewObserver
- *(cli,tools)* list-linters subcommand + canonical catalogue
- *(cli)* test-webhook subcommand for deploy smoke-tests
- *(cli)* doctor subcommand for deployment health checks
- *(cli)* doctor verifies configured LLM model names are loaded
- *(metrics)* proper Prometheus histogram for review duration
- *(gateway)* /readyz endpoint distinct from /healthz
- *(metrics)* poller observability counters
- *(review)* pre-merge checks alongside the LLM review
- *(review)* custom natural-language pre-merge checks
- *(review)* linter-only review mode
- *(cli,forgejo)* list-webhooks and unregister-webhook
- *(gateway)* GET /info runtime-config snapshot
- *(gateway)* webhook rate limiter (T7 mitigation)
- *(bench)* --baseline FILE comparison + --fail-on-regression
- *(cli)* status subcommand for live gateway snapshot
- *(cli,review)* validate-config --strict catches typo'd keys
- *(review)* AR_SEVERITY_FLOOR for signal-to-noise tuning
- *(gateway)* webhook delivery dedup via X-Forgejo-Delivery
- *(orchestrator)* SqliteReviewHistory for restart-survival
- *(gateway)* graceful shutdown on SIGTERM/SIGINT
- *(cli)* reset-pr subcommand for ad-hoc history clears
- *(cli)* list-learnings + forget-learning admin commands
- *(cli,orchestrator)* purge-history for long-running deploys
- *(orchestrator)* severity breakdown in commit-status description
- *(cli)* explain-routing for understanding linter dispatch
- *(metrics)* track verifier-dropped findings as a counter
- *(orchestrator)* AR_REVIEW_CONCURRENCY caps in-flight reviews
- *(metrics)* track queue waits on the concurrency cap
- *(review)* defense-in-depth path guard before posting findings
- *(gateway)* warn at startup when WEBHOOK_SECRET is too short
- *(bench)* expand_fixture_paths now recurses into subdirectories
- *(doctor)* probe for git in PATH
- *(gateway)* probe git at startup, log result
- *(tools)* languagetool HTTP runner (opt-in via LANGUAGETOOL_URL)
- *(index)* SqliteVectorStore — persistent embeddings without protoc
- *(sandbox)* support docker as OCI runtime alongside podman
- persist runtime state across restarts (closes #19) (#25)
- *(gateway)* add CI-triggered review dispatch (#48)
- *(review)* retire bundled linter execution (#54)
- *(review)* suggest missing CI linters (#65)
- *(actions)* add CI review gateway action (#68)
- *(actions)* add Forgejo CI action and quiet compare fallback (#69)
- *(actions)* wire CI semantic review job (#70)
- *(actions)* automate release PR workflow (#74)
- *(release)* generate changelog from commits (#86)

### Fixed

- *(orchestrator)* log panics and cancellations from spawned review tasks
- *(orchestrator)* post a crash status when the review task panics
- *(gateway)* honour AR_BOT_LOGIN/AR_BOT_NAME in webhook chat path
- *(forgejo)* paginate list_changed_files for large PRs
- *(forgejo)* paginate list_pr_review_comments and list_webhooks
- *(orchestrator)* log when agentic verifier silently downgrades
- *(metrics)* unknown skip reasons no longer misbucket as disabled
- *(metrics)* unknown failure classes no longer misbucket as unhealable
- *(forgejo)* support subpath-deployed Forgejo by normalising base URL
- *(forgejo)* normalise InitClient base URL the same as main Client
- *(llm)* normalise OpenAI provider base URL for subpath deploys
- *(poller)* first-sight seeding must not dispatch historical mentions
- *(gateway)* bail on startup when LLM_REASONING_MODEL is empty
- *(gateway)* validate AR_BOT_LOGIN/NAME at startup, dedupe reads
- *(gateway)* treat empty LLM_*_MODEL env vars as unset
- *(gateway)* apply read_non_empty_env to remaining sandbox/db checks
- *(index)* batch embed_symbols calls to handle large repos
- *(chat)* autofix/docstrings drops patches outside the PR's diff
- *(bench)* success-rate drop now triggers --fail-on-regression
- *(poller)* case-insensitive self-loop match (matches webhook handler)
- *(chat)* cap freeform LLM reply size before posting to Forgejo
- *(orchestrator,metrics)* bucket review-task panics so counts add up
- *(orchestrator)* defer Started until after early-skip checks pass
- *(cli)* reject empty/weak webhook secrets at registration time
- *(chat)* grow autofix suggestion fence past internal backtick runs
- *(chat)* grow test-scaffold fence past internal backtick runs
- *(orchestrator)* cap commit-status description at 240 chars
- *(workspace_tools)* skip symlinks during recursive search
- *(chat)* cap remember text at 4 KiB before embedding/storing
- *(chat)* cap freeform question text at 4 KiB before LLM call
- *(workspace)* reject non-hex head_sha before reaching git argv
- *(forgejo)* mark auth header sensitive on the main Client
- *(prompts)* cap PR title (512 B) and body (8 KiB) before LLM call
- *(prompts)* cap guidelines (8 KiB) and repo_context (16 KiB) too
- *(prompts)* cap rendered linter findings at 100 with overflow summary
- *(config)* cap .auto_review.yaml at 64 KiB before read
- *(workspace_tools)* cap scan_file/read_file at 1 MiB per file
- *(workspace_tools)* cap walk_dir recursion depth at 64 levels
- *(index)* cap index_workspace walker depth at 64 (parity with verifier)
- *(gateway)* warn when only one half of rate-limit env vars is set
- *(gateway)* warn when env-var integer parses fail (don't silently default)
- *(bench)* bail on empty LLM_BASE_URL or LLM_REASONING_MODEL
- *(chat)* cap autofix patch reason (1 KiB) and replacement (4 KiB)
- *(chat)* cap test-scaffold source at 4 KiB before posting
- *(chat)* case-insensitive mention parsing matches Forgejo's UI
- *(poller)* case-insensitive mention pre-filter (parity with parser)
- *(llm)* cap LLM provider error response body at 1 KiB
- *(forgejo)* cap Forgejo error response body at 1 KiB
- *(forgejo)* apply cap_for_error to remaining direct API call sites
- *(triage)* cap embedded diff at 40 KiB before cheap-tier call
- *(context)* cap diff at 32 KiB before embedding for RAG query
- *(verify)* cap embedded diff at 40 KiB before cheap-tier call
- *(agentic_verify)* cap initial diff at 40 KiB (parity with simple verifier)
- *(pre_merge_llm)* cap embedded diff at 40 KiB before cheap-tier call
- *(chat)* skip re-review on closed/merged PRs
- *(chat)* skip autofix/docstrings/tests on closed PRs (parity with re_review)
- *(review)* verifier_dropped now excludes path-guard drops
- *(chat)* autofix distinguishes "no patches" from "all hallucinated"
- *(chat)* test scaffolds distinguishes "no coverage needed" from malformed
- *(webhook)* skip non-Created issue_comment actions
- *(triage)* pure-removal PRs are not skippable
- *(orchestrator)* only record review history on successful reviews
- *(review)* cap rendered review body at 32 KiB before posting
- *(review)* cap each finding's message at 4 KiB before posting
- *(poller)* prune cursor entries for purged PRs each cycle
- *(llm)* set temperature=0 on every deterministic LLM call
- *(poller)* Delay missed-tick behaviour avoids catch-up bursts
- *(chat)* paths_in_diff handles git's quoted-path form
- *(ignored)* extract_diff_path handles git's quoted-path form too
- *(ci)* satisfy rustfmt + clippy gates on M0 deliverables
- *(ci)* switch runs-on to ubuntu-latest; install Nix via DeterminateSystems
- *(ci)* use registered 'docker' label; install Nix via official script
- *(pre-merge)* scan every marker occurrence in contains_todo_marker (#4)
- *(post)* default AR_SEVERITY_FLOOR to warning (#6) (#17)
- *(embed)* size embedding pass for local Ollama (#20) (#21)
- *(rag)* wire diff embed through EmbedConfig (closes #26) (#27)
- *(dev)* use separate bacon gateway port (#33)
- *(dev)* recover bacon run after transient errors (#34)
- *(review)* detect inline Rust tests in pre-merge checks (#36)
- *(review)* request changes for failed pre-merge checks (#38)
- *(nix)* align cargo-fmt rustfmt with dev shell (#39)
- *(review)* show linter run summaries in reviews (#40)
- *(review)* approve clean re-reviews (#53)
- *(gateway)* separate Forgejo token env (#61)
- *(gateway)* handle bot review requests (#60)
- *(gateway)* deduplicate chat mentions (#62)
- *(review)* drop pre-merge checks (#64)
- *(review)* rescope normal sandbox runtime (#67)
- *(gateway)* gate semantic reviews behind CI (#72)
- *(forgejo)* use web compare diff route (#73)
- *(actions)* trigger release prep on main (#75)
- *(actions)* use Forgejo release prep auth (#76)
- *(actions)* read release prep token from environment (#77)
- *(actions)* fall back to compatible repo token (#78)
- *(actions)* require release prep token (#79)
- *(actions)* stabilize release automation auth (#81)
- *(actions)* allow release checks after prepare (#83)
- *(actions)* keep release lockfile current (#84)
- *(actions)* stage release lockfile (#85)

### Other

- untrack ralph-loop session state
- QUICKSTART walkthrough + README status update
- CHANGELOG.md cataloging the cumulative pre-0.1.0 work
- *(quickstart)* document the review-once smoke-test flow
- add LICENSE (AGPL-3.0-or-later) with rationale
- example .auto_review.yaml with annotated fields
- CONTRIBUTING.md with workflow + testing + architecture notes
- refresh CHANGELOG with the Milestone 2 RAG groundwork
- *(review)* bundle review_pull_request inputs into ReviewArgs struct
- *(tools,review)* route every linter spawn through ar_sandbox::Sandbox
- *(readme)* document the AR_SANDBOX_IMAGE / production sandbox path
- *(changelog)* catch up entries for verifier, sandbox, chat, persistent learnings
- *(adr)* ADR-0002 captures the linter sandbox decision
- *(changelog,cli)* freshen CHANGELOG; correct ar-cli crate description
- bring linter table + status to 14 (sqlfluff + osv-scanner)
- *(bench)* assert shipped bench/fixtures parse against the Fixture struct
- *(contributing)* refresh for 17-linter set + sandbox + bench
- *(changelog)* document agentic verifier substrate + the wiring follow-up
- *(changelog)* agentic verifier wiring complete; remove TODO entry
- *(red-team)* adversarial-input coverage for workspace tools + sandbox argv
- threat model document
- operations runbook
- drop-in Prometheus rules pack
- drop-in Grafana dashboard
- expand labelled corpus from 1 to 5 fixtures
- USER-GUIDE.md for PR authors
- hardened systemd unit for non-container self-hosters
- SECURITY.md vulnerability disclosure policy
- cargo-deny supply-chain config + CI integration
- *(red-team)* pin threat-model mitigations as CI tests
- ADR-0003 observability and runtime introspection
- per-crate READMEs for all 11 workspace crates
- project-tooling polish (.dockerignore, PR template, Renovate)
- *(review)* apply severity floor BEFORE the verifier
- CONTRIBUTING cookbook for new CLI + chat commands
- QUICKSTART.md refresh covers operator triad + new env vars
- AutoReviewQueueSaturation alert for the cap
- Grafana dashboard panels for quality + saturation
- *(tools)* contract test linking catalogue to sandbox image
- *(config)* example yaml + contract test for RepoConfig drift
- *(cli)* README contract test catches subcommand drift
- stale numeric stats sweep
- README freshness sweep — remove stale claims
- *(contributing)* sweep stale prereq + reorder build commands
- *(helm)* wire env vars added since the chart shipped
- *(chat)* contract test linking HELP_TEXT to ChatCommand variants
- cargo fmt sweep for rustfmt 1.8.0 stable style drift
- *(chat)* round-trip HELP_TEXT keywords through the parser
- *(review)* embed the diff once per RAG context build
- *(metrics)* contract test pinning every counter exposed at /metrics
- *(cli)* init's next-step hint suggests `openssl rand -hex 32`
- *(diff)* consolidate cheap-tier diff caps into one helper
- *(context)* use cap_for_prompt helper for embed query cap too
- *(orchestrator)* fetch list_changed_files once, share with lint phase
- *(orchestrator)* share raw_diff between lint phase and pipeline
- *(sandbox)* container-escape harness exercises real podman
- *(e2e)* runbook + Gherkin scenarios for real-Forgejo verification
- release announcement copy for Codeberg, r/selfhosted, lobste.rs
- *(adr-0002)* pin podman/docker as production sandbox; youki = future
- *(orchestrator)* synthetic e2e drives webhook-to-review in-process
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- refresh dev-setup, status, runbook for the nightly+flake reality
- gitignore .claude/scheduled_tasks.lock (loop runtime state)
- persistent Nix store volume for fast warm-cache builds (#1)
- drop push:main trigger; only run CI on pull_request (#2)
- register han marketplace and enable plugins (#3)
- untrack .envrc and gitignore it (#16)
- add run-ar-gateway skill and zellij to devshell (#18)
- add CLAUDE.md for future Claude Code sessions (#22)
- *(claude)* add TDD + review-reply guardrail hooks (#23)
- replace zellij watcher skill with bacon (#24)
- *(kilo)* add project workflow configuration (#32)
- document Forgejo comment resolution gap (#35)
- *(kilo)* relax agent step limits (#37)
- *(kilo)* route TDD through RGR agents (#47)
- *(kilo)* configure Forgejo MCP (#56)
- repo maintenance (#57)
- *(kilo)* configure rust-analyzer lsp (#59)
