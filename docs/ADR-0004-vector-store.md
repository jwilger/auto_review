# ADR-0004: Vector store: SQLite now, LanceDB-ready

## Status

Accepted (2026-04-30).

## Context

The review pipeline embeds repo symbols and runs nearest-neighbour search to
seed an LLM's review context. Early planning called for **LanceDB** as the
vector store because it is open-source, embedded, written in Rust, and used by
CodeRabbit's public architecture.

Architecturally, the pipeline depends only on the
`ar_index::VectorStore` trait. The original plan was to ship an
`InMemoryVectorStore` first (no persistence) and a LanceDB-backed
impl later. With the in-memory impl already shipped, the question
became: persist via LanceDB or via SQLite?

## Decision

Ship a SQLite-backed `SqliteVectorStore` as the persistent
default. Keep `InMemoryVectorStore` for tests. Defer LanceDB —
the trait abstraction lets a `LanceDbVectorStore` drop in later
without touching callers.

## Why not LanceDB now?

1. **Build dependency: `protoc`.** LanceDB pulls in `lance-encoding`,
   which pulls in `prost-build`, which requires the `protoc`
   protobuf compiler at build time. That means every developer
   workstation, CI runner, and Dockerfile needs `protoc`
   installed. The project currently has zero protobuf usage.
2. **Transitive dependency weight.** Pulling in `lancedb 0.27`
   adds Apache Arrow + DataFusion + Lance — ~150 transitive
   crates. Cold compile time grows ~5–10×.
3. **Scale headroom is unused at this deployment shape.**
   auto_review targets one Forgejo instance. Realistic ceiling:
   ~tens of thousands of symbols across ~dozens of repos. Brute-
   force cosine over an in-memory `SELECT *` runs in ≤20 ms at
   that scale. LanceDB's IVF-PQ / HNSW indexes start to matter
   at ≥100k vectors with a sub-100 ms latency budget; we are
   1–2 orders of magnitude under that threshold.
4. **Persistence is the actual ops requirement.** The thing that
   broke about `InMemoryVectorStore` was "embeddings vanish on
   restart and the next review pays a full re-index." SQLite
   solves that without any new build deps (`sqlx` + `sqlite` is
   already in the workspace for `SqliteLearningsStore` and
   `SqliteReviewHistory`).

## What we lose by choosing SQLite

- **ANN index speed past 1M vectors.** SQLite cosine is O(n)
  full-scan; LanceDB stays sub-linear. Functionally equivalent
  results below ~100k vectors; worse latency above.
- **Columnar compression.** Lance stores embeddings ~2–3× more
  compactly on disk than a SQLite BLOB column.
- **Filter pushdown.** Hybrid `WHERE language='rust' ORDER BY
  vector_distance` queries can use a single Lance index;
  SQLite forces fetch-then-filter in app code.
- **Versioning / time-travel.** Lance has it; SQLite doesn't.
  Probably never matters for our use case.
- **The "matches CodeRabbit's stack" property.** A real
  divergence from the feasibility-study spec, called out
  explicitly here so future readers don't get confused about
  why we differ.

## Migration path

`SqliteVectorStore` and a future `LanceDbVectorStore` both
implement `VectorStore`. Swap is a one-line wiring change in
`SpawningDispatcher::with_vector_store(...)` plus the persistent-
storage migration path (read all rows out of SQLite → upsert
into LanceDB → drop the SQLite file). Trigger to revisit:

- a deployment reaches >100k symbols across its repo set, OR
- p95 RAG-context build time exceeds 1s, OR
- we want hybrid filter+vector queries.

## References

- `crates/ar-index/src/vector_store.rs` — the trait
- `crates/ar-index/src/sqlite_vector_store.rs` — this impl
- ADR-0001 — overall review-pipeline architecture
