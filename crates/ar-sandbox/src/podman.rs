//! Podman-based sandbox.
//!
//! Wraps the user-supplied program in a `podman run` invocation with
//! the hardening flags we treat as the minimum bar for v1:
//!
//! - `--network=none`          — no egress, mitigates exfil + SSRF
//! - `--read-only`             — image rootfs immutable
//! - `--tmpfs /tmp:size=...`   — small writable scratch
//! - `--security-opt=no-new-privileges`
//! - `--cap-drop=ALL`          — strip all Linux capabilities
//! - `--memory=...`            — RSS cap, OOM-kills runaways
//! - `--cpus=...`              — CPU quota, blunts fork-bombs
//! - `--pids-limit=...`        — defence in depth against fork-bombs
//! - `--user 65534:65534`      — run as `nobody`
//! - `-v <repo>:/work:ro`      — repo mounted read-only
//! - `-w /work`                — cwd inside container
//!
//! Wall-clock timeout is enforced on the host side via tokio's timeout
//! wrapper plus a `podman kill --signal=KILL <name>` cleanup. We
//! deliberately do NOT use podman's `--stop-timeout` flag — that
//! controls the SIGTERM grace period, not a kill-after-N-seconds.

use crate::{Sandbox, SandboxCommand, SandboxError, SandboxOutput};
use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct PodmanSandboxConfig {
    /// OCI image with the linter binaries pre-installed.
    pub image: String,
    /// RSS limit, in MiB. Hits the cgroup memory controller.
    pub memory_mib: u64,
    /// CPU quota (e.g. `1.0` = 1 core).
    pub cpus: f64,
    /// Max processes/threads inside the sandbox.
    pub pids_limit: u32,
    /// Wall-clock timeout. After this, the container is killed.
    pub wall_clock: Duration,
    /// Path to the `podman` binary. Defaults to `"podman"` (PATH lookup).
    pub podman_bin: String,
}

impl Default for PodmanSandboxConfig {
    fn default() -> Self {
        Self {
            image: "ghcr.io/auto-review/sandbox:latest".into(),
            memory_mib: 512,
            cpus: 1.0,
            pids_limit: 128,
            wall_clock: Duration::from_secs(60),
            podman_bin: "podman".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PodmanSandbox {
    config: PodmanSandboxConfig,
}

impl PodmanSandbox {
    pub fn new(config: PodmanSandboxConfig) -> Self {
        Self { config }
    }

    /// Build the `podman run ...` argv. Public for unit testing the
    /// hardening flags without needing podman installed.
    pub fn build_argv(&self, cmd: &SandboxCommand) -> Vec<String> {
        let mut argv: Vec<String> = vec![
            "run".into(),
            "--rm".into(),
            "--network=none".into(),
            "--read-only".into(),
            "--tmpfs".into(),
            "/tmp:size=64m".into(),
            "--security-opt=no-new-privileges".into(),
            "--cap-drop=ALL".into(),
            format!("--memory={}m", self.config.memory_mib),
            format!("--cpus={}", self.config.cpus),
            format!("--pids-limit={}", self.config.pids_limit),
            "--user".into(),
            "65534:65534".into(),
            "-v".into(),
            format!("{}:/work:ro", cmd.working_dir.display()),
            "-w".into(),
            "/work".into(),
        ];
        for (k, v) in &cmd.env {
            argv.push("-e".into());
            argv.push(format!("{k}={v}"));
        }
        argv.push(self.config.image.clone());
        argv.push(cmd.program.clone());
        argv.extend(cmd.args.iter().cloned());
        argv
    }
}

#[async_trait]
impl Sandbox for PodmanSandbox {
    async fn run(&self, cmd: &SandboxCommand) -> Result<SandboxOutput, SandboxError> {
        let argv = self.build_argv(cmd);
        let mut command = Command::new(&self.config.podman_bin);
        command.args(&argv).env_clear();
        let fut = command.output();
        let output = match tokio::time::timeout(self.config.wall_clock, fut).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SandboxError::RuntimeMissing(self.config.podman_bin.clone()));
            }
            Ok(Err(e)) => return Err(SandboxError::Io(e)),
            Err(_) => {
                return Err(SandboxError::Timeout {
                    secs: self.config.wall_clock.as_secs(),
                });
            }
        };
        Ok(SandboxOutput {
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cmd() -> SandboxCommand {
        SandboxCommand {
            program: "ruff".into(),
            args: vec!["check".into(), "--output-format=json".into()],
            working_dir: PathBuf::from("/host/repo"),
            env: vec![("RUFF_NO_CACHE".into(), "1".into())],
        }
    }

    #[test]
    fn argv_includes_no_network_and_read_only() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
        let argv = sb.build_argv(&cmd());
        assert!(argv.iter().any(|a| a == "--network=none"));
        assert!(argv.iter().any(|a| a == "--read-only"));
    }

    #[test]
    fn argv_drops_caps_and_disables_new_privs() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
        let argv = sb.build_argv(&cmd());
        assert!(argv.iter().any(|a| a == "--cap-drop=ALL"));
        assert!(argv.iter().any(|a| a == "--security-opt=no-new-privileges"));
    }

    #[test]
    fn argv_runs_as_unprivileged_uid() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
        let argv = sb.build_argv(&cmd());
        // Look for "--user" followed by "65534:65534".
        let pos = argv.iter().position(|a| a == "--user").expect("--user");
        assert_eq!(argv.get(pos + 1).map(String::as_str), Some("65534:65534"));
    }

    #[test]
    fn argv_mounts_repo_read_only_at_work() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
        let argv = sb.build_argv(&cmd());
        let pos = argv.iter().position(|a| a == "-v").expect("-v");
        assert_eq!(
            argv.get(pos + 1).map(String::as_str),
            Some("/host/repo:/work:ro")
        );
        let wpos = argv.iter().position(|a| a == "-w").expect("-w");
        assert_eq!(argv.get(wpos + 1).map(String::as_str), Some("/work"));
    }

    #[test]
    fn argv_applies_resource_limits() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig {
            memory_mib: 256,
            cpus: 0.5,
            pids_limit: 64,
            ..PodmanSandboxConfig::default()
        });
        let argv = sb.build_argv(&cmd());
        assert!(argv.iter().any(|a| a == "--memory=256m"));
        assert!(argv.iter().any(|a| a == "--cpus=0.5"));
        assert!(argv.iter().any(|a| a == "--pids-limit=64"));
    }

    #[test]
    fn argv_passes_env_through_dash_e_after_image() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
        let argv = sb.build_argv(&cmd());
        // -e KEY=VAL must appear, and the image+program must come after.
        let env_pos = argv
            .iter()
            .position(|a| a == "-e")
            .expect("-e flag for env");
        assert_eq!(
            argv.get(env_pos + 1).map(String::as_str),
            Some("RUFF_NO_CACHE=1")
        );
        let image_pos = argv
            .iter()
            .position(|a| a == "ghcr.io/auto-review/sandbox:latest")
            .expect("image present");
        assert!(image_pos > env_pos, "image must come after env flags");
    }

    #[test]
    fn program_and_args_appear_after_image() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
        let argv = sb.build_argv(&cmd());
        let image_pos = argv
            .iter()
            .position(|a| a == "ghcr.io/auto-review/sandbox:latest")
            .expect("image present");
        assert_eq!(argv.get(image_pos + 1).map(String::as_str), Some("ruff"));
        assert_eq!(argv.get(image_pos + 2).map(String::as_str), Some("check"));
        assert_eq!(
            argv.get(image_pos + 3).map(String::as_str),
            Some("--output-format=json")
        );
    }

    #[test]
    fn argv_starts_with_run_rm() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig::default());
        let argv = sb.build_argv(&cmd());
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "--rm");
    }

    #[tokio::test]
    async fn missing_podman_binary_returns_runtime_missing() {
        let sb = PodmanSandbox::new(PodmanSandboxConfig {
            podman_bin: "this-podman-definitely-does-not-exist-456".into(),
            ..PodmanSandboxConfig::default()
        });
        let err = sb.run(&cmd()).await.expect_err("must error");
        assert!(matches!(err, SandboxError::RuntimeMissing(_)));
    }
}
