//! SQLite-backed [`VectorStore`] for persistent symbol embeddings
//! across orchestrator restarts.
//!
//! Mirrors the [`SqliteLearningsStore`](crate::SqliteLearningsStore)
//! pattern: BLOB-encoded f32 vectors, full-table-scan cosine
//! similarity. Sufficient for tens-of-thousands of symbols (the
//! scale a single Forgejo instance's repos accumulate); `LanceDB`
//! drops in behind the same trait when ANN search becomes worth
//! the protoc + Arrow build-dep weight.

use crate::embed::EmbeddedSymbol;
use crate::symbols::{Symbol, SymbolKind};
use crate::vector_store::{ScoredSymbol, VectorStore, VectorStoreError};
use crate::walker::IndexedSymbol;
use async_trait::async_trait;
use core::cmp::Ordering;
use core::str::FromStr as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Pool, Row as _, Sqlite};
use std::collections::HashMap;
use std::path::Path;

/// Idempotent DDL for the `vector_symbols` table, keyed by
/// `(path, name, line_start)`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS vector_symbols (
    path        TEXT NOT NULL,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    line_start  INTEGER NOT NULL,
    line_end    INTEGER NOT NULL,
    content     TEXT NOT NULL,
    embedding   BLOB NOT NULL,
    PRIMARY KEY (path, name, line_start)
);
";

/// [`VectorStore`] persisted in a SQLite database.
pub struct SqliteVectorStore {
    /// Connection pool over the database holding `vector_symbols`.
    pool: Pool<Sqlite>,
}

impl SqliteVectorStore {
    /// Open a fresh in-memory database. For tests and for callers
    /// that want SQLite-shaped semantics without persistence.
    ///
    /// # Errors
    ///
    /// Returns [`VectorStoreError::Storage`] when the database cannot be
    /// opened or the schema cannot be applied.
    #[inline]
    pub async fn in_memory() -> Result<Self, VectorStoreError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        sqlx::query(SCHEMA)
            .execute(&pool)
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        Ok(Self { pool })
    }

    /// Whether the store holds no symbols.
    ///
    /// # Errors
    ///
    /// Returns [`VectorStoreError::Storage`] when the count query fails.
    #[inline]
    pub async fn is_empty(&self) -> Result<bool, VectorStoreError> {
        Ok(self.len().await? == 0)
    }

    /// Number of stored symbols.
    ///
    /// # Errors
    ///
    /// Returns [`VectorStoreError::Storage`] when the count query fails.
    #[inline]
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "COUNT(*) is never negative and a row count of an on-disk table fits usize"
    )]
    pub async fn len(&self) -> Result<usize, VectorStoreError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM vector_symbols")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        let n: i64 = row.get("n");
        Ok(n as usize)
    }

    /// Open or create a database at `path`. Schema is applied
    /// idempotently on first connect.
    ///
    /// # Errors
    ///
    /// Returns [`VectorStoreError::Storage`] when the database cannot be
    /// opened or created, or the schema cannot be applied.
    #[inline]
    pub async fn open(path: &Path) -> Result<Self, VectorStoreError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        sqlx::query(SCHEMA)
            .execute(&pool)
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl VectorStore for SqliteVectorStore {
    #[inline]
    async fn fetch_by_keys(
        &self,
        keys: &[(String, String, u32)],
    ) -> Result<HashMap<(String, String, u32), EmbeddedSymbol>, VectorStoreError> {
        let mut out = HashMap::new();
        if keys.is_empty() {
            return Ok(out);
        }
        // SQLite parameter limit defaults to 999. Typical RAG calls
        // see a few hundred symbols max; chunking to 200 keeps us
        // well clear and is still one round-trip per chunk.
        for chunk in keys.chunks(200) {
            let mut sql = String::from(
                "SELECT path, name, kind, line_start, line_end, content, embedding \
                 FROM vector_symbols WHERE ",
            );
            let clauses: Vec<&str> =
                vec!["(path = ? AND name = ? AND line_start = ?)"; chunk.len()];
            sql.push_str(&clauses.join(" OR "));
            // The statement is assembled solely from static fragments and
            // `?` placeholders; every caller-supplied value is bound below.
            let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql));
            for key in chunk {
                statement = statement.bind(&key.0).bind(&key.1).bind(i64::from(key.2));
            }
            let rows = statement
                .fetch_all(&self.pool)
                .await
                .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
            for row in rows {
                let symbol = row_to_embedded(&row)?;
                let key = (
                    symbol.indexed.path.clone(),
                    symbol.indexed.symbol.name.clone(),
                    symbol.indexed.symbol.line_start,
                );
                out.insert(key, symbol);
            }
        }
        Ok(out)
    }

    #[inline]
    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredSymbol>, VectorStoreError> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT path, name, kind, line_start, line_end, content, embedding FROM vector_symbols",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| VectorStoreError::Storage(error.to_string()))?;

        let mut scored: Vec<ScoredSymbol> = Vec::with_capacity(rows.len());
        for row in rows {
            let symbol = row_to_embedded(&row)?;
            let score = cosine_similarity(query, &symbol.embedding);
            scored.push(ScoredSymbol { score, symbol });
        }
        scored.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }

    #[inline]
    async fn upsert(&self, symbols: &[EmbeddedSymbol]) -> Result<(), VectorStoreError> {
        if symbols.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        for sym in symbols {
            let bytes = vec_f32_to_bytes(&sym.embedding);
            let kind_str = symbol_kind_to_str(sym.indexed.symbol.kind);
            sqlx::query(
                "
                INSERT INTO vector_symbols
                    (path, name, kind, line_start, line_end, content, embedding)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(path, name, line_start) DO UPDATE SET
                    kind       = excluded.kind,
                    line_end   = excluded.line_end,
                    content    = excluded.content,
                    embedding  = excluded.embedding
                ",
            )
            .bind(&sym.indexed.path)
            .bind(&sym.indexed.symbol.name)
            .bind(kind_str)
            .bind(i64::from(sym.indexed.symbol.line_start))
            .bind(i64::from(sym.indexed.symbol.line_end))
            .bind(&sym.content)
            .bind(&bytes)
            .execute(&mut *tx)
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|error| VectorStoreError::Storage(error.to_string()))?;
        Ok(())
    }
}

/// Decode a little-endian f32 BLOB written by [`vec_f32_to_bytes`].
/// Trailing bytes that do not fill a whole f32 are ignored.
#[expect(
    clippy::little_endian_bytes,
    reason = "the embedding BLOB format is little-endian f32 by design; reading it any other way \
              would misdecode every stored vector"
)]
fn bytes_to_vec_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

/// Cosine similarity of two equal-length vectors; 0.0 when the lengths
/// differ, either is empty, or either has zero magnitude.
#[expect(
    clippy::float_arithmetic,
    reason = "cosine similarity over f32 embeddings is inherently float arithmetic"
)]
fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let dot: f32 = left.iter().zip(right.iter()).map(|(x, y)| x * y).sum();
    let norm_a = left.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = right.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Rebuild an [`EmbeddedSymbol`] from a `vector_symbols` row.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "line_start and line_end were written from u32 line numbers by upsert, so the \
              stored i64 values convert back to u32 losslessly"
)]
fn row_to_embedded(row: &SqliteRow) -> Result<EmbeddedSymbol, VectorStoreError> {
    let path: String = row.get("path");
    let name: String = row.get("name");
    let kind_str: String = row.get("kind");
    let line_start: i64 = row.get("line_start");
    let line_end: i64 = row.get("line_end");
    let content: String = row.get("content");
    let bytes: Vec<u8> = row.get("embedding");
    let kind = symbol_kind_from_str(&kind_str)?;
    Ok(EmbeddedSymbol {
        indexed: IndexedSymbol {
            path,
            symbol: Symbol {
                kind,
                name,
                line_start: line_start as u32,
                line_end: line_end as u32,
            },
        },
        content,
        embedding: bytes_to_vec_f32(&bytes),
    })
}

/// Parse the `kind` column written by [`symbol_kind_to_str`].
fn symbol_kind_from_str(stored: &str) -> Result<SymbolKind, VectorStoreError> {
    match stored {
        "function" => Ok(SymbolKind::Function),
        "struct" => Ok(SymbolKind::Struct),
        "enum" => Ok(SymbolKind::Enum),
        "trait" => Ok(SymbolKind::Trait),
        "impl" => Ok(SymbolKind::Impl),
        "module" => Ok(SymbolKind::Module),
        "type_alias" => Ok(SymbolKind::TypeAlias),
        "constant" => Ok(SymbolKind::Constant),
        "static" => Ok(SymbolKind::Static),
        "macro" => Ok(SymbolKind::Macro),
        other => Err(VectorStoreError::Storage(format!(
            "unknown SymbolKind variant in DB: {other}"
        ))),
    }
}

/// Stable `kind` column value for each [`SymbolKind`].
fn symbol_kind_to_str(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Module => "module",
        SymbolKind::TypeAlias => "type_alias",
        SymbolKind::Constant => "constant",
        SymbolKind::Static => "static",
        SymbolKind::Macro => "macro",
    }
}

/// Encode an embedding as a little-endian f32 BLOB.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the input slice already occupies len * 4 bytes in memory, so the byte count cannot \
              overflow usize"
)]
#[expect(
    clippy::little_endian_bytes,
    reason = "the embedding BLOB format is little-endian f32 by design; writing it any other way \
              would make stored vectors unreadable"
)]
fn vec_f32_to_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::PI;

    fn esym(path: &str, name: &str, line_start: u32, embedding: Vec<f32>) -> EmbeddedSymbol {
        EmbeddedSymbol {
            indexed: IndexedSymbol {
                path: path.into(),
                symbol: Symbol {
                    kind: SymbolKind::Function,
                    name: name.into(),
                    line_start,
                    line_end: line_start,
                },
            },
            content: format!("fn {name}() {{}}"),
            embedding,
        }
    }

    #[tokio::test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn empty_store_query_returns_no_matches() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        assert_eq!(store.len().await.unwrap(), 0);
        let r = store.query_nearest(&[1.0, 0.0], 5).await.unwrap();
        assert!(r.is_empty());
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn upsert_then_query_returns_results_ranked_by_similarity() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        store
            .upsert(&[
                esym("a.rs", "east", 1, vec![1.0, 0.0]),
                esym("a.rs", "ne", 2, vec![1.0, 1.0]),
                esym("a.rs", "north", 3, vec![0.0, 1.0]),
                esym("a.rs", "west", 4, vec![-1.0, 0.0]),
            ])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 4);

        let r = store.query_nearest(&[1.0, 0.0], 3).await.unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].symbol.indexed.symbol.name, "east");
        assert!(r[0].score > 0.99);
        assert_eq!(r[1].symbol.indexed.symbol.name, "ne");
        assert!(r[1].score > 0.7 && r[1].score < 0.71);
        assert_eq!(r[2].symbol.indexed.symbol.name, "north");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn upsert_replaces_record_with_same_primary_key() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        let v1 = esym("a.rs", "x", 1, vec![1.0, 0.0]);
        store.upsert(&[v1]).await.unwrap();

        // Same path + name + line_start, different embedding +
        // content; ON CONFLICT DO UPDATE should overwrite.
        let mut v2 = esym("a.rs", "x", 1, vec![0.0, 1.0]);
        v2.content = "fn x() { /* updated */ }".into();
        store.upsert(&[v2]).await.unwrap();

        assert_eq!(store.len().await.unwrap(), 1);
        let r = store.query_nearest(&[0.0, 1.0], 1).await.unwrap();
        assert!(r[0].score > 0.99);
        assert_eq!(r[0].symbol.embedding, vec![0.0, 1.0]);
        assert!(r[0].symbol.content.contains("updated"));
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn upsert_with_empty_slice_is_noop_and_does_not_error() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        store.upsert(&[]).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 0);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn top_k_caps_results() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        for i in 0..10 {
            store
                .upsert(&[esym("a.rs", &format!("s{i}"), i + 1, vec![1.0, 0.0])])
                .await
                .unwrap();
        }
        let r = store.query_nearest(&[1.0, 0.0], 3).await.unwrap();
        assert_eq!(r.len(), 3);
    }

    #[tokio::test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn top_k_zero_returns_empty_without_db_round_trip() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        store
            .upsert(&[esym("a.rs", "x", 1, vec![1.0])])
            .await
            .unwrap();
        let r = store.query_nearest(&[1.0], 0).await.unwrap();
        assert!(r.is_empty());
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn embedding_roundtrip_preserves_floats() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        let original: Vec<f32> = vec![0.1, -0.2, PI, -1e6, 0.0];
        store
            .upsert(&[esym("a.rs", "v", 1, original.clone())])
            .await
            .unwrap();
        let r = store.query_nearest(&original, 1).await.unwrap();
        assert_eq!(r[0].symbol.embedding, original);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::shadow_unrelated,
        reason = "`r` is rebound to each query result in turn and read by the assertions after it; renaming would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn cosine_handles_zero_vectors_and_mismatched_lengths() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        store
            .upsert(&[esym("a.rs", "zero", 1, vec![0.0, 0.0])])
            .await
            .unwrap();
        let r = store.query_nearest(&[1.0, 0.0], 1).await.unwrap();
        assert_eq!(r[0].score, 0.0);
        // Mismatched length → 0.0
        let r = store.query_nearest(&[1.0], 1).await.unwrap();
        assert_eq!(r[0].score, 0.0);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn open_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.sqlite");
        let store = SqliteVectorStore::open(&path).await.expect("open");
        store
            .upsert(&[esym("a.rs", "x", 1, vec![1.0])])
            .await
            .unwrap();
        // Drop the store and re-open: the row should still be there.
        drop(store);
        let reopened = SqliteVectorStore::open(&path).await.expect("reopen");
        assert_eq!(reopened.len().await.unwrap(), 1);
        let r = reopened.query_nearest(&[1.0], 1).await.unwrap();
        assert!(r[0].score > 0.99);
    }

    #[tokio::test]
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "test fixture values (small loop indices, ids and byte lengths) are far inside the target type's range, so the cast is exact"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn each_symbol_kind_roundtrips_through_storage() {
        // Catches drift: if a new SymbolKind variant is added but
        // the *_to_str / *_from_str pair is forgotten, this test
        // fails on the new variant.
        let store = SqliteVectorStore::in_memory().await.expect("open");
        let kinds = [
            SymbolKind::Function,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Trait,
            SymbolKind::Impl,
            SymbolKind::Module,
            SymbolKind::TypeAlias,
            SymbolKind::Constant,
            SymbolKind::Static,
            SymbolKind::Macro,
        ];
        for (i, kind) in kinds.iter().enumerate() {
            let mut symbol = esym("a.rs", &format!("s{i}"), (i as u32) + 1, vec![1.0, 0.0]);
            symbol.indexed.symbol.kind = *kind;
            store.upsert(&[symbol]).await.unwrap();
        }
        let r = store.query_nearest(&[1.0, 0.0], 100).await.unwrap();
        assert_eq!(r.len(), kinds.len());
        // Verify each kind round-tripped through the DB intact.
        let mut got: Vec<SymbolKind> = r
            .into_iter()
            .map(|scored| scored.symbol.indexed.symbol.kind)
            .collect();
        got.sort_by_key(|kind| symbol_kind_to_str(*kind));
        let mut expected: Vec<SymbolKind> = kinds.to_vec();
        expected.sort_by_key(|kind| symbol_kind_to_str(*kind));
        assert_eq!(got, expected);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn fetch_by_keys_returns_only_matching_keys() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        store
            .upsert(&[
                esym("a.rs", "foo", 1, vec![1.0]),
                esym("a.rs", "bar", 5, vec![0.5]),
            ])
            .await
            .unwrap();
        let got = store
            .fetch_by_keys(&[
                ("a.rs".into(), "foo".into(), 1),
                ("a.rs".into(), "missing".into(), 10),
            ])
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        let key: (String, String, u32) = ("a.rs".to_owned(), "foo".to_owned(), 1);
        let cached = got.get(&key).expect("foo present");
        assert_eq!(cached.embedding, vec![1.0]);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn distinct_paths_with_same_symbol_name_coexist() {
        // Two files defining `helper` at line 1 should produce
        // two rows; primary key includes path.
        let store = SqliteVectorStore::in_memory().await.expect("open");
        store
            .upsert(&[
                esym("a.rs", "helper", 1, vec![1.0, 0.0]),
                esym("b.rs", "helper", 1, vec![0.0, 1.0]),
            ])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 2);
    }
}
