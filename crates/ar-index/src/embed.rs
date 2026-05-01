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

/// Embed each symbol via the router's Embedding tier. `file_contents`
/// must contain an entry for every distinct path referenced by
/// `symbols`; missing paths are reported as `MissingFile`.
///
/// Symbols are batched into a single `embed()` call per provider for
/// efficiency. Tier mis-configuration (no Embedding provider) bubbles
/// up as an `ar_llm::Error::NoProvider` via the `?`.
pub async fn embed_symbols(
    router: &Router,
    symbols: &[IndexedSymbol],
    file_contents: &HashMap<String, String>,
) -> Result<Vec<EmbeddedSymbol>, EmbedError> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    // Slice each symbol's source range.
    let mut snippets: Vec<String> = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let content = file_contents
            .get(&sym.path)
            .ok_or_else(|| EmbedError::MissingFile(sym.path.clone()))?;
        snippets.push(snippet_for_symbol(content, sym)?);
    }

    let vectors = router.embed(ModelTier::Embedding, &snippets).await?;

    let out = symbols
        .iter()
        .zip(snippets.into_iter())
        .zip(vectors.into_iter())
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
