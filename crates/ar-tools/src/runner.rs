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
    #[error("skipped: {0}")]
    Skipped(String),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinterRun {
    pub name: String,
    pub status: LinterRunStatus,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinterRunStatus {
    Ok,
    Skipped(String),
    Failed(String),
}

/// Convenience wrapper around [`Sandbox::run`] that reports the
/// "binary not installed" / "sandbox runtime missing" cases as
/// [`RunnerError::Skipped`]. `run_all` preserves the historical behavior
/// by swallowing skips; `run_all_with_status` surfaces them in review-body
/// linter summaries. Other sandbox errors propagate as [`RunnerError::Sandbox`].
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
        Ok(out) if out.exit_code == Some(127) => Err(RunnerError::Skipped(format!(
            "{program} not found in sandbox image"
        ))),
        Ok(out) => Ok(out),
        Err(SandboxError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(RunnerError::Skipped(format!("{program} not on PATH")))
        }
        Err(SandboxError::RuntimeMissing(e)) => Err(RunnerError::Skipped(e.to_string())),
        Err(e) => Err(RunnerError::Sandbox(e.to_string())),
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
            Err(RunnerError::Skipped(_)) => Vec::new(),
            Err(e) => {
                tracing::warn!(tool = r.name(), error = %e, "linter failed; ignoring");
                Vec::new()
            }
        }
    });
    join_all(futures).await.into_iter().flatten().collect()
}

/// Run linters and retain one execution summary per runner for review-body
/// transparency. Findings remain available by flattening successful runs.
pub async fn run_all_with_status(
    runners: &[Box<dyn LinterRunner>],
    sandbox: &dyn Sandbox,
    repo_dir: &Path,
) -> Vec<LinterRun> {
    let futures = runners.iter().map(|r| async move {
        match r.run(sandbox, repo_dir).await {
            Ok(findings) => LinterRun {
                name: r.name().to_string(),
                status: LinterRunStatus::Ok,
                findings,
            },
            Err(RunnerError::Skipped(reason)) => LinterRun {
                name: r.name().to_string(),
                status: LinterRunStatus::Skipped(reason),
                findings: Vec::new(),
            },
            Err(e) => {
                tracing::warn!(tool = r.name(), error = %e, "linter failed; ignoring");
                LinterRun {
                    name: r.name().to_string(),
                    status: LinterRunStatus::Failed(e.to_string()),
                    findings: Vec::new(),
                }
            }
        }
    });
    join_all(futures).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::Severity;
    use ar_sandbox::{DirectSandbox, SandboxCommand, SandboxError, SandboxOutput};

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

    struct MissingBinaryRunner;

    #[async_trait]
    impl LinterRunner for MissingBinaryRunner {
        fn name(&self) -> &str {
            "missing-tool"
        }

        async fn run(
            &self,
            sandbox: &dyn Sandbox,
            repo_dir: &Path,
        ) -> Result<Vec<Finding>, RunnerError> {
            run_in_sandbox(
                sandbox,
                repo_dir,
                "definitely-not-a-real-binary-zzz",
                vec![],
                vec![],
            )
            .await?;
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn run_all_with_status_marks_missing_binary_as_skipped() {
        let runners: Vec<Box<dyn LinterRunner>> = vec![Box::new(MissingBinaryRunner)];
        let sandbox = DirectSandbox::new();
        let cwd = std::env::current_dir().unwrap();

        let runs = run_all_with_status(&runners, &sandbox, &cwd).await;

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "missing-tool");
        assert!(matches!(runs[0].status, LinterRunStatus::Skipped(_)));
        assert!(runs[0].findings.is_empty());
    }

    struct Exit127Sandbox;

    #[async_trait]
    impl Sandbox for Exit127Sandbox {
        async fn run(&self, _cmd: &SandboxCommand) -> Result<SandboxOutput, SandboxError> {
            Ok(SandboxOutput {
                exit_code: Some(127),
                stdout: Vec::new(),
                stderr: b"not found".to_vec(),
            })
        }
    }

    #[tokio::test]
    async fn run_all_with_status_marks_sandbox_exit_127_as_skipped() {
        let runners: Vec<Box<dyn LinterRunner>> = vec![Box::new(MissingBinaryRunner)];
        let cwd = std::env::current_dir().unwrap();

        let runs = run_all_with_status(&runners, &Exit127Sandbox, &cwd).await;

        assert_eq!(runs.len(), 1);
        assert!(matches!(runs[0].status, LinterRunStatus::Skipped(_)));
        assert!(runs[0].findings.is_empty());
    }

    #[tokio::test]
    async fn run_in_sandbox_reports_missing_binary_as_skipped() {
        let sandbox = DirectSandbox::new();
        let cwd = std::env::current_dir().unwrap();
        let err = run_in_sandbox(
            &sandbox,
            &cwd,
            "definitely-not-a-real-binary-zzz",
            vec![],
            vec![],
        )
        .await
        .expect_err("missing binary should be a skipped linter");
        assert!(matches!(err, RunnerError::Skipped(_)));
    }
}
