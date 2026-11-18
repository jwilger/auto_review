//! Repository indexing and retrieval support.
//!
//! Provides tree-sitter symbol extraction, embedding helpers, SQLite and
//! in-memory vector stores, and learnings stores used by review context
//! construction and chat `remember`/`forget` workflows.

extern crate alloc;

pub mod dynamodb_learnings;
pub mod embed;
pub mod learnings;
pub mod repo_scope;
pub mod sqlite_learnings;
pub mod sqlite_vector_store;
pub mod symbols;
pub mod vector_store;
pub mod walker;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-chat, ar-cli, ar-gateway, ar-orchestrator and ar-review import these items from \
              the crate root (for example ar_index::LearningsStore and \
              ar_index::embed_symbols_with_config); dropping the re-exports breaks them"
)]
pub use crate::{
    dynamodb_learnings::{
        AwsDynamoDbLearningsClient, DynamoDbLearningsClient, DynamoDbLearningsStore,
    },
    embed::{
        embed_symbols, embed_symbols_with_config, EmbedConfig, EmbedError, EmbeddedSymbol,
        DEFAULT_EMBED_BATCH_SIZE, DEFAULT_EMBED_INPUT_CAP_BYTES,
    },
    learnings::{
        InMemoryLearningsStore, LearningRecord, LearningSource, LearningsError, LearningsFilter,
        LearningsStore, ScoredLearning,
    },
    repo_scope::{parse_legacy_correction_repo, RepoScope},
    sqlite_learnings::SqliteLearningsStore,
    sqlite_vector_store::SqliteVectorStore,
    symbols::{
        extract_go_symbols, extract_python_symbols, extract_rust_symbols, extract_symbols_for_path,
        extract_tsx_symbols, extract_typescript_symbols, ExtractError, Symbol, SymbolKind,
    },
    vector_store::{InMemoryVectorStore, ScoredSymbol, VectorStore, VectorStoreError},
    walker::{index_workspace, IndexedSymbol, WalkError},
};
