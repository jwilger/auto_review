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
use std::env;

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

/// Default maximum symbols per embed batch. See [`EmbedConfig`].
pub const DEFAULT_EMBED_BATCH_SIZE: usize = 32;

/// Default per-input byte cap. Sized for small local embedders
/// (e.g. `nomic-embed-text` at the Ollama default `num_ctx=2048`,
/// roughly 8 KiB English). 6 KiB leaves headroom for tokenisation
/// overhead and avoids silent server-side truncation. Operators
/// pointing at hosted OpenAI-class embedders (8191-token / ~32
/// KiB ceiling) raise this via `AR_EMBED_INPUT_CAP_BYTES` to keep
/// more snippet context per symbol.
pub const DEFAULT_EMBED_INPUT_CAP_BYTES: usize = 6 * 1024;

/// Tunable knobs for the embedding pass. Defaults are conservative
/// for small local Ollama embedders. Operators with hosted OpenAI
/// embedders should raise `input_cap_bytes` via env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbedConfig {
    /// Maximum bytes per single input snippet. Snippets longer
    /// than this are truncated at a char boundary before being
    /// sent to the embedder. Sized to fit comfortably inside the
    /// embedder's context window after tokenisation overhead.
    /// Override with `AR_EMBED_INPUT_CAP_BYTES`.
    pub input_cap_bytes: usize,
    /// Maximum number of inputs per `router.embed(...)` call.
    /// Symbol counts above this are split across multiple
    /// sequential calls. Override with `AR_EMBED_BATCH_SIZE`.
    pub batch_size: usize,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            input_cap_bytes: DEFAULT_EMBED_INPUT_CAP_BYTES,
            batch_size: DEFAULT_EMBED_BATCH_SIZE,
        }
    }
}

impl EmbedConfig {
    /// Read overrides from the process environment. Unset / empty /
    /// unparseable / zero values fall through to [`EmbedConfig::default`]
    /// with a `warn` log so a typo doesn't silently keep the previous
    /// (wrong) cap.
    pub fn from_env() -> Self {
        Self::from_env_lookup(|k| env::var(k).ok())
    }

    /// Same as [`EmbedConfig::from_env`] but with an injected lookup
    /// function for testability — process env is global and tests
    /// would otherwise race.
    pub fn from_env_lookup<F>(lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let default = Self::default();
        let input_cap_bytes = parse_positive_usize_env(
            "AR_EMBED_INPUT_CAP_BYTES",
            lookup("AR_EMBED_INPUT_CAP_BYTES"),
        )
        .unwrap_or(default.input_cap_bytes);
        let batch_size =
            parse_positive_usize_env("AR_EMBED_BATCH_SIZE", lookup("AR_EMBED_BATCH_SIZE"))
                .unwrap_or(default.batch_size);
        Self {
            input_cap_bytes,
            batch_size,
        }
    }
}

fn parse_positive_usize_env(name: &str, raw: Option<String>) -> Option<usize> {
    let value = raw?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        tracing::warn!(
            env = name,
            "env var set to an empty/whitespace value; using default"
        );
        return None;
    }
    match trimmed.parse::<usize>() {
        Ok(0) => {
            tracing::warn!(
                env = name,
                "env var set to 0; using default (0 disables embedding)"
            );
            None
        }
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                env = name,
                value = %trimmed,
                error = %e,
                "env var set to an unparseable value; using default"
            );
            None
        }
    }
}

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

/// Embed each symbol via the router's Embedding tier with the
/// process-default [`EmbedConfig`] (env-var driven). Convenience
/// wrapper for callers that don't already hold a config.
pub async fn embed_symbols(
    router: &Router,
    symbols: &[IndexedSymbol],
    file_contents: &HashMap<String, String>,
) -> Result<Vec<EmbeddedSymbol>, EmbedError> {
    embed_symbols_with_config(router, symbols, file_contents, &EmbedConfig::from_env()).await
}

/// Embed each symbol via the router's Embedding tier. `file_contents`
/// must contain an entry for every distinct path referenced by
/// `symbols`; missing paths are reported as `MissingFile`.
///
/// Symbols are batched in groups of `config.batch_size` so a large
/// repo doesn't exceed the provider's per-request size limit. Tier
/// mis-configuration (no Embedding provider) bubbles up as an
/// `ar_llm::Error::NoProvider` via the `?`.
pub async fn embed_symbols_with_config(
    router: &Router,
    symbols: &[IndexedSymbol],
    file_contents: &HashMap<String, String>,
    config: &EmbedConfig,
) -> Result<Vec<EmbeddedSymbol>, EmbedError> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    // Slice each symbol's source range, then cap each snippet at
    // config.input_cap_bytes so a single huge symbol can't fail the
    // whole batch (and so a small local embedder doesn't silently
    // truncate inputs that exceed its context window). Track which
    // snippets had to be truncated so the per-batch debug log can
    // attribute the truncation count to the right batch.
    let mut snippets: Vec<String> = Vec::with_capacity(symbols.len());
    let mut truncated_flags: Vec<bool> = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let content = file_contents
            .get(&sym.path)
            .ok_or_else(|| EmbedError::MissingFile(sym.path.clone()))?;
        let snippet = snippet_for_symbol(content, sym)?;
        let original_len = snippet.len();
        let capped = truncate_at_char_boundary(snippet, config.input_cap_bytes);
        truncated_flags.push(capped.len() < original_len);
        snippets.push(capped);
    }

    // Batch the embed calls so a 5k-symbol repo doesn't try to
    // POST 5k inputs in one request.
    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(snippets.len());
    let snippet_chunks = snippets.chunks(config.batch_size);
    let flag_chunks = truncated_flags.chunks(config.batch_size);
    for (chunk, flags) in snippet_chunks.zip(flag_chunks) {
        let max_bytes = chunk.iter().map(|s| s.len()).max().unwrap_or(0);
        let truncated_in_batch = flags.iter().filter(|t| **t).count();
        tracing::debug!(
            cap_bytes = config.input_cap_bytes,
            batch_size = chunk.len(),
            max_input_bytes = max_bytes,
            truncated_in_batch,
            "embedding batch"
        );
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
        // config.batch_size.
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());

        // 80 trivial symbols spread across two files.
        let mut files = HashMap::new();
        let lines: String = (0..80).map(|i| format!("fn s{i}() {{}}\n")).collect();
        files.insert("src/a.rs".into(), lines);
        let symbols: Vec<IndexedSymbol> = (0..80)
            .map(|i| isym("src/a.rs", &format!("s{i}"), i + 1, i + 1))
            .collect();

        let out = embed_symbols_with_config(&router, &symbols, &files, &EmbedConfig::default())
            .await
            .expect("ok");
        assert_eq!(out.len(), 80);

        let calls = embedder.seen.lock().unwrap();
        // 80 / 32 = 3 batches (32, 32, 16).
        assert_eq!(calls.len(), 3, "expected 3 batches, got {}", calls.len());
        assert_eq!(calls[0].len(), DEFAULT_EMBED_BATCH_SIZE);
        assert_eq!(calls[1].len(), DEFAULT_EMBED_BATCH_SIZE);
        assert_eq!(calls[2].len(), 80 - 2 * DEFAULT_EMBED_BATCH_SIZE);
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
        // Regression: a single symbol with body larger than the
        // embedder's window used to fail the whole RAG pass — either
        // with HTTP 400 from hosted OpenAI, or with silent server-side
        // truncation on Ollama. Truncate to config.input_cap_bytes so
        // retrieval still works on the prefix.
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());

        // Explicit cap (not EmbedConfig::default()) so the test
        // states its assumption directly and stays meaningful if
        // the default ever changes.
        const CAP: usize = 1024;
        let cfg = EmbedConfig {
            input_cap_bytes: CAP,
            batch_size: 32,
        };
        let huge_line = "x".repeat(CAP * 2);
        let mut files = HashMap::new();
        files.insert(
            "src/big.rs".into(),
            format!("fn huge() {{\n{huge_line}\n}}\n"),
        );
        let symbols = vec![isym("src/big.rs", "huge", 1, 3)];

        let _ = embed_symbols_with_config(&router, &symbols, &files, &cfg)
            .await
            .expect("ok");

        let calls = embedder.seen.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 1);
        assert!(
            calls[0][0].len() <= CAP,
            "snippet not truncated: {} bytes vs {CAP} cap",
            calls[0][0].len()
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

    #[test]
    fn embed_config_default_is_safe_for_small_local_embedders() {
        // Defaults should fit comfortably under nomic-embed-text's
        // num_ctx=2048 (~8 KiB English) so the local-Ollama default
        // setup doesn't silently truncate inputs server-side.
        let cfg = EmbedConfig::default();
        assert_eq!(cfg.input_cap_bytes, 6 * 1024);
        assert_eq!(cfg.batch_size, 32);
    }

    #[test]
    fn embed_config_from_env_reads_overrides() {
        let lookup = |k: &str| match k {
            "AR_EMBED_INPUT_CAP_BYTES" => Some("16384".to_string()),
            "AR_EMBED_BATCH_SIZE" => Some("8".to_string()),
            _ => None,
        };
        let cfg = EmbedConfig::from_env_lookup(lookup);
        assert_eq!(cfg.input_cap_bytes, 16384);
        assert_eq!(cfg.batch_size, 8);
    }

    #[test]
    fn embed_config_from_env_falls_back_when_unset() {
        let cfg = EmbedConfig::from_env_lookup(|_| None);
        assert_eq!(cfg, EmbedConfig::default());
    }

    #[test]
    fn embed_config_from_env_falls_back_on_unparseable() {
        // A typo like AR_EMBED_BATCH_SIZE=eight should not silently
        // disable batching. Fall through to the default.
        let lookup = |k: &str| match k {
            "AR_EMBED_BATCH_SIZE" => Some("eight".to_string()),
            _ => None,
        };
        let cfg = EmbedConfig::from_env_lookup(lookup);
        assert_eq!(cfg.batch_size, EmbedConfig::default().batch_size);
    }

    #[test]
    fn embed_config_from_env_falls_back_on_zero() {
        // Zero would mean "don't truncate" (input_cap) or panic-on-
        // chunks(0) for batch_size; treat as misconfiguration.
        let lookup = |k: &str| match k {
            "AR_EMBED_INPUT_CAP_BYTES" => Some("0".to_string()),
            "AR_EMBED_BATCH_SIZE" => Some("0".to_string()),
            _ => None,
        };
        let cfg = EmbedConfig::from_env_lookup(lookup);
        assert_eq!(cfg, EmbedConfig::default());
    }

    #[test]
    fn embed_config_from_env_falls_back_on_empty_value() {
        let lookup = |k: &str| match k {
            "AR_EMBED_INPUT_CAP_BYTES" => Some("   ".to_string()),
            _ => None,
        };
        let cfg = EmbedConfig::from_env_lookup(lookup);
        assert_eq!(cfg.input_cap_bytes, EmbedConfig::default().input_cap_bytes);
    }

    #[tokio::test]
    async fn custom_input_cap_is_honoured() {
        // Operator points at a tighter local embedder by setting
        // AR_EMBED_INPUT_CAP_BYTES=512: every snippet must be capped
        // at 512 bytes regardless of the default.
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());

        let cfg = EmbedConfig {
            input_cap_bytes: 512,
            batch_size: 32,
        };
        let huge = "x".repeat(4096);
        let mut files = HashMap::new();
        files.insert("src/x.rs".into(), format!("fn x() {{\n{huge}\n}}\n"));
        let symbols = vec![isym("src/x.rs", "x", 1, 3)];

        let _ = embed_symbols_with_config(&router, &symbols, &files, &cfg)
            .await
            .expect("ok");

        let calls = embedder.seen.lock().unwrap();
        assert!(
            calls[0][0].len() <= 512,
            "expected cap=512, got {} bytes",
            calls[0][0].len()
        );
    }

    #[tokio::test]
    async fn custom_batch_size_is_honoured() {
        // batch_size=10 with 25 symbols produces 3 batches: 10, 10, 5.
        let embedder = Arc::new(DeterministicEmbedder::new());
        let router = Router::new().with(ModelTier::Embedding, embedder.clone());

        let cfg = EmbedConfig {
            input_cap_bytes: 6 * 1024,
            batch_size: 10,
        };
        let lines: String = (0..25).map(|i| format!("fn s{i}() {{}}\n")).collect();
        let mut files = HashMap::new();
        files.insert("src/a.rs".into(), lines);
        let symbols: Vec<IndexedSymbol> = (0..25)
            .map(|i| isym("src/a.rs", &format!("s{i}"), i + 1, i + 1))
            .collect();

        let _ = embed_symbols_with_config(&router, &symbols, &files, &cfg)
            .await
            .expect("ok");

        let calls = embedder.seen.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].len(), 10);
        assert_eq!(calls[1].len(), 10);
        assert_eq!(calls[2].len(), 5);
    }
}
