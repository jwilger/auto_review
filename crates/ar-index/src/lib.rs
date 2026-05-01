//! Repo indexing: tree-sitter symbol extraction, LanceDB embeddings, and
//! a co-change graph derived from `git log --name-only`.
//!
//! The index serves both review-time context retrieval and the persistent
//! "learnings" store.
