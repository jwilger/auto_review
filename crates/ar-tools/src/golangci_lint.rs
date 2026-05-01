//! golangci-lint runner. Parses
//! `golangci-lint run --out-format json --issues-exit-code 0` output.
//!
//! golangci-lint is the standard Go aggregator linter — wraps a couple
//! dozen Go linters (errcheck, govet, staticcheck, etc.) under one
//! roof. Output structure: one Issue per finding with FromLinter +
//! Text + Pos {Filename, Line, Column} + Severity.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "golangci-lint";

#[derive(Debug, Deserialize)]
struct GolangciOutput {
    #[serde(default, rename = "Issues")]
    issues: Vec<Issue>,
}

#[derive(Debug, Deserialize)]
struct Issue {
    #[serde(rename = "FromLinter")]
    from_linter: String,
    #[serde(rename = "Text")]
    text: String,
    #[serde(rename = "Pos")]
    pos: Pos,
    #[serde(default, rename = "Severity")]
    severity: String,
}

#[derive(Debug, Deserialize)]
struct Pos {
    #[serde(rename = "Filename")]
    filename: String,
    #[serde(rename = "Line")]
    line: u32,
}

pub fn parse_golangci_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: GolangciOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .issues
        .into_iter()
        .map(|i| Finding {
            source_tool: TOOL.into(),
            rule_id: Some(i.from_linter),
            path: i.pos.filename,
            line_start: i.pos.line,
            line_end: i.pos.line,
            severity: severity_from(&i.severity),
            message: i.text,
        })
        .collect())
}

fn severity_from(level: &str) -> Severity {
    // golangci-lint default severity is empty string for most issues;
    // some configs set "error" / "warning" / "info" / etc.
    let lower = level.to_ascii_lowercase();
    match lower.as_str() {
        "error" => Severity::Error,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct GolangciLintRunner;

#[async_trait]
impl LinterRunner for GolangciLintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // --issues-exit-code=0: don't exit non-zero when issues are
        // found (we want the JSON, not a process error).
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "golangci-lint",
            vec![
                "run".into(),
                "--out-format=json".into(),
                "--issues-exit-code=0".into(),
            ],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_golangci_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_output_with_severity() {
        let json = r#"{
            "Issues": [
                {
                    "FromLinter": "errcheck",
                    "Text": "Error return value not checked",
                    "Pos": {"Filename": "main.go", "Line": 12, "Column": 5},
                    "Severity": "error"
                },
                {
                    "FromLinter": "staticcheck",
                    "Text": "should use a different approach",
                    "Pos": {"Filename": "lib.go", "Line": 7, "Column": 1},
                    "Severity": "warning"
                }
            ]
        }"#;
        let f = parse_golangci_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("errcheck"));
        assert_eq!(f[0].path, "main.go");
        assert_eq!(f[0].line_start, 12);
        assert_eq!(f[0].severity, Severity::Error);
        assert_eq!(f[1].severity, Severity::Warning);
    }

    #[test]
    fn empty_severity_falls_back_to_note() {
        let json = r#"{
            "Issues": [{
                "FromLinter": "govet",
                "Text": "...",
                "Pos": {"Filename": "x.go", "Line": 1, "Column": 1},
                "Severity": ""
            }]
        }"#;
        let f = parse_golangci_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_severity_defaults_to_empty_string_then_note() {
        let json = r#"{
            "Issues": [{
                "FromLinter": "govet",
                "Text": "...",
                "Pos": {"Filename": "x.go", "Line": 1, "Column": 1}
            }]
        }"#;
        let f = parse_golangci_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn empty_issues_yields_zero_findings() {
        let json = r#"{"Issues":[]}"#;
        let f = parse_golangci_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_issues_field_decodes_to_empty() {
        let json = r#"{}"#;
        let f = parse_golangci_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_golangci_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }
}
