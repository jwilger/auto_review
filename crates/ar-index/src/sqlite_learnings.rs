//! SQLite-backed [`LearningsStore`] for persistent storage of
//! repo-specific guidance across gateway restarts.
//!
//! Embeddings are stored as raw `f32` byte slices (host byte order —
//! the database file isn't expected to be portable across endianness;
//! all known production hosts are little-endian). Cosine similarity
//! is computed in Rust over a full table scan; for the tens-to-low-
//! thousands of rows a typical repo accumulates this is fine. A
//! LanceDB backing for ANN search at higher scale is still pending.

use crate::learnings::{
    LearningRecord, LearningSource, LearningsError, LearningsStore, ScoredLearning,
};
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Row, Sqlite};
use std::path::Path;
use std::str::FromStr;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS learnings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL,
    source TEXT NOT NULL,
    embedding BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
"#;

pub struct SqliteLearningsStore {
    pool: Pool<Sqlite>,
}

impl SqliteLearningsStore {
    /// Open or create a database at `path`. The schema is applied
    /// idempotently on first connect.
    pub async fn open(path: &Path) -> Result<Self, LearningsError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|e| LearningsError::Storage(e.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA)
            .execute(&pool)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Open a fresh in-memory database. Used by tests; useful as a
    /// drop-in for [`InMemoryLearningsStore`](crate::InMemoryLearningsStore)
    /// when you want SQLite-shaped semantics without touching disk.
    pub async fn in_memory() -> Result<Self, LearningsError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA)
            .execute(&pool)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl LearningsStore for SqliteLearningsStore {
    async fn add(
        &self,
        text: String,
        source: LearningSource,
        embedding: Vec<f32>,
        now: i64,
    ) -> Result<LearningRecord, LearningsError> {
        let bytes = vec_f32_to_bytes(&embedding);
        let source_str = source_to_str(source);
        let row = sqlx::query(
            r#"
            INSERT INTO learnings (text, source, embedding, created_at)
            VALUES (?, ?, ?, ?)
            RETURNING id
            "#,
        )
        .bind(&text)
        .bind(source_str)
        .bind(&bytes)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| LearningsError::Storage(e.to_string()))?;
        let id: i64 = row.get("id");
        Ok(LearningRecord {
            id: id as u64,
            text,
            source,
            embedding,
            created_at: now,
        })
    }

    async fn list(&self) -> Result<Vec<LearningRecord>, LearningsError> {
        let rows = sqlx::query("SELECT id, text, source, embedding, created_at FROM learnings")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        rows.into_iter().map(row_to_record).collect()
    }

    async fn remove(&self, id: u64) -> Result<(), LearningsError> {
        let result = sqlx::query("DELETE FROM learnings WHERE id = ?")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(LearningsError::NotFound(id));
        }
        Ok(())
    }

    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredLearning>, LearningsError> {
        let all = self.list().await?;
        let mut scored: Vec<ScoredLearning> = all
            .into_iter()
            .map(|learning| {
                let score = cosine_similarity(query, &learning.embedding);
                ScoredLearning { learning, score }
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }
}

fn row_to_record(row: sqlx::sqlite::SqliteRow) -> Result<LearningRecord, LearningsError> {
    let id: i64 = row.get("id");
    let text: String = row.get("text");
    let source_str: String = row.get("source");
    let embedding_bytes: Vec<u8> = row.get("embedding");
    let created_at: i64 = row.get("created_at");
    Ok(LearningRecord {
        id: id as u64,
        text,
        source: source_from_str(&source_str)?,
        embedding: bytes_to_vec_f32(&embedding_bytes),
        created_at,
    })
}

fn source_to_str(s: LearningSource) -> &'static str {
    match s {
        LearningSource::Chat => "chat",
        LearningSource::Guideline => "guideline",
        LearningSource::Inferred => "inferred",
    }
}

fn source_from_str(s: &str) -> Result<LearningSource, LearningsError> {
    match s {
        "chat" => Ok(LearningSource::Chat),
        "guideline" => Ok(LearningSource::Guideline),
        "inferred" => Ok(LearningSource::Inferred),
        other => Err(LearningsError::Storage(format!(
            "unknown source variant in DB: {other}"
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

    #[tokio::test]
    async fn add_then_list_returns_inserted_record() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store
            .add(
                "Forbid unwrap() outside tests.".into(),
                LearningSource::Guideline,
                vec![0.1, 0.2, 0.3],
                1700000000,
            )
            .await
            .unwrap();
        assert!(r.id >= 1);
        assert_eq!(r.source, LearningSource::Guideline);
        let all = store.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "Forbid unwrap() outside tests.");
        assert_eq!(all[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[tokio::test]
    async fn ids_are_monotonic_per_row() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r1 = store
            .add("a".into(), LearningSource::Chat, vec![1.0], 0)
            .await
            .unwrap();
        let r2 = store
            .add("b".into(), LearningSource::Chat, vec![1.0], 0)
            .await
            .unwrap();
        assert!(r2.id > r1.id);
    }

    #[tokio::test]
    async fn remove_drops_the_matching_record() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store
            .add("x".into(), LearningSource::Chat, vec![1.0], 0)
            .await
            .unwrap();
        store.remove(r.id).await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_returns_not_found_for_unknown_id() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let err = store.remove(999).await.expect_err("err");
        assert!(matches!(err, LearningsError::NotFound(999)));
    }

    #[tokio::test]
    async fn embedding_roundtrip_preserves_floats() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let original = vec![0.1, -0.2, std::f32::consts::PI, -1e6, 0.0];
        let r = store
            .add("v".into(), LearningSource::Chat, original.clone(), 0)
            .await
            .unwrap();
        assert_eq!(r.embedding, original);
        let listed = store.list().await.unwrap();
        assert_eq!(listed[0].embedding, original);
    }

    #[tokio::test]
    async fn source_roundtrip_for_each_variant() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        for src in [
            LearningSource::Chat,
            LearningSource::Guideline,
            LearningSource::Inferred,
        ] {
            store.add("x".into(), src, vec![1.0], 0).await.unwrap();
        }
        let all = store.list().await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().map(|l| l.source).collect::<Vec<_>>(),
            vec![
                LearningSource::Chat,
                LearningSource::Guideline,
                LearningSource::Inferred,
            ]
        );
    }

    #[tokio::test]
    async fn query_nearest_returns_top_k_by_similarity() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        store
            .add("east".into(), LearningSource::Chat, vec![1.0, 0.0], 0)
            .await
            .unwrap();
        store
            .add("ne".into(), LearningSource::Chat, vec![1.0, 1.0], 0)
            .await
            .unwrap();
        store
            .add("north".into(), LearningSource::Chat, vec![0.0, 1.0], 0)
            .await
            .unwrap();
        let r = store.query_nearest(&[1.0, 0.0], 2).await.unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].learning.text, "east");
        assert!(r[0].score > 0.99);
        assert_eq!(r[1].learning.text, "ne");
    }

    #[tokio::test]
    async fn query_against_empty_store_returns_empty_vec() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store.query_nearest(&[1.0, 0.0], 5).await.unwrap();
        assert!(r.is_empty());
    }

    #[tokio::test]
    async fn open_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learnings.sqlite");
        let store = SqliteLearningsStore::open(&path).await.expect("open");
        store
            .add("x".into(), LearningSource::Chat, vec![1.0], 0)
            .await
            .unwrap();
        // Drop the store and re-open: the row should still be there.
        drop(store);
        let reopened = SqliteLearningsStore::open(&path).await.expect("reopen");
        assert_eq!(reopened.list().await.unwrap().len(), 1);
    }
}
