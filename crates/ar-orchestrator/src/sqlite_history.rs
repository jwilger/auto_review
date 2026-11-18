//! SQLite-backed [`ReviewHistory`] for persistent per-PR
//! incremental-review tracking across gateway restarts.
//!
//! With this backing the orchestrator's "we already reviewed this
//! SHA" check survives a `systemctl restart auto_review`. Without
//! it (the in-memory backing), every restart triggers a fresh
//! full review on the next webhook for any open PR.
//!
//! Schema is one row per PR keyed by `(owner, repo, pr_number)`.
//! `record` is an UPSERT that overwrites the SHA so retries don't
//! duplicate rows.
//!
//! Pairs with the existing `SqliteLearningsStore` in `ar-index`.

use crate::review_history::{HistoryError, PrKey, ReviewHistory};
use async_trait::async_trait;
use core::str::FromStr as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Row as _, Sqlite};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// DDL for the one-row-per-PR history table.
const SCHEMA_TABLE: &str = "
CREATE TABLE IF NOT EXISTS review_history (
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    head_sha TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    per_review_cost_usd REAL NOT NULL DEFAULT 0.42,
    PRIMARY KEY (owner, repo, pr_number)
);
";

/// DDL for the `updated_at` index that `purge_older_than` scans.
const SCHEMA_INDEX: &str = "
CREATE INDEX IF NOT EXISTS review_history_updated_at_idx
    ON review_history (updated_at);
";

/// [`ReviewHistory`] persisted in a `SQLite` database.
pub struct SqliteReviewHistory {
    /// Connection pool for the history database.
    pool: Pool<Sqlite>,
}

impl SqliteReviewHistory {
    /// Open a fresh in-memory database. Used by tests; useful as
    /// a drop-in for `InMemoryReviewHistory` when you want
    /// SQLite-shaped semantics without touching disk.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Storage`] when the connection or the
    /// schema statements fail.
    #[inline]
    pub async fn in_memory() -> Result<Self, HistoryError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        sqlx::query(SCHEMA_TABLE)
            .execute(&pool)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        sqlx::query(SCHEMA_INDEX)
            .execute(&pool)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(Self { pool })
    }

    /// Open or create a database at `path`. Schema is applied
    /// idempotently on first connect.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Storage`] when the database cannot be
    /// opened or a schema statement fails.
    #[inline]
    pub async fn open(path: &Path) -> Result<Self, HistoryError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|err| HistoryError::Storage(err.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        sqlx::query(SCHEMA_TABLE)
            .execute(&pool)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        if let Err(err) = sqlx::query(
            "ALTER TABLE review_history ADD COLUMN per_review_cost_usd REAL NOT NULL DEFAULT 0.42",
        )
        .execute(&pool)
        .await
        {
            let msg = err.to_string();
            if !msg.contains("duplicate column name") {
                return Err(HistoryError::Storage(msg));
            }
        }
        sqlx::query(SCHEMA_INDEX)
            .execute(&pool)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(Self { pool })
    }

    /// Drop every row whose `updated_at` is older than
    /// `cutoff_unix_secs`. Returns the number of rows deleted.
    /// Operators wire this into a periodic cleanup (cron, systemd
    /// timer, or `auto_review purge-history`) for long-running
    /// deployments — closed PRs accumulate over months/years and
    /// the dedup table doesn't need their old SHAs.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Storage`] when the delete fails.
    #[inline]
    pub async fn purge_older_than(&self, cutoff_unix_secs: i64) -> Result<u64, HistoryError> {
        let result = sqlx::query("DELETE FROM review_history WHERE updated_at < ?1")
            .bind(cutoff_unix_secs)
            .execute(&self.pool)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Test-only: record a SHA at an explicit timestamp.
    /// Production code calls `record` (which uses
    /// `SystemTime::now`); tests use this to deterministically
    /// position rows before/after a cutoff. Not gated under
    /// `#[cfg(test)]` so downstream crate tests can use it; the
    /// doc comment is the only signal that production code
    /// shouldn't.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Storage`] when the upsert fails.
    #[inline]
    pub async fn record_at(
        &self,
        key: &PrKey,
        sha: &str,
        updated_at: i64,
    ) -> Result<(), HistoryError> {
        sqlx::query(
            "INSERT INTO review_history (owner, repo, pr_number, head_sha, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(owner, repo, pr_number) DO UPDATE \
             SET head_sha = excluded.head_sha, updated_at = excluded.updated_at",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number.cast_signed())
        .bind(sha)
        .bind(updated_at)
        .execute(&self.pool)
        .await
        .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(())
    }

    /// Record a SHA with an explicit per-review cost.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Storage`] when the upsert fails.
    #[inline]
    #[expect(
        clippy::same_name_method,
        reason = "this crate's integration tests and the ReviewHistory impl below call the \
                  inherent record_with_cost; renaming the pub method breaks them"
    )]
    pub async fn record_with_cost(
        &self,
        key: &PrKey,
        sha: &str,
        per_review_cost_usd: f64,
    ) -> Result<(), HistoryError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs().cast_signed());
        sqlx::query(
            "INSERT INTO review_history (owner, repo, pr_number, head_sha, updated_at, per_review_cost_usd) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(owner, repo, pr_number) DO UPDATE \
             SET head_sha = excluded.head_sha, updated_at = excluded.updated_at, \
                 per_review_cost_usd = excluded.per_review_cost_usd",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number.cast_signed())
        .bind(sha)
        .bind(now)
        .bind(per_review_cost_usd)
        .execute(&self.pool)
        .await
        .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl ReviewHistory for SqliteReviewHistory {
    #[inline]
    async fn clear(&self, key: &PrKey) -> Result<(), HistoryError> {
        sqlx::query(
            "DELETE FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number.cast_signed())
        .execute(&self.pool)
        .await
        .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(())
    }

    #[inline]
    async fn last_reviewed(&self, key: &PrKey) -> Result<Option<String>, HistoryError> {
        let row = sqlx::query(
            "SELECT head_sha FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number.cast_signed())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(row.map(|found| found.get::<String, _>("head_sha")))
    }

    #[inline]
    async fn list_known(&self) -> Result<Vec<PrKey>, HistoryError> {
        let rows = sqlx::query("SELECT owner, repo, pr_number FROM review_history")
            .fetch_all(&self.pool)
            .await
            .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| PrKey {
                owner: row.get::<String, _>("owner"),
                repo: row.get::<String, _>("repo"),
                pr_number: row.get::<i64, _>("pr_number").cast_unsigned(),
            })
            .collect())
    }

    #[inline]
    async fn record(&self, key: &PrKey, sha: &str) -> Result<(), HistoryError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs().cast_signed());
        sqlx::query(
            "INSERT INTO review_history (owner, repo, pr_number, head_sha, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(owner, repo, pr_number) DO UPDATE \
             SET head_sha = excluded.head_sha, updated_at = excluded.updated_at",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number.cast_signed())
        .bind(sha)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|err| HistoryError::Storage(err.to_string()))?;
        Ok(())
    }

    #[inline]
    async fn record_with_cost(
        &self,
        key: &PrKey,
        sha: &str,
        per_review_cost_usd: f64,
    ) -> Result<(), HistoryError> {
        SqliteReviewHistory::record_with_cost(self, key, sha, per_review_cost_usd).await
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failed storage call should fail the test at that line; unwrap reports the error and the tests stay as plain assertions; indexing follows a length assertion on the same vector"
)]
mod tests {
    use super::*;

    fn key(owner: &str, repo: &str, pr: u64) -> PrKey {
        PrKey {
            owner: owner.into(),
            repo: repo.into(),
            pr_number: pr,
        }
    }

    #[tokio::test]
    async fn unknown_pr_returns_none() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        assert_eq!(
            history.last_reviewed(&key("o", "r", 1)).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn record_then_lookup_round_trips() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        let pr_key = key("o", "r", 1);
        history.record(&pr_key, "deadbeef").await.unwrap();
        assert_eq!(
            history.last_reviewed(&pr_key).await.unwrap().as_deref(),
            Some("deadbeef")
        );
    }

    #[tokio::test]
    async fn record_replaces_previous_sha_via_upsert() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        let pr_key = key("o", "r", 1);
        history.record(&pr_key, "old").await.unwrap();
        history.record(&pr_key, "new").await.unwrap();
        assert_eq!(
            history.last_reviewed(&pr_key).await.unwrap().as_deref(),
            Some("new")
        );
        // And only one row exists (would otherwise leak).
        let rows = history.list_known().await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn clear_drops_the_record() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        let pr_key = key("o", "r", 1);
        history.record(&pr_key, "deadbeef").await.unwrap();
        history.clear(&pr_key).await.unwrap();
        assert_eq!(history.last_reviewed(&pr_key).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_on_unknown_key_is_a_noop() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        history.clear(&key("o", "r", 999)).await.unwrap();
    }

    #[tokio::test]
    async fn distinct_prs_are_isolated() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        let k1 = key("o", "r", 1);
        let k2 = key("o", "r", 2);
        let k3 = key("o", "other-repo", 1);
        history.record(&k1, "a").await.unwrap();
        history.record(&k2, "b").await.unwrap();
        history.record(&k3, "c").await.unwrap();
        assert_eq!(
            history.last_reviewed(&k1).await.unwrap().as_deref(),
            Some("a")
        );
        assert_eq!(
            history.last_reviewed(&k2).await.unwrap().as_deref(),
            Some("b")
        );
        assert_eq!(
            history.last_reviewed(&k3).await.unwrap().as_deref(),
            Some("c")
        );
    }

    #[tokio::test]
    async fn list_known_returns_every_recorded_pr() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        history.record(&key("alice", "x", 1), "a").await.unwrap();
        history.record(&key("alice", "y", 2), "b").await.unwrap();
        history.record(&key("bob", "z", 3), "c").await.unwrap();
        let mut listed = history.list_known().await.unwrap();
        listed.sort_by(|left, right| {
            (left.owner.as_str(), left.repo.as_str(), left.pr_number).cmp(&(
                right.owner.as_str(),
                right.repo.as_str(),
                right.pr_number,
            ))
        });
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0], key("alice", "x", 1));
        assert_eq!(listed[1], key("alice", "y", 2));
        assert_eq!(listed[2], key("bob", "z", 3));
    }

    #[tokio::test]
    async fn purge_drops_rows_older_than_cutoff() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        history
            .record_at(&key("o", "r", 1), "deadbeef", 100)
            .await
            .unwrap();
        history
            .record_at(&key("o", "r", 2), "cafef00d", 200)
            .await
            .unwrap();
        // Cutoff = 150: only the first row qualifies.
        let dropped = history.purge_older_than(150).await.unwrap();
        assert_eq!(dropped, 1);
        let remaining = history.list_known().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].pr_number, 2);
    }

    #[tokio::test]
    async fn purge_strict_less_than_keeps_row_at_exact_cutoff() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        history
            .record_at(&key("o", "r", 1), "x", 100)
            .await
            .unwrap();
        // Cutoff equals the row's timestamp: row stays.
        let dropped = history.purge_older_than(100).await.unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(history.list_known().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn purge_keeps_rows_at_or_after_cutoff() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        history
            .record_at(&key("o", "r", 1), "x", 1000)
            .await
            .unwrap();
        // Cutoff is far in the past; nothing should drop.
        let dropped = history.purge_older_than(0).await.unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(history.list_known().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn purge_returns_zero_on_empty_table() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        let dropped = history.purge_older_than(i64::MAX).await.unwrap();
        assert_eq!(dropped, 0);
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "the expected cost literal takes its type from the f64 it is compared with; a \
                  literal suffix would trip separated_literal_suffix or unseparated_literal_suffix \
                  instead"
    )]
    async fn persist_sha_with_per_review_cost_aggregate() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        let pr_key = key("o", "r", 1);
        history.record(&pr_key, "deadbeef").await.unwrap();

        let row = sqlx::query(
            "SELECT head_sha, per_review_cost_usd FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&pr_key.owner)
        .bind(&pr_key.repo)
        .bind(pr_key.pr_number.cast_signed())
        .fetch_one(&history.pool)
        .await
        .unwrap();

        let head_sha: String = row.get("head_sha");
        let per_review_cost_usd: f64 = row.get("per_review_cost_usd");

        assert_eq!(head_sha.as_str(), "deadbeef");
        assert_eq!(per_review_cost_usd, 0.42);
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "the expected cost literal takes its type from the f64 it is compared with; a \
                  literal suffix would trip separated_literal_suffix or unseparated_literal_suffix \
                  instead"
    )]
    async fn caller_can_record_explicit_per_review_cost_and_read_it_back() {
        let history = SqliteReviewHistory::in_memory().await.unwrap();
        let pr_key = key("o", "r", 1);
        history
            .record_with_cost(&pr_key, "deadbeef", 1.23)
            .await
            .unwrap();

        let cost: f64 = sqlx::query_scalar(
            "SELECT per_review_cost_usd FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&pr_key.owner)
        .bind(&pr_key.repo)
        .bind(pr_key.pr_number.cast_signed())
        .fetch_one(&history.pool)
        .await
        .unwrap();

        assert_eq!(cost, 1.23);
    }

    #[tokio::test]
    async fn persists_across_handle_drops_when_backed_by_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        let pr_key = key("o", "r", 7);
        {
            let history = SqliteReviewHistory::open(&path).await.unwrap();
            history.record(&pr_key, "persisted").await.unwrap();
        };
        // Open a fresh handle to the same file.
        let history = SqliteReviewHistory::open(&path).await.unwrap();
        assert_eq!(
            history.last_reviewed(&pr_key).await.unwrap().as_deref(),
            Some("persisted")
        );
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "the expected cost literal takes its type from the f64 it is compared with; a \
                  literal suffix would trip separated_literal_suffix or unseparated_literal_suffix \
                  instead"
    )]
    async fn open_upgrades_legacy_table_without_cost_column_for_record_with_cost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        let legacy_opts =
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
                .unwrap()
                .create_if_missing(true);

        let legacy_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(legacy_opts)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE review_history (
                owner TEXT NOT NULL,
                repo TEXT NOT NULL,
                pr_number INTEGER NOT NULL,
                head_sha TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (owner, repo, pr_number)
            )",
        )
        .execute(&legacy_pool)
        .await
        .unwrap();
        drop(legacy_pool);

        let history = SqliteReviewHistory::open(&path).await.unwrap();
        let pr_key = key("o", "r", 1);
        history
            .record_with_cost(&pr_key, "deadbeef", 1.23)
            .await
            .unwrap();

        let cost: f64 = sqlx::query_scalar(
            "SELECT per_review_cost_usd FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&pr_key.owner)
        .bind(&pr_key.repo)
        .bind(pr_key.pr_number.cast_signed())
        .fetch_one(&history.pool)
        .await
        .unwrap();

        assert_eq!(cost, 1.23);
    }
}
