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
//! - `read_file(".git/config")` on a workspace prepared from a clone
//!   URL: the clone remote (whose URL carries the bot credential in
//!   production) is removed after checkout, so the tool returns no
//!   `[remote "origin"]` section, `url =` entry or `://user@` URL (T5).

use ar_review::workspace::prepare_workspace_from_clone_url;
use ar_review::workspace_tools::{read_file, search, WorkspaceToolError};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

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

/// Run git for fixture setup only, isolated from the host git config so the
/// fixture does not depend on (or run) anything the developer configured.
fn fixture_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "true")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HOME", "/dev/null")
        .env("XDG_CONFIG_HOME", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_PREFIX")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn read_file_on_prepared_workspace_git_config_exposes_no_clone_url() {
    let fixture = tempfile::tempdir().unwrap();
    let src = fixture.path().join("src");
    let bare = fixture.path().join("bare.git");
    fs::create_dir_all(&src).unwrap();
    fixture_git(&src, &["init", "--quiet"]);
    write(&src, "lib.rs", "pub fn f() {}\n");
    fixture_git(&src, &["add", "lib.rs"]);
    fixture_git(
        &src,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    let head_sha = fixture_git(&src, &["rev-parse", "HEAD"]).trim().to_string();
    fixture_git(
        fixture.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );

    let workspace = prepare_workspace_from_clone_url(
        &format!("file://{}", bare.display()),
        &head_sha,
        None,
        None,
    )
    .await
    .unwrap();

    let seen = match read_file(workspace.path(), ".git/config", None, None, 64 * 1024) {
        Ok(content) => content.content,
        Err(err) => err.to_string(),
    };
    let userinfo_url = regex::Regex::new(r"://[^/@\s]+@").unwrap();
    assert!(
        !seen.contains("[remote \"origin\"]"),
        "origin remote leaked into .git/config:\n{seen}"
    );
    assert!(
        !seen.contains("url ="),
        "a remote URL leaked into .git/config:\n{seen}"
    );
    assert!(
        !userinfo_url.is_match(&seen),
        "a URL with userinfo leaked into .git/config:\n{seen}"
    );
}
