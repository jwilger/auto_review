//! Repo indexing.
//!
//! Currently provides tree-sitter symbol extraction (Milestone 2 RAG
//! groundwork). Embeddings + LanceDB + co-change graph land in
//! follow-up commits.

pub mod symbols;

pub use symbols::{
    extract_python_symbols, extract_rust_symbols, extract_symbols_for_path, extract_tsx_symbols,
    extract_typescript_symbols, ExtractError, Symbol, SymbolKind,
};
