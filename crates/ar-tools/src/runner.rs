use crate::finding::Finding;
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
}

/// One linter runner.
///
/// Implementations:
/// 1. Decide if there's anything to scan (`should_run`).
/// 2. Exec the linter binary against `repo_dir`.
/// 3. Parse stdout into [`Finding`] structs.
///
/// Implementations must be tolerant of the linter not being installed
/// (returning `Ok(vec![])` rather than `Err`) so a missing optional tool
/// doesn't fail the whole review.
#[async_trait]
pub trait LinterRunner: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError>;
}

/// Run a set of linters in parallel against the same working tree, collect
/// findings, and discard runners whose binary is missing. Other runner
/// errors are logged but don't abort the batch.
pub async fn run_all(runners: &[Box<dyn LinterRunner>], repo_dir: &Path) -> Vec<Finding> {
    let futures = runners.iter().map(|r| async move {
        match r.run(repo_dir).await {
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
        async fn run(&self, _repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
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
        let all = run_all(&runners, Path::new("/tmp")).await;
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
        let all = run_all(&runners, Path::new("/tmp")).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].source_tool, "a");
    }
}
