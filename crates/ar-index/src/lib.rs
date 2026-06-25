//! Repository indexing and retrieval support.
//!
//! Provides tree-sitter symbol extraction, embedding helpers, SQLite and
//! in-memory vector stores, co-change graph utilities, and learnings stores used
//! by review context construction and chat `remember`/`forget` workflows.

pub mod co_change;
pub mod dynamodb_learnings;
pub mod embed;
pub mod learnings;
pub mod sqlite_learnings;
pub mod sqlite_vector_store;
pub mod symbols;
pub mod vector_store;
pub mod walker;

pub use co_change::{compute_co_change, parse_git_log_co_change, CoChangeError, CoChangeGraph};
pub use dynamodb_learnings::{
    AwsDynamoDbLearningsClient, DynamoDbLearningsClient, DynamoDbLearningsStore,
};
pub use embed::{
    embed_symbols, embed_symbols_with_config, EmbedConfig, EmbedError, EmbeddedSymbol,
    DEFAULT_EMBED_BATCH_SIZE, DEFAULT_EMBED_INPUT_CAP_BYTES,
};
pub use learnings::{
    InMemoryLearningsStore, LearningRecord, LearningSource, LearningsError, LearningsStore,
    ScoredLearning,
};
pub use sqlite_learnings::SqliteLearningsStore;
pub use sqlite_vector_store::SqliteVectorStore;
pub use symbols::{
    extract_go_symbols, extract_python_symbols, extract_rust_symbols, extract_symbols_for_path,
    extract_tsx_symbols, extract_typescript_symbols, ExtractError, Symbol, SymbolKind,
};
pub use vector_store::{InMemoryVectorStore, ScoredSymbol, VectorStore, VectorStoreError};
pub use walker::{index_workspace, IndexedSymbol, WalkError};
