//! Format retrieved context (similar code and learnings) as
//! markdown for the LLM prompt's `repo_context` slot.
//!
//! Pure function — the caller does the actual retrieval (querying
//! ar-index's stores) and hands the results in. Keeps this layer
//! easily unit-testable without dragging the embedder + vector
//! store into every test.

use ar_index::{ScoredLearning, ScoredSymbol};

/// Render a `repo_context` markdown block from retrieved snippets.
/// Sections are emitted only when their input is non-empty, so the
/// resulting string is safe to drop into the prompt — no empty
/// headers when there's nothing to show.
#[inline]
#[must_use = "formatting has no side effects; the rendered block is the only result"]
pub fn format_repo_context(
    similar_symbols: &[ScoredSymbol],
    relevant_learnings: &[ScoredLearning],
) -> String {
    let mut out = String::new();

    if !similar_symbols.is_empty() {
        out.push_str("### Similar code in this repo\n");
        for scored in similar_symbols {
            #[expect(
                clippy::format_push_string,
                reason = "one short header per retrieved snippet; write! into a String returns an \
                          infallible fmt::Result that would have to be discarded"
            )]
            out.push_str(&format!(
                "- **{}** ({:?}) at `{}`:{}-{}\n",
                scored.symbol.indexed.symbol.name,
                scored.symbol.indexed.symbol.kind,
                scored.symbol.indexed.path,
                scored.symbol.indexed.symbol.line_start,
                scored.symbol.indexed.symbol.line_end,
            ));
            // Indent the snippet by 4 spaces so it renders as a code
            // block continuation under the bullet.
            for line in scored.symbol.content.lines() {
                out.push_str("    ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push('\n');
    }

    if !relevant_learnings.is_empty() {
        out.push_str("### Project conventions and past feedback\n");
        for scored in relevant_learnings {
            #[expect(
                clippy::format_push_string,
                reason = "one short bullet per retrieved learning; write! into a String returns an \
                          infallible fmt::Result that would have to be discarded"
            )]
            out.push_str(&format!(
                "- ({:?}) {}\n",
                scored.learning.source, scored.learning.text
            ));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_index::{
        EmbeddedSymbol, IndexedSymbol, LearningRecord, LearningSource, Symbol, SymbolKind,
    };

    fn sym(path: &str, name: &str, content: &str) -> ScoredSymbol {
        ScoredSymbol {
            symbol: EmbeddedSymbol {
                indexed: IndexedSymbol {
                    path: path.into(),
                    symbol: Symbol {
                        kind: SymbolKind::Function,
                        name: name.into(),
                        line_start: 1,
                        line_end: 3,
                    },
                },
                content: content.into(),
                embedding: vec![],
            },
            score: 0.9,
        }
    }

    fn learning(text: &str, source: LearningSource) -> ScoredLearning {
        ScoredLearning {
            learning: LearningRecord {
                id: 1,
                text: text.into(),
                source,
                embedding: vec![],
                created_at: 0,
                repo: None,
                origin_pr: None,
            },
            score: 0.9,
        }
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    fn empty_inputs_produce_empty_output() {
        let out = format_repo_context(&[], &[]);
        assert!(out.is_empty());
    }

    #[test]
    fn similar_symbols_render_with_path_lines_and_snippet() {
        let symbols = vec![sym(
            "src/lib.rs",
            "process",
            "fn process() {\n    todo!()\n}",
        )];
        let out = format_repo_context(&symbols, &[]);
        assert!(out.contains("Similar code in this repo"));
        assert!(out.contains("**process**"));
        assert!(out.contains("`src/lib.rs`:1-3"));
        assert!(out.contains("    fn process() {"));
        assert!(out.contains("    }"));
    }

    #[test]
    fn learnings_render_under_their_section_with_source_label() {
        let learnings = vec![
            learning("Forbid unwrap() outside tests.", LearningSource::Guideline),
            learning("We use Result::Err for I/O failures.", LearningSource::Chat),
        ];
        let out = format_repo_context(&[], &learnings);
        assert!(out.contains("Project conventions and past feedback"));
        assert!(out.contains("(Guideline) Forbid unwrap"));
        assert!(out.contains("(Chat) We use Result"));
    }

    #[test]
    fn omits_section_headers_for_empty_inputs() {
        let symbols = vec![sym("a.rs", "x", "fn x() {}")];
        let out = format_repo_context(&symbols, &[]);
        assert!(out.contains("Similar code"));
        assert!(!out.contains("Project conventions"));
    }
}
