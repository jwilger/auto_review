# ar-index

Code-symbol parsing, embeddings, and the persistent `LearningsStore`.

This crate's two big halves:

1. **Repo indexing** — tree-sitter symbol extraction + embedding
   for RAG retrieval (`embed`, `extract`, `parse`, `walk`).
2. **Learnings store** — repo-level guidance the bot accumulates
   from `@<bot> remember` chat commands and explicit guidelines.
   Two backings ship: in-memory (volatile) and SQLite (persistent
   across restart).

## Public surface

### RAG indexing

| Item | Purpose |
|------|---------|
| `walk::walk_workspace` | Lists files of supported languages, respecting `.gitignore`. |
| `parse::parse_file` | Tree-sitter symbol extraction (Rust, Python, TypeScript, Go for now). |
| `extract::Symbol` | Per-symbol record (name, kind, file, line range). |
| `embed::embed_symbols` | Batches symbols through the configured Embedding-tier LLM provider. |
| `cochange::CoChangeGraph` | Co-change graph from `git log --name-only`. Surfaces "files frequently changed together". |

### Learnings

| Item | Purpose |
|------|---------|
| `learnings::LearningsStore` (trait) | `add` / `list` / `remove` / `query_nearest`. |
| `learnings::LearningRecord`, `ScoredLearning`, `LearningSource` | DTOs. |
| `learnings::InMemoryLearningsStore` | Volatile across restart. |
| `sqlite_learnings::SqliteLearningsStore` | Persistent. Cosine-similarity computed in Rust over a full-table scan; fine for tens-to-low-thousands of rows per repo. A LanceDB backing is the next-step upgrade for higher-scale deployments. |

## Tests

`cargo test -p ar-index` covers tree-sitter extraction against
captured fixtures, the in-memory and SQLite learnings stores'
add/list/remove/query semantics, embedding-batch shape (with a
`DeterministicEmbedder` in tests), and the co-change graph's
"files that changed together" computation.

## Dependencies

`tree-sitter` + per-language grammar crates (Rust, Python, TS,
Go), `sqlx` for the SQLite store, `walkdir` for the file walker.
