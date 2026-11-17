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
    rank_in_scope, LearningRecord, LearningSource, LearningsError, LearningsFilter, LearningsStore,
    ScoredLearning,
};
use crate::repo_scope::{parse_legacy_correction_repo, RepoScope};
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

/// Columns added after the original schema, applied with
/// `ALTER TABLE ... ADD COLUMN` so older databases migrate in place.
const ADDED_COLUMNS: [&str; 2] = [
    "ALTER TABLE learnings ADD COLUMN repo TEXT",
    "ALTER TABLE learnings ADD COLUMN origin_pr INTEGER",
];

async fn apply_schema(pool: &Pool<Sqlite>) -> Result<(), LearningsError> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .map_err(|e| LearningsError::Storage(e.to_string()))?;
    for statement in ADDED_COLUMNS {
        if let Err(e) = sqlx::query(statement).execute(pool).await {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(LearningsError::Storage(msg));
            }
        }
    }
    backfill_legacy_scopes(pool).await
}

/// Scope legacy rows whose text carries the old
/// `Repository <owner>/<repo> only.` review-correction prefix. Other
/// legacy rows keep a NULL `repo` and are never retrieved into reviews.
async fn backfill_legacy_scopes(pool: &Pool<Sqlite>) -> Result<(), LearningsError> {
    let rows = sqlx::query("SELECT id, text FROM learnings WHERE repo IS NULL")
        .fetch_all(pool)
        .await
        .map_err(|e| LearningsError::Storage(e.to_string()))?;
    for row in rows {
        let text: String = row.get("text");
        let Some(scope) = parse_legacy_correction_repo(&text) else {
            continue;
        };
        let id: i64 = row.get("id");
        sqlx::query("UPDATE learnings SET repo = ? WHERE id = ? AND repo IS NULL")
            .bind(scope.to_string())
            .bind(id)
            .execute(pool)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
    }
    Ok(())
}

pub struct SqliteLearningsStore {
    pool: Pool<Sqlite>,
}

impl SqliteLearningsStore {
    /// Open or create a database at `path`. The schema is applied
    /// idempotently on first connect; a database from before learnings
    /// were scoped gains the `repo` and `origin_pr` columns and has its
    /// legacy review corrections backfilled with their repository.
    pub async fn open(path: &Path) -> Result<Self, LearningsError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|e| LearningsError::Storage(e.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        apply_schema(&pool).await?;
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
        apply_schema(&pool).await?;
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
        scope: &RepoScope,
        origin_pr: Option<u64>,
    ) -> Result<LearningRecord, LearningsError> {
        let bytes = vec_f32_to_bytes(&embedding);
        let source_str = source_to_str(source);
        let row = sqlx::query(
            r#"
            INSERT INTO learnings (text, source, embedding, created_at, repo, origin_pr)
            VALUES (?, ?, ?, ?, ?, ?)
            RETURNING id
            "#,
        )
        .bind(&text)
        .bind(source_str)
        .bind(&bytes)
        .bind(now)
        .bind(scope.to_string())
        .bind(origin_pr.map(|pr| pr as i64))
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
            repo: Some(scope.clone()),
            origin_pr,
        })
    }

    async fn list(&self, filter: LearningsFilter) -> Result<Vec<LearningRecord>, LearningsError> {
        let query = match &filter {
            LearningsFilter::All => sqlx::query(
                "SELECT id, text, source, embedding, created_at, repo, origin_pr \
                 FROM learnings ORDER BY id",
            ),
            LearningsFilter::Repo(scope) => sqlx::query(
                "SELECT id, text, source, embedding, created_at, repo, origin_pr \
                 FROM learnings WHERE repo = ? ORDER BY id",
            )
            .bind(scope.to_string()),
            LearningsFilter::Unscoped => sqlx::query(
                "SELECT id, text, source, embedding, created_at, repo, origin_pr \
                 FROM learnings WHERE repo IS NULL ORDER BY id",
            ),
        };
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| LearningsError::Storage(e.to_string()))?;
        rows.into_iter().map(row_to_record).collect()
    }

    async fn remove(&self, id: u64, scope: Option<&RepoScope>) -> Result<(), LearningsError> {
        let result = match scope {
            None => sqlx::query("DELETE FROM learnings WHERE id = ?").bind(id as i64),
            Some(scope) => sqlx::query("DELETE FROM learnings WHERE id = ? AND repo = ?")
                .bind(id as i64)
                .bind(scope.to_string()),
        }
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
        scope: &RepoScope,
    ) -> Result<Vec<ScoredLearning>, LearningsError> {
        let scoped = self.list(LearningsFilter::Repo(scope.clone())).await?;
        Ok(rank_in_scope(scoped, query, top_k, scope))
    }
}

fn row_to_record(row: sqlx::sqlite::SqliteRow) -> Result<LearningRecord, LearningsError> {
    let id: i64 = row.get("id");
    let text: String = row.get("text");
    let source_str: String = row.get("source");
    let embedding_bytes: Vec<u8> = row.get("embedding");
    let created_at: i64 = row.get("created_at");
    let repo: Option<String> = row.get("repo");
    let origin_pr: Option<i64> = row.get("origin_pr");
    Ok(LearningRecord {
        id: id as u64,
        text,
        source: source_from_str(&source_str)?,
        embedding: bytes_to_vec_f32(&embedding_bytes),
        created_at,
        repo: repo.as_deref().and_then(RepoScope::parse),
        origin_pr: origin_pr.map(|pr| pr as u64),
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
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> RepoScope {
        RepoScope::new("acme", "widget")
    }

    async fn insert_legacy(store: &SqliteLearningsStore, text: &str) -> u64 {
        let row = sqlx::query(
            "INSERT INTO learnings (text, source, embedding, created_at) \
             VALUES (?, 'inferred', ?, 0) RETURNING id",
        )
        .bind(text)
        .bind(vec_f32_to_bytes(&[1.0, 0.0]))
        .fetch_one(&store.pool)
        .await
        .expect("insert legacy row");
        row.get::<i64, _>("id") as u64
    }

    /// One learning in each of two scopes plus one unscoped legacy row,
    /// all with the same embedding.
    async fn two_scopes_and_legacy() -> (SqliteLearningsStore, LearningRecord, LearningRecord, u64)
    {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let legacy = insert_legacy(&store, "legacy").await;
        let a = store
            .add(
                "from a".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &RepoScope::new("acme", "a"),
                Some(3),
            )
            .await
            .unwrap();
        let b = store
            .add(
                "from b".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &RepoScope::new("acme", "b"),
                None,
            )
            .await
            .unwrap();
        (store, a, b, legacy)
    }

    #[tokio::test]
    async fn query_nearest_excludes_other_scopes_and_unscoped_records() {
        let (store, a, _, _) = two_scopes_and_legacy().await;
        let r = store
            .query_nearest(&[1.0, 0.0], 5, &RepoScope::new("Acme", "A"))
            .await
            .unwrap();
        assert_eq!(
            r.iter().map(|s| s.learning.id).collect::<Vec<_>>(),
            vec![a.id]
        );
    }

    #[tokio::test]
    async fn scoped_remove_only_deletes_records_of_that_scope() {
        let (store, a, b, legacy) = two_scopes_and_legacy().await;
        let scope_b = RepoScope::new("acme", "b");
        for id in [a.id, legacy, 999] {
            let err = store
                .remove(id, Some(&scope_b))
                .await
                .expect_err("not found");
            assert!(matches!(err, LearningsError::NotFound(n) if n == id));
        }
        assert_eq!(store.list(LearningsFilter::All).await.unwrap().len(), 3);
        store.remove(b.id, Some(&scope_b)).await.unwrap();
        store.remove(legacy, None).await.unwrap();
        assert_eq!(store.list(LearningsFilter::All).await.unwrap(), vec![a]);
    }

    #[tokio::test]
    async fn list_filters_by_scope() {
        let (store, a, b, legacy) = two_scopes_and_legacy().await;
        assert_eq!(a.origin_pr, Some(3));
        assert_eq!(
            store
                .list(LearningsFilter::Repo(RepoScope::new("acme", "b")))
                .await
                .unwrap(),
            vec![b]
        );
        let unscoped = store.list(LearningsFilter::Unscoped).await.unwrap();
        assert_eq!(
            unscoped.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![legacy]
        );
        assert_eq!(store.list(LearningsFilter::All).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn open_migrates_and_backfills_a_legacy_schema_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite");
        {
            let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true);
            let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
            sqlx::query(
                "CREATE TABLE learnings (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                 text TEXT NOT NULL, source TEXT NOT NULL, embedding BLOB NOT NULL, \
                 created_at INTEGER NOT NULL)",
            )
            .execute(&pool)
            .await
            .unwrap();
            for text in [
                "Repository Acme/Widget only. Review correction from PR feedback: x",
                "Prefer Result over panics.",
            ] {
                sqlx::query(
                    "INSERT INTO learnings (text, source, embedding, created_at) \
                     VALUES (?, 'inferred', ?, 0)",
                )
                .bind(text)
                .bind(vec_f32_to_bytes(&[1.0, 0.0]))
                .execute(&pool)
                .await
                .unwrap();
            }
            pool.close().await;
        }
        drop(SqliteLearningsStore::open(&path).await.expect("first open"));
        let store = SqliteLearningsStore::open(&path)
            .await
            .expect("second open");

        let hits = store
            .query_nearest(&[1.0, 0.0], 5, &RepoScope::new("acme", "widget"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0]
            .learning
            .text
            .starts_with("Repository Acme/Widget only."));

        let unscoped = store.list(LearningsFilter::Unscoped).await.unwrap();
        assert_eq!(unscoped.len(), 1);
        assert_eq!(unscoped[0].text, "Prefer Result over panics.");
        assert_eq!(store.list(LearningsFilter::All).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn add_then_list_returns_inserted_record() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store
            .add(
                "Forbid unwrap() outside tests.".into(),
                LearningSource::Guideline,
                vec![0.1, 0.2, 0.3],
                1700000000,
                &scope(),
                Some(7),
            )
            .await
            .unwrap();
        assert!(r.id >= 1);
        assert_eq!(r.source, LearningSource::Guideline);
        let all = store.list(LearningsFilter::All).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "Forbid unwrap() outside tests.");
        assert_eq!(all[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[tokio::test]
    async fn ids_are_monotonic_per_row() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r1 = store
            .add(
                "a".into(),
                LearningSource::Chat,
                vec![1.0],
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        let r2 = store
            .add(
                "b".into(),
                LearningSource::Chat,
                vec![1.0],
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        assert!(r2.id > r1.id);
    }

    #[tokio::test]
    async fn remove_drops_the_matching_record() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store
            .add(
                "x".into(),
                LearningSource::Chat,
                vec![1.0],
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        store.remove(r.id, None).await.unwrap();
        assert!(store.list(LearningsFilter::All).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_returns_not_found_for_unknown_id() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let err = store.remove(999, None).await.expect_err("err");
        assert!(matches!(err, LearningsError::NotFound(999)));
    }

    #[tokio::test]
    async fn embedding_roundtrip_preserves_floats() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let original = vec![0.1, -0.2, std::f32::consts::PI, -1e6, 0.0];
        let r = store
            .add(
                "v".into(),
                LearningSource::Chat,
                original.clone(),
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(r.embedding, original);
        let listed = store.list(LearningsFilter::All).await.unwrap();
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
            store
                .add("x".into(), src, vec![1.0], 0, &scope(), None)
                .await
                .unwrap();
        }
        let all = store.list(LearningsFilter::All).await.unwrap();
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
            .add(
                "east".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        store
            .add(
                "ne".into(),
                LearningSource::Chat,
                vec![1.0, 1.0],
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        store
            .add(
                "north".into(),
                LearningSource::Chat,
                vec![0.0, 1.0],
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        let r = store.query_nearest(&[1.0, 0.0], 2, &scope()).await.unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].learning.text, "east");
        assert!(r[0].score > 0.99);
        assert_eq!(r[1].learning.text, "ne");
    }

    #[tokio::test]
    async fn query_against_empty_store_returns_empty_vec() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store.query_nearest(&[1.0, 0.0], 5, &scope()).await.unwrap();
        assert!(r.is_empty());
    }

    #[tokio::test]
    async fn open_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learnings.sqlite");
        let store = SqliteLearningsStore::open(&path).await.expect("open");
        store
            .add(
                "x".into(),
                LearningSource::Chat,
                vec![1.0],
                0,
                &scope(),
                None,
            )
            .await
            .unwrap();
        // Drop the store and re-open: the row should still be there.
        drop(store);
        let reopened = SqliteLearningsStore::open(&path).await.expect("reopen");
        let rows = reopened.list(LearningsFilter::All).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, Some(scope()));
        assert_eq!(rows[0].origin_pr, None);
        let scoped = reopened
            .add(
                "y".into(),
                LearningSource::Chat,
                vec![1.0],
                0,
                &RepoScope::new("o", "r"),
                Some(42),
            )
            .await
            .unwrap();
        drop(reopened);
        let again = SqliteLearningsStore::open(&path)
            .await
            .expect("reopen twice");
        let rows = again
            .list(LearningsFilter::Repo(RepoScope::new("O", "R")))
            .await
            .unwrap();
        assert_eq!(rows, vec![scoped]);
        assert_eq!(rows[0].origin_pr, Some(42));
    }
}
