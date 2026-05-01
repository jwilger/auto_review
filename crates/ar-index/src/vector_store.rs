//! Pluggable vector store. The pipeline only knows the trait;
//! production swaps in a LanceDB backing, tests swap in
//! [`InMemoryVectorStore`] (cosine similarity, no persistence).
//!
//! Decoupling now means LanceDB integration lands as one new file
//! later without touching the review pipeline.

use crate::embed::EmbeddedSymbol;
use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum VectorStoreError {
    #[error("storage error: {0}")]
    Storage(String),
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Insert one or more pre-embedded symbols. Re-inserting an
    /// existing symbol (same path + name + line_start) replaces the
    /// previous record.
    async fn upsert(&self, symbols: &[EmbeddedSymbol]) -> Result<(), VectorStoreError>;

    /// Return the `top_k` symbols whose embeddings are most similar
    /// to `query`, descending by similarity score (1.0 = identical
    /// direction, 0.0 = orthogonal, -1.0 = opposite).
    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredSymbol>, VectorStoreError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredSymbol {
    pub symbol: EmbeddedSymbol,
    pub score: f32,
}

/// Pure-Rust, no-persistence vector store. Replaces records on
/// duplicate keys (path + symbol name + line_start). Cosine-
/// similarity search over the full set on every query — fine for the
/// thousands of symbols a typical repo produces; LanceDB takes over
/// when scale demands it.
pub struct InMemoryVectorStore {
    inner: tokio::sync::Mutex<Vec<EmbeddedSymbol>>,
}

impl Default for InMemoryVectorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryVectorStore {
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }
}

#[async_trait]
impl VectorStore for InMemoryVectorStore {
    async fn upsert(&self, symbols: &[EmbeddedSymbol]) -> Result<(), VectorStoreError> {
        let mut guard = self.inner.lock().await;
        for sym in symbols {
            let key = symbol_key(sym);
            if let Some(existing) = guard.iter_mut().find(|s| symbol_key(s) == key) {
                *existing = sym.clone();
            } else {
                guard.push(sym.clone());
            }
        }
        Ok(())
    }

    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredSymbol>, VectorStoreError> {
        let guard = self.inner.lock().await;
        let mut scored: Vec<ScoredSymbol> = guard
            .iter()
            .map(|s| ScoredSymbol {
                symbol: s.clone(),
                score: cosine_similarity(query, &s.embedding),
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

fn symbol_key(s: &EmbeddedSymbol) -> (String, String, u32) {
    (
        s.indexed.path.clone(),
        s.indexed.symbol.name.clone(),
        s.indexed.symbol.line_start,
    )
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
    use crate::symbols::{Symbol, SymbolKind};
    use crate::walker::IndexedSymbol;

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
            content: String::new(),
            embedding,
        }
    }

    #[tokio::test]
    async fn empty_store_returns_no_matches() {
        let store = InMemoryVectorStore::new();
        let r = store.query_nearest(&[1.0, 0.0], 5).await.unwrap();
        assert!(r.is_empty());
        assert!(store.is_empty().await);
    }

    #[tokio::test]
    async fn upsert_then_query_returns_ranked_results() {
        let store = InMemoryVectorStore::new();
        let east = esym("a.rs", "east", 1, vec![1.0, 0.0]);
        let north_east = esym("a.rs", "ne", 2, vec![1.0, 1.0]);
        let north = esym("a.rs", "north", 3, vec![0.0, 1.0]);
        let west = esym("a.rs", "west", 4, vec![-1.0, 0.0]);
        store
            .upsert(&[
                east.clone(),
                north_east.clone(),
                north.clone(),
                west.clone(),
            ])
            .await
            .unwrap();
        assert_eq!(store.len().await, 4);

        let r = store.query_nearest(&[1.0, 0.0], 3).await.unwrap();
        assert_eq!(r.len(), 3);
        // Closest: east (cos=1.0), then north_east (cos≈0.707), then
        // north (cos=0.0).
        assert_eq!(r[0].symbol.indexed.symbol.name, "east");
        assert!(r[0].score > 0.99);
        assert_eq!(r[1].symbol.indexed.symbol.name, "ne");
        assert!(r[1].score > 0.7 && r[1].score < 0.71);
        assert_eq!(r[2].symbol.indexed.symbol.name, "north");
    }

    #[tokio::test]
    async fn upsert_replaces_on_same_key() {
        let store = InMemoryVectorStore::new();
        let v1 = esym("a.rs", "x", 1, vec![1.0, 0.0]);
        store.upsert(&[v1]).await.unwrap();
        let v2 = esym("a.rs", "x", 1, vec![0.0, 1.0]);
        store.upsert(std::slice::from_ref(&v2)).await.unwrap();
        assert_eq!(store.len().await, 1);
        let r = store.query_nearest(&[0.0, 1.0], 1).await.unwrap();
        assert!(r[0].score > 0.99);
        assert_eq!(r[0].symbol.embedding, vec![0.0, 1.0]);
    }

    #[tokio::test]
    async fn cosine_handles_zero_vectors_and_mismatched_lengths() {
        let store = InMemoryVectorStore::new();
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
    async fn top_k_caps_results() {
        let store = InMemoryVectorStore::new();
        for i in 0..10 {
            store
                .upsert(&[esym("a.rs", &format!("s{i}"), i as u32 + 1, vec![1.0, 0.0])])
                .await
                .unwrap();
        }
        let r = store.query_nearest(&[1.0, 0.0], 3).await.unwrap();
        assert_eq!(r.len(), 3);
    }
}
