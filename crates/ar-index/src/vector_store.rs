//! Pluggable vector store. The pipeline only knows the trait;
//! production swaps in a `LanceDB` backing, tests swap in
//! [`InMemoryVectorStore`] (cosine similarity, no persistence).
//!
//! Decoupling now means `LanceDB` integration lands as one new file
//! later without touching the review pipeline.

use crate::embed::EmbeddedSymbol;
use async_trait::async_trait;
use core::cmp::Ordering;
use std::collections::HashMap;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::VectorStoreError, where the module name \
              does not appear; ar-review imports it under that name"
)]
pub enum VectorStoreError {
    #[error("storage error: {0}")]
    Storage(String),
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Look up rows whose `(path, name, line_start)` matches one of
    /// `keys`. Returned map is keyed by the same tuple. Callers use
    /// this to skip embedding work for symbols whose stored content
    /// matches the current snippet.
    async fn fetch_by_keys(
        &self,
        keys: &[(String, String, u32)],
    ) -> Result<HashMap<(String, String, u32), EmbeddedSymbol>, VectorStoreError>;

    /// Return the `top_k` symbols whose embeddings are most similar
    /// to `query`, descending by similarity score (1.0 = identical
    /// direction, 0.0 = orthogonal, -1.0 = opposite).
    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredSymbol>, VectorStoreError>;

    /// Insert one or more pre-embedded symbols. Re-inserting an
    /// existing symbol (same path + name + `line_start`) replaces the
    /// previous record.
    async fn upsert(&self, symbols: &[EmbeddedSymbol]) -> Result<(), VectorStoreError>;
}

/// A stored symbol with its similarity to a query embedding.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (context_builder, rag_context) builds ScoredSymbol with a struct \
              literal; #[non_exhaustive] would break that construction"
)]
pub struct ScoredSymbol {
    /// Cosine similarity between the query and `symbol.embedding`.
    pub score: f32,
    /// The matched symbol, including its stored embedding.
    pub symbol: EmbeddedSymbol,
}

/// Pure-Rust, no-persistence vector store. Replaces records on
/// duplicate keys (path + symbol name + `line_start`). Cosine-
/// similarity search over the full set on every query — fine for the
/// thousands of symbols a typical repo produces; `LanceDB` takes over
/// when scale demands it.
#[expect(
    clippy::module_name_repetitions,
    reason = "re-exported at the crate root as ar_index::InMemoryVectorStore, where the module \
              name does not appear; ar-gateway and ar-review use it under that name"
)]
pub struct InMemoryVectorStore {
    /// Every stored symbol, at most one per `(path, name, line_start)` key.
    inner: Mutex<Vec<EmbeddedSymbol>>,
}

impl Default for InMemoryVectorStore {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryVectorStore {
    /// Whether the store holds no symbols.
    #[inline]
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    /// Number of stored symbols.
    #[inline]
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// An empty store.
    #[inline]
    #[must_use = "building a store has no side effects; the store is the only result"]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl VectorStore for InMemoryVectorStore {
    #[inline]
    async fn fetch_by_keys(
        &self,
        keys: &[(String, String, u32)],
    ) -> Result<HashMap<(String, String, u32), EmbeddedSymbol>, VectorStoreError> {
        let guard = self.inner.lock().await;
        let mut out = HashMap::with_capacity(keys.len());
        for sym in guard.iter() {
            let key = symbol_key(sym);
            if keys.contains(&key) {
                out.insert(key, sym.clone());
            }
        }
        Ok(out)
    }

    #[inline]
    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredSymbol>, VectorStoreError> {
        let guard = self.inner.lock().await;
        let mut scored: Vec<ScoredSymbol> = guard
            .iter()
            .map(|stored| ScoredSymbol {
                symbol: stored.clone(),
                score: cosine_similarity(query, &stored.embedding),
            })
            .collect();
        scored.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }

    #[inline]
    async fn upsert(&self, symbols: &[EmbeddedSymbol]) -> Result<(), VectorStoreError> {
        let mut guard = self.inner.lock().await;
        for sym in symbols {
            let key = symbol_key(sym);
            if let Some(existing) = guard.iter_mut().find(|stored| symbol_key(stored) == key) {
                *existing = sym.clone();
            } else {
                guard.push(sym.clone());
            }
        }
        Ok(())
    }
}

/// Identity of a stored symbol: `(path, name, line_start)`.
fn symbol_key(symbol: &EmbeddedSymbol) -> (String, String, u32) {
    (
        symbol.indexed.path.clone(),
        symbol.indexed.symbol.name.clone(),
        symbol.indexed.symbol.line_start,
    )
}

/// Cosine similarity of two equal-length vectors; 0.0 when the lengths
/// differ, either is empty, or either has zero magnitude.
#[expect(
    clippy::float_arithmetic,
    reason = "cosine similarity over f32 embeddings is inherently float arithmetic"
)]
fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let dot: f32 = left.iter().zip(right.iter()).map(|(x, y)| x * y).sum();
    let norm_a = left.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = right.iter().map(|x| x * x).sum::<f32>().sqrt();
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
    use core::slice;

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
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn empty_store_returns_no_matches() {
        let store = InMemoryVectorStore::new();
        let r = store.query_nearest(&[1.0, 0.0], 5).await.unwrap();
        assert!(r.is_empty());
        assert!(store.is_empty().await);
    }

    #[tokio::test]
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
    async fn upsert_replaces_on_same_key() {
        let store = InMemoryVectorStore::new();
        let v1 = esym("a.rs", "x", 1, vec![1.0, 0.0]);
        store.upsert(&[v1]).await.unwrap();
        let v2 = esym("a.rs", "x", 1, vec![0.0, 1.0]);
        store.upsert(slice::from_ref(&v2)).await.unwrap();
        assert_eq!(store.len().await, 1);
        let r = store.query_nearest(&[0.0, 1.0], 1).await.unwrap();
        assert!(r[0].score > 0.99);
        assert_eq!(r[0].symbol.embedding, vec![0.0, 1.0]);
    }

    #[tokio::test]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::shadow_unrelated,
        reason = "`r` is rebound to each query result in turn and read by the assertions after it; renaming would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
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
    #[expect(
        clippy::get_unwrap,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn in_memory_fetch_by_keys_returns_only_matches() {
        let store = InMemoryVectorStore::new();
        store
            .upsert(&[
                esym("a.rs", "foo", 1, vec![1.0]),
                esym("a.rs", "bar", 5, vec![0.5]),
            ])
            .await
            .unwrap();
        let got = store
            .fetch_by_keys(&[
                ("a.rs".into(), "foo".into(), 1),
                ("a.rs".into(), "missing".into(), 9),
            ])
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        let key: (String, String, u32) = ("a.rs".to_owned(), "foo".to_owned(), 1);
        assert_eq!(got.get(&key).unwrap().embedding, vec![1.0]);
    }

    #[tokio::test]
    #[expect(
        clippy::as_conversions,
        clippy::cast_sign_loss,
        reason = "test fixture values (small loop indices, ids and byte lengths) are far inside the target type's range, so the cast is exact"
    )]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "the loop bounds are small literals that take the default integer type; the fixture casts each index explicitly where it needs u32"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
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
