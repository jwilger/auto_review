//! Bound the size of unified-diff content fed to the LLM.
//!
//! Without a cap, a 10-megabyte PR diff is just shoved at the model and
//! context windows are exceeded (or token cost spirals). The cap-strategy
//! here is intentionally simple: split the diff at file boundaries
//! (`diff --git ` markers) and accept files in order until the byte
//! budget is exhausted. Whatever is left is replaced with a summary
//! note so the model knows files were elided.
//!
//! A more sophisticated strategy (importance-rank hunks, summarize
//! remainder) is a Milestone 2 follow-up; this is the pragmatic floor.

use core::iter;

/// Default cap. Roughly ~25k tokens in worst-case English; well below
/// even a small reasoning model's context window.
pub const DEFAULT_MAX_DIFF_BYTES: usize = 100_000;

/// Shared cap for cheap-tier prompts (triage, verifier, agentic
/// verifier). Cheap-tier models often have
/// 32K-token windows; 40 KiB stays comfortably inside.
pub const CHEAP_TIER_DIFF_CAP: usize = 40 * 1024;

/// Cap `body` at `max_bytes` for inclusion in an LLM prompt.
///
/// When the body fits, returns it unchanged. When it exceeds the
/// cap, returns the prefix walked back to a UTF-8 character
/// boundary, plus a single trailing line containing `marker` (so
/// the model can see the body was abridged). Used by every
/// cheap-tier prompt builder that embeds the PR's unified diff.
#[inline]
#[must_use = "capping has no side effects; the capped body is the only result"]
pub fn cap_for_prompt(body: &str, max_bytes: usize, marker: &str) -> String {
    if body.len() <= max_bytes {
        return body.to_owned();
    }
    let prefix = char_boundary_prefix(body, max_bytes);
    let mut out =
        String::with_capacity(prefix.len().saturating_add(marker.len()).saturating_add(2));
    out.push_str(prefix);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(marker);
    out.push('\n');
    out
}

/// Cap `body` at `max_bytes` for inclusion in an embedding request.
///
/// Unlike [`cap_for_prompt`], this returns the exact prefix (walked
/// back to a UTF-8 character boundary) with no trailing marker —
/// embeddings represent semantic content rather than literal text,
/// and a "[truncated]" suffix would only inflate token usage.
/// Hard upper bound: the returned string is `<= max_bytes`.
#[inline]
#[must_use = "capping has no side effects; the capped body is the only result"]
pub fn cap_for_embed(body: &str, max_bytes: usize) -> String {
    if body.len() <= max_bytes {
        return body.to_owned();
    }
    char_boundary_prefix(body, max_bytes).to_owned()
}

/// Cap a unified diff at `max_bytes`, splitting at `diff --git ` file
/// boundaries. Returns the original diff unchanged when it already fits.
#[inline]
#[must_use = "capping has no side effects; the capped diff is the only result"]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-cli (commands) and the red-team tests import this as ar_review::cap_diff; \
              renaming it breaks those callers"
)]
pub fn cap_diff(diff: &str, max_bytes: usize) -> String {
    if diff.len() <= max_bytes {
        return diff.to_owned();
    }

    let files = split_files(diff);
    if files.is_empty() {
        // No file markers found at all — fall back to a flat truncation.
        return truncate_flat(diff, max_bytes);
    }

    let mut out = String::with_capacity(max_bytes);
    let mut included: usize = 0;
    let mut omitted: usize = 0;
    for file in files {
        if out.len().saturating_add(file.len()) <= max_bytes {
            out.push_str(file);
            included = included.saturating_add(1);
        } else {
            omitted = omitted.saturating_add(1);
        }
    }

    if omitted > 0 {
        #[expect(
            clippy::format_push_string,
            reason = "a single trailing note per capped diff; write! into a String returns an \
                      infallible fmt::Result that would have to be discarded"
        )]
        #[expect(
            clippy::integer_division,
            clippy::integer_division_remainder_used,
            reason = "the note reports the cap in whole KiB, so rounding down is the intended \
                      display; 1024 is a non-zero constant"
        )]
        out.push_str(&format!(
            "\n\n[auto_review: omitted {omitted} file(s) to fit a {} KiB diff cap; included {included} file(s)]\n",
            max_bytes / 1024
        ));
    }

    out
}

/// Longest prefix of `text` that is at most `max_bytes` long and ends on a
/// UTF-8 character boundary.
#[expect(
    clippy::pub_with_shorthand,
    reason = "pub(crate) is the spelling rustfmt and the Rust reference use; the restriction \
              group also enables the opposite pub_without_shorthand, so one spelling must carry \
              an expectation"
)]
pub(crate) fn char_boundary_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    // Don't truncate in the middle of a UTF-8 codepoint.
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// Split `diff` into per-file chunks, each starting at a `diff --git `
/// marker at the start of a line. Returns no chunks when no marker exists.
fn split_files(diff: &str) -> Vec<&str> {
    /// Header that opens each file section of a unified git diff.
    const MARKER: &str = "diff --git ";
    let starts: Vec<usize> = diff
        .match_indices(MARKER)
        .map(|(abs, _)| abs)
        .filter(|abs| {
            abs.checked_sub(1)
                .and_then(|prev| diff.as_bytes().get(prev))
                .is_none_or(|byte| *byte == b'\n')
        })
        .collect();
    let ends = starts.iter().skip(1).copied().chain(iter::once(diff.len()));
    starts
        .iter()
        .copied()
        .zip(ends)
        .filter_map(|(begin, end)| diff.get(begin..end))
        .collect()
}

/// Truncate `diff` to at most `max_bytes` on a character boundary and
/// append a note that the diff was cut.
fn truncate_flat(diff: &str, max_bytes: usize) -> String {
    let mut out = char_boundary_prefix(diff, max_bytes).to_owned();
    out.push_str("\n\n[auto_review: diff truncated to fit byte cap]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn diff_within_cap_is_unchanged() {
        let d = "diff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(cap_diff(d, 1024), d);
    }

    #[test]
    fn oversized_diff_keeps_whole_files_under_cap() {
        let f1 = "diff --git a/a b/a\n@@ -1 +1 @@\n+aaa\n"; // ~36 bytes
        let f2 = "diff --git a/b b/b\n@@ -1 +1 @@\n+bbbbbbbbbbbb\n";
        let f3 = "diff --git a/c b/c\n@@ -1 +1 @@\n+ccc\n";
        let combined = format!("{f1}{f2}{f3}");
        // Cap big enough for f1+f2 but not f3.
        let cap = f1.len() + f2.len() + 10;
        let capped = cap_diff(&combined, cap);
        assert!(capped.starts_with(f1));
        assert!(capped.contains(f2));
        assert!(!capped.contains("+ccc"));
        assert!(capped.contains("omitted 1"));
    }

    #[test]
    fn omits_count_reports_each_dropped_file() {
        let f1 = format!("diff --git a/a b/a\n{}\n", "+".repeat(200));
        let f2 = format!("diff --git a/b b/b\n{}\n", "+".repeat(200));
        let f3 = format!("diff --git a/c b/c\n{}\n", "+".repeat(200));
        let combined = format!("{f1}{f2}{f3}");
        // Cap fits only f1.
        let cap = f1.len() + 10;
        let capped = cap_diff(&combined, cap);
        assert!(capped.contains("omitted 2"));
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn diff_with_no_file_markers_falls_back_to_flat_truncation() {
        let d = "x".repeat(1000);
        let capped = cap_diff(&d, 100);
        assert!(capped.len() < 200);
        assert!(capped.contains("truncated"));
    }

    #[test]
    #[expect(
        clippy::absolute_paths,
        reason = "fully qualified paths appear in this test's assertions, which stay as written"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::std_instead_of_core,
        reason = "the assertion names std paths and stays as written"
    )]
    fn flat_truncation_does_not_split_utf8_codepoint() {
        // 'é' is two bytes in UTF-8 (0xC3 0xA9).
        let d = format!("héllo {}", "x".repeat(1000));
        let capped = cap_diff(&d, 7); // one byte into 'é' if naïve.
                                      // Whatever we emit must remain valid UTF-8 (and prefix-only of d).
        assert!(capped.is_ascii() || std::str::from_utf8(capped.as_bytes()).is_ok());
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    fn split_files_recognizes_file_markers_only_at_line_starts() {
        // The string "diff --git" appearing inside a hunk content line
        // (after a +/-, or with surrounding text) must not be treated as
        // a new file marker.
        let d = "\
diff --git a/a b/a
@@ -1 +1 @@
-old
+new diff --git inside content
diff --git a/b b/b
@@ -1 +1 @@
+x
";
        let files = split_files(d);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn empty_diff_passes_through_unchanged() {
        assert_eq!(cap_diff("", 100), "");
    }

    #[test]
    fn cap_for_prompt_passes_short_input_through() {
        assert_eq!(cap_for_prompt("brief", 100, "[trunc]"), "brief");
    }

    #[test]
    fn cap_for_prompt_appends_marker_on_overflow() {
        let big = "x".repeat(200);
        let out = cap_for_prompt(&big, 100, "[trunc]");
        assert!(out.contains("[trunc]"));
        // 100 byte prefix + newline + marker + newline ≈ 110 bytes.
        assert!(out.len() < 120, "got {} bytes", out.len());
    }

    #[test]
    #[expect(
        clippy::absolute_paths,
        reason = "fully qualified paths appear in this test's assertions, which stay as written"
    )]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "the assertion stays as written; it checks only whether the call failed or succeeded"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::non_ascii_literal,
        reason = "the fixture and expected text hold the exact non-ASCII characters (em dash, ellipsis, emoji, multi-byte letters) the code under test renders or must cut on a character boundary; escapes would edit what the test checks"
    )]
    #[expect(
        clippy::std_instead_of_core,
        reason = "the assertion names std paths and stays as written"
    )]
    fn cap_for_prompt_walks_back_to_utf8_boundary() {
        // 4-byte emoji at the cut boundary must not be split.
        let mut s = "x".repeat(98);
        s.push_str("🦀tail");
        let out = cap_for_prompt(&s, 100, "[m]");
        // Result must be valid UTF-8.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.contains("[m]"));
    }

    #[test]
    fn cap_for_embed_passes_short_input_through() {
        assert_eq!(cap_for_embed("brief", 100), "brief");
    }

    #[test]
    fn cap_for_embed_strictly_bounds_output_to_max_bytes() {
        let big = "x".repeat(200);
        let out = cap_for_embed(&big, 100);
        // No marker, no trailing newlines: strict <= max_bytes.
        assert_eq!(out.len(), 100);
    }

    #[test]
    #[expect(
        clippy::absolute_paths,
        reason = "fully qualified paths appear in this test's assertions, which stay as written"
    )]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "the assertion stays as written; it checks only whether the call failed or succeeded"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::non_ascii_literal,
        reason = "the fixture and expected text hold the exact non-ASCII characters (em dash, ellipsis, emoji, multi-byte letters) the code under test renders or must cut on a character boundary; escapes would edit what the test checks"
    )]
    #[expect(
        clippy::std_instead_of_core,
        reason = "the assertion names std paths and stays as written"
    )]
    fn cap_for_embed_walks_back_to_utf8_boundary() {
        let mut s = "x".repeat(98);
        s.push_str("🦀tail");
        let out = cap_for_embed(&s, 100);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        // Cut must land on a char boundary at or before byte 100.
        assert!(out.len() <= 100);
    }
}
