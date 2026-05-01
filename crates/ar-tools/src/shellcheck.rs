//! ShellCheck runner. Parses `shellcheck --format=json1` output.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "shellcheck";

#[derive(Debug, Deserialize)]
struct ShellCheckOutput {
    comments: Vec<ShellCheckComment>,
}

#[derive(Debug, Deserialize)]
struct ShellCheckComment {
    file: String,
    line: u32,
    #[serde(default, rename = "endLine")]
    end_line: Option<u32>,
    level: String,
    code: u32,
    message: String,
}

/// Map shellcheck `--format=json1` output to normalized findings.
pub fn parse_shellcheck_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: ShellCheckOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .comments
        .into_iter()
        .map(|c| Finding {
            source_tool: TOOL.into(),
            rule_id: Some(format!("SC{}", c.code)),
            path: c.file,
            line_start: c.line,
            line_end: c.end_line.unwrap_or(c.line),
            severity: severity_from(&c.level),
            message: c.message,
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

pub struct ShellCheckRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for ShellCheckRunner {
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
        let mut args = vec!["--format=json1".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "shellcheck", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_shellcheck_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_shellcheck_output() {
        let json = r#"{
            "comments": [
                {"file":"a.sh","line":3,"endLine":3,"column":1,"endColumn":14,
                 "level":"warning","code":2034,"message":"var appears unused"},
                {"file":"b.sh","line":7,"column":5,
                 "level":"error","code":1078,"message":"missing fi"}
            ]
        }"#;
        let f = parse_shellcheck_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("SC2034"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].line_end, 3);
        assert_eq!(f[1].severity, Severity::Error);
        assert_eq!(f[1].line_end, 7); // missing endLine → fallback to line
    }

    #[test]
    fn empty_comments_yields_zero_findings() {
        let f = parse_shellcheck_output(r#"{"comments":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn unrecognized_level_falls_back_to_note() {
        let json = r#"{"comments":[
            {"file":"a","line":1,"column":1,"level":"info","code":1,"message":"m"}
        ]}"#;
        let f = parse_shellcheck_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }
}
