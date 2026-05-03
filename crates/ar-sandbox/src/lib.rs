//! Sandbox launcher retained for workspace-isolation rescope work.
//!
//! Normal review/orchestrator jobs no longer execute bundled linters or
//! LLM-issued shell commands. The linter-era design remains in this crate so
//! issue #46 can decide which, if any, future runtime execution paths need a
//! sandbox boundary.
//!
//! Two implementations:
//! - [`DirectSandbox`] just shells out with `tokio::process::Command`.
//!   No isolation; only safe for tests and local dev where the operator
//!   has already decided the inputs are trusted.
//! - [`PodmanSandbox`] wraps `podman run` with hardening flags
//!   (no network, read-only rootfs, dropped caps, no-new-privileges,
//!   memory/cpu/pid limits, non-root uid). Needs `podman` on PATH and
//!   a caller-provided image.
//!
//! A future `YoukiSandbox` will drive the OCI runtime directly without
//! shelling out to podman; the trait surface is shaped so that swap is
//! a one-line change at the call site.
//!
//! The unit tests here cover the argv shape; an end-to-end integration test
//! (against a real podman daemon) belongs in the deploy harness, not here.

pub mod direct;
pub mod podman;

pub use direct::DirectSandbox;
pub use podman::{PodmanSandbox, PodmanSandboxConfig};

use async_trait::async_trait;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sandbox runtime missing: {0}")]
    RuntimeMissing(String),
    #[error("wall-clock timeout after {secs}s")]
    Timeout { secs: u64 },
}

/// One sandboxed command invocation. The working directory is mounted
/// read-only by isolated implementations; passing a path here that
/// doesn't exist on the host is a programmer error.
#[derive(Debug, Clone)]
pub struct SandboxCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Mounted read-only at /work inside the sandbox. The command runs
    /// with `/work` as its cwd. For [`DirectSandbox`] this is the
    /// literal cwd of the host process.
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct SandboxOutput {
    /// `Some(code)` on normal exit, `None` if killed by signal.
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn run(&self, cmd: &SandboxCommand) -> Result<SandboxOutput, SandboxError>;
}
