//! Read-only inspection tools the agentic verifier exposes to the LLM.
//!
//! Two operations:
//! - [`read_file`]: read a workspace-relative file, optionally
//!   restricted to a 1-indexed inclusive line range.
//! - [`search`]: regex-search a single file or the whole workspace.
//!
//! Both functions enforce that the resolved target stays inside the
//! workspace root (no `..` traversal, no symlinks pointing outside).
//! The cloned PR contents are attacker-controlled, but reading them
//! into the LLM prompt is no different from including the diff —
//! prompt-injection risk is identical and already accepted upstream.
//! What we *don't* do is execute anything in the workspace; the LLM
//! gets bytes-in-bytes-out.
//!
//! Each function caps its output (file bytes / search match count) to
//! keep tool results from blowing past the cheap-tier model's context
//! window. The caps are conservative defaults; the agentic verifier
//! takes them as parameters so future callers can tune.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceToolError {
    #[error("path escapes workspace root: {0}")]
    PathEscape(String),
    #[error("path not found: {0}")]
    NotFound(String),
    #[error("invalid regex: {0}")]
    BadRegex(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of a [`read_file`] call. `truncated` is set when we
/// surrendered a tail because the requested range exceeded
/// `max_bytes`.
#[derive(Debug, Clone)]
pub struct ReadResult {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub content: String,
    pub truncated: bool,
}

/// Read a workspace-relative file. When `start_line` and `end_line`
/// are both `Some`, only that 1-indexed inclusive range is returned;
/// when both are `None`, the whole file (subject to `max_bytes`).
///
/// Errors:
/// - [`WorkspaceToolError::PathEscape`] if `rel_path` resolves outside
///   `workspace_root` (covers `..` traversal and absolute paths).
/// - [`WorkspaceToolError::NotFound`] if the resolved path doesn't
///   exist or isn't a regular file.
pub fn read_file(
    workspace_root: &Path,
    rel_path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    max_bytes: usize,
) -> Result<ReadResult, WorkspaceToolError> {
    let resolved = resolve_inside(workspace_root, rel_path)?;
    let raw = fs::read_to_string(&resolved).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            WorkspaceToolError::NotFound(rel_path.into())
        } else {
            WorkspaceToolError::Io(e)
        }
    })?;
    let total_lines = raw.lines().count() as u32;
    let (start, end) = match (start_line, end_line) {
        (Some(s), Some(e)) if s >= 1 && e >= s => (s, e.min(total_lines.max(1))),
        _ => (1, total_lines.max(1)),
    };
    let mut content = String::new();
    for (idx, line) in raw.lines().enumerate() {
        let line_no = idx as u32 + 1;
        if line_no < start {
            continue;
        }
        if line_no > end {
            break;
        }
        content.push_str(line);
        content.push('\n');
    }
    let truncated = if content.len() > max_bytes {
        content.truncate_to_char_boundary(max_bytes);
        true
    } else {
        false
    };
    Ok(ReadResult {
        path: rel_path.into(),
        start_line: start,
        end_line: end,
        content,
        truncated,
    })
}

/// Single match record from [`search`].
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    pub line_text: String,
}

/// Search the workspace for lines matching `pattern`. When
/// `rel_path` is `Some`, only that file is searched; when `None`,
/// the recursive walk skips heavy directories (`.git`, `target`,
/// `node_modules`, `vendor`, `__pycache__`, `.venv`, `dist`, `build`,
/// `.next`, `.cache`).
///
/// Output is capped at `max_hits` (excess matches are silently
/// discarded — the caller can fall back to a more specific search).
pub fn search(
    workspace_root: &Path,
    pattern: &str,
    rel_path: Option<&str>,
    max_hits: usize,
) -> Result<Vec<SearchHit>, WorkspaceToolError> {
    let regex = Regex::new(pattern)
        .map_err(|e| WorkspaceToolError::BadRegex(format!("/{pattern}/: {e}")))?;

    let target = match rel_path {
        Some(p) => Some(resolve_inside(workspace_root, p)?),
        None => None,
    };

    let mut hits: Vec<SearchHit> = Vec::new();

    if let Some(path) = target {
        if path.is_file() {
            scan_file(workspace_root, &path, &regex, &mut hits, max_hits)?;
        } else if path.is_dir() {
            walk_dir(workspace_root, &path, &regex, &mut hits, max_hits)?;
        } else {
            return Err(WorkspaceToolError::NotFound(
                rel_path.unwrap_or("").to_string(),
            ));
        }
    } else {
        walk_dir(workspace_root, workspace_root, &regex, &mut hits, max_hits)?;
    }
    Ok(hits)
}

fn walk_dir(
    workspace_root: &Path,
    dir: &Path,
    regex: &Regex,
    hits: &mut Vec<SearchHit>,
    max_hits: usize,
) -> Result<(), WorkspaceToolError> {
    if hits.len() >= max_hits {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // Skip unreadable directories silently.
    };
    for entry in entries.flatten() {
        if hits.len() >= max_hits {
            return Ok(());
        }
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if is_skipped_dirname(name) {
            continue;
        }
        if path.is_dir() {
            walk_dir(workspace_root, &path, regex, hits, max_hits)?;
        } else if path.is_file() {
            scan_file(workspace_root, &path, regex, hits, max_hits)?;
        }
    }
    Ok(())
}

fn scan_file(
    workspace_root: &Path,
    path: &Path,
    regex: &Regex,
    hits: &mut Vec<SearchHit>,
    max_hits: usize,
) -> Result<(), WorkspaceToolError> {
    if hits.len() >= max_hits {
        return Ok(());
    }
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(()), // Binary / non-utf8 / unreadable files skipped.
    };
    let rel = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .display()
        .to_string();
    for (idx, line) in content.lines().enumerate() {
        if hits.len() >= max_hits {
            break;
        }
        if regex.is_match(line) {
            hits.push(SearchHit {
                path: rel.clone(),
                line: idx as u32 + 1,
                line_text: line.to_string(),
            });
        }
    }
    Ok(())
}

fn is_skipped_dirname(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "vendor"
            | "third_party"
            | "__pycache__"
            | ".venv"
            | "dist"
            | "build"
            | ".next"
            | ".cache"
    )
}

/// Resolve `rel_path` against `workspace_root` and confirm the
/// resulting absolute path stays under `workspace_root` after
/// canonicalization. Rejects absolute inputs, `..` traversal, and
/// symlinks that point outside the workspace.
fn resolve_inside(workspace_root: &Path, rel_path: &str) -> Result<PathBuf, WorkspaceToolError> {
    let candidate = Path::new(rel_path);
    if candidate.is_absolute() {
        return Err(WorkspaceToolError::PathEscape(rel_path.into()));
    }
    // Walk components manually to reject `..` traversal even before
    // canonicalize() (which would error if the parent doesn't exist
    // for a symbolic-link-relative path).
    for comp in candidate.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(WorkspaceToolError::PathEscape(rel_path.into()));
        }
    }
    let joined = workspace_root.join(candidate);
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(WorkspaceToolError::Io)?;
    let canonical = joined.canonicalize().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            WorkspaceToolError::NotFound(rel_path.into())
        } else {
            WorkspaceToolError::Io(e)
        }
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(WorkspaceToolError::PathEscape(rel_path.into()));
    }
    Ok(canonical)
}

trait TruncateUtf8 {
    fn truncate_to_char_boundary(&mut self, max: usize);
}

impl TruncateUtf8 for String {
    fn truncate_to_char_boundary(&mut self, max: usize) {
        if self.len() <= max {
            return;
        }
        let mut cut = max;
        while cut > 0 && !self.is_char_boundary(cut) {
            cut -= 1;
        }
        self.truncate(cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn read_file_returns_full_content_when_no_range_given() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/lib.rs", "line1\nline2\nline3\n");
        let r = read_file(dir.path(), "src/lib.rs", None, None, 4096).expect("ok");
        assert_eq!(r.start_line, 1);
        assert_eq!(r.end_line, 3);
        assert_eq!(r.content, "line1\nline2\nline3\n");
        assert!(!r.truncated);
    }

    #[test]
    fn read_file_respects_line_range() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.txt", "a\nb\nc\nd\ne\n");
        let r = read_file(dir.path(), "x.txt", Some(2), Some(4), 4096).expect("ok");
        assert_eq!(r.start_line, 2);
        assert_eq!(r.end_line, 4);
        assert_eq!(r.content, "b\nc\nd\n");
    }

    #[test]
    fn read_file_clamps_end_to_total_lines() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.txt", "a\nb\n");
        let r = read_file(dir.path(), "x.txt", Some(1), Some(99), 4096).expect("ok");
        assert_eq!(r.end_line, 2);
    }

    #[test]
    fn read_file_marks_truncated_when_content_exceeds_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "big.txt", &"x".repeat(2_000));
        let r = read_file(dir.path(), "big.txt", None, None, 100).expect("ok");
        assert!(r.truncated);
        assert!(r.content.len() <= 100);
    }

    #[test]
    fn read_file_rejects_parent_dir_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let r = read_file(dir.path(), "../etc/passwd", None, None, 4096);
        assert!(matches!(r, Err(WorkspaceToolError::PathEscape(_))));
    }

    #[test]
    fn read_file_rejects_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let r = read_file(dir.path(), "/etc/passwd", None, None, 4096);
        assert!(matches!(r, Err(WorkspaceToolError::PathEscape(_))));
    }

    #[test]
    fn read_file_returns_not_found_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let r = read_file(dir.path(), "nope.txt", None, None, 4096);
        assert!(matches!(r, Err(WorkspaceToolError::NotFound(_))));
    }

    #[test]
    fn search_finds_matches_in_single_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/auth.rs",
            "fn validate() {\n    panic!(\"oops\");\n}\n",
        );
        let hits = search(dir.path(), r"panic!", Some("src/auth.rs"), 100).expect("ok");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert!(hits[0].line_text.contains("panic!"));
    }

    #[test]
    fn search_walks_workspace_when_no_path_given() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "todo: refactor\n");
        write(dir.path(), "src/b.rs", "todo: rewrite\n");
        write(dir.path(), "src/c.rs", "no match here\n");
        let hits = search(dir.path(), r"todo:", None, 100).expect("ok");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn search_skips_node_modules_and_target_directories() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/x.rs", "needle\n");
        write(dir.path(), "node_modules/junk.js", "needle\n");
        write(dir.path(), "target/debug/y.rs", "needle\n");
        let hits = search(dir.path(), "needle", None, 100).expect("ok");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/x.rs");
    }

    #[test]
    fn search_caps_results_at_max_hits() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.rs", "TODO\nTODO\nTODO\nTODO\nTODO\n");
        let hits = search(dir.path(), "TODO", Some("x.rs"), 3).expect("ok");
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn search_invalid_regex_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let r = search(dir.path(), "(unclosed", None, 100);
        assert!(matches!(r, Err(WorkspaceToolError::BadRegex(_))));
    }

    #[test]
    fn search_rejects_path_escape() {
        let dir = tempfile::tempdir().unwrap();
        let r = search(dir.path(), "x", Some("../etc"), 100);
        assert!(matches!(r, Err(WorkspaceToolError::PathEscape(_))));
    }
}
