//! SQLite-backed [`LearningsStore`] for persistent storage of
//! repo-specific guidance across gateway restarts.
//!
//! Embeddings are stored as raw `f32` byte slices (host byte order —
//! the database file isn't expected to be portable across endianness;
//! all known production hosts are little-endian). Cosine similarity
//! is computed in Rust over a full table scan; for the tens-to-low-
//! thousands of rows a typical repo accumulates this is fine. A
//! `LanceDB` backing for ANN search at higher scale is still pending.

use crate::learnings::{
    rank_in_scope, LearningRecord, LearningSource, LearningsError, LearningsFilter, LearningsStore,
    ScoredLearning,
};
use crate::repo_scope::{parse_legacy_correction_repo, RepoScope};
use async_trait::async_trait;
use core::str::FromStr as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Pool, Row as _, Sqlite};
use std::path::Path;

/// Columns added after the original schema, applied with
/// `ALTER TABLE ... ADD COLUMN` so older databases migrate in place.
const ADDED_COLUMNS: [&str; 2] = [
    "ALTER TABLE learnings ADD COLUMN repo TEXT",
    "ALTER TABLE learnings ADD COLUMN origin_pr INTEGER",
];

/// Original idempotent DDL for the `learnings` table; later columns are
/// in [`ADDED_COLUMNS`].
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS learnings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL,
    source TEXT NOT NULL,
    embedding BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
";

/// [`LearningsStore`] persisted in a SQLite database.
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::SqliteLearningsStore, where the module \
              name does not appear; ar-cli, ar-gateway and ar-orchestrator use it under that name"
)]
pub struct SqliteLearningsStore {
    /// Connection pool over the database holding `learnings`.
    pool: Pool<Sqlite>,
}

impl SqliteLearningsStore {
    /// Open a fresh in-memory database. Used by tests; useful as a
    /// drop-in for [`InMemoryLearningsStore`](crate::InMemoryLearningsStore)
    /// when you want SQLite-shaped semantics without touching disk.
    ///
    /// # Errors
    ///
    /// Returns [`LearningsError::Storage`] when the database cannot be
    /// opened or the schema cannot be applied.
    #[inline]
    pub async fn in_memory() -> Result<Self, LearningsError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|error| LearningsError::Storage(error.to_string()))?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|error| LearningsError::Storage(error.to_string()))?;
        apply_schema(&pool).await?;
        Ok(Self { pool })
    }

    /// Open or create a database at `path`. The schema is applied
    /// idempotently on first connect; a database from before learnings
    /// were scoped gains the `repo` and `origin_pr` columns and has its
    /// legacy review corrections backfilled with their repository.
    ///
    /// # Errors
    ///
    /// Returns [`LearningsError::Storage`] when the database cannot be
    /// opened or created, or the schema migration fails.
    #[inline]
    pub async fn open(path: &Path) -> Result<Self, LearningsError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|error| LearningsError::Storage(error.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|error| LearningsError::Storage(error.to_string()))?;
        apply_schema(&pool).await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl LearningsStore for SqliteLearningsStore {
    #[inline]
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "ids and PR numbers round-trip through SQLite INTEGER (i64); they are positive \
                  and far below i64::MAX, and the cast keeps the stored representation unchanged"
    )]
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
            "
            INSERT INTO learnings (text, source, embedding, created_at, repo, origin_pr)
            VALUES (?, ?, ?, ?, ?, ?)
            RETURNING id
            ",
        )
        .bind(&text)
        .bind(source_str)
        .bind(&bytes)
        .bind(now)
        .bind(scope.to_string())
        .bind(origin_pr.map(|pr| pr as i64))
        .fetch_one(&self.pool)
        .await
        .map_err(|error| LearningsError::Storage(error.to_string()))?;
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

    #[inline]
    #[expect(
        clippy::pattern_type_mismatch,
        reason = "borrowing the scope out of the filter, which is still needed afterwards, needs \
                  either default binding modes or `ref` patterns, and clippy::ref_patterns \
                  forbids `ref`"
    )]
    async fn list(&self, filter: LearningsFilter) -> Result<Vec<LearningRecord>, LearningsError> {
        // `Unscoped` filters after decoding rather than on `repo IS NULL`,
        // so a row whose stored `repo` does not parse counts as unscoped
        // here, the same way it decodes everywhere else.
        let query = match &filter {
            LearningsFilter::Repo(scope) => sqlx::query(
                "SELECT id, text, source, embedding, created_at, repo, origin_pr \
                 FROM learnings WHERE repo = ? ORDER BY id",
            )
            .bind(scope.to_string()),
            LearningsFilter::All | LearningsFilter::Unscoped => sqlx::query(
                "SELECT id, text, source, embedding, created_at, repo, origin_pr \
                 FROM learnings ORDER BY id",
            ),
        };
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| LearningsError::Storage(error.to_string()))?;
        let mut records = rows
            .iter()
            .map(row_to_record)
            .collect::<Result<Vec<_>, _>>()?;
        records.retain(|record| filter.matches(record));
        Ok(records)
    }

    #[inline]
    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
        scope: &RepoScope,
    ) -> Result<Vec<ScoredLearning>, LearningsError> {
        let scoped = self.list(LearningsFilter::Repo(scope.clone())).await?;
        Ok(rank_in_scope(scoped, query, top_k, scope))
    }

    #[inline]
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_wrap,
        reason = "ids round-trip through SQLite INTEGER (i64); they are positive and far below \
                  i64::MAX, and the cast keeps the stored representation unchanged"
    )]
    async fn remove(&self, id: u64, scope: Option<&RepoScope>) -> Result<(), LearningsError> {
        let result = match scope {
            None => sqlx::query("DELETE FROM learnings WHERE id = ?").bind(id as i64),
            Some(repo_scope) => sqlx::query("DELETE FROM learnings WHERE id = ? AND repo = ?")
                .bind(id as i64)
                .bind(repo_scope.to_string()),
        }
        .execute(&self.pool)
        .await
        .map_err(|error| LearningsError::Storage(error.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(LearningsError::NotFound(id));
        }
        Ok(())
    }
}

/// Create the table, add any missing later columns, and backfill legacy
/// scopes.
async fn apply_schema(pool: &Pool<Sqlite>) -> Result<(), LearningsError> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .map_err(|error| LearningsError::Storage(error.to_string()))?;
    for statement in ADDED_COLUMNS {
        if let Err(error) = sqlx::query(statement).execute(pool).await {
            let msg = error.to_string();
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
        .map_err(|error| LearningsError::Storage(error.to_string()))?;
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
            .map_err(|error| LearningsError::Storage(error.to_string()))?;
    }
    Ok(())
}

/// Decode a little-endian f32 BLOB written by [`vec_f32_to_bytes`].
/// Trailing bytes that do not fill a whole f32 are ignored.
#[expect(
    clippy::little_endian_bytes,
    reason = "stored embedding BLOBs are little-endian f32; reading them any other way would \
              misdecode existing databases"
)]
fn bytes_to_vec_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

/// Rebuild a [`LearningRecord`] from a `learnings` row. An unparseable
/// `repo` value decodes as unscoped and logs a warning.
#[expect(
    clippy::as_conversions,
    clippy::cast_sign_loss,
    reason = "ids and PR numbers were written from u64 values by add, so the stored i64 values \
              convert back unchanged"
)]
fn row_to_record(row: &SqliteRow) -> Result<LearningRecord, LearningsError> {
    let id: i64 = row.get("id");
    let text: String = row.get("text");
    let source_str: String = row.get("source");
    let embedding_bytes: Vec<u8> = row.get("embedding");
    let created_at: i64 = row.get("created_at");
    let stored_repo: Option<String> = row.get("repo");
    let origin_pr: Option<i64> = row.get("origin_pr");
    let repo = stored_repo.as_deref().and_then(|value| {
        let scope = RepoScope::parse(value);
        if scope.is_none() {
            tracing::warn!(
                learning_id = id,
                repo = value,
                "learning has an unparseable repo; treating it as unscoped"
            );
        }
        scope
    });
    Ok(LearningRecord {
        id: id as u64,
        text,
        source: source_from_str(&source_str)?,
        embedding: bytes_to_vec_f32(&embedding_bytes),
        created_at,
        repo,
        origin_pr: origin_pr.map(|pr| pr as u64),
    })
}

/// Parse the `source` column written by [`source_to_str`].
fn source_from_str(stored: &str) -> Result<LearningSource, LearningsError> {
    match stored {
        "chat" => Ok(LearningSource::Chat),
        "guideline" => Ok(LearningSource::Guideline),
        "inferred" => Ok(LearningSource::Inferred),
        other => Err(LearningsError::Storage(format!(
            "unknown source variant in DB: {other}"
        ))),
    }
}

/// Stable `source` column value for each [`LearningSource`].
fn source_to_str(source: LearningSource) -> &'static str {
    match source {
        LearningSource::Chat => "chat",
        LearningSource::Guideline => "guideline",
        LearningSource::Inferred => "inferred",
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
    reason = "stored embedding BLOBs are little-endian f32; writing them any other way would \
              make existing databases unreadable"
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

    fn scope() -> RepoScope {
        RepoScope::new("acme", "widget")
    }

    #[expect(
        clippy::as_conversions,
        clippy::cast_sign_loss,
        reason = "test fixture values (small loop indices, ids and byte lengths) are far inside the target type's range, so the cast is exact"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
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
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn two_scopes_and_legacy() -> (SqliteLearningsStore, LearningRecord, LearningRecord, u64)
    {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let legacy = insert_legacy(&store, "legacy").await;
        let in_a = store
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
        let in_b = store
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
        (store, in_a, in_b, legacy)
    }

    #[tokio::test]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
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
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
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
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_wrap,
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
    async fn unparseable_stored_repo_is_listed_as_unscoped() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let bad = insert_legacy(&store, "hand edited").await;
        sqlx::query("UPDATE learnings SET repo = 'not-a-scope' WHERE id = ?")
            .bind(bad as i64)
            .execute(&store.pool)
            .await
            .expect("corrupt repo");
        let unscoped = store.list(LearningsFilter::Unscoped).await.unwrap();
        assert_eq!(unscoped.iter().map(|r| r.id).collect::<Vec<_>>(), vec![bad]);
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
        clippy::semicolon_outside_block,
        reason = "the restriction group also enables the contradicting semicolon_inside_block, so this guard-scoping block keeps its original statement-inside form"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
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
    async fn add_then_list_returns_inserted_record() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store
            .add(
                "Forbid unwrap() outside tests.".into(),
                LearningSource::Guideline,
                vec![0.1, 0.2, 0.3],
                1_700_000_000,
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
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
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
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    async fn remove_returns_not_found_for_unknown_id() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let err = store.remove(999, None).await.expect_err("err");
        assert!(matches!(err, LearningsError::NotFound(999)));
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
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let original = vec![0.1, -0.2, PI, -1e6, 0.0];
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
    async fn query_against_empty_store_returns_empty_vec() {
        let store = SqliteLearningsStore::in_memory().await.expect("open");
        let r = store.query_nearest(&[1.0, 0.0], 5, &scope()).await.unwrap();
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
        clippy::shadow_unrelated,
        reason = "`rows` is rebound to each reopened store's listing and read by the assertions after it; renaming would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
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
