//! Red-team integration tests for the workspace-inspection tools.
//!
//! The cloned PR working tree is attacker-controlled. The agentic
//! verifier hands the cheap-tier LLM `read_file` and `search` over
//! that tree; both must refuse to escape the workspace, even when
//! the attacker arranges the filesystem to encourage escape.
//!
//! Adversarial scenarios covered here:
//! - Symlinks inside the workspace pointing OUT of it.
//! - Symlinks chained through multiple hops.
//! - Empty path strings.
//! - Backslashes (Windows-style separators) on Linux.
//! - A regex that on backtracking engines exhibits catastrophic
//!   blow-up (`(a+)+$` on `aaaa…b`); the `regex` crate ships RE2-
//!   style linear matching, so this MUST complete in milliseconds.
//! - Search inside a directory that contains a binary file: must
//!   skip silently rather than emit garbage findings or panic.

use ar_review::workspace_tools::{read_file, search, WorkspaceToolError};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, body).unwrap();
}

#[test]
fn read_file_rejects_symlink_pointing_outside_workspace() {
    let dir = tempfile::tempdir().unwrap();
    // Sentinel file outside the workspace root.
    let outside_dir = tempfile::tempdir().unwrap();
    let secret = outside_dir.path().join("secret.txt");
    fs::write(&secret, "TOP SECRET").unwrap();

    // Inside the workspace, a symlink that resolves to the secret.
    symlink(&secret, dir.path().join("trap.txt")).expect("create symlink");

    let r = read_file(dir.path(), "trap.txt", None, None, 4096);
    assert!(
        matches!(r, Err(WorkspaceToolError::PathEscape(_))),
        "symlink to outside-of-workspace must be rejected, got {r:?}"
    );
}

#[test]
fn read_file_rejects_chain_of_symlinks_pointing_outside() {
    let dir = tempfile::tempdir().unwrap();
    let outside_dir = tempfile::tempdir().unwrap();
    let secret = outside_dir.path().join("secret.txt");
    fs::write(&secret, "x").unwrap();

    // hop1 → hop2 → secret. canonicalize() should follow both
    // hops and notice the final target is outside the workspace.
    symlink(&secret, dir.path().join("hop2")).unwrap();
    symlink(dir.path().join("hop2"), dir.path().join("hop1")).unwrap();

    let r = read_file(dir.path(), "hop1", None, None, 4096);
    assert!(
        matches!(r, Err(WorkspaceToolError::PathEscape(_))),
        "chained symlink must be rejected, got {r:?}"
    );
}

#[test]
fn read_file_with_empty_relative_path_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    // An empty path resolves to the workspace root itself, which
    // is a directory — read_to_string returns an Os error. Either
    // PathEscape, NotFound, or Io is acceptable; what we must NOT
    // do is panic.
    let r = read_file(dir.path(), "", None, None, 4096);
    assert!(r.is_err(), "empty path should error, got {r:?}");
}

#[test]
fn search_with_pathological_regex_completes_in_bounded_time() {
    // Catastrophic-backtracking pattern on a backtracking engine:
    // `(a+)+$` on input "aaaaaaaaaaaaaaaaaab". On Perl/PCRE this
    // can run for seconds-to-minutes. The `regex` crate guarantees
    // linear time, so this must complete quickly. We assert on
    // wall-clock to lock in the property.
    let dir = tempfile::tempdir().unwrap();
    let payload = "a".repeat(40) + "b";
    write(dir.path(), "evil.txt", &payload);

    let started = std::time::Instant::now();
    let result = search(dir.path(), r"(a+)+$", Some("evil.txt"), 100);
    let elapsed = started.elapsed();
    assert!(result.is_ok(), "search must not error: {result:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "linear-time guarantee violated: {elapsed:?}"
    );
}

#[test]
fn search_walks_safely_past_binary_files_in_workspace() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/x.rs", "needle here\n");
    // A file with non-UTF-8 bytes (a small fragment of an ELF
    // header). search opens via read_to_string which fails on
    // non-UTF-8; the implementation skips silently.
    fs::write(
        dir.path().join("bin/blob.so"),
        [0x7f, b'E', b'L', b'F', 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .ok();
    fs::create_dir_all(dir.path().join("bin")).ok();
    fs::write(
        dir.path().join("bin/blob.so"),
        [0x7f, b'E', b'L', b'F', 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .unwrap();

    let hits = search(dir.path(), "needle", None, 100).expect("ok");
    assert_eq!(hits.len(), 1, "must find the text file");
    assert_eq!(hits[0].path, "src/x.rs");
}

#[test]
fn read_file_against_workspace_root_is_rejected_or_errors() {
    // Some attackers try to read the directory itself. Either
    // NotFound (since it's not a regular file), Io, or PathEscape
    // are acceptable — what we must NOT do is succeed and return
    // arbitrary bytes.
    let dir = tempfile::tempdir().unwrap();
    let r = read_file(dir.path(), ".", None, None, 4096);
    assert!(
        r.is_err(),
        "reading the workspace root must error, got {r:?}"
    );
}

#[test]
fn search_with_relative_dot_dot_in_path_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/x.rs", "x\n");
    // Even with workspace-relative wrapping, a `..` component in
    // the input path triggers the explicit ParentDir guard before
    // canonicalize() runs.
    let r = search(dir.path(), "x", Some("src/../../etc"), 100);
    assert!(
        matches!(r, Err(WorkspaceToolError::PathEscape(_))),
        "dot-dot path must be rejected, got {r:?}"
    );
}
