//! oxlint runner. Parses `oxlint --format=json` output.
//!
//! oxlint is a JS/TS linter written in Rust — a JS-ecosystem
//! complement to biome and eslint. We run it alongside the others
//! because each catches a different rule set in practice; users
//! disabling overlap via `.auto_review.yaml`'s `disabled_tools`
//! is the supported escape hatch.
//!
//! Output structure: a top-level `{diagnostics: [...]}` array
//! (closely modelled on biome's). Each diagnostic carries
//! `{severity, message, labels: [{filename, span: {start, end}}]}`
//! where the location lives inside the first label.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "oxlint";

#[derive(Debug, Deserialize)]
struct OxlintOutput {
    #[serde(default)]
    diagnostics: Vec<OxlintDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct OxlintDiagnostic {
    #[serde(default)]
    severity: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    labels: Vec<OxlintLabel>,
}

#[derive(Debug, Deserialize)]
struct OxlintLabel {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    span: Option<OxlintSpan>,
}

#[derive(Debug, Deserialize, Default)]
struct OxlintSpan {
    #[serde(default)]
    start: Option<OxlintPosition>,
    #[serde(default)]
    end: Option<OxlintPosition>,
}

#[derive(Debug, Deserialize, Default)]
struct OxlintPosition {
    #[serde(default)]
    line: Option<u32>,
}

pub fn parse_oxlint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: OxlintOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .diagnostics
        .into_iter()
        .map(|d| {
            let label = d.labels.into_iter().next();
            let path = label
                .as_ref()
                .map(|l| l.filename.clone())
                .unwrap_or_default();
            let start_line = label
                .as_ref()
                .and_then(|l| l.span.as_ref())
                .and_then(|s| s.start.as_ref())
                .and_then(|p| p.line)
                .unwrap_or(1);
            let end_line = label
                .as_ref()
                .and_then(|l| l.span.as_ref())
                .and_then(|s| s.end.as_ref())
                .and_then(|p| p.line)
                .unwrap_or(start_line);
            Finding {
                source_tool: TOOL.into(),
                rule_id: d.code,
                path,
                line_start: start_line,
                line_end: end_line,
                severity: severity_from(&d.severity),
                message: d.message,
            }
        })
        .collect())
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_lowercase().as_str() {
        "error" => Severity::Error,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct OxlintRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for OxlintRunner {
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
        let mut args = vec!["--format=json".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "oxlint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_oxlint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_oxlint_output() {
        let json = r#"{
            "diagnostics": [
                {
                    "severity": "warning",
                    "message": "Unexpected console statement.",
                    "code": "no-console",
                    "labels": [{
                        "filename": "src/foo.ts",
                        "span": {
                            "start": {"line": 12, "column": 5},
                            "end": {"line": 12, "column": 30}
                        }
                    }]
                },
                {
                    "severity": "error",
                    "message": "Avoid 'any'.",
                    "code": "no-explicit-any",
                    "labels": [{
                        "filename": "src/bar.tsx",
                        "span": {
                            "start": {"line": 1, "column": 1},
                            "end": {"line": 3, "column": 1}
                        }
                    }]
                }
            ]
        }"#;
        let f = parse_oxlint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("no-console"));
        assert_eq!(f[0].path, "src/foo.ts");
        assert_eq!(f[0].line_start, 12);
        assert_eq!(f[0].line_end, 12);
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[1].severity, Severity::Error);
        assert_eq!(f[1].line_end, 3);
    }

    #[test]
    fn empty_diagnostics_yields_zero_findings() {
        let f = parse_oxlint_output(r#"{"diagnostics":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_diagnostics_field_decodes_to_empty() {
        let f = parse_oxlint_output("{}").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_oxlint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn diagnostic_without_labels_falls_back_to_line_one() {
        let json = r#"{
            "diagnostics": [{
                "severity": "warning",
                "message": "no location",
                "code": "x",
                "labels": []
            }]
        }"#;
        let f = parse_oxlint_output(json).expect("ok");
        assert_eq!(f[0].path, "");
        assert_eq!(f[0].line_start, 1);
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"{
            "diagnostics": [{
                "severity": "info",
                "message": "x",
                "labels": [{"filename":"a.js","span":{"start":{"line":1,"column":1},"end":{"line":1,"column":2}}}]
            }]
        }"#;
        let f = parse_oxlint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_end_position_mirrors_start() {
        let json = r#"{
            "diagnostics": [{
                "severity": "warning",
                "message": "x",
                "labels": [{"filename":"a.js","span":{"start":{"line":7}}}]
            }]
        }"#;
        let f = parse_oxlint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 7);
        assert_eq!(f[0].line_end, 7);
    }
}
