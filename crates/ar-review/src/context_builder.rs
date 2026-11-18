//! Build the `repo_context` markdown for the review prompt by
//! orchestrating the ar-index pieces (walker + embedder + vector
//! store + learnings store) end-to-end.
//!
//! Currently builds a fresh in-memory index per review — fine for
//! repos up to a few thousand source files; LanceDB-backed
//! persistence and incremental updates land later.

use crate::diff::cap_for_embed;
use crate::rag_context::format_repo_context;
use ar_index::{
    embed_symbols_with_config, index_workspace, EmbedConfig, EmbedError, EmbeddedSymbol,
    InMemoryVectorStore, LearningsStore, RepoScope, ScoredLearning, ScoredSymbol, VectorStore,
    WalkError,
};
use ar_llm::{Error as LlmError, ModelTier, Router};
use core::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// Hard failure while building the RAG context block.
#[derive(Debug, thiserror::Error)]
pub enum ContextBuildError {
    /// Embedding symbols or querying a store failed.
    #[error("embed: {0}")]
    Embed(#[from] EmbedError),
    /// Reading from the workspace failed.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// Walking the workspace for symbols failed.
    #[error("walk: {0}")]
    Walk(#[from] WalkError),
}

/// Build a markdown context block for the LLM prompt. Returns an empty
/// string when there's nothing useful to inject (no Embedding tier
/// configured, workspace had no extractable symbols, embedder failed
/// gracefully). Errors only on hard failures.
///
/// `top_k` controls how many similar symbols + learnings to include.
/// Only learnings scoped to `scope`, the reviewed repository, are
/// retrieved.
///
/// Back-compat shim: builds a fresh in-memory vector store per call.
/// New callers should prefer [`build_review_context_with_store`] and
/// thread a shared, persistent store so symbol embeddings survive
/// across reviews.
///
/// # Errors
///
/// Returns [`ContextBuildError::Walk`] when the workspace cannot be
/// indexed; embedding and store failures degrade to an empty section.
#[inline]
pub async fn build_review_context(
    workspace_path: &Path,
    router: &Router,
    diff: &str,
    learnings: Option<&dyn LearningsStore>,
    scope: &RepoScope,
    top_k: usize,
) -> Result<String, ContextBuildError> {
    build_review_context_with_store(workspace_path, router, diff, learnings, scope, top_k, None)
        .await
}

/// Like [`build_review_context`], but threads a caller-owned vector
/// store through the symbol-embedding step so cached embeddings survive
/// across reviews and across gateway restarts (when the store is
/// SQLite-backed).
///
/// When `vector_store` is `None`, behaves exactly like the back-compat
/// helper: a fresh in-memory store is constructed for the call and
/// thrown away.
///
/// # Errors
///
/// Returns [`ContextBuildError::Walk`] when the workspace cannot be
/// indexed; embedding and store failures degrade to an empty section.
#[inline]
pub async fn build_review_context_with_store(
    workspace_path: &Path,
    router: &Router,
    diff: &str,
    learnings: Option<&dyn LearningsStore>,
    scope: &RepoScope,
    top_k: usize,
    vector_store: Option<&dyn VectorStore>,
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
            Err(err) => {
                tracing::debug!(path = %sym.path, error = %err, "skip unreadable file");
            }
        }
    }

    // Drop symbols whose file we couldn't read.
    let readable_symbols: Vec<_> = symbols
        .into_iter()
        .filter(|sym| file_contents.contains_key(&sym.path))
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
    let query_text = cap_for_embed(diff, cfg.input_cap_bytes);
    let query_vec = match router.embed(ModelTier::Embedding, &[query_text]).await {
        Ok(mut vecs) => vecs.pop().unwrap_or_default(),
        Err(err) => {
            tracing::warn!(error = %err, "diff embedding failed; skipping RAG context");
            return Ok(String::new());
        }
    };
    if query_vec.is_empty() {
        return Ok(String::new());
    }

    let scored_symbols = embed_and_query_symbols(
        router,
        &readable_symbols,
        &file_contents,
        &query_vec,
        top_k,
        &cfg,
        vector_store,
    )
    .await
    .unwrap_or_else(|err| {
        tracing::warn!(error = %err, "symbol embedding/query failed; skipping that section");
        Vec::new()
    });

    let scored_learnings = match learnings {
        Some(store) => query_learnings(store, &query_vec, top_k, scope)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "learnings query failed; skipping that section");
                Vec::new()
            }),
        None => Vec::new(),
    };

    Ok(format_repo_context(&scored_symbols, &scored_learnings))
}

/// Score `symbols` against `query_vec`, reusing cached embeddings from
/// `shared_store` when present, and return the `top_k` nearest.
async fn embed_and_query_symbols(
    router: &Router,
    symbols: &[ar_index::IndexedSymbol],
    file_contents: &HashMap<String, String>,
    query_vec: &[f32],
    top_k: usize,
    cfg: &EmbedConfig,
    shared_store: Option<&dyn VectorStore>,
) -> Result<Vec<ScoredSymbol>, ContextBuildError> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    if let Some(store) = shared_store {
        // Cache-aware path: pre-compute each symbol's snippet, look
        // up cached entries by `(path, name, line_start)`, and skip
        // embedding for symbols whose stored content matches the
        // current snippet bit-for-bit. The win is amortised across
        // re-reviews of the same PR — the typical case for any
        // mid-life PR receiving multiple pushes.
        let snippets = compute_snippets(symbols, file_contents)?;
        let keys: Vec<(String, String, u32)> = symbols
            .iter()
            .map(|sym| {
                (
                    sym.path.clone(),
                    sym.symbol.name.clone(),
                    sym.symbol.line_start,
                )
            })
            .collect();
        let cached = store
            .fetch_by_keys(&keys)
            .await
            .map_err(|err| map_vector_err(&err))?;

        let mut to_embed: Vec<ar_index::IndexedSymbol> = Vec::new();
        let mut cached_for_query: Vec<EmbeddedSymbol> = Vec::new();
        for (sym, snippet) in symbols.iter().zip(snippets.iter()) {
            let key = (
                sym.path.clone(),
                sym.symbol.name.clone(),
                sym.symbol.line_start,
            );
            match cached.get(&key) {
                Some(prev) if &prev.content == snippet => {
                    cached_for_query.push(prev.clone());
                }
                _ => to_embed.push(sym.clone()),
            }
        }

        let newly_embedded = if to_embed.is_empty() {
            Vec::new()
        } else {
            embed_symbols_with_config(router, &to_embed, file_contents, cfg).await?
        };
        if !newly_embedded.is_empty() {
            store
                .upsert(&newly_embedded)
                .await
                .map_err(|err| map_vector_err(&err))?;
        }

        let mut all = cached_for_query;
        all.extend(newly_embedded);
        let scored = score_against_query(&all, query_vec, top_k);
        Ok(scored)
    } else {
        let embedded: Vec<EmbeddedSymbol> =
            embed_symbols_with_config(router, symbols, file_contents, cfg).await?;
        let store = InMemoryVectorStore::new();
        store
            .upsert(&embedded)
            .await
            .map_err(|err| map_vector_err(&err))?;
        let scored = store
            .query_nearest(query_vec, top_k)
            .await
            .map_err(|err| map_vector_err(&err))?;
        Ok(scored)
    }
}

/// Slice each symbol's source lines out of `file_contents`, mirroring
/// what the embedder stores as the symbol's content.
fn compute_snippets(
    symbols: &[ar_index::IndexedSymbol],
    file_contents: &HashMap<String, String>,
) -> Result<Vec<String>, ContextBuildError> {
    let mut out = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let content = file_contents
            .get(&sym.path)
            .ok_or_else(|| ContextBuildError::Embed(EmbedError::MissingFile(sym.path.clone())))?;
        // Mirror the slicing logic in `embed_symbols`: 1-based,
        // inclusive line range, joined with `\n`. Truncation cap
        // doesn't matter for a content-equality compare — both
        // sides are truncated the same way at embed-time, so a
        // change in content past the cap still produces different
        // bytes pre-cap and re-embeds correctly.
        let lines: Vec<&str> = content.lines().collect();
        let out_of_range = || {
            ContextBuildError::Embed(EmbedError::OutOfRange {
                path: sym.path.clone(),
                line_end: sym.symbol.line_end,
            })
        };
        // A line number that does not fit usize cannot be in range, so it
        // saturates and `get` reports it as out of range below.
        let start = usize::try_from(sym.symbol.line_start.saturating_sub(1)).unwrap_or(usize::MAX);
        let end = usize::try_from(sym.symbol.line_end).unwrap_or(usize::MAX);
        let snippet = lines.get(start..end).ok_or_else(out_of_range)?;
        out.push(snippet.join("\n"));
    }
    Ok(out)
}

/// Rank `embedded` by cosine similarity to `query`, keeping the `top_k`.
fn score_against_query(
    embedded: &[EmbeddedSymbol],
    query: &[f32],
    top_k: usize,
) -> Vec<ScoredSymbol> {
    if top_k == 0 {
        return Vec::new();
    }
    let mut scored: Vec<ScoredSymbol> = embedded
        .iter()
        .map(|sym| ScoredSymbol {
            symbol: sym.clone(),
            score: cosine_similarity(query, &sym.embedding),
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
    });
    scored.truncate(top_k);
    scored
}

/// Cosine similarity of two equal-length vectors; 0 for mismatched,
/// empty or zero-norm inputs.
#[expect(
    clippy::float_arithmetic,
    reason = "cosine similarity is inherently float math over f32 embeddings, and the ranking \
              must match ar-index's vector stores exactly"
)]
fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let dot: f32 = left
        .iter()
        .zip(right.iter())
        .map(|(lhs, rhs)| lhs * rhs)
        .sum();
    let left_norm = left.iter().map(|val| val * val).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|val| val * val).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm * right_norm)
    }
}

/// Report a vector-store failure as a provider-style embed error.
fn map_vector_err(err: &ar_index::VectorStoreError) -> ContextBuildError {
    ContextBuildError::Embed(EmbedError::Llm(LlmError::Provider {
        status: 500,
        body: err.to_string(),
    }))
}

/// Fetch the `top_k` learnings nearest `query_vec` within `scope`.
async fn query_learnings(
    store: &(dyn LearningsStore + Sync),
    query_vec: &[f32],
    top_k: usize,
    scope: &RepoScope,
) -> Result<Vec<ScoredLearning>, ContextBuildError> {
    let scored = store
        .query_nearest(query_vec, top_k, scope)
        .await
        .map_err(|err| {
            ContextBuildError::Embed(EmbedError::Llm(LlmError::Provider {
                status: 500,
                body: err.to_string(),
            }))
        })?;
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
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
        #[expect(
            clippy::unwrap_used,
            reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
        )]
        fn call_count(&self) -> usize {
            self.seen.lock().unwrap().len()
        }

        fn new() -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for DeterministicEmbedder {
        #[expect(
            clippy::unimplemented,
            reason = "the embedding fake never serves completions; reaching complete() would be a bug in the test"
        )]
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            unimplemented!()
        }
        #[expect(
            clippy::as_conversions,
            reason = "test fixtures convert small counts and bytes with `as`; the values are tiny, so nothing truncates or loses precision"
        )]
        #[expect(
            clippy::cast_lossless,
            reason = "the fake embedder's u8-to-f32 conversion is the fixture's vector formula, kept as written"
        )]
        #[expect(
            clippy::cast_precision_loss,
            reason = "the fake embedder turns small byte counts into f32 vector components; the values are tiny, so nothing is lost"
        )]
        #[expect(
            clippy::unwrap_used,
            reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
        )]
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            self.seen.lock().unwrap().push(texts.to_vec());
            Ok(texts
                .iter()
                .map(|text| {
                    let bytes = text.as_bytes();
                    vec![
                        bytes.len() as f32,
                        bytes.first().copied().unwrap_or(0) as f32,
                        bytes.last().copied().unwrap_or(0) as f32,
                    ]
                })
                .collect())
        }
    }

    fn scope() -> RepoScope {
        RepoScope::new("acme", "widget")
    }

    #[tokio::test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn returns_empty_when_no_embedding_tier_configured() {
        let dir = tempdir().unwrap();
        let router = Router::new();
        let result = build_review_context(dir.path(), &router, "diff", None, &scope(), 5)
            .await
            .expect("ok");
        assert!(result.is_empty());
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn returns_empty_for_workspace_with_no_extractable_symbols() {
        let dir = tempdir().unwrap();
        // Only files we don't have grammars for.
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        fs::write(dir.path().join("README"), "hello").unwrap();

        let embedder = alloc::sync::Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let result = build_review_context(dir.path(), &router, "diff", None, &scope(), 5)
            .await
            .expect("ok");
        assert!(result.is_empty());
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::let_underscore_untyped,
        reason = "the test discards this result on purpose: it checks side effects recorded by the fakes, or does best-effort cleanup"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn diff_is_embedded_only_once_even_with_learnings_store() {
        // Regression: previously the diff was embedded twice — once
        // for symbol-similarity, once for learnings-similarity. This
        // test pins the dedup so a future refactor doesn't silently
        // re-introduce the double-call.
        use ar_index::InMemoryLearningsStore;

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn foo() {}\n").unwrap();

        let embedder = alloc::sync::Arc::new(DeterministicEmbedder::new());
        let calls_handle = Arc::clone(&embedder);
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let store = InMemoryLearningsStore::new();

        let _ = build_review_context(dir.path(), &router, "diff text", Some(&store), &scope(), 3)
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
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn learnings_of_another_repository_are_never_retrieved() {
        use ar_index::{InMemoryLearningsStore, LearningSource, LearningsStore as _, RepoScope};

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn foo() {}\n").unwrap();
        let embedder = alloc::sync::Arc::new(DeterministicEmbedder::new());
        let diff = "diff text";
        let diff_vec = embedder.embed(&[diff.to_owned()]).await.unwrap().remove(0);
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let store = InMemoryLearningsStore::new();
        let repo_a = RepoScope::new("acme", "repo-a");
        store
            .add(
                "Repo A forbids unwrap.".into(),
                LearningSource::Chat,
                diff_vec,
                0,
                &repo_a,
                Some(1),
            )
            .await
            .unwrap();

        let for_b = build_review_context(
            dir.path(),
            &router,
            diff,
            Some(&store),
            &RepoScope::new("acme", "repo-b"),
            3,
        )
        .await
        .expect("ok");
        assert!(!for_b.contains("Repo A forbids unwrap."), "{for_b}");

        let for_a = build_review_context(dir.path(), &router, diff, Some(&store), &repo_a, 3)
            .await
            .expect("ok");
        assert!(for_a.contains("Repo A forbids unwrap."), "{for_a}");
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the test discards this result on purpose: it checks side effects recorded by the fakes, or does best-effort cleanup"
    )]
    #[expect(
        clippy::let_underscore_untyped,
        reason = "the test discards this result on purpose: it checks side effects recorded by the fakes, or does best-effort cleanup"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
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

        let embedder = alloc::sync::Arc::new(DeterministicEmbedder::new());
        let recorder = Arc::clone(&embedder);
        let router = Router::new().with(ModelTier::Embedding, embedder);

        // 256 KiB diff — well above the historical 32 KiB cap and
        // many multiples of the 6 KiB default.
        let big_diff = "x".repeat(256 * 1024);
        let _ = build_review_context(dir.path(), &router, &big_diff, None, &scope(), 3).await;

        let seen = recorder.seen.lock().unwrap();
        let diff_call = seen
            .iter()
            .find(|batch| batch.iter().any(|text| text.starts_with('x')))
            .expect("expected a diff embed call");
        let diff_input_bytes = diff_call[0].len();
        assert!(
            diff_input_bytes <= DEFAULT_EMBED_INPUT_CAP_BYTES,
            "diff embed input was {diff_input_bytes} bytes; expected <= {DEFAULT_EMBED_INPUT_CAP_BYTES} (EmbedConfig default)"
        );
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn second_review_with_unchanged_content_skips_re_embedding_symbols() {
        // The persistence win: embeddings of symbols whose source
        // hasn't changed between reviews are reused from the shared
        // store, so a re-review of an open PR doesn't burn embed
        // calls re-embedding the same code.
        use ar_index::SqliteVectorStore;

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn foo() {}\n").unwrap();
        let embedder = alloc::sync::Arc::new(DeterministicEmbedder::new());
        let calls_handle = Arc::clone(&embedder);
        let router = Router::new().with(ModelTier::Embedding, embedder);

        let store = SqliteVectorStore::in_memory().await.expect("open store");

        // First review: embeds the symbol (1 batch) + the diff.
        build_review_context_with_store(
            dir.path(),
            &router,
            "diff",
            None,
            &scope(),
            5,
            Some(&store),
        )
        .await
        .expect("first review");
        let after_first = calls_handle.call_count();

        // Second review with identical workspace + diff: the symbol
        // embedding should be reused; only the diff is re-embedded.
        build_review_context_with_store(
            dir.path(),
            &router,
            "diff",
            None,
            &scope(),
            5,
            Some(&store),
        )
        .await
        .expect("second review");
        let after_second = calls_handle.call_count();

        let extra = after_second - after_first;
        assert_eq!(
            extra, 1,
            "second review should embed only the diff (1 call), got {extra}",
        );
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn changed_content_invalidates_cached_embedding() {
        // Invalidation: when a symbol's source bytes change, the
        // cached entry must NOT be reused — otherwise stale
        // embeddings would shadow the freshly-edited code in RAG
        // retrieval.
        use ar_index::SqliteVectorStore;

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn foo() {}\n").unwrap();
        let embedder = alloc::sync::Arc::new(DeterministicEmbedder::new());
        let calls_handle = Arc::clone(&embedder);
        let router = Router::new().with(ModelTier::Embedding, embedder);

        let store = SqliteVectorStore::in_memory().await.expect("open store");

        // First review fills the cache.
        build_review_context_with_store(
            dir.path(),
            &router,
            "diff",
            None,
            &scope(),
            5,
            Some(&store),
        )
        .await
        .expect("first review");
        let after_first = calls_handle.call_count();

        // Edit foo's body so the snippet bytes change.
        fs::write(dir.path().join("a.rs"), "pub fn foo() { let x = 1; }\n").unwrap();

        build_review_context_with_store(
            dir.path(),
            &router,
            "diff",
            None,
            &scope(),
            5,
            Some(&store),
        )
        .await
        .expect("second review");
        let after_second = calls_handle.call_count();

        let extra = after_second - after_first;
        assert_eq!(
            extra, 2,
            "changed content must trigger one symbol embed plus the diff embed; got {extra}",
        );
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test spells out crate paths (std, tempfile, ar_prompts and similar) inline where it uses them, as written; importing each into the shared test module for one use would widen every test's namespace"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
    )]
    async fn produces_repo_context_markdown_when_workspace_has_symbols() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("a.rs"),
            "pub fn foo() {\n    let _ = 1;\n}\n",
        )
        .unwrap();

        let embedder = alloc::sync::Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let result = build_review_context(dir.path(), &router, "diff", None, &scope(), 5)
            .await
            .expect("ok");
        // Markdown should contain the formatted-context section header.
        assert!(result.contains("Similar code in this repo"));
        assert!(result.contains("**foo**"));
    }
}
