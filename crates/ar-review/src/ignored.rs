//! Filter changed files and unified-diff content by gitignore-style globs.
//!
//! Used by the orchestrator to honor `.auto_review.yaml`'s `ignored_paths`
//! list. The matcher is `globset`-based and tolerates malformed patterns
//! by skipping them (with a log warning) so a single bad entry doesn't
//! break the whole filter.

use ar_forge::ChangedFile;
use core::iter;
use globset::{Glob, GlobSet, GlobSetBuilder};

/// Compile a list of glob patterns into a [`GlobSet`]. Patterns that fail
/// to compile are dropped with a warning log; the resulting matcher
/// covers the survivors.
#[inline]
#[must_use = "building the matcher has no side effects beyond warnings; the set is the result"]
pub fn build_glob_set(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        match Glob::new(pattern) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(err) => {
                tracing::warn!(pattern, error = %err, "ignoring malformed glob pattern");
            }
        }
    }
    builder.build().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "failed to build glob set; using empty matcher");
        GlobSet::empty()
    })
}

/// Drop any [`ChangedFile`] whose `filename` matches the glob set.
#[inline]
#[must_use = "filtering has no side effects; the surviving files are the only result"]
pub fn filter_changed_files(files: &[ChangedFile], ignored: &GlobSet) -> Vec<ChangedFile> {
    if ignored.is_empty() {
        return files.to_vec();
    }
    files
        .iter()
        .filter(|file| !ignored.is_match(&file.filename))
        .cloned()
        .collect()
}

/// Strip per-file diff sections whose path matches the glob set.
///
/// Recognizes file boundaries via the `diff --git ` marker (only at line
/// starts). The path is extracted from `diff --git a/<path> b/<path>`;
/// when extraction fails, the section is kept (fail-open).
#[inline]
#[must_use = "filtering has no side effects; the filtered diff is the only result"]
pub fn filter_diff_paths(diff: &str, ignored: &GlobSet) -> String {
    if ignored.is_empty() {
        return diff.to_owned();
    }
    let sections = split_diff_sections(diff);
    if sections.is_empty() {
        return diff.to_owned();
    }

    let mut out = String::with_capacity(diff.len());
    let mut omitted: usize = 0;
    for section in sections {
        match extract_diff_path(section) {
            Some(path) if ignored.is_match(path) => omitted = omitted.saturating_add(1),
            _ => out.push_str(section),
        }
    }

    if omitted > 0 {
        #[expect(
            clippy::format_push_string,
            reason = "a single trailing note per filtered diff; write! into a String returns an \
                      infallible fmt::Result that would have to be discarded"
        )]
        out.push_str(&format!(
            "\n[auto_review: omitted {omitted} file section(s) matching ignored_paths]\n"
        ));
    }
    out
}

/// Return the new-side filenames named by each per-file diff section.
#[inline]
pub fn diff_changed_paths(diff: &str) -> Vec<String> {
    split_diff_sections(diff)
        .into_iter()
        .filter_map(extract_diff_path)
        .map(String::from)
        .collect()
}

/// Split `diff` into per-file sections, each starting at a `diff --git `
/// marker at the start of a line. Returns no sections when no marker exists.
fn split_diff_sections(diff: &str) -> Vec<&str> {
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
    if let Some((_, after)) = rest.split_once("\"b/") {
        if let Some((path, _)) = after.split_once('"') {
            return Some(path);
        }
    }
    // Unquoted: `... b/<path>` after a space.
    let (_, b_path) = rest.split_once(" b/")?;
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
            previous_filename: None,
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
        let names: Vec<&str> = kept.iter().map(|file| file.filename.as_str()).collect();
        assert_eq!(names, vec!["a.rs"]);
    }

    #[test]
    fn malformed_pattern_is_skipped_silently() {
        // `[` without a matching `]` is invalid; the other pattern still
        // compiles and is honored.
        let set = build_glob_set(&["[invalid".into(), "*.lock".into()]);
        let files = vec![cf("Cargo.lock"), cf("src/main.rs")];
        let kept = filter_changed_files(&files, &set);
        let names: Vec<&str> = kept.iter().map(|file| file.filename.as_str()).collect();
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
