//! Filter changed files and unified-diff content by gitignore-style globs.
//!
//! Used by the orchestrator to honor `.auto_review.yaml`'s `ignored_paths`
//! list. The matcher is `globset`-based and tolerates malformed patterns
//! by skipping them (with a log warning) so a single bad entry doesn't
//! break the whole filter.

use ar_forgejo::ChangedFile;
use globset::{Glob, GlobSet, GlobSetBuilder};

/// Compile a list of glob patterns into a [`GlobSet`]. Patterns that fail
/// to compile are dropped with a warning log; the resulting matcher
/// covers the survivors.
pub fn build_glob_set(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        match Glob::new(p) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => {
                tracing::warn!(pattern = p, error = %e, "ignoring malformed glob pattern");
            }
        }
    }
    builder.build().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to build glob set; using empty matcher");
        GlobSet::empty()
    })
}

/// Drop any [`ChangedFile`] whose `filename` matches the glob set.
pub fn filter_changed_files(files: &[ChangedFile], ignored: &GlobSet) -> Vec<ChangedFile> {
    if ignored.is_empty() {
        return files.to_vec();
    }
    files
        .iter()
        .filter(|f| !ignored.is_match(&f.filename))
        .cloned()
        .collect()
}

/// Strip per-file diff sections whose path matches the glob set.
///
/// Recognizes file boundaries via the `diff --git ` marker (only at line
/// starts). The path is extracted from `diff --git a/<path> b/<path>`;
/// when extraction fails, the section is kept (fail-open).
pub fn filter_diff_paths(diff: &str, ignored: &GlobSet) -> String {
    if ignored.is_empty() {
        return diff.to_string();
    }
    let sections = split_diff_sections(diff);
    if sections.is_empty() {
        return diff.to_string();
    }

    let mut out = String::with_capacity(diff.len());
    let mut omitted = 0usize;
    for s in sections {
        match extract_diff_path(s) {
            Some(path) if ignored.is_match(path) => omitted += 1,
            _ => out.push_str(s),
        }
    }

    if omitted > 0 {
        out.push_str(&format!(
            "\n[auto_review: omitted {omitted} file section(s) matching ignored_paths]\n"
        ));
    }
    out
}

/// Return the new-side filenames named by each per-file diff section.
pub fn diff_changed_paths(diff: &str) -> Vec<String> {
    split_diff_sections(diff)
        .into_iter()
        .filter_map(extract_diff_path)
        .map(String::from)
        .collect()
}

fn split_diff_sections(diff: &str) -> Vec<&str> {
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

/// Extract the new-side path from a `diff --git a/<path> b/<path>` header.
///
/// Handles git's quoted form too — when `core.quotepath=true` (default)
/// and a path contains spaces or special characters, git emits
/// `diff --git "a/old name" "b/new name"`. Without quoted handling
/// the unquoted ` b/` anchor wouldn't match (the b/ is preceded by `"`,
/// not space) and the section's path-based filter would silently
/// fall through.
fn extract_diff_path(section: &str) -> Option<&str> {
    let line = section.lines().next()?;
    let rest = line.strip_prefix("diff --git ")?;
    // Try the quoted form first: `... "b/<path>"`.
    if let Some(b_idx) = rest.find("\"b/") {
        let after = &rest[b_idx + "\"b/".len()..];
        if let Some(end) = after.find('"') {
            return Some(&after[..end]);
        }
    }
    // Unquoted: `... b/<path>` after a space.
    let b_idx = rest.find(" b/")?;
    let b_path = &rest[b_idx + " b/".len()..];
    Some(b_path.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cf(name: &str) -> ChangedFile {
        ChangedFile {
            filename: name.into(),
            status: "modified".into(),
            additions: 0,
            deletions: 0,
            changes: 0,
            patch: None,
        }
    }

    #[test]
    fn empty_glob_set_passes_files_through_unchanged() {
        let set = build_glob_set(&[]);
        let files = vec![cf("a.rs"), cf("vendor/x.go")];
        let kept = filter_changed_files(&files, &set);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn glob_filters_matching_files() {
        let set = build_glob_set(&["vendor/**".into()]);
        let files = vec![cf("a.rs"), cf("vendor/x.go"), cf("vendor/sub/y.go")];
        let kept = filter_changed_files(&files, &set);
        let names: Vec<&str> = kept.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, vec!["a.rs"]);
    }

    #[test]
    fn malformed_pattern_is_skipped_silently() {
        // `[` without a matching `]` is invalid; the other pattern still
        // compiles and is honored.
        let set = build_glob_set(&["[invalid".into(), "*.lock".into()]);
        let files = vec![cf("Cargo.lock"), cf("src/main.rs")];
        let kept = filter_changed_files(&files, &set);
        let names: Vec<&str> = kept.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, vec!["src/main.rs"]);
    }

    #[test]
    fn filter_diff_paths_drops_matching_sections() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
@@ -1 +1 @@
-old
+new
diff --git a/vendor/x.go b/vendor/x.go
@@ -1 +1 @@
-old
+new
";
        let set = build_glob_set(&["vendor/**".into()]);
        let out = filter_diff_paths(diff, &set);
        assert!(out.contains("src/main.rs"));
        assert!(!out.contains("vendor/x.go"));
        assert!(out.contains("omitted 1"));
    }

    #[test]
    fn filter_diff_paths_with_empty_set_returns_diff_unchanged() {
        let diff = "diff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let set = build_glob_set(&[]);
        assert_eq!(filter_diff_paths(diff, &set), diff);
    }

    #[test]
    fn filter_diff_paths_with_no_markers_returns_input_unchanged() {
        let diff = "this is not a diff";
        let set = build_glob_set(&["*".into()]);
        assert_eq!(filter_diff_paths(diff, &set), diff);
    }

    #[test]
    fn extract_diff_path_handles_typical_header() {
        assert_eq!(
            extract_diff_path("diff --git a/src/main.rs b/src/main.rs\n@@ -1 +1 @@\n"),
            Some("src/main.rs")
        );
    }

    #[test]
    fn extract_diff_path_handles_path_with_directories() {
        assert_eq!(
            extract_diff_path("diff --git a/deep/nested/path.go b/deep/nested/path.go\n"),
            Some("deep/nested/path.go")
        );
    }

    #[test]
    fn extract_diff_path_returns_none_on_malformed_header() {
        assert!(extract_diff_path("not a header").is_none());
        assert!(extract_diff_path("diff --git only-a-side\n").is_none());
    }

    #[test]
    fn extract_diff_path_handles_quoted_path_with_spaces() {
        // git's default core.quotepath=true emits `"a/path" "b/path"`
        // for paths with spaces / special chars. Without quoted-form
        // support, the section's path-based filter would silently
        // fall through and ignored_paths globs wouldn't match.
        let section = "diff --git \"a/My File.txt\" \"b/My File.txt\"\n";
        assert_eq!(extract_diff_path(section), Some("My File.txt"));
    }

    #[test]
    fn extract_diff_path_handles_quoted_rename() {
        let section = "diff --git \"a/old name.rs\" \"b/new name.rs\"\n";
        assert_eq!(extract_diff_path(section), Some("new name.rs"));
    }
}
