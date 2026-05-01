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

/// Default cap. Roughly ~25k tokens in worst-case English; well below
/// even a small reasoning model's context window.
pub const DEFAULT_MAX_DIFF_BYTES: usize = 100_000;

/// Shared cap for cheap-tier prompts (triage, verifier, agentic
/// verifier, pre-merge custom checks). Cheap-tier models often have
/// 32K-token windows; 40 KiB stays comfortably inside.
pub const CHEAP_TIER_DIFF_CAP: usize = 40 * 1024;

/// Cap `body` at `max_bytes` for inclusion in an LLM prompt.
///
/// When the body fits, returns it unchanged. When it exceeds the
/// cap, returns the prefix walked back to a UTF-8 character
/// boundary, plus a single trailing line containing `marker` (so
/// the model can see the body was abridged). Used by every
/// cheap-tier prompt builder that embeds the PR's unified diff.
pub fn cap_for_prompt(body: &str, max_bytes: usize, marker: &str) -> String {
    if body.len() <= max_bytes {
        return body.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(cut + marker.len() + 2);
    out.push_str(&body[..cut]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(marker);
    out.push('\n');
    out
}

/// Cap a unified diff at `max_bytes`, splitting at `diff --git ` file
/// boundaries. Returns the original diff unchanged when it already fits.
pub fn cap_diff(diff: &str, max_bytes: usize) -> String {
    if diff.len() <= max_bytes {
        return diff.to_string();
    }

    let files = split_files(diff);
    if files.is_empty() {
        // No file markers found at all — fall back to a flat truncation.
        return truncate_flat(diff, max_bytes);
    }

    let mut out = String::with_capacity(max_bytes);
    let mut included = 0usize;
    let mut omitted = 0usize;
    for f in files {
        if out.len() + f.len() <= max_bytes {
            out.push_str(f);
            included += 1;
        } else {
            omitted += 1;
        }
    }

    if omitted > 0 {
        out.push_str(&format!(
            "\n\n[auto_review: omitted {omitted} file(s) to fit a {} KiB diff cap; included {included} file(s)]\n",
            max_bytes / 1024
        ));
    }

    out
}

fn split_files(diff: &str) -> Vec<&str> {
    const MARKER: &str = "diff --git ";
    let mut starts: Vec<usize> = Vec::new();
    let mut search_start = 0;
    while let Some(rel) = diff[search_start..].find(MARKER) {
        let abs = search_start + rel;
        if abs == 0 || diff.as_bytes()[abs - 1] == b'\n' {
            starts.push(abs);
        }
        search_start = abs + MARKER.len();
    }

    if starts.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(starts.len());
    for i in 0..starts.len() {
        let begin = starts[i];
        let end = starts.get(i + 1).copied().unwrap_or(diff.len());
        out.push(&diff[begin..end]);
    }
    out
}

fn truncate_flat(diff: &str, max_bytes: usize) -> String {
    let mut end = max_bytes.min(diff.len());
    // Don't truncate in the middle of a UTF-8 codepoint.
    while end > 0 && !diff.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = diff[..end].to_string();
    out.push_str("\n\n[auto_review: diff truncated to fit byte cap]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
    fn diff_with_no_file_markers_falls_back_to_flat_truncation() {
        let d = "x".repeat(1000);
        let capped = cap_diff(&d, 100);
        assert!(capped.len() < 200);
        assert!(capped.contains("truncated"));
    }

    #[test]
    fn flat_truncation_does_not_split_utf8_codepoint() {
        // 'é' is two bytes in UTF-8 (0xC3 0xA9).
        let d = format!("héllo {}", "x".repeat(1000));
        let capped = cap_diff(&d, 7); // one byte into 'é' if naïve.
                                      // Whatever we emit must remain valid UTF-8 (and prefix-only of d).
        assert!(capped.is_ascii() || std::str::from_utf8(capped.as_bytes()).is_ok());
    }

    #[test]
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
    fn cap_for_prompt_walks_back_to_utf8_boundary() {
        // 4-byte emoji at the cut boundary must not be split.
        let mut s = "x".repeat(98);
        s.push_str("🦀tail");
        let out = cap_for_prompt(&s, 100, "[m]");
        // Result must be valid UTF-8.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.contains("[m]"));
    }
}
