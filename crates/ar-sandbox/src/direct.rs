//! Direct (non-isolated) sandbox: spawns the command on the host.
//!
//! No isolation guarantees. Only use in tests and local dev where the
//! operator has decided the inputs are trusted. Production deployments
//! must wire [`PodmanSandbox`](crate::PodmanSandbox).

use crate::{Sandbox, SandboxCommand, SandboxError, SandboxOutput};
use async_trait::async_trait;
use tokio::process::Command;

#[derive(Default, Debug, Clone)]
pub struct DirectSandbox;

impl DirectSandbox {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Sandbox for DirectSandbox {
    async fn run(&self, cmd: &SandboxCommand) -> Result<SandboxOutput, SandboxError> {
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .current_dir(&cmd.working_dir)
            .env_clear();
        for (k, v) in &cmd.env {
            command.env(k, v);
        }
        let output = command.output().await?;
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

    fn cwd() -> PathBuf {
        std::env::current_dir().expect("cwd")
    }

    #[tokio::test]
    async fn echoes_args_to_stdout() {
        let sb = DirectSandbox::new();
        let out = sb
            .run(&SandboxCommand {
                program: "echo".into(),
                args: vec!["hello".into(), "world".into()],
                working_dir: cwd(),
                env: vec![],
            })
            .await
            .expect("run");
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello world");
    }

    #[tokio::test]
    async fn nonzero_exit_is_returned_in_output_not_as_error() {
        let sb = DirectSandbox::new();
        let out = sb
            .run(&SandboxCommand {
                program: "sh".into(),
                args: vec!["-c".into(), "exit 7".into()],
                working_dir: cwd(),
                env: vec![],
            })
            .await
            .expect("run");
        assert_eq!(out.exit_code, Some(7));
    }

    #[tokio::test]
    async fn missing_binary_yields_io_error() {
        let sb = DirectSandbox::new();
        let err = sb
            .run(&SandboxCommand {
                program: "this-binary-definitely-does-not-exist-123".into(),
                args: vec![],
                working_dir: cwd(),
                env: vec![],
            })
            .await
            .expect_err("missing binary should error");
        assert!(matches!(err, SandboxError::Io(_)));
    }

    #[tokio::test]
    async fn env_clear_starts_from_empty_environment() {
        let sb = DirectSandbox::new();
        let out = sb
            .run(&SandboxCommand {
                program: "sh".into(),
                args: vec!["-c".into(), "echo $AR_SANDBOX_PROBE".into()],
                working_dir: cwd(),
                env: vec![("AR_SANDBOX_PROBE".into(), "set-by-sandbox".into())],
            })
            .await
            .expect("run");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "set-by-sandbox"
        );
    }
}
