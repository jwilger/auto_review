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
    ) -> Result<LearningRecord, LearningsError>;

    async fn list(&self) -> Result<Vec<LearningRecord>, LearningsError>;

    async fn remove(&self, id: u64) -> Result<(), LearningsError>;

    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
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
}

#[async_trait]
impl LearningsStore for InMemoryLearningsStore {
    async fn add(
        &self,
        text: String,
        source: LearningSource,
        embedding: Vec<f32>,
        now: i64,
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
        };
        state.records.push(record.clone());
        Ok(record)
    }

    async fn list(&self) -> Result<Vec<LearningRecord>, LearningsError> {
        Ok(self.inner.lock().await.records.clone())
    }

    async fn remove(&self, id: u64) -> Result<(), LearningsError> {
        let mut state = self.inner.lock().await;
        let before = state.records.len();
        state.records.retain(|r| r.id != id);
        if state.records.len() == before {
            return Err(LearningsError::NotFound(id));
        }
        Ok(())
    }

    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredLearning>, LearningsError> {
        let state = self.inner.lock().await;
        let mut scored: Vec<ScoredLearning> = state
            .records
            .iter()
            .map(|r| ScoredLearning {
                learning: r.clone(),
                score: cosine_similarity(query, &r.embedding),
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
        let store = InMemoryLearningsStore::new();
        let r = store
            .add(
                "Forbid unwrap() outside tests.".into(),
                LearningSource::Guideline,
                vec![0.1, 0.2, 0.3],
                1700000000,
            )
            .await
            .unwrap();
        assert_eq!(r.id, 1);
        assert_eq!(r.source, LearningSource::Guideline);
        let all = store.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "Forbid unwrap() outside tests.");
    }

    #[tokio::test]
    async fn ids_are_monotonic() {
        let store = InMemoryLearningsStore::new();
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
        let store = InMemoryLearningsStore::new();
        let r = store
            .add("x".into(), LearningSource::Chat, vec![1.0], 0)
            .await
            .unwrap();
        store.remove(r.id).await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_returns_not_found_for_unknown_id() {
        let store = InMemoryLearningsStore::new();
        let err = store.remove(999).await.expect_err("err");
        assert!(matches!(err, LearningsError::NotFound(999)));
    }

    #[tokio::test]
    async fn query_nearest_returns_top_k_by_similarity() {
        let store = InMemoryLearningsStore::new();
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
        let store = InMemoryLearningsStore::new();
        let r = store.query_nearest(&[1.0, 0.0], 5).await.unwrap();
        assert!(r.is_empty());
    }
}
