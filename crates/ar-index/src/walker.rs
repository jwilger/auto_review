//! Walk a cloned workspace and extract symbols from every file we
//! recognize. Produces the [`IndexedSymbol`] stream that feeds the
//! Milestone 2 embedder.

use crate::symbols::{extract_symbols_for_path, Symbol};
use serde::{Deserialize, Serialize};
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedSymbol {
    /// Repo-relative path. Forward-slash separated even on Windows.
    pub path: String,
    #[serde(flatten)]
    pub symbol: Symbol,
}

#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
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
pub fn index_workspace(repo_dir: &Path) -> Result<Vec<IndexedSymbol>, WalkError> {
    const MAX_FILE_BYTES: u64 = 1024 * 1024;
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
    for entry in walker.filter_entry(|e| {
        e.depth() == 0 || !is_skipped_dir(e.file_name().to_string_lossy().as_ref())
    }) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(path = %entry.path().display(), error = %e, "stat failed; skip");
                continue;
            }
        };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let rel = match entry.path().strip_prefix(repo_dir) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue, // not utf-8
            Err(e) => {
                tracing::debug!(path = %entry.path().display(), error = %e, "read failed; skip");
                continue;
            }
        };
        let symbols = match extract_symbols_for_path(&rel, &content) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(path = %rel, error = %e, "extraction failed; skip");
                continue;
            }
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
    fn indexes_supported_languages_in_a_workspace() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "pub fn foo() {}").unwrap();
        fs::write(dir.path().join("b.py"), "def bar(): pass").unwrap();
        fs::create_dir(dir.path().join("ui")).unwrap();
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
    fn skips_dotfile_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/x.rs"), "fn hidden() {}").unwrap();
        fs::write(dir.path().join("y.rs"), "fn visible() {}").unwrap();

        let symbols = index_workspace(dir.path()).expect("ok");
        let names: Vec<&str> = symbols.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"visible"));
        assert!(!names.contains(&"hidden"));
    }

    #[test]
    fn skips_node_modules_and_target() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("node_modules/lib")).unwrap();
        fs::write(dir.path().join("node_modules/lib/x.ts"), "function nm() {}").unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/x.rs"), "fn tg() {}").unwrap();
        fs::write(dir.path().join("y.rs"), "fn keep() {}").unwrap();

        let symbols = index_workspace(dir.path()).expect("ok");
        let names: Vec<&str> = symbols.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"keep"));
        assert!(!names.contains(&"nm"));
        assert!(!names.contains(&"tg"));
    }

    #[test]
    fn empty_workspace_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let symbols = index_workspace(dir.path()).expect("ok");
        assert!(symbols.is_empty());
    }
}
