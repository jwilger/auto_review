//! tflint runner. Parses `tflint --format=json` output for
//! Terraform-specific lint rules.
//!
//! tflint is distinct from checkov: checkov is a multi-framework
//! IaC scanner; tflint is Terraform-specific and supports provider
//! plugins (AWS/Azure/GCP) that catch resource-level mistakes
//! checkov doesn't see (e.g. invalid AMI ids, deprecated instance
//! types, naming-convention violations).
//!
//! Routes on `.tf` / `.tfvars` / `.hcl` alongside checkov.
//!
//! Output structure: `{issues: [{rule: {name, severity}, message,
//! range: {filename, start: {line}, end: {line}}}]}`. Severity
//! values: `error` → Error, `warning` → Warning, `notice`/`info`
//! → Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "tflint";

#[derive(Debug, Deserialize)]
struct TflintOutput {
    #[serde(default)]
    issues: Vec<TflintIssue>,
}

#[derive(Debug, Deserialize)]
struct TflintIssue {
    #[serde(default)]
    rule: TflintRule,
    #[serde(default)]
    message: String,
    #[serde(default)]
    range: TflintRange,
}

#[derive(Debug, Default, Deserialize)]
struct TflintRule {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    severity: String,
}

#[derive(Debug, Default, Deserialize)]
struct TflintRange {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    start: TflintPos,
    #[serde(default)]
    end: TflintPos,
}

#[derive(Debug, Default, Deserialize)]
struct TflintPos {
    #[serde(default)]
    line: u32,
}

pub fn parse_tflint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: TflintOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .issues
        .into_iter()
        .map(|i| {
            let start = i.range.start.line.max(1);
            let end = if i.range.end.line >= start {
                i.range.end.line
            } else {
                start
            };
            Finding {
                source_tool: TOOL.into(),
                rule_id: i.rule.name,
                path: i.range.filename,
                line_start: start,
                line_end: end,
                severity: severity_from(&i.rule.severity),
                message: i.message,
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

pub struct TflintRunner;

#[async_trait]
impl LinterRunner for TflintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // tflint scans the cwd as a Terraform module by default.
        // --format=json gives structured output. tflint exits
        // non-zero on findings; we read stdout regardless.
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "tflint",
            vec!["--format=json".into()],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_tflint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_tflint_output() {
        let json = r#"{
            "issues": [
                {
                    "rule": {
                        "name": "terraform_unused_declarations",
                        "severity": "warning",
                        "link": "..."
                    },
                    "message": "variable 'unused' is declared but not used",
                    "range": {
                        "filename": "variables.tf",
                        "start": {"line": 5, "column": 1},
                        "end": {"line": 5, "column": 30}
                    }
                },
                {
                    "rule": {
                        "name": "aws_instance_invalid_type",
                        "severity": "error"
                    },
                    "message": "'t1.tiny' is invalid",
                    "range": {
                        "filename": "main.tf",
                        "start": {"line": 12, "column": 1},
                        "end": {"line": 14, "column": 1}
                    }
                }
            ],
            "errors": []
        }"#;
        let f = parse_tflint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "variables.tf");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(
            f[0].rule_id.as_deref(),
            Some("terraform_unused_declarations")
        );
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[1].line_end, 14);
        assert_eq!(f[1].severity, Severity::Error);
    }

    #[test]
    fn notice_severity_falls_back_to_note() {
        let json = r#"{
            "issues":[{
                "rule":{"name":"r","severity":"notice"},
                "message":"m",
                "range":{"filename":"x.tf","start":{"line":1},"end":{"line":1}}
            }]
        }"#;
        let f = parse_tflint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn empty_issues_yields_zero_findings() {
        let f = parse_tflint_output(r#"{"issues":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_issues_field_decodes_to_empty() {
        let f = parse_tflint_output("{}").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_tflint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_rule_name_drops_rule_id() {
        let json = r#"{
            "issues":[{
                "rule":{"severity":"warning"},
                "message":"m",
                "range":{"filename":"x.tf","start":{"line":1},"end":{"line":1}}
            }]
        }"#;
        let f = parse_tflint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn missing_lines_falls_back_to_one() {
        let json = r#"{
            "issues":[{
                "rule":{"name":"r","severity":"warning"},
                "message":"m",
                "range":{"filename":"x.tf"}
            }]
        }"#;
        let f = parse_tflint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn end_line_below_start_is_clamped() {
        let json = r#"{
            "issues":[{
                "rule":{"name":"r","severity":"warning"},
                "message":"m",
                "range":{"filename":"x.tf","start":{"line":10},"end":{"line":3}}
            }]
        }"#;
        let f = parse_tflint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 10);
        assert_eq!(f[0].line_end, 10);
    }
}
