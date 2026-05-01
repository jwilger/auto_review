//! Container-escape harness for [`PodmanSandbox`].
//!
//! These tests actually invoke an OCI runtime (`podman` if
//! installed, otherwise `docker`) and verify that the hardening
//! flags hold up against hostile inputs. They are `#[ignore]`d
//! by default because:
//!
//! 1. They require either `podman` or `docker` on PATH and
//!    runnable by the current user (rootless podman, docker
//!    group membership, or root).
//! 2. They pull `docker.io/library/alpine:3` on first run.
//! 3. They take a few seconds each (container start + work).
//!
//! Run them explicitly with:
//!
//! ```text
//! cargo test -p ar-sandbox --test escape -- --ignored
//! ```
//!
//! Each test models an attack the Kudelski-style threat model
//! cares about (RCE → exfil, RCE → resource exhaustion, RCE →
//! tampering with the host filesystem). A test PASSES when the
//! attack is contained; it FAILS if the attack succeeds (which
//! would indicate a regression in the sandbox hardening).
//!
//! `PodmanSandbox` despite its name accepts either binary via
//! `podman_bin: String` — the flag set (`--network=none`,
//! `--read-only`, `--cap-drop=ALL`, `--security-opt=no-new-
//! privileges`, etc.) is identical between podman and docker.

use ar_sandbox::{PodmanSandbox, PodmanSandboxConfig, Sandbox, SandboxCommand, SandboxError};
use std::path::PathBuf;
use std::time::Duration;

const TEST_IMAGE: &str = "docker.io/library/alpine:3";

/// Pick the first available OCI runtime on PATH. podman first
/// (rootless, no daemon), docker as a drop-in fallback.
async fn pick_runtime() -> Option<&'static str> {
    for bin in ["podman", "docker"] {
        let out = tokio::process::Command::new(bin)
            .arg("--version")
            .output()
            .await;
        if let Ok(o) = out {
            if o.status.success() {
                return Some(bin);
            }
        }
    }
    None
}

fn cfg(runtime: &str) -> PodmanSandboxConfig {
    PodmanSandboxConfig {
        image: TEST_IMAGE.into(),
        memory_mib: 128,
        cpus: 0.5,
        pids_limit: 64,
        wall_clock: Duration::from_secs(20),
        podman_bin: runtime.into(),
    }
}

fn cmd(program: &str, args: Vec<String>, working_dir: PathBuf) -> SandboxCommand {
    SandboxCommand {
        program: program.into(),
        args,
        working_dir,
        env: vec![],
    }
}

/// Skip the test cleanly when neither podman nor docker is on
/// PATH. Tests use the early-return idiom on `None`.
async fn detect_runtime() -> Option<&'static str> {
    let r = pick_runtime().await;
    if r.is_none() {
        eprintln!("skipping: neither podman nor docker found on PATH");
    }
    r
}

fn workdir_with_script(script: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("attack.sh"), script).expect("write");
    dir
}

#[tokio::test]
#[ignore = "requires podman or docker; run with --ignored"]
async fn sandbox_escape_network_egress_is_denied() {
    let Some(runtime) = detect_runtime().await else {
        return;
    };
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg(runtime));
    let res = sb
        .run(&cmd(
            "wget",
            vec![
                "-q".into(),
                "-O-".into(),
                "--timeout=3".into(),
                "https://example.com/".into(),
            ],
            dir.path().to_path_buf(),
        ))
        .await;
    let out = res.expect("runtime ran");
    assert_ne!(
        out.exit_code,
        Some(0),
        "wget should NOT succeed inside --network=none sandbox"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let looks_like_network_failure = combined.contains("bad address")
        || combined.contains("Network is unreachable")
        || combined.contains("Temporary failure in name resolution")
        || combined.contains("Could not resolve")
        || combined.contains("Connection refused");
    assert!(
        looks_like_network_failure,
        "expected network-class error; got: {combined:?}"
    );
}

#[tokio::test]
#[ignore = "requires podman or docker; run with --ignored"]
async fn sandbox_escape_fork_bomb_is_contained_by_pids_limit() {
    let Some(runtime) = detect_runtime().await else {
        return;
    };
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg(runtime));
    let res = sb
        .run(&cmd(
            "sh",
            vec!["-c".into(), ":(){ :|:& };: ; sleep 5".into()],
            dir.path().to_path_buf(),
        ))
        .await;
    match res {
        Ok(_) => {}
        Err(SandboxError::Timeout { .. }) => {}
        Err(other) => panic!("unexpected sandbox error: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires podman or docker; run with --ignored"]
async fn sandbox_escape_wall_clock_terminates_long_running_command() {
    let Some(runtime) = detect_runtime().await else {
        return;
    };
    let mut config = cfg(runtime);
    config.wall_clock = Duration::from_secs(2);
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(config);
    let res = sb
        .run(&cmd("sleep", vec!["30".into()], dir.path().to_path_buf()))
        .await;
    match res {
        Err(SandboxError::Timeout { secs: 2 }) => {}
        other => panic!("expected Timeout{{secs:2}}, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires podman or docker; run with --ignored"]
async fn sandbox_escape_repo_mount_is_read_only() {
    let Some(runtime) = detect_runtime().await else {
        return;
    };
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg(runtime));
    let res = sb
        .run(&cmd(
            "sh",
            vec!["-c".into(), "echo pwned > /work/pwned.txt".into()],
            dir.path().to_path_buf(),
        ))
        .await;
    let out = res.expect("runtime ran");
    assert_ne!(
        out.exit_code,
        Some(0),
        "writing to /work should fail (it's mounted read-only)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Read-only file system") || stderr.contains("read-only"),
        "expected read-only error; got: {stderr:?}"
    );
    assert!(
        !dir.path().join("pwned.txt").exists(),
        "host file system was written through the read-only mount"
    );
}

#[tokio::test]
#[ignore = "requires podman or docker; run with --ignored"]
async fn sandbox_escape_runs_as_unprivileged_uid() {
    let Some(runtime) = detect_runtime().await else {
        return;
    };
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg(runtime));
    let res = sb
        .run(&cmd("id", vec!["-u".into()], dir.path().to_path_buf()))
        .await;
    let out = res.expect("runtime ran");
    assert_eq!(out.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "65534",
        "expected uid 65534 (nobody) inside container"
    );
}

#[tokio::test]
#[ignore = "requires podman or docker; run with --ignored"]
async fn sandbox_escape_no_new_privileges_blocks_setuid() {
    let Some(runtime) = detect_runtime().await else {
        return;
    };
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg(runtime));
    let res = sb
        .run(&cmd(
            "sh",
            vec!["-c".into(), "grep '^NoNewPrivs' /proc/self/status".into()],
            dir.path().to_path_buf(),
        ))
        .await;
    let out = res.expect("runtime ran");
    assert_eq!(out.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("NoNewPrivs:\t1"),
        "expected NoNewPrivs:1 inside sandbox; got: {stdout:?}"
    );
}

#[tokio::test]
#[ignore = "requires podman or docker; run with --ignored"]
async fn sandbox_escape_capabilities_are_dropped() {
    let Some(runtime) = detect_runtime().await else {
        return;
    };
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg(runtime));
    let res = sb
        .run(&cmd(
            "sh",
            vec![
                "-c".into(),
                "grep -E '^Cap(Eff|Prm|Bnd)' /proc/self/status".into(),
            ],
            dir.path().to_path_buf(),
        ))
        .await;
    let out = res.expect("runtime ran");
    assert_eq!(out.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some((_label, hex)) = line.split_once('\t') {
            assert_eq!(
                hex.trim(),
                "0000000000000000",
                "non-zero capability set: {line}"
            );
        }
    }
}
