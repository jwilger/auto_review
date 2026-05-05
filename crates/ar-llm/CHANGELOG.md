# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-llm-v0.1.0) - 2026-05-05

### Added

- *(llm)* OpenAI-compatible provider and tier-based router
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(embed)* size embedding pass for local Ollama ([#20](https://git.johnwilger.com/jwilger/auto_review/pulls/20)) ([#21](https://git.johnwilger.com/jwilger/auto_review/pulls/21))
- *(llm)* cap LLM provider error response body at 1 KiB
- *(llm)* normalise OpenAI provider base URL for subpath deploys

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- per-crate READMEs for all 11 workspace crates
