# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-review-v0.1.0) - 2026-05-05

### Added

- *(review)* suggest missing CI linters ([#65](https://git.johnwilger.com/jwilger/auto_review/pulls/65))
- *(review)* retire bundled linter execution ([#54](https://git.johnwilger.com/jwilger/auto_review/pulls/54))
- persist runtime state across restarts (closes #19) ([#25](https://git.johnwilger.com/jwilger/auto_review/pulls/25))
- *(tools)* languagetool HTTP runner (opt-in via LANGUAGETOOL_URL)
- *(review)* defense-in-depth path guard before posting findings
- *(metrics)* track verifier-dropped findings as a counter
- *(orchestrator)* severity breakdown in commit-status description
- *(review)* AR_SEVERITY_FLOOR for signal-to-noise tuning
- *(cli,review)* validate-config --strict catches typo'd keys
- *(review)* linter-only review mode
- *(review)* custom natural-language pre-merge checks
- *(review)* pre-merge checks alongside the LLM review
- *(cli,tools)* list-linters subcommand + canonical catalogue
- *(cli)* validate-config subcommand for .auto_review.yaml files
- *(tools,review)* jsonlint runner. 44th bundled linter.
- *(tools,review)* nilaway runner. 43rd bundled linter.
- *(tools,review)* vint runner. 42nd bundled linter.
- *(tools,review)* shfmt runner. 41st bundled linter.
- *(tools,review)* helm runner. 40th bundled linter.
- *(tools,review)* prettier runner. 39th bundled linter.
- *(tools,review)* htmlhint runner. 38th bundled linter.
- *(tools,review)* pylint runner. 37th bundled linter.
- *(tools,review)* ktlint runner. 36th bundled linter.
- *(tools,review)* stylelint runner. 35th bundled linter.
- *(tools,review)* staticcheck runner. 34th bundled linter.
- *(tools,review)* tflint runner. 33rd bundled linter.
- *(tools,review)* ansible-lint runner. 32nd bundled linter.
- *(tools,review)* gosec runner. 31st bundled linter.
- *(tools,review)* pmd runner. 30th bundled linter.
- *(tools,review)* cppcheck runner. 29th bundled linter.
- *(tools,review)* typos runner. 28th bundled linter.
- *(tools,review)* buf runner. 27th bundled linter.
- *(tools,review)* swiftlint runner. 26th bundled linter.
- *(tools,review)* vale runner. 25th bundled linter.
- *(tools,review)* bandit runner. 24th bundled linter.
- *(tools,review)* mypy runner. 23rd bundled linter.
- *(tools,review)* kubeconform runner. 22nd bundled linter.
- *(tools,review)* dotenv-linter runner. 21st bundled linter.
- *(tools,review)* checkov runner. 20th bundled linter.
- *(tools,review)* taplo runner. 19th bundled linter.
- *(tools,review)* oxlint runner. 18th bundled linter.
- *(orchestrator,review)* wire agentic verifier behind AR_AGENTIC_VERIFIER
- *(review)* agentic verifier with sandboxed read_file/search tools
- *(tools,review)* phpstan runner. 17th bundled linter.
- *(tools,review)* biome runner. 16th bundled linter.
- *(tools,review)* ast-grep runner. 15th bundled linter.
- *(tools,review)* sqlfluff runner. 14th bundled linter.
- *(tools,review)* osv-scanner runner. 13th bundled linter.
- *(orchestrator,gateway)* production sandbox via AR_SANDBOX_IMAGE
- *(orchestrator,gateway)* share LearningsStore between chat and RAG
- *(tools,review)* trivy runner. 12th bundled linter.
- *(tools,review)* rubocop runner. 11th bundled linter.
- *(tools,review)* golangci-lint runner. 10th bundled linter.
- *(prompts,review)* verification agent (Milestone 3 piece)
- *(tools,review)* semgrep runner (multi-language SAST). 9th linter.
- *(orchestrator,review)* incremental diff via Client::get_compare_diff
- *(review)* build_review_context integrates the RAG layer end-to-end
- *(review)* format_repo_context renders RAG retrieval as prompt markdown
- *(prompts,review,cli)* repo_context field for RAG-retrieved context
- *(review)* triage_files_with_llm wires the cheap-tier classifier
- *(tools,review)* yamllint runner for general YAML
- *(tools,review)* actionlint runner for workflow YAML
- *(prompts,review,orchestrator)* inject .auto_review.yaml guidelines into LLM prompt
- *(review,orchestrator)* wire ignored_paths through review pipeline
- *(review)* glob-based ignored_paths utilities for diff/file filtering
- *(orchestrator)* honor .auto_review.yaml enabled + disabled_tools
- *(review)* repo-level .auto_review.yaml config loader
- *(review)* cap PR diff at 100 KiB before sending to LLM
- *(tools,review)* gitleaks runner for committed-secret detection
- *(prompts,review)* optional walkthrough + Mermaid diagram in review body
- *(review,orchestrator)* heuristic triage skips lockfile-only PRs
- *(tools,review)* eslint runner + routing for JS/TS extensions
- *(orchestrator,review)* wire clone+lint into the review pipeline
- *(review)* route changed files to language-specific linters
- *(review)* thread linter findings through review_pull_request
- *(review)* workspace prep with shallow clone + auth-stripping logs
- *(prompts)* inject static-analysis findings section into review prompt
- *(review)* single-pass review pipeline with self-heal validation
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(review)* rescope normal sandbox runtime ([#67](https://git.johnwilger.com/jwilger/auto_review/pulls/67))
- *(review)* drop pre-merge checks ([#64](https://git.johnwilger.com/jwilger/auto_review/pulls/64))
- *(review)* approve clean re-reviews ([#53](https://git.johnwilger.com/jwilger/auto_review/pulls/53))
- *(review)* show linter run summaries in reviews ([#40](https://git.johnwilger.com/jwilger/auto_review/pulls/40))
- *(review)* request changes for failed pre-merge checks ([#38](https://git.johnwilger.com/jwilger/auto_review/pulls/38))
- *(review)* detect inline Rust tests in pre-merge checks ([#36](https://git.johnwilger.com/jwilger/auto_review/pulls/36))
- *(rag)* wire diff embed through EmbedConfig (closes #26) ([#27](https://git.johnwilger.com/jwilger/auto_review/pulls/27))
- *(pre-merge)* scan every marker occurrence in contains_todo_marker ([#4](https://git.johnwilger.com/jwilger/auto_review/pulls/4))
- *(ignored)* extract_diff_path handles git's quoted-path form too
- *(llm)* set temperature=0 on every deterministic LLM call
- *(review)* cap each finding's message at 4 KiB before posting
- *(review)* cap rendered review body at 32 KiB before posting
- *(triage)* pure-removal PRs are not skippable
- *(review)* verifier_dropped now excludes path-guard drops
- *(pre_merge_llm)* cap embedded diff at 40 KiB before cheap-tier call
- *(agentic_verify)* cap initial diff at 40 KiB (parity with simple verifier)
- *(verify)* cap embedded diff at 40 KiB before cheap-tier call
- *(context)* cap diff at 32 KiB before embedding for RAG query
- *(triage)* cap embedded diff at 40 KiB before cheap-tier call
- *(workspace_tools)* cap walk_dir recursion depth at 64 levels
- *(workspace_tools)* cap scan_file/read_file at 1 MiB per file
- *(config)* cap .auto_review.yaml at 64 KiB before read
- *(workspace)* reject non-hex head_sha before reaching git argv
- *(workspace_tools)* skip symlinks during recursive search

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- *(context)* use cap_for_prompt helper for embed query cap too
- *(diff)* consolidate cheap-tier diff caps into one helper
- *(review)* embed the diff once per RAG context build
- cargo fmt sweep for rustfmt 1.8.0 stable style drift
- *(config)* example yaml + contract test for RepoConfig drift
- *(review)* apply severity floor BEFORE the verifier
- per-crate READMEs for all 11 workspace crates
- *(red-team)* pin threat-model mitigations as CI tests
- *(red-team)* adversarial-input coverage for workspace tools + sandbox argv
- *(tools,review)* route every linter spawn through ar_sandbox::Sandbox
- *(review)* bundle review_pull_request inputs into ReviewArgs struct
