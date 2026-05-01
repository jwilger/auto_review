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
    // Refuse to pull a 1 GiB file into RAM just to truncate it
    // afterwards. The same 1 MiB cap as scan_file: matches the
    // indexer's per-file ceiling and covers any source file the
    // verifier would meaningfully read.
    if let Ok(meta) = fs::metadata(&resolved) {
        if meta.len() > SCAN_FILE_MAX_BYTES {
            return Err(WorkspaceToolError::NotFound(format!(
                "{rel_path} (exceeds {SCAN_FILE_MAX_BYTES} byte read cap)"
            )));
        }
    }
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

/// Cap on directory-tree recursion depth. A malicious PR could
/// commit `a/b/c/.../z/` nested thousands of levels deep (most
/// filesystems allow it even though POSIX PATH_MAX = 4096
/// usually prevents the path string from being usable). Each
/// `walk_dir` frame uses a few hundred bytes of stack, so an
/// unbounded recursion eventually overflows the default 8 MiB
/// stack. 64 covers any reasonable repo and matches what
/// `find -maxdepth` operators typically use.
const WALK_MAX_DEPTH: usize = 64;

fn walk_dir(
    workspace_root: &Path,
    dir: &Path,
    regex: &Regex,
    hits: &mut Vec<SearchHit>,
    max_hits: usize,
) -> Result<(), WorkspaceToolError> {
    walk_dir_at_depth(workspace_root, dir, regex, hits, max_hits, 0)
}

fn walk_dir_at_depth(
    workspace_root: &Path,
    dir: &Path,
    regex: &Regex,
    hits: &mut Vec<SearchHit>,
    max_hits: usize,
    depth: usize,
) -> Result<(), WorkspaceToolError> {
    if hits.len() >= max_hits || depth >= WALK_MAX_DEPTH {
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
        // Skip symlinks. A malicious PR could commit a directory
        // symlink that points back into the workspace (or itself);
        // following it would either spin indefinitely or escape
        // workspace_root via the canonicalize path. The agentic
        // verifier reads source code, not symlinked content, so
        // dropping them is safe.
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk_dir_at_depth(workspace_root, &path, regex, hits, max_hits, depth + 1)?;
        } else if file_type.is_file() {
            scan_file(workspace_root, &path, regex, hits, max_hits)?;
        }
    }
    Ok(())
}

/// Hard cap on the bytes scan_file will pull into memory per
/// file. Files committed to a PR are attacker-controllable;
/// without this, a malicious 1+ GiB committed text file would
/// OOM the gateway during the agentic verifier's search pass.
/// 1 MiB matches the indexer's per-file limit and easily covers
/// any source file the verifier would meaningfully grep.
const SCAN_FILE_MAX_BYTES: u64 = 1024 * 1024;

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
    // Stat first; refuse to read pathologically large files. A PR
    // committing a 1 GiB text file would otherwise slurp it into
    // RAM here.
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > SCAN_FILE_MAX_BYTES {
            return Ok(());
        }
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

    #[test]
    fn walk_dir_caps_recursion_depth() {
        // A malicious PR could commit a path nested thousands of
        // levels deep. Without the depth cap, walk_dir would
        // recurse until the stack overflows. With the cap (64),
        // the search returns silently with no hits past the cap.
        let dir = tempfile::tempdir().unwrap();
        // Build a 70-level deep tree; place a "needle" file at
        // each level so we can count how many levels were scanned.
        let mut path = dir.path().to_path_buf();
        for i in 0..70 {
            path = path.join(format!("d{i}"));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("file.txt"), "needle\n").unwrap();
        }
        let hits = search(dir.path(), "needle", None, 1000).expect("ok");
        // Depth 0 is the root; depth 64 means we processed 64
        // levels of subdirectories. Each level has one file. So
        // we expect <= 64 hits (depending on whether the cap
        // counts inclusive or exclusive), never 70.
        assert!(
            hits.len() <= WALK_MAX_DEPTH,
            "expected ≤ {WALK_MAX_DEPTH} hits, got {}",
            hits.len()
        );
        assert!(
            hits.len() >= 30,
            "expected at least some hits to verify the walk worked, got {}",
            hits.len()
        );
    }

    #[test]
    fn scan_file_skips_oversized_files() {
        // A PR can commit anything; without the size cap, a
        // committed 100 MiB text file would slurp into RAM. The
        // cap (1 MiB) skips the file silently — no error, no
        // hits.
        let dir = tempfile::tempdir().unwrap();
        let big_path = dir.path().join("huge.txt");
        // Write just over 1 MiB of content that matches.
        let payload: String = "needle\n".repeat(160_000); // ~1.1 MiB
        std::fs::write(&big_path, &payload).unwrap();
        // Also write a small file with the same content so we can
        // verify search is still working.
        write(dir.path(), "small.txt", "needle\n");

        let hits = search(dir.path(), "needle", None, 100).expect("ok");
        // Only the small file should be scanned.
        let small_hits: Vec<&SearchHit> = hits.iter().filter(|h| h.path == "small.txt").collect();
        let big_hits: Vec<&SearchHit> = hits.iter().filter(|h| h.path == "huge.txt").collect();
        assert_eq!(small_hits.len(), 1);
        assert!(
            big_hits.is_empty(),
            "oversized file must be skipped, got: {big_hits:?}"
        );
    }

    #[test]
    fn read_file_rejects_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let big_path = dir.path().join("huge.txt");
        // Just over 1 MiB.
        let payload: String = "x".repeat(1024 * 1024 + 100);
        std::fs::write(&big_path, &payload).unwrap();
        let err = read_file(dir.path(), "huge.txt", None, None, 4096)
            .expect_err("must reject oversized file");
        match err {
            WorkspaceToolError::NotFound(msg) => {
                assert!(msg.contains("read cap"), "expected cap message; got {msg}");
            }
            other => panic!("unexpected error class: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn walk_dir_skips_symlink_loop_without_spinning() {
        // A malicious PR could commit a directory symlink that
        // points back into the workspace (or itself). Without
        // skipping symlinks, walk_dir would either recurse forever
        // or canonicalize through the symlink and escape the
        // workspace root. The fix: skip every symlink entry.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "real.rs", "needle\n");

        // Create a self-referential symlink: dir/loop -> dir/
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        // And a file symlink to a real file.
        std::os::unix::fs::symlink(dir.path().join("real.rs"), dir.path().join("alias.rs"))
            .unwrap();

        // Search would loop / multi-count without symlink skipping.
        // With the fix it returns once for the real file.
        let hits = search(dir.path(), "needle", None, 100).expect("ok");
        assert_eq!(hits.len(), 1, "expected one hit; got: {hits:?}");
        assert_eq!(hits[0].path, "real.rs");
    }
}
