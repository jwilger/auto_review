# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://git.johnwilger.com/jwilger/auto_review/releases/tag/ar-index-v0.1.0) - 2026-05-05

### Added

- persist runtime state across restarts (closes #19) ([#25](https://git.johnwilger.com/jwilger/auto_review/pulls/25))
- *(index)* SqliteVectorStore — persistent embeddings without protoc
- *(index)* SQLite-backed LearningsStore for cross-restart persistence
- *(index)* tree-sitter symbol extraction for Go
- *(index)* persistent learnings store (in-memory implementation)
- *(index)* VectorStore trait + InMemoryVectorStore implementation
- *(index)* symbol-level embedding pass via the LLM router
- *(index)* co-change graph from git history
- *(index)* index_workspace walks a clone and emits IndexedSymbols
- *(index)* extract_symbols_for_path dispatches by file extension
- *(index)* tree-sitter symbol extraction for TypeScript and TSX
- *(index)* tree-sitter symbol extraction for Python
- *(index)* tree-sitter symbol extraction for Rust (Milestone 2 RAG groundwork)
- bootstrap workspace for Forgejo AI PR reviewer

### Fixed

- *(embed)* size embedding pass for local Ollama ([#20](https://git.johnwilger.com/jwilger/auto_review/pulls/20)) ([#21](https://git.johnwilger.com/jwilger/auto_review/pulls/21))
- *(pre-merge)* scan every marker occurrence in contains_todo_marker ([#4](https://git.johnwilger.com/jwilger/auto_review/pulls/4))
- *(ci)* satisfy rustfmt + clippy gates on M0 deliverables
- *(index)* cap index_workspace walker depth at 64 (parity with verifier)
- *(index)* batch embed_symbols calls to handle large repos

### Other

- *(toolchain)* switch to rust nightly via flake-pinned snapshot
- *(ci)* pin toolchain via flake.nix; CI runs nix flake check
- per-crate READMEs for all 11 workspace crates
