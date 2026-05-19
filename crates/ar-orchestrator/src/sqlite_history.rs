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
//! Pairs with the existing SqliteLearningsStore in `ar-index`.

use crate::review_history::{HistoryError, PrKey, ReviewHistory};
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Row, Sqlite};
use std::path::Path;
use std::str::FromStr;

const SCHEMA_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS review_history (
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    head_sha TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    per_review_cost_usd REAL NOT NULL DEFAULT 0.42,
    PRIMARY KEY (owner, repo, pr_number)
);
"#;

const SCHEMA_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS review_history_updated_at_idx
    ON review_history (updated_at);
"#;

pub struct SqliteReviewHistory {
    pool: Pool<Sqlite>,
}

impl SqliteReviewHistory {
    /// Open or create a database at `path`. Schema is applied
    /// idempotently on first connect.
    pub async fn open(path: &Path) -> Result<Self, HistoryError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.to_string_lossy()))
            .map_err(|e| HistoryError::Storage(e.to_string()))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA_TABLE)
            .execute(&pool)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA_INDEX)
            .execute(&pool)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Test-only: record a SHA at an explicit timestamp.
    /// Production code calls `record` (which uses
    /// `SystemTime::now`); tests use this to deterministically
    /// position rows before/after a cutoff. Not gated under
    /// `#[cfg(test)]` so downstream crate tests can use it; the
    /// doc comment is the only signal that production code
    /// shouldn't.
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
        .bind(key.pr_number as i64)
        .bind(sha)
        .bind(updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Drop every row whose `updated_at` is older than
    /// `cutoff_unix_secs`. Returns the number of rows deleted.
    /// Operators wire this into a periodic cleanup (cron, systemd
    /// timer, or `auto_review purge-history`) for long-running
    /// deployments — closed PRs accumulate over months/years and
    /// the dedup table doesn't need their old SHAs.
    pub async fn purge_older_than(&self, cutoff_unix_secs: i64) -> Result<u64, HistoryError> {
        let result = sqlx::query("DELETE FROM review_history WHERE updated_at < ?1")
            .bind(cutoff_unix_secs)
            .execute(&self.pool)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Open a fresh in-memory database. Used by tests; useful as
    /// a drop-in for `InMemoryReviewHistory` when you want
    /// SQLite-shaped semantics without touching disk.
    pub async fn in_memory() -> Result<Self, HistoryError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA_TABLE)
            .execute(&pool)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        sqlx::query(SCHEMA_INDEX)
            .execute(&pool)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl ReviewHistory for SqliteReviewHistory {
    async fn last_reviewed(&self, key: &PrKey) -> Result<Option<String>, HistoryError> {
        let row = sqlx::query(
            "SELECT head_sha FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(row.map(|r| r.get::<String, _>("head_sha")))
    }

    async fn record(&self, key: &PrKey, sha: &str) -> Result<(), HistoryError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        sqlx::query(
            "INSERT INTO review_history (owner, repo, pr_number, head_sha, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(owner, repo, pr_number) DO UPDATE \
             SET head_sha = excluded.head_sha, updated_at = excluded.updated_at",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number as i64)
        .bind(sha)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn clear(&self, key: &PrKey) -> Result<(), HistoryError> {
        sqlx::query(
            "DELETE FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&key.owner)
        .bind(&key.repo)
        .bind(key.pr_number as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_known(&self) -> Result<Vec<PrKey>, HistoryError> {
        let rows = sqlx::query("SELECT owner, repo, pr_number FROM review_history")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| HistoryError::Storage(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| PrKey {
                owner: r.get::<String, _>("owner"),
                repo: r.get::<String, _>("repo"),
                pr_number: r.get::<i64, _>("pr_number") as u64,
            })
            .collect())
    }
}

#[cfg(test)]
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
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        assert_eq!(h.last_reviewed(&key("o", "r", 1)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn record_then_lookup_round_trips() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        let k = key("o", "r", 1);
        h.record(&k, "deadbeef").await.unwrap();
        assert_eq!(
            h.last_reviewed(&k).await.unwrap().as_deref(),
            Some("deadbeef")
        );
    }

    #[tokio::test]
    async fn record_replaces_previous_sha_via_upsert() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        let k = key("o", "r", 1);
        h.record(&k, "old").await.unwrap();
        h.record(&k, "new").await.unwrap();
        assert_eq!(h.last_reviewed(&k).await.unwrap().as_deref(), Some("new"));
        // And only one row exists (would otherwise leak).
        let rows = h.list_known().await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn clear_drops_the_record() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        let k = key("o", "r", 1);
        h.record(&k, "deadbeef").await.unwrap();
        h.clear(&k).await.unwrap();
        assert_eq!(h.last_reviewed(&k).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_on_unknown_key_is_a_noop() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        h.clear(&key("o", "r", 999)).await.unwrap();
    }

    #[tokio::test]
    async fn distinct_prs_are_isolated() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        let k1 = key("o", "r", 1);
        let k2 = key("o", "r", 2);
        let k3 = key("o", "other-repo", 1);
        h.record(&k1, "a").await.unwrap();
        h.record(&k2, "b").await.unwrap();
        h.record(&k3, "c").await.unwrap();
        assert_eq!(h.last_reviewed(&k1).await.unwrap().as_deref(), Some("a"));
        assert_eq!(h.last_reviewed(&k2).await.unwrap().as_deref(), Some("b"));
        assert_eq!(h.last_reviewed(&k3).await.unwrap().as_deref(), Some("c"));
    }

    #[tokio::test]
    async fn list_known_returns_every_recorded_pr() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        h.record(&key("alice", "x", 1), "a").await.unwrap();
        h.record(&key("alice", "y", 2), "b").await.unwrap();
        h.record(&key("bob", "z", 3), "c").await.unwrap();
        let mut listed = h.list_known().await.unwrap();
        listed.sort_by(|l, r| {
            (l.owner.as_str(), l.repo.as_str(), l.pr_number).cmp(&(
                r.owner.as_str(),
                r.repo.as_str(),
                r.pr_number,
            ))
        });
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0], key("alice", "x", 1));
        assert_eq!(listed[1], key("alice", "y", 2));
        assert_eq!(listed[2], key("bob", "z", 3));
    }

    #[tokio::test]
    async fn purge_drops_rows_older_than_cutoff() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        h.record_at(&key("o", "r", 1), "deadbeef", 100)
            .await
            .unwrap();
        h.record_at(&key("o", "r", 2), "cafef00d", 200)
            .await
            .unwrap();
        // Cutoff = 150: only the first row qualifies.
        let dropped = h.purge_older_than(150).await.unwrap();
        assert_eq!(dropped, 1);
        let remaining = h.list_known().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].pr_number, 2);
    }

    #[tokio::test]
    async fn purge_strict_less_than_keeps_row_at_exact_cutoff() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        h.record_at(&key("o", "r", 1), "x", 100).await.unwrap();
        // Cutoff equals the row's timestamp: row stays.
        let dropped = h.purge_older_than(100).await.unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(h.list_known().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn purge_keeps_rows_at_or_after_cutoff() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        h.record_at(&key("o", "r", 1), "x", 1000).await.unwrap();
        // Cutoff is far in the past; nothing should drop.
        let dropped = h.purge_older_than(0).await.unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(h.list_known().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn purge_returns_zero_on_empty_table() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        let dropped = h.purge_older_than(i64::MAX).await.unwrap();
        assert_eq!(dropped, 0);
    }

    #[tokio::test]
    async fn persist_sha_with_per_review_cost_aggregate() {
        let h = SqliteReviewHistory::in_memory().await.unwrap();
        let k = key("o", "r", 1);
        h.record(&k, "deadbeef").await.unwrap();

        let row = sqlx::query(
            "SELECT head_sha, per_review_cost_usd FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind(&k.owner)
        .bind(&k.repo)
        .bind(k.pr_number as i64)
        .fetch_one(&h.pool)
        .await
        .unwrap();

        let head_sha: String = row.get("head_sha");
        let per_review_cost_usd: f64 = row.get("per_review_cost_usd");

        assert_eq!(head_sha.as_str(), "deadbeef");
        assert_eq!(per_review_cost_usd, 0.42);
    }

    #[tokio::test]
    async fn persists_across_handle_drops_when_backed_by_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review_history.db");
        let k = key("o", "r", 7);
        {
            let h = SqliteReviewHistory::open(&path).await.unwrap();
            h.record(&k, "persisted").await.unwrap();
        }
        // Open a fresh handle to the same file.
        let h = SqliteReviewHistory::open(&path).await.unwrap();
        assert_eq!(
            h.last_reviewed(&k).await.unwrap().as_deref(),
            Some("persisted")
        );
    }
}
