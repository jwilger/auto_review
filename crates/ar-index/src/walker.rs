//! Walk a cloned workspace and extract symbols from every file we
//! recognize. Produces the [`IndexedSymbol`] stream that feeds the
//! Milestone 2 embedder.

use crate::symbols::{extract_symbols_for_path, Symbol};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;
use walkdir::{DirEntry, WalkDir};

/// One extracted symbol together with the repo-relative file it came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (rag_context tests) builds IndexedSymbol with a struct literal; \
              #[non_exhaustive] would break that construction"
)]
pub struct IndexedSymbol {
    /// Repo-relative path. Forward-slash separated even on Windows.
    pub path: String,
    #[serde(flatten)]
    pub symbol: Symbol,
}

#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("walkdir: {0}")]
    Walk(#[from] walkdir::Error),
}

/// Walk `repo_dir` recursively and return one [`IndexedSymbol`] per
/// extractable definition. Skips:
/// - `.git/` and any other dotfile-prefixed directory
/// - `target/`, `node_modules/`, `vendor/`, `third_party/`
/// - non-UTF-8 files (logged as warning, not fatal)
/// - files beyond a max-size threshold (default 1 MiB)
///
/// Per-file extraction errors are logged and the file is skipped — one
/// malformed file shouldn't poison the whole index pass.
///
/// # Errors
///
/// Returns [`WalkError::Walk`] when the directory walk itself fails (for
/// example an unreadable directory under `repo_dir`).
#[inline]
pub fn index_workspace(repo_dir: &Path) -> Result<Vec<IndexedSymbol>, WalkError> {
    // Cap recursion depth. A PR can commit deeply-nested directory
    // trees; without a max_depth, walkdir would happily descend
    // through them. 64 covers any realistic repo and matches
    // `ar_review::workspace_tools::WALK_MAX_DEPTH` for cross-tool
    // consistency.
    const MAX_DEPTH: usize = 64;

    let mut out = Vec::new();
    let walker = WalkDir::new(repo_dir)
        .follow_links(false)
        .max_depth(MAX_DEPTH)
        .into_iter();
    // depth == 0 is the walk root itself, which we always traverse —
    // tempdirs and dotfile-prefixed install dirs would otherwise be
    // filtered out by the dot-prefix rule below.
    for walked in walker.filter_entry(|candidate| {
        candidate.depth() == 0 || !is_skipped_dir(candidate.file_name().to_string_lossy().as_ref())
    }) {
        let entry = walked?;
        let Some((rel, symbols)) = extract_entry_symbols(repo_dir, &entry) else {
            continue;
        };
        for symbol in symbols {
            out.push(IndexedSymbol {
                path: rel.clone(),
                symbol,
            });
        }
    }
    Ok(out)
}

/// Repo-relative path and extracted symbols for one walked entry, or
/// `None` when the entry is skipped: not a regular file, too large,
/// unreadable, not UTF-8, or failing extraction.
#[expect(
    clippy::filetype_is_file,
    reason = "the walk does not follow links, so symlinks, sockets and other special entries \
              must be skipped along with directories; only regular files are read"
)]
fn extract_entry_symbols(repo_dir: &Path, entry: &DirEntry) -> Option<(String, Vec<Symbol>)> {
    const MAX_FILE_BYTES: u64 = 1024 * 1024;

    if !entry.file_type().is_file() {
        return None;
    }
    let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::debug!(path = %entry.path().display(), error = %error, "stat failed; skip");
            return None;
        }
    };
    if metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let rel = match entry.path().strip_prefix(repo_dir) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => return None,
    };
    let content = read_utf8_source(entry)?;
    match extract_symbols_for_path(&rel, &content) {
        Ok(symbols) => Some((rel, symbols)),
        Err(error) => {
            tracing::debug!(path = %rel, error = %error, "extraction failed; skip");
            None
        }
    }
}

/// Contents of `entry` as UTF-8 text, or `None` (logged at debug level
/// unless the file is simply not UTF-8) when it cannot be read.
fn read_utf8_source(entry: &DirEntry) -> Option<String> {
    match fs::read_to_string(entry.path()) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => None, // not utf-8
        Err(error) => {
            tracing::debug!(path = %entry.path().display(), error = %error, "read failed; skip");
            None
        }
    }
}

/// Whether a directory named `name` is pruned from the walk: VCS,
/// build-output, dependency and cache directories, plus any other
/// dot-prefixed directory.
fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "vendor"
            | "third_party"
            | ".venv"
            | "venv"
            | "__pycache__"
            | "dist"
            | "build"
            | ".next"
            | ".cache"
    ) || (name.starts_with('.') && name != "." && name != "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    fn indexes_supported_languages_in_a_workspace() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn foo() {}").unwrap();
        fs::write(dir.path().join("b.py"), "def bar(): pass").unwrap();
        fs::create_dir_all(dir.path().join("ui")).unwrap();
        fs::write(
            dir.path().join("ui/c.tsx"),
            "export function Baz() { return null }",
        )
        .unwrap();
        // Untyped/binary file should not appear and not error.
        fs::write(dir.path().join("README"), "hello").unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();

        let symbols = index_workspace(dir.path()).expect("ok");
        let names: Vec<&str> = symbols.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"Baz"));

        // Paths are relative, forward-slashed.
        assert!(symbols.iter().any(|s| s.path == "a.rs"));
        assert!(symbols.iter().any(|s| s.path == "ui/c.tsx"));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    fn skips_dotfile_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/x.rs"), "fn hidden() {}").unwrap();
        fs::write(dir.path().join("y.rs"), "fn visible() {}").unwrap();

        let symbols = index_workspace(dir.path()).expect("ok");
        let names: Vec<&str> = symbols.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"visible"));
        assert!(!names.contains(&"hidden"));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    fn skips_node_modules_and_target() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("node_modules/lib")).unwrap();
        fs::write(dir.path().join("node_modules/lib/x.ts"), "function nm() {}").unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/x.rs"), "fn tg() {}").unwrap();
        fs::write(dir.path().join("y.rs"), "fn keep() {}").unwrap();

        let symbols = index_workspace(dir.path()).expect("ok");
        let names: Vec<&str> = symbols.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"keep"));
        assert!(!names.contains(&"nm"));
        assert!(!names.contains(&"tg"));
    }

    #[test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    fn empty_workspace_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let symbols = index_workspace(dir.path()).expect("ok");
        assert!(symbols.is_empty());
    }
}
