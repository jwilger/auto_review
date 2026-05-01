//! Hadolint Dockerfile linter runner. Parses `hadolint --format=json` output.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "hadolint";

#[derive(Debug, Deserialize)]
struct HadolintDiagnostic {
    line: u32,
    code: String,
    message: String,
    file: String,
    level: String,
}

pub fn parse_hadolint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<HadolintDiagnostic> =
        serde_json::from_str(json).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: e.to_string(),
        })?;
    Ok(raw
        .into_iter()
        .map(|d| Finding {
            source_tool: TOOL.into(),
            rule_id: Some(d.code),
            path: d.file,
            line_start: d.line,
            line_end: d.line,
            severity: severity_from(&d.level),
            message: d.message,
        })
        .collect())
}

fn severity_from(level: &str) -> Severity {
    match level {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct HadolintRunner {
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for HadolintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        let mut args = vec!["--format=json".into(), "--no-fail".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "hadolint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_hadolint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_hadolint_output() {
        let json = r#"[
            {"line":8,"code":"DL3008","message":"Pin versions in apt-get install",
             "column":1,"file":"Dockerfile","level":"warning"},
            {"line":1,"code":"DL3007","message":"Using latest is prone to errors",
             "column":1,"file":"Dockerfile","level":"info"}
        ]"#;
        let f = parse_hadolint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("DL3008"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[1].severity, Severity::Note);
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_hadolint_output("[]").expect("ok");
        assert!(f.is_empty());
    }
}
