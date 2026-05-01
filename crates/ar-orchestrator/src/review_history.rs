//! Per-PR "last reviewed SHA" tracking.
//!
//! Drives incremental review: when a PR is updated and we already
//! reviewed it at SHA X, the orchestrator can ask Forgejo for the
//! diff between X and the new head SHA instead of the whole PR. The
//! result: fewer tokens spent and no duplicate inline comments on
//! lines that haven't changed since the last review.
//!
//! Currently in-memory. A SQLite or Postgres backing can replace
//! [`InMemoryReviewHistory`] by implementing [`ReviewHistory`].

use async_trait::async_trait;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrKey {
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("storage error: {0}")]
    Storage(String),
}

#[async_trait]
pub trait ReviewHistory: Send + Sync {
    /// Look up the SHA we most recently posted a review for, if any.
    async fn last_reviewed(&self, key: &PrKey) -> Result<Option<String>, HistoryError>;

    /// Record that we just posted a review at `sha`. Replaces any
    /// previous record for the same PR.
    async fn record(&self, key: &PrKey, sha: &str) -> Result<(), HistoryError>;

    /// Drop the recorded SHA — used when a PR is closed or reopened
    /// to force the next review to be a full one.
    async fn clear(&self, key: &PrKey) -> Result<(), HistoryError>;

    /// List every PR we've recorded a review for. Used by the chat
    /// poller to know which PRs to scan for new mentions; the
    /// `pull_request_review_comment` webhook is unreliable on
    /// Forgejo so a periodic poll picks up the gap.
    async fn list_known(&self) -> Result<Vec<PrKey>, HistoryError>;
}

#[derive(Default)]
pub struct InMemoryReviewHistory {
    inner: tokio::sync::Mutex<HashMap<PrKey, String>>,
}

impl InMemoryReviewHistory {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ReviewHistory for InMemoryReviewHistory {
    async fn last_reviewed(&self, key: &PrKey) -> Result<Option<String>, HistoryError> {
        Ok(self.inner.lock().await.get(key).cloned())
    }

    async fn record(&self, key: &PrKey, sha: &str) -> Result<(), HistoryError> {
        self.inner.lock().await.insert(key.clone(), sha.to_string());
        Ok(())
    }

    async fn clear(&self, key: &PrKey) -> Result<(), HistoryError> {
        self.inner.lock().await.remove(key);
        Ok(())
    }

    async fn list_known(&self) -> Result<Vec<PrKey>, HistoryError> {
        Ok(self.inner.lock().await.keys().cloned().collect())
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
        let h = InMemoryReviewHistory::new();
        assert_eq!(h.last_reviewed(&key("o", "r", 1)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn record_then_lookup_returns_the_sha() {
        let h = InMemoryReviewHistory::new();
        let k = key("o", "r", 1);
        h.record(&k, "deadbeef").await.unwrap();
        assert_eq!(
            h.last_reviewed(&k).await.unwrap().as_deref(),
            Some("deadbeef")
        );
    }

    #[tokio::test]
    async fn record_replaces_previous_sha_for_same_pr() {
        let h = InMemoryReviewHistory::new();
        let k = key("o", "r", 1);
        h.record(&k, "old").await.unwrap();
        h.record(&k, "new").await.unwrap();
        assert_eq!(h.last_reviewed(&k).await.unwrap().as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn clear_drops_the_record() {
        let h = InMemoryReviewHistory::new();
        let k = key("o", "r", 1);
        h.record(&k, "deadbeef").await.unwrap();
        h.clear(&k).await.unwrap();
        assert_eq!(h.last_reviewed(&k).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_on_unknown_key_is_a_noop() {
        let h = InMemoryReviewHistory::new();
        h.clear(&key("o", "r", 999)).await.unwrap();
    }

    #[tokio::test]
    async fn distinct_prs_are_isolated() {
        let h = InMemoryReviewHistory::new();
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
}
