# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-forgejo-v0.1.0) - 2026-05-05

### Added

- *(cli,forgejo)* list-webhooks and unregister-webhook
- *(gateway,forgejo,orchestrator)* polling fallback for review-thread mentions
- *(forgejo,chat)* post_issue_comment + ChatHandler with help/remember/forget
- *(forgejo,gateway,chat)* wire issue_comment events through chat command parser
- *(forgejo)* get_compare_diff for incremental review support
- *(forgejo)* get_pull_request returns a PullRequestSummary
- *(forgejo,gateway)* /version endpoint + Forgejo get_server_version
- *(forgejo)* InitClient + create_access_token + create_webhook
- *(forgejo,gateway)* implement Forgejo client and webhook intake
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(forgejo)* use web compare diff route ([#73](https://git.johnwilger.com/jwilger/auto_review/pulls/73))
- *(gateway)* handle bot review requests ([#60](https://git.johnwilger.com/jwilger/auto_review/pulls/60))
- *(pre-merge)* scan every marker occurrence in contains_todo_marker ([#4](https://git.johnwilger.com/jwilger/auto_review/pulls/4))
- *(chat)* skip re-review on closed/merged PRs
- *(forgejo)* apply cap_for_error to remaining direct API call sites
- *(forgejo)* cap Forgejo error response body at 1 KiB
- *(forgejo)* mark auth header sensitive on the main Client
- *(forgejo)* normalise InitClient base URL the same as main Client
- *(forgejo)* support subpath-deployed Forgejo by normalising base URL
- *(forgejo)* paginate list_pr_review_comments and list_webhooks
- *(forgejo)* paginate list_changed_files for large PRs

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- cargo fmt sweep for rustfmt 1.8.0 stable style drift
- per-crate READMEs for all 11 workspace crates
