//! Persistent "learnings" — short text snippets the bot or its users
//! teach the reviewer about repo-specific conventions, false-positive
//! patterns, project-specific gotchas, etc.
//!
//! Modeled after CodeRabbit's per-repo memory: at review time, the
//! pipeline retrieves the top-K learnings whose embeddings match the
//! diff, and injects them into the LLM prompt as supplementary
//! context.
//!
//! This commit ships the in-memory store. A persistent SQLite
//! backing or LanceDB-backed store can swap in by implementing the
//! `LearningsStore` trait.

use crate::repo_scope::RepoScope;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningSource {
    /// Learned from a `@auto_review remember/forget` chat command.
    Chat,
    /// Manually authored as a repo guideline (e.g. via API).
    Guideline,
    /// Inferred by the bot itself from PR feedback patterns.
    Inferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningRecord {
    pub id: u64,
    pub text: String,
    pub source: LearningSource,
    pub embedding: Vec<f32>,
    /// Unix timestamp seconds.
    pub created_at: i64,
    /// Repository the learning belongs to. `None` marks a legacy row
    /// written before learnings were scoped; such rows are never
    /// retrieved into reviews and only the operator CLI can remove them.
    pub repo: Option<RepoScope>,
    /// Pull request the learning was taught on, when known.
    pub origin_pr: Option<u64>,
}

/// Which learnings [`LearningsStore::list`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LearningsFilter {
    /// Every learning, scoped or not.
    All,
    /// Only learnings scoped to this repository.
    Repo(RepoScope),
    /// Only legacy learnings with no repository.
    Unscoped,
}

impl LearningsFilter {
    pub fn matches(&self, record: &LearningRecord) -> bool {
        match self {
            Self::All => true,
            Self::Repo(scope) => record.repo.as_ref() == Some(scope),
            Self::Unscoped => record.repo.is_none(),
        }
    }
}

/// Whether `remove(id, scope)` may delete `record`. `None` is the
/// operator path and may delete anything; a repository scope may only
/// delete its own learnings.
pub(crate) fn removable_in_scope(record: &LearningRecord, scope: Option<&RepoScope>) -> bool {
    match scope {
        None => true,
        Some(scope) => record.repo.as_ref() == Some(scope),
    }
}

/// Rank `records` scoped to `scope` by cosine similarity to `query`
/// and keep the best `top_k`. Unscoped records never match.
pub(crate) fn rank_in_scope(
    records: impl IntoIterator<Item = LearningRecord>,
    query: &[f32],
    top_k: usize,
    scope: &RepoScope,
) -> Vec<ScoredLearning> {
    let mut scored: Vec<ScoredLearning> = records
        .into_iter()
        .filter(|r| r.repo.as_ref() == Some(scope))
        .map(|learning| ScoredLearning {
            score: cosine_similarity(query, &learning.embedding),
            learning,
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(top_k);
    scored
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredLearning {
    pub learning: LearningRecord,
    pub score: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum LearningsError {
    #[error("learning {0} not found")]
    NotFound(u64),
    #[error("storage error: {0}")]
    Storage(String),
}

#[async_trait]
pub trait LearningsStore: Send + Sync {
    async fn add(
        &self,
        text: String,
        source: LearningSource,
        embedding: Vec<f32>,
        now: i64,
        scope: &RepoScope,
        origin_pr: Option<u64>,
    ) -> Result<LearningRecord, LearningsError>;

    async fn list(&self, filter: LearningsFilter) -> Result<Vec<LearningRecord>, LearningsError>;

    /// Remove learning `id`. With `Some(scope)` the learning must belong
    /// to that repository, otherwise the call fails with
    /// [`LearningsError::NotFound`] and nothing is removed. `None` is
    /// the operator CLI path and removes any learning by id.
    async fn remove(&self, id: u64, scope: Option<&RepoScope>) -> Result<(), LearningsError>;

    /// The `top_k` learnings of `scope` most similar to `query`.
    /// Learnings of other repositories and unscoped learnings are never
    /// returned.
    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
        scope: &RepoScope,
    ) -> Result<Vec<ScoredLearning>, LearningsError>;
}

pub struct InMemoryLearningsStore {
    inner: tokio::sync::Mutex<InMemoryState>,
}

struct InMemoryState {
    next_id: u64,
    records: Vec<LearningRecord>,
}

impl Default for InMemoryLearningsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryLearningsStore {
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(InMemoryState {
                next_id: 1,
                records: Vec::new(),
            }),
        }
    }

    /// Seed a store with existing records, including legacy unscoped
    /// ones. New ids continue after the highest seeded id.
    pub fn from_records(records: Vec<LearningRecord>) -> Self {
        let next_id = records.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        Self {
            inner: tokio::sync::Mutex::new(InMemoryState { next_id, records }),
        }
    }
}

#[async_trait]
impl LearningsStore for InMemoryLearningsStore {
    async fn add(
        &self,
        text: String,
        source: LearningSource,
        embedding: Vec<f32>,
        now: i64,
        scope: &RepoScope,
        origin_pr: Option<u64>,
    ) -> Result<LearningRecord, LearningsError> {
        let mut state = self.inner.lock().await;
        let id = state.next_id;
        state.next_id += 1;
        let record = LearningRecord {
            id,
            text,
            source,
            embedding,
            created_at: now,
            repo: Some(scope.clone()),
            origin_pr,
        };
        state.records.push(record.clone());
        Ok(record)
    }

    async fn list(&self, filter: LearningsFilter) -> Result<Vec<LearningRecord>, LearningsError> {
        Ok(self
            .inner
            .lock()
            .await
            .records
            .iter()
            .filter(|r| filter.matches(r))
            .cloned()
            .collect())
    }

    async fn remove(&self, id: u64, scope: Option<&RepoScope>) -> Result<(), LearningsError> {
        let mut state = self.inner.lock().await;
        let Some(index) = state
            .records
            .iter()
            .position(|r| r.id == id && removable_in_scope(r, scope))
        else {
            return Err(LearningsError::NotFound(id));
        };
        state.records.remove(index);
        Ok(())
    }

    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
        scope: &RepoScope,
    ) -> Result<Vec<ScoredLearning>, LearningsError> {
        let records = self.inner.lock().await.records.clone();
        Ok(rank_in_scope(records, query, top_k, scope))
    }
}

pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
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

    fn scope() -> RepoScope {
        RepoScope::new("acme", "widget")
    }

    fn legacy(id: u64, text: &str) -> LearningRecord {
        LearningRecord {
            id,
            text: text.into(),
            source: LearningSource::Inferred,
            embedding: vec![1.0, 0.0],
            created_at: 0,
            repo: None,
            origin_pr: None,
        }
    }

    /// One learning in each of two scopes plus one unscoped legacy row,
    /// all with the same embedding.
    async fn two_scopes_and_legacy() -> (InMemoryLearningsStore, LearningRecord, LearningRecord) {
        let store = InMemoryLearningsStore::from_records(vec![legacy(1, "legacy")]);
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
        (store, a, b)
    }

    #[tokio::test]
    async fn query_nearest_excludes_other_scopes_and_unscoped_records() {
        let (store, a, _) = two_scopes_and_legacy().await;
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
        let (store, a, b) = two_scopes_and_legacy().await;
        let scope_b = RepoScope::new("acme", "b");
        for id in [a.id, 1, 999] {
            let err = store
                .remove(id, Some(&scope_b))
                .await
                .expect_err("not found");
            assert!(matches!(err, LearningsError::NotFound(n) if n == id));
        }
        assert_eq!(store.list(LearningsFilter::All).await.unwrap().len(), 3);
        store.remove(b.id, Some(&scope_b)).await.unwrap();
        store.remove(1, None).await.unwrap();
        let left = store.list(LearningsFilter::All).await.unwrap();
        assert_eq!(left, vec![a]);
    }

    #[tokio::test]
    async fn list_filters_by_scope() {
        let (store, a, b) = two_scopes_and_legacy().await;
        assert_eq!(a.repo, Some(RepoScope::new("acme", "a")));
        assert_eq!(a.origin_pr, Some(3));
        assert_eq!(
            store
                .list(LearningsFilter::Repo(RepoScope::new("acme", "b")))
                .await
                .unwrap(),
            vec![b]
        );
        let unscoped = store.list(LearningsFilter::Unscoped).await.unwrap();
        assert_eq!(unscoped.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(store.list(LearningsFilter::All).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn add_then_list_returns_inserted_record() {
        let store = InMemoryLearningsStore::new();
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
        assert_eq!(r.id, 1);
        assert_eq!(r.source, LearningSource::Guideline);
        let all = store.list(LearningsFilter::All).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "Forbid unwrap() outside tests.");
    }

    #[tokio::test]
    async fn ids_are_monotonic() {
        let store = InMemoryLearningsStore::new();
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
        let store = InMemoryLearningsStore::new();
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
        let store = InMemoryLearningsStore::new();
        let err = store.remove(999, None).await.expect_err("err");
        assert!(matches!(err, LearningsError::NotFound(999)));
    }

    #[tokio::test]
    async fn query_nearest_returns_top_k_by_similarity() {
        let store = InMemoryLearningsStore::new();
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
    async fn query_nearest_only_returns_learnings_of_the_requested_scope() {
        let store = InMemoryLearningsStore::new();
        let scope_a = RepoScope::new("acme", "a");
        let scope_b = RepoScope::new("acme", "b");
        store
            .add(
                "from a".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &scope_a,
                None,
            )
            .await
            .unwrap();
        store
            .add(
                "from b".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &scope_b,
                None,
            )
            .await
            .unwrap();
        let r = store.query_nearest(&[1.0, 0.0], 5, &scope_b).await.unwrap();
        let texts: Vec<&str> = r.iter().map(|s| s.learning.text.as_str()).collect();
        assert_eq!(texts, vec!["from b"]);
    }

    #[tokio::test]
    async fn query_against_empty_store_returns_empty_vec() {
        let store = InMemoryLearningsStore::new();
        let r = store.query_nearest(&[1.0, 0.0], 5, &scope()).await.unwrap();
        assert!(r.is_empty());
    }
}
