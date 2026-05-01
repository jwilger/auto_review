//! Symbol-level embedding pass.
//!
//! Takes a list of [`IndexedSymbol`]s plus the file contents they came
//! from, slices each symbol's source range, embeds via the
//! Embedding-tier LLM, and emits [`EmbeddedSymbol`] records ready for
//! a vector store. The vector store itself (LanceDB or pgvector) is
//! still pending — this commit just produces the records.

use crate::walker::IndexedSymbol;
use ar_llm::{ModelTier, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddedSymbol {
    #[serde(flatten)]
    pub indexed: IndexedSymbol,
    /// The text actually embedded — useful for downstream retrieval
    /// to show snippets alongside the match.
    pub content: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("LLM error: {0}")]
    Llm(#[from] ar_llm::Error),
    #[error("symbol referenced unknown file: {0}")]
    MissingFile(String),
    #[error("symbol's line range exceeds file length: {path} {line_end}")]
    OutOfRange { path: String, line_end: u32 },
}

/// Maximum symbols sent in a single `router.embed(...)` call.
/// Conservative cap that fits comfortably under hosted providers'
/// batch limits (OpenAI's `text-embedding-3-*` accepts up to 2048
/// inputs but rejects payloads above ~300k tokens; 32 small
/// snippets is well below either bound) and well-known local
/// embedders. Symbol counts above this are split across multiple
/// sequential calls — bounded slowdown on large repos in exchange
/// for not failing the whole RAG pass.
pub const EMBED_BATCH_SIZE: usize = 32;

/// Per-input byte cap. OpenAI's `text-embedding-3-*` rejects any
/// single input exceeding 8191 tokens (~32 KiB English) with HTTP
/// 400, which would fail the whole batch. A single oversized
/// symbol — generated code, a giant Lua string literal, an
/// ML-pipeline notebook converted to one fn — used to take the
/// entire RAG pass down with it. Truncate at the char boundary
/// instead; semantic similarity from the prefix is good enough
/// for retrieval ranking.
pub const EMBED_INPUT_CAP_BYTES: usize = 24 * 1024;

fn truncate_at_char_boundary(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = s;
    out.truncate(cut);
    out
}

/// Embed each symbol via the router's Embedding tier. `file_contents`
/// must contain an entry for every distinct path referenced by
/// `symbols`; missing paths are reported as `MissingFile`.
///
/// Symbols are batched in groups of [`EMBED_BATCH_SIZE`] so a large
/// repo doesn't exceed the provider's per-request size limit. Tier
/// mis-configuration (no Embedding provider) bubbles up as an
/// `ar_llm::Error::NoProvider` via the `?`.
pub async fn embed_symbols(
    router: &Router,
    symbols: &[IndexedSymbol],
    file_contents: &HashMap<String, String>,
) -> Result<Vec<EmbeddedSymbol>, EmbedError> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    // Slice each symbol's source range, then cap each snippet at
    // EMBED_INPUT_CAP_BYTES so a single huge symbol can't fail the
    // whole batch.
    let mut snippets: Vec<String> = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let content = file_contents
            .get(&sym.path)
            .ok_or_else(|| EmbedError::MissingFile(sym.path.clone()))?;
        let snippet = snippet_for_symbol(content, sym)?;
        snippets.push(truncate_at_char_boundary(snippet, EMBED_INPUT_CAP_BYTES));
    }

    // Batch the embed calls so a 5k-symbol repo doesn't try to
    // POST 5k inputs in one request.
    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(snippets.len());
    for chunk in snippets.chunks(EMBED_BATCH_SIZE) {
        let chunk_vec = router.embed(ModelTier::Embedding, chunk).await?;
        vectors.extend(chunk_vec);
    }

    let out = symbols
        .iter()
        .zip(snippets)
        .zip(vectors)
        .map(|((sym, content), embedding)| EmbeddedSymbol {
            indexed: sym.clone(),
            content,
            embedding,
        })
        .collect();
    Ok(out)
}

/// Extract the source slice covered by `symbol` from `file_content`.
/// Lines are 1-based and inclusive on both ends.
fn snippet_for_symbol(file_content: &str, symbol: &IndexedSymbol) -> Result<String, EmbedError> {
    let lines: Vec<&str> = file_content.lines().collect();
    let start = symbol.symbol.line_start.saturating_sub(1) as usize;
    let end = symbol.symbol.line_end as usize;
    if end > lines.len() {
        return Err(EmbedError::OutOfRange {
            path: symbol.path.clone(),
            line_end: symbol.symbol.line_end,
        });
    }
    let slice = lines[start..end].join("\n");
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::{Symbol, SymbolKind};
    use ar_llm::{CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Minimal fake provider that returns a deterministic vector per text:
    /// `[len_as_f32, first_byte_as_f32, last_byte_as_f32]`. Records what
    /// it was asked to embed.
    struct DeterministicEmbedder {
        seen: Mutex<Vec<Vec<String>>>,
    }

    impl DeterministicEmbedder {
        fn new() -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for DeterministicEmbedder {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            unimplemented!("not used")
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

    fn isym(path: &str, name: &str, line_start: u32, line_end: u32) -> IndexedSymbol {
        IndexedSymbol {
            path: path.into(),
            symbol: Symbol {
                kind: SymbolKind::Function,
                name: name.into(),
                line_start,
                line_end,
            },
        }
    }

    #[tokio::test]
    async fn empty_input_returns_empty_output_without_calling_embedder() {
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());
        let out = embed_symbols(&router, &[], &HashMap::new())
            .await
            .expect("ok");
        assert!(out.is_empty());
        assert!(embedder.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn embeds_each_symbol_in_a_single_batch() {
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());

        let mut files = HashMap::new();
        files.insert(
            "src/a.rs".into(),
            "pub fn foo() {\n    1 + 1\n}\npub fn bar() {}\n".into(),
        );

        let symbols = vec![isym("src/a.rs", "foo", 1, 3), isym("src/a.rs", "bar", 4, 4)];

        let out = embed_symbols(&router, &symbols, &files).await.expect("ok");
        assert_eq!(out.len(), 2);
        // Snippet for `foo` should span the first three lines.
        assert!(out[0].content.contains("pub fn foo"));
        assert!(out[0].content.contains("1 + 1"));
        // Snippet for `bar` is its single line.
        assert_eq!(out[1].content, "pub fn bar() {}");
        // Both embedded.
        assert_eq!(out[0].embedding.len(), 3);
        assert_eq!(out[1].embedding.len(), 3);
        // Single batch: embedder saw one call with both texts.
        let calls = embedder.seen.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 2);
    }

    #[tokio::test]
    async fn batches_when_symbol_count_exceeds_batch_size() {
        // Defence: a 5k-symbol repo shouldn't try to POST all 5k
        // inputs in one embed call. Batching caps each request at
        // EMBED_BATCH_SIZE.
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());

        // 80 trivial symbols spread across two files.
        let mut files = HashMap::new();
        let lines: String = (0..80).map(|i| format!("fn s{i}() {{}}\n")).collect();
        files.insert("src/a.rs".into(), lines);
        let symbols: Vec<IndexedSymbol> = (0..80)
            .map(|i| isym("src/a.rs", &format!("s{i}"), i + 1, i + 1))
            .collect();

        let out = embed_symbols(&router, &symbols, &files).await.expect("ok");
        assert_eq!(out.len(), 80);

        let calls = embedder.seen.lock().unwrap();
        // 80 / 32 = 3 batches (32, 32, 16).
        assert_eq!(calls.len(), 3, "expected 3 batches, got {}", calls.len());
        assert_eq!(calls[0].len(), EMBED_BATCH_SIZE);
        assert_eq!(calls[1].len(), EMBED_BATCH_SIZE);
        assert_eq!(calls[2].len(), 80 - 2 * EMBED_BATCH_SIZE);
        // Total inputs across batches must equal symbol count — no
        // duplicates, no drops.
        let total: usize = calls.iter().map(|c| c.len()).sum();
        assert_eq!(total, symbols.len());
    }

    #[tokio::test]
    async fn missing_file_returns_specific_error() {
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let symbols = vec![isym("does/not/exist.rs", "x", 1, 1)];
        let err = embed_symbols(&router, &symbols, &HashMap::new())
            .await
            .expect_err("err");
        assert!(matches!(err, EmbedError::MissingFile(p) if p == "does/not/exist.rs"));
    }

    #[tokio::test]
    async fn oversized_symbol_snippet_is_truncated_before_embed_call() {
        // Regression: a single symbol with body >8k tokens used to fail
        // the whole RAG pass with HTTP 400 from OpenAI. Truncate to
        // EMBED_INPUT_CAP_BYTES so retrieval still works on the prefix.
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());

        let huge_line = "x".repeat(EMBED_INPUT_CAP_BYTES * 2);
        let mut files = HashMap::new();
        files.insert(
            "src/big.rs".into(),
            format!("fn huge() {{\n{huge_line}\n}}\n"),
        );
        let symbols = vec![isym("src/big.rs", "huge", 1, 3)];

        let _ = embed_symbols(&router, &symbols, &files).await.expect("ok");

        let calls = embedder.seen.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 1);
        assert!(
            calls[0][0].len() <= EMBED_INPUT_CAP_BYTES,
            "snippet not truncated: {} bytes vs {} cap",
            calls[0][0].len(),
            EMBED_INPUT_CAP_BYTES
        );
    }

    #[tokio::test]
    async fn out_of_range_line_returns_specific_error() {
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder);
        let mut files = HashMap::new();
        files.insert("a.rs".into(), "fn x() {}\n".into());
        let symbols = vec![isym("a.rs", "x", 1, 99)];
        let err = embed_symbols(&router, &symbols, &files)
            .await
            .expect_err("err");
        assert!(matches!(err, EmbedError::OutOfRange { line_end: 99, .. }));
    }
}
