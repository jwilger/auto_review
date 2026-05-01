use crate::finding::Finding;
use ar_sandbox::{Sandbox, SandboxCommand, SandboxError, SandboxOutput};
use async_trait::async_trait;
use futures::future::join_all;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tool {tool} exited {code}: {stderr}")]
    NonZeroExit {
        tool: String,
        code: i32,
        stderr: String,
    },
    #[error("parse error in {tool} output: {detail}")]
    Parse { tool: String, detail: String },
    #[error("sandbox: {0}")]
    Sandbox(String),
}

/// One linter runner.
///
/// Implementations:
/// 1. Decide if there's anything to scan (based on the runner's own state).
/// 2. Build a [`SandboxCommand`] and dispatch through the supplied
///    [`Sandbox`] — never spawn `tokio::process::Command` directly. The
///    sandbox is responsible for whatever isolation is appropriate
///    ([`ar_sandbox::DirectSandbox`] for tests/dev,
///    [`ar_sandbox::PodmanSandbox`] for production).
/// 3. Parse stdout into [`Finding`] structs.
///
/// Implementations must be tolerant of the linter not being installed
/// (returning `Ok(vec![])` rather than `Err`) so a missing optional tool
/// doesn't fail the whole review. The [`run_in_sandbox`] helper handles
/// that case uniformly.
#[async_trait]
pub trait LinterRunner: Send + Sync {
    fn name(&self) -> &str;
    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError>;
}

/// Convenience wrapper around [`Sandbox::run`] that swallows the
/// "binary not installed" / "sandbox runtime missing" cases as
/// `Ok(empty_output)` — the runners' own "no stdout → no findings"
/// branch then takes over. Other sandbox errors propagate as
/// [`RunnerError::Sandbox`].
pub async fn run_in_sandbox(
    sandbox: &dyn Sandbox,
    repo_dir: &Path,
    program: &str,
    args: Vec<String>,
    env: Vec<(String, String)>,
) -> Result<SandboxOutput, RunnerError> {
    let cmd = SandboxCommand {
        program: program.into(),
        args,
        working_dir: repo_dir.into(),
        env,
    };
    match sandbox.run(&cmd).await {
        Ok(out) => Ok(out),
        Err(SandboxError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(empty_output()),
        Err(SandboxError::RuntimeMissing(_)) => Ok(empty_output()),
        Err(e) => Err(RunnerError::Sandbox(e.to_string())),
    }
}

fn empty_output() -> SandboxOutput {
    SandboxOutput {
        exit_code: Some(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}

/// Run a set of linters in parallel against the same working tree, collect
/// findings, and discard runners whose binary is missing. Other runner
/// errors are logged but don't abort the batch.
pub async fn run_all(
    runners: &[Box<dyn LinterRunner>],
    sandbox: &dyn Sandbox,
    repo_dir: &Path,
) -> Vec<Finding> {
    let futures = runners.iter().map(|r| async move {
        match r.run(sandbox, repo_dir).await {
            Ok(findings) => findings,
            Err(e) => {
                tracing::warn!(tool = r.name(), error = %e, "linter failed; ignoring");
                Vec::new()
            }
        }
    });
    join_all(futures).await.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::Severity;
    use ar_sandbox::DirectSandbox;

    struct StaticRunner {
        name: &'static str,
        findings: Vec<Finding>,
        fail: bool,
    }

    #[async_trait]
    impl LinterRunner for StaticRunner {
        fn name(&self) -> &str {
            self.name
        }
        async fn run(
            &self,
            _sandbox: &dyn Sandbox,
            _repo_dir: &Path,
        ) -> Result<Vec<Finding>, RunnerError> {
            if self.fail {
                return Err(RunnerError::Parse {
                    tool: self.name.into(),
                    detail: "scripted failure".into(),
                });
            }
            Ok(self.findings.clone())
        }
    }

    fn finding(tool: &str) -> Finding {
        Finding {
            source_tool: tool.into(),
            rule_id: None,
            path: "x".into(),
            line_start: 1,
            line_end: 1,
            severity: Severity::Warning,
            message: "m".into(),
        }
    }

    #[tokio::test]
    async fn run_all_aggregates_findings_from_every_runner() {
        let runners: Vec<Box<dyn LinterRunner>> = vec![
            Box::new(StaticRunner {
                name: "a",
                findings: vec![finding("a")],
                fail: false,
            }),
            Box::new(StaticRunner {
                name: "b",
                findings: vec![finding("b"), finding("b")],
                fail: false,
            }),
        ];
        let sandbox = DirectSandbox::new();
        let all = run_all(&runners, &sandbox, Path::new("/tmp")).await;
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn run_all_swallows_errors_from_individual_runners() {
        let runners: Vec<Box<dyn LinterRunner>> = vec![
            Box::new(StaticRunner {
                name: "a",
                findings: vec![finding("a")],
                fail: false,
            }),
            Box::new(StaticRunner {
                name: "b",
                findings: vec![],
                fail: true,
            }),
        ];
        let sandbox = DirectSandbox::new();
        let all = run_all(&runners, &sandbox, Path::new("/tmp")).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].source_tool, "a");
    }

    #[tokio::test]
    async fn run_in_sandbox_swallows_missing_binary_as_empty_output() {
        let sandbox = DirectSandbox::new();
        let cwd = std::env::current_dir().unwrap();
        let out = run_in_sandbox(
            &sandbox,
            &cwd,
            "definitely-not-a-real-binary-zzz",
            vec![],
            vec![],
        )
        .await
        .expect("must not error on missing binary");
        assert!(out.stdout.is_empty());
    }
}
