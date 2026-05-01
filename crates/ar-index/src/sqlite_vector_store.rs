//! SQLite-backed [`VectorStore`] for persistent symbol embeddings
//! across orchestrator restarts.
//!
//! Mirrors the [`SqliteLearningsStore`](crate::SqliteLearningsStore)
//! pattern: BLOB-encoded f32 vectors, full-table-scan cosine
//! similarity. Sufficient for tens-of-thousands of symbols (the
//! scale a single Forgejo instance's repos accumulate); LanceDB
//! drops in behind the same trait when ANN search becomes worth
//! the protoc + Arrow build-dep weight.

use crate::embed::EmbeddedSymbol;
use crate::symbols::{Symbol, SymbolKind};
use crate::vector_store::{ScoredSymbol, VectorStore, VectorStoreError};
use crate::walker::IndexedSymbol;
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Row, Sqlite};
use std::path::Path;
use std::str::FromStr;

const SCHEMA: &str = r#"
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
"#;

pub struct SqliteVectorStore {
    pool: Pool<Sqlite>,
}

impl SqliteVectorStore {
    /// Open or create a database at `path`. Schema is applied
    /// idempotently on first connect.
    pub async fn open(path: &Path) -> Result<Self, VectorStoreError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA)
            .execute(&pool)
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Open a fresh in-memory database. For tests and for callers
    /// that want SQLite-shaped semantics without persistence.
    pub async fn in_memory() -> Result<Self, VectorStoreError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA)
            .execute(&pool)
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }

    pub async fn len(&self) -> Result<usize, VectorStoreError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM vector_symbols")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        let n: i64 = row.get("n");
        Ok(n as usize)
    }
}

#[async_trait]
impl VectorStore for SqliteVectorStore {
    async fn upsert(&self, symbols: &[EmbeddedSymbol]) -> Result<(), VectorStoreError> {
        if symbols.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        for sym in symbols {
            let bytes = vec_f32_to_bytes(&sym.embedding);
            let kind_str = symbol_kind_to_str(sym.indexed.symbol.kind);
            sqlx::query(
                r#"
                INSERT INTO vector_symbols
                    (path, name, kind, line_start, line_end, content, embedding)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(path, name, line_start) DO UPDATE SET
                    kind       = excluded.kind,
                    line_end   = excluded.line_end,
                    content    = excluded.content,
                    embedding  = excluded.embedding
                "#,
            )
            .bind(&sym.indexed.path)
            .bind(&sym.indexed.symbol.name)
            .bind(kind_str)
            .bind(sym.indexed.symbol.line_start as i64)
            .bind(sym.indexed.symbol.line_end as i64)
            .bind(&sym.content)
            .bind(&bytes)
            .execute(&mut *tx)
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| VectorStoreError::Storage(e.to_string()))?;
        Ok(())
    }

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
        .map_err(|e| VectorStoreError::Storage(e.to_string()))?;

        let mut scored: Vec<ScoredSymbol> = Vec::with_capacity(rows.len());
        for row in rows {
            let symbol = row_to_embedded(row)?;
            let score = cosine_similarity(query, &symbol.embedding);
            scored.push(ScoredSymbol { symbol, score });
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }
}

fn row_to_embedded(row: sqlx::sqlite::SqliteRow) -> Result<EmbeddedSymbol, VectorStoreError> {
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

fn symbol_kind_to_str(k: SymbolKind) -> &'static str {
    match k {
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

fn symbol_kind_from_str(s: &str) -> Result<SymbolKind, VectorStoreError> {
    match s {
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

fn vec_f32_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn bytes_to_vec_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn empty_store_query_returns_no_matches() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        assert_eq!(store.len().await.unwrap(), 0);
        let r = store.query_nearest(&[1.0, 0.0], 5).await.unwrap();
        assert!(r.is_empty());
    }

    #[tokio::test]
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
    async fn upsert_with_empty_slice_is_noop_and_does_not_error() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        store.upsert(&[]).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 0);
    }

    #[tokio::test]
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
    async fn embedding_roundtrip_preserves_floats() {
        let store = SqliteVectorStore::in_memory().await.expect("open");
        let original = vec![0.1f32, -0.2, std::f32::consts::PI, -1e6, 0.0];
        store
            .upsert(&[esym("a.rs", "v", 1, original.clone())])
            .await
            .unwrap();
        let r = store.query_nearest(&original, 1).await.unwrap();
        assert_eq!(r[0].symbol.embedding, original);
    }

    #[tokio::test]
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
            let mut s = esym("a.rs", &format!("s{i}"), (i as u32) + 1, vec![1.0, 0.0]);
            s.indexed.symbol.kind = *kind;
            store.upsert(&[s]).await.unwrap();
        }
        let r = store.query_nearest(&[1.0, 0.0], 100).await.unwrap();
        assert_eq!(r.len(), kinds.len());
        // Verify each kind round-tripped through the DB intact.
        let mut got: Vec<SymbolKind> =
            r.into_iter().map(|s| s.symbol.indexed.symbol.kind).collect();
        got.sort_by_key(|k| symbol_kind_to_str(*k));
        let mut expected: Vec<SymbolKind> = kinds.to_vec();
        expected.sort_by_key(|k| symbol_kind_to_str(*k));
        assert_eq!(got, expected);
    }

    #[tokio::test]
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
