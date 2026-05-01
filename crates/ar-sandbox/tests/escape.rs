//! Container-escape harness for [`PodmanSandbox`].
//!
//! These tests actually invoke `podman run` and verify that the
//! hardening flags hold up against hostile inputs. They are
//! `#[ignore]`d by default because:
//!
//! 1. They require `podman` to be installed and runnable as the
//!    current user (rootless podman counts).
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

use ar_sandbox::{PodmanSandbox, PodmanSandboxConfig, Sandbox, SandboxCommand, SandboxError};
use std::path::PathBuf;
use std::time::Duration;

/// Public Alpine image; small enough to pull quickly, ships busybox
/// utilities (`wget`, `dd`, `id`, `sh`) we need for the attack
/// scripts. Pinned to `alpine:3` to avoid floating-tag drift.
const TEST_IMAGE: &str = "docker.io/library/alpine:3";

fn cfg() -> PodmanSandboxConfig {
    PodmanSandboxConfig {
        image: TEST_IMAGE.into(),
        // Tight limits so misbehaviour terminates fast in CI.
        memory_mib: 128,
        cpus: 0.5,
        pids_limit: 64,
        wall_clock: Duration::from_secs(20),
        podman_bin: "podman".into(),
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

/// Skip the test cleanly when podman isn't on PATH or the user
/// can't run it. Returns `Some(skip_reason)` to pass to
/// `eprintln!` so CI logs explain why the test no-op'd.
async fn detect_podman() -> Option<String> {
    let out = tokio::process::Command::new("podman")
        .arg("--version")
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(format!(
            "podman --version exited {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr)
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some("podman binary not found on PATH".into())
        }
        Err(e) => Some(format!("podman --version failed to spawn: {e}")),
    }
}

/// Write a script into the working tree the sandbox sees as
/// `/work` (mounted ro). Returns the temp dir guard the caller
/// must keep alive for the duration of the run.
fn workdir_with_script(script: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("attack.sh"), script).expect("write");
    dir
}

#[tokio::test]
#[ignore = "requires podman; run with --ignored"]
async fn sandbox_escape_network_egress_is_denied() {
    if let Some(why) = detect_podman().await {
        eprintln!("skipping: {why}");
        return;
    }
    // Try to reach the internet. With --network=none, the network
    // namespace has only loopback — DNS and TCP egress both fail.
    // wget exits non-zero. We assert the exit is non-zero AND the
    // error mentions network-class failure (DNS / unreachable),
    // not e.g. "wget not found".
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg());
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
    let out = res.expect("podman ran");
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
    // Loose match: alpine's busybox-wget says one of these on
    // network failure. We don't pin to one because the wording
    // varies across alpine point releases.
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
#[ignore = "requires podman; run with --ignored"]
async fn sandbox_escape_fork_bomb_is_contained_by_pids_limit() {
    if let Some(why) = detect_podman().await {
        eprintln!("skipping: {why}");
        return;
    }
    // Classic POSIX fork bomb. With --pids-limit=64 the kernel
    // refuses new clones once the limit is hit and the bomb stops
    // multiplying. We don't require the container to "succeed";
    // we require it to TERMINATE (either by exhausting forks and
    // returning, or by hitting wall-clock and being killed). The
    // attack wins only if it crashes the host or hangs forever.
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg());
    let res = sb
        .run(&cmd(
            "sh",
            vec![
                "-c".into(),
                ":(){ :|:& };: ; sleep 5".into(), // fork bomb then sleep
            ],
            dir.path().to_path_buf(),
        ))
        .await;
    // Pass if: process returned (any exit code) OR timed out.
    // Fail only if: some unrelated SandboxError variant fires.
    match res {
        Ok(_) => {}
        Err(SandboxError::Timeout { .. }) => {}
        Err(other) => panic!("unexpected sandbox error: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires podman; run with --ignored"]
async fn sandbox_escape_wall_clock_terminates_long_running_command() {
    if let Some(why) = detect_podman().await {
        eprintln!("skipping: {why}");
        return;
    }
    // Sleep for longer than wall_clock. The host-side timeout
    // fires; SandboxError::Timeout is returned. The container is
    // killed via tokio dropping the child (and ultimately
    // `--rm` cleans it up).
    let mut config = cfg();
    config.wall_clock = Duration::from_secs(2);
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(config);
    let res = sb
        .run(&cmd(
            "sleep",
            vec!["30".into()],
            dir.path().to_path_buf(),
        ))
        .await;
    match res {
        Err(SandboxError::Timeout { secs: 2 }) => {}
        other => panic!("expected Timeout{{secs:2}}, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires podman; run with --ignored"]
async fn sandbox_escape_repo_mount_is_read_only() {
    if let Some(why) = detect_podman().await {
        eprintln!("skipping: {why}");
        return;
    }
    // The repo is bind-mounted at /work with `:ro`. Any write
    // must fail with EROFS (read-only filesystem). Without this
    // check, a malicious linter rule could clobber files in the
    // user's checkout.
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg());
    let res = sb
        .run(&cmd(
            "sh",
            vec!["-c".into(), "echo pwned > /work/pwned.txt".into()],
            dir.path().to_path_buf(),
        ))
        .await;
    let out = res.expect("podman ran");
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
    // Belt-and-braces: nothing was actually written to the host
    // checkout.
    assert!(
        !dir.path().join("pwned.txt").exists(),
        "host file system was written through the read-only mount"
    );
}

#[tokio::test]
#[ignore = "requires podman; run with --ignored"]
async fn sandbox_escape_runs_as_unprivileged_uid() {
    if let Some(why) = detect_podman().await {
        eprintln!("skipping: {why}");
        return;
    }
    // The hardening calls for `--user 65534:65534` (`nobody`).
    // Verify the process inside actually runs as that uid; if a
    // future config change forgot the flag the test catches it.
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg());
    let res = sb
        .run(&cmd("id", vec!["-u".into()], dir.path().to_path_buf()))
        .await;
    let out = res.expect("podman ran");
    assert_eq!(out.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "65534",
        "expected uid 65534 (nobody) inside container"
    );
}

#[tokio::test]
#[ignore = "requires podman; run with --ignored"]
async fn sandbox_escape_no_new_privileges_blocks_setuid() {
    if let Some(why) = detect_podman().await {
        eprintln!("skipping: {why}");
        return;
    }
    // With `--security-opt=no-new-privileges`, even a setuid
    // binary inside the container can't elevate. We probe via
    // /proc/self/status's NoNewPrivs flag (kernel-exposed). If
    // the bit is clear the hardening is missing.
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg());
    let res = sb
        .run(&cmd(
            "sh",
            vec![
                "-c".into(),
                "grep '^NoNewPrivs' /proc/self/status".into(),
            ],
            dir.path().to_path_buf(),
        ))
        .await;
    let out = res.expect("podman ran");
    assert_eq!(out.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Format: "NoNewPrivs:\t1"
    assert!(
        stdout.contains("NoNewPrivs:\t1"),
        "expected NoNewPrivs:1 inside sandbox; got: {stdout:?}"
    );
}

#[tokio::test]
#[ignore = "requires podman; run with --ignored"]
async fn sandbox_escape_capabilities_are_dropped() {
    if let Some(why) = detect_podman().await {
        eprintln!("skipping: {why}");
        return;
    }
    // --cap-drop=ALL leaves CapEff = 0. Any non-zero permitted
    // or effective set means a regression in the hardening.
    let dir = workdir_with_script("");
    let sb = PodmanSandbox::new(cfg());
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
    let out = res.expect("podman ran");
    assert_eq!(out.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Each line: "CapXXX:\t0000000000000000"
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
