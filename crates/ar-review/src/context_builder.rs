//! Build the `repo_context` markdown for the review prompt by
//! orchestrating the ar-index pieces (walker + embedder + vector
//! store + learnings store) end-to-end.
//!
//! Currently builds a fresh in-memory index per review — fine for
//! repos up to a few thousand source files; LanceDB-backed
//! persistence and incremental updates land later.

use crate::rag_context::format_repo_context;
use ar_index::{
    embed_symbols_with_config, index_workspace, EmbedConfig, EmbedError, EmbeddedSymbol,
    InMemoryVectorStore, LearningsStore, ScoredLearning, ScoredSymbol, VectorStore, WalkError,
};
use ar_llm::{ModelTier, Router};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ContextBuildError {
    #[error("walk: {0}")]
    Walk(#[from] WalkError),
    #[error("embed: {0}")]
    Embed(#[from] EmbedError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Build a markdown context block for the LLM prompt. Returns an empty
/// string when there's nothing useful to inject (no Embedding tier
/// configured, workspace had no extractable symbols, embedder failed
/// gracefully). Errors only on hard failures.
///
/// `top_k` controls how many similar symbols + learnings to include.
pub async fn build_review_context(
    workspace_path: &Path,
    router: &Router,
    diff: &str,
    learnings: Option<&dyn LearningsStore>,
    top_k: usize,
) -> Result<String, ContextBuildError> {
    // No Embedding tier ⇒ no RAG, return empty.
    if router.provider(ModelTier::Embedding).is_err() {
        return Ok(String::new());
    }

    let symbols = index_workspace(workspace_path)?;
    if symbols.is_empty() && learnings.is_none() {
        return Ok(String::new());
    }

    // Read each touched file's contents into a map for the embedder.
    let mut file_contents: HashMap<String, String> = HashMap::new();
    for sym in &symbols {
        if file_contents.contains_key(&sym.path) {
            continue;
        }
        let abs = workspace_path.join(&sym.path);
        match fs::read_to_string(&abs) {
            Ok(contents) => {
                file_contents.insert(sym.path.clone(), contents);
            }
            Err(e) => {
                tracing::debug!(path = %sym.path, error = %e, "skip unreadable file");
            }
        }
    }

    // Drop symbols whose file we couldn't read.
    let symbols: Vec<_> = symbols
        .into_iter()
        .filter(|s| file_contents.contains_key(&s.path))
        .collect();

    // Embed the diff once and reuse for both the symbol-similarity
    // and learnings-similarity queries. Previously each helper
    // re-embedded the diff independently — wasted one round-trip
    // per review with both stores configured.
    //
    // Cap the embedded text using the same EmbedConfig that governs
    // symbol embeddings, so a single AR_EMBED_INPUT_CAP_BYTES knob
    // controls every embed call and providers with tight token
    // ceilings (e.g. text-embedding-3-small at 8192 tokens) don't
    // get a 400 they can't recover from. See #26.
    let cfg = EmbedConfig::from_env();
    let query_text = crate::diff::cap_for_embed(diff, cfg.input_cap_bytes);
    let query_vec = match router.embed(ModelTier::Embedding, &[query_text]).await {
        Ok(mut vecs) => vecs.pop().unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "diff embedding failed; skipping RAG context");
            return Ok(String::new());
        }
    };
    if query_vec.is_empty() {
        return Ok(String::new());
    }

    let scored_symbols =
        embed_and_query_symbols(router, &symbols, &file_contents, &query_vec, top_k, &cfg)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "symbol embedding/query failed; skipping that section");
                Vec::new()
            });

    let scored_learnings = match learnings {
        Some(store) => query_learnings(store, &query_vec, top_k)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "learnings query failed; skipping that section");
                Vec::new()
            }),
        None => Vec::new(),
    };

    Ok(format_repo_context(&scored_symbols, &scored_learnings, &[]))
}

async fn embed_and_query_symbols(
    router: &Router,
    symbols: &[ar_index::IndexedSymbol],
    file_contents: &HashMap<String, String>,
    query_vec: &[f32],
    top_k: usize,
    cfg: &EmbedConfig,
) -> Result<Vec<ScoredSymbol>, ContextBuildError> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    let embedded: Vec<EmbeddedSymbol> =
        embed_symbols_with_config(router, symbols, file_contents, cfg).await?;

    let store = InMemoryVectorStore::new();
    store.upsert(&embedded).await.map_err(|e| {
        ContextBuildError::Embed(EmbedError::Llm(ar_llm::Error::Provider {
            status: 500,
            body: e.to_string(),
        }))
    })?;

    let scored = store.query_nearest(query_vec, top_k).await.map_err(|e| {
        ContextBuildError::Embed(EmbedError::Llm(ar_llm::Error::Provider {
            status: 500,
            body: e.to_string(),
        }))
    })?;
    Ok(scored)
}

async fn query_learnings(
    store: &(dyn LearningsStore + Sync),
    query_vec: &[f32],
    top_k: usize,
) -> Result<Vec<ScoredLearning>, ContextBuildError> {
    let scored = store.query_nearest(query_vec, top_k).await.map_err(|e| {
        ContextBuildError::Embed(EmbedError::Llm(ar_llm::Error::Provider {
            status: 500,
            body: e.to_string(),
        }))
    })?;
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::{CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider};
    use async_trait::async_trait;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Embedder that returns deterministic 3-D vectors keyed off
    /// content and records every batch it was asked to embed.
    /// Different test inputs get distinct directions.
    struct DeterministicEmbedder {
        seen: Mutex<Vec<Vec<String>>>,
    }

    impl DeterministicEmbedder {
        fn new() -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl LlmProvider for DeterministicEmbedder {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            unimplemented!()
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            self.seen.lock().unwrap().push(texts.to_vec());
            Ok(texts
                .iter()
                .map(|t| {
                    let bytes = t.as_bytes();
                    vec![
                        bytes.len() as f32,
                        bytes.first().copied().unwrap_or(0) as f32,
                        bytes.last().copied().unwrap_or(0) as f32,
                    ]
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn returns_empty_when_no_embedding_tier_configured() {
        let dir = tempdir().unwrap();
        let router = Router::new();
        let result = build_review_context(dir.path(), &router, "diff", None, 5)
            .await
            .expect("ok");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn returns_empty_for_workspace_with_no_extractable_symbols() {
        let dir = tempdir().unwrap();
        // Only files we don't have grammars for.
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        fs::write(dir.path().join("README"), "hello").unwrap();

        let embedder = std::sync::Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let result = build_review_context(dir.path(), &router, "diff", None, 5)
            .await
            .expect("ok");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn diff_is_embedded_only_once_even_with_learnings_store() {
        // Regression: previously the diff was embedded twice — once
        // for symbol-similarity, once for learnings-similarity. This
        // test pins the dedup so a future refactor doesn't silently
        // re-introduce the double-call.
        use ar_index::InMemoryLearningsStore;

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn foo() {}\n").unwrap();

        let embedder = std::sync::Arc::new(DeterministicEmbedder::new());
        let calls_handle = embedder.clone();
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let store = InMemoryLearningsStore::new();

        let _ = build_review_context(dir.path(), &router, "diff text", Some(&store), 3)
            .await
            .expect("ok");

        // Expected calls: 1 for symbols batch + 1 for diff = 2.
        // Previously was 1 + 2 (two diff embeds, one per query
        // helper) = 3.
        let calls = calls_handle.call_count();
        assert_eq!(
            calls, 2,
            "expected one symbols embed + one diff embed; got {calls}"
        );
    }

    #[tokio::test]
    async fn diff_embedding_input_is_capped_to_embed_config_default() {
        // Regression for #26: the diff used to be capped at a hardcoded
        // 32 KiB byte cap that exceeded text-embedding-3-small's
        // 8192-token limit on dense source. The cap must follow
        // EmbedConfig::input_cap_bytes (default 6 KiB) so the same
        // AR_EMBED_INPUT_CAP_BYTES knob governs both diff embedding
        // and symbol embedding.
        use ar_index::DEFAULT_EMBED_INPUT_CAP_BYTES;

        let dir = tempdir().unwrap();
        // One small symbol file so build_review_context proceeds past
        // its early-return; the symbol embed call is filtered out of
        // the assertion below by the leading-`x` predicate.
        fs::write(dir.path().join("a.rs"), "pub fn ok() {}\n").unwrap();

        let embedder = std::sync::Arc::new(DeterministicEmbedder::new());
        let recorder = embedder.clone();
        let router = Router::new().with(ModelTier::Embedding, embedder);

        // 256 KiB diff — well above the historical 32 KiB cap and
        // many multiples of the 6 KiB default.
        let big_diff = "x".repeat(256 * 1024);
        let _ = build_review_context(dir.path(), &router, &big_diff, None, 3).await;

        let seen = recorder.seen.lock().unwrap();
        let diff_call = seen
            .iter()
            .find(|batch| batch.iter().any(|t| t.starts_with('x')))
            .expect("expected a diff embed call");
        let diff_input_bytes = diff_call[0].len();
        assert!(
            diff_input_bytes <= DEFAULT_EMBED_INPUT_CAP_BYTES,
            "diff embed input was {diff_input_bytes} bytes; expected <= {DEFAULT_EMBED_INPUT_CAP_BYTES} (EmbedConfig default)"
        );
    }

    #[tokio::test]
    async fn produces_repo_context_markdown_when_workspace_has_symbols() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("a.rs"),
            "pub fn foo() {\n    let _ = 1;\n}\n",
        )
        .unwrap();

        let embedder = std::sync::Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let result = build_review_context(dir.path(), &router, "diff", None, 5)
            .await
            .expect("ok");
        // Markdown should contain the formatted-context section header.
        assert!(result.contains("Similar code in this repo"));
        assert!(result.contains("**foo**"));
    }
}
