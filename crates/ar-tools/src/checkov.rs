//! checkov runner. Parses `checkov -o json` output for
//! infrastructure-as-code misconfigurations.
//!
//! checkov covers Terraform, CloudFormation, Kubernetes, Helm,
//! Serverless, ARM, Bicep, and a few others. Trivy already catches
//! some of this surface, but checkov's rule library is broader for
//! Terraform specifically and uses different signatures, so running
//! both is worthwhile in a Terraform-heavy repo.
//!
//! Output structure: a top-level object with `results.failed_checks`
//! (an array of `{check_id, check_name, file_path, file_line_range:
//! [start, end], severity, …}`). Some invocations emit a top-level
//! array of multi-framework reports — we handle both shapes by
//! tolerating either as input.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "checkov";

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CheckovOutput {
    /// Single-framework run: `{ "results": { "failed_checks": [...] } }`.
    Single { results: CheckovResults },
    /// Multi-framework run: array of `{ "results": ... }`.
    Multi(Vec<CheckovReport>),
}

#[derive(Debug, Deserialize)]
struct CheckovReport {
    #[serde(default)]
    results: CheckovResults,
}

#[derive(Debug, Default, Deserialize)]
struct CheckovResults {
    #[serde(default)]
    failed_checks: Vec<FailedCheck>,
}

#[derive(Debug, Deserialize)]
struct FailedCheck {
    check_id: String,
    #[serde(default)]
    check_name: String,
    file_path: String,
    /// `[start, end]`. Either or both can be 0 when checkov can't
    /// resolve a line; we coerce to 1.
    #[serde(default)]
    file_line_range: Vec<u32>,
    #[serde(default)]
    severity: Option<String>,
}

pub fn parse_checkov_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: CheckovOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    match raw {
        CheckovOutput::Single { results } => collect(&mut out, results.failed_checks),
        CheckovOutput::Multi(reports) => {
            for report in reports {
                collect(&mut out, report.results.failed_checks);
            }
        }
    }
    Ok(out)
}

fn collect(out: &mut Vec<Finding>, checks: Vec<FailedCheck>) {
    for c in checks {
        let (start, end) = match c.file_line_range.as_slice() {
            [s, e] => (sanitize_line(*s), sanitize_line(*e)),
            [s] => {
                let s = sanitize_line(*s);
                (s, s)
            }
            _ => (1, 1),
        };
        let mut path = c.file_path;
        if let Some(stripped) = path.strip_prefix('/') {
            path = stripped.to_string();
        }
        let message = if c.check_name.is_empty() {
            format!("{}: failed", c.check_id)
        } else {
            format!("{}: {}", c.check_id, c.check_name)
        };
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: Some(c.check_id),
            path,
            line_start: start,
            line_end: end,
            severity: severity_from(c.severity.as_deref()),
            message,
        });
    }
}

fn sanitize_line(line: u32) -> u32 {
    line.max(1)
}

fn severity_from(level: Option<&str>) -> Severity {
    match level.unwrap_or_default().to_ascii_uppercase().as_str() {
        "CRITICAL" | "HIGH" => Severity::Error,
        "MEDIUM" | "LOW" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct CheckovRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for CheckovRunner {
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
        // -d takes a directory; we use --file repeatedly to scan only
        // the changed files. --quiet suppresses the human banner.
        // --soft-fail keeps the exit code at 0 so we can read JSON
        // from stdout.
        let mut args = vec![
            "-o".into(),
            "json".into(),
            "--quiet".into(),
            "--soft-fail".into(),
        ];
        for file in &self.files {
            args.push("--file".into());
            args.push(file.clone());
        }
        let output = run_in_sandbox(sandbox, repo_dir, "checkov", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_checkov_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_framework_output() {
        let json = r#"{
            "check_type": "terraform",
            "results": {
                "failed_checks": [
                    {
                        "check_id": "CKV_AWS_20",
                        "check_name": "S3 Bucket has an ACL defined which allows public READ access",
                        "file_path": "/main.tf",
                        "file_line_range": [3, 10],
                        "resource": "aws_s3_bucket.public",
                        "severity": "HIGH"
                    },
                    {
                        "check_id": "CKV_AWS_21",
                        "check_name": "Versioning enabled",
                        "file_path": "/main.tf",
                        "file_line_range": [12, 18],
                        "severity": "LOW"
                    }
                ]
            }
        }"#;
        let f = parse_checkov_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("CKV_AWS_20"));
        assert_eq!(f[0].path, "main.tf"); // leading / stripped
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].line_end, 10);
        assert_eq!(f[0].severity, Severity::Error); // HIGH
        assert_eq!(f[1].severity, Severity::Warning); // LOW
    }

    #[test]
    fn parses_multi_framework_output() {
        let json = r#"[
            {"check_type":"terraform","results":{"failed_checks":[
                {"check_id":"CKV_TF_1","check_name":"x","file_path":"a.tf",
                 "file_line_range":[1,1]}
            ]}},
            {"check_type":"kubernetes","results":{"failed_checks":[
                {"check_id":"CKV_K8S_1","check_name":"y","file_path":"b.yaml",
                 "file_line_range":[5,7],"severity":"CRITICAL"}
            ]}}
        ]"#;
        let f = parse_checkov_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("CKV_TF_1"));
        assert_eq!(f[1].rule_id.as_deref(), Some("CKV_K8S_1"));
        assert_eq!(f[1].severity, Severity::Error); // CRITICAL
    }

    #[test]
    fn empty_failed_checks_yields_zero_findings() {
        let json = r#"{"check_type":"terraform","results":{"failed_checks":[]}}"#;
        let f = parse_checkov_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_checkov_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn zero_or_missing_lines_coerce_to_one() {
        let json = r#"{
            "results": {
                "failed_checks": [
                    {"check_id":"X","check_name":"y","file_path":"a.tf",
                     "file_line_range":[0,0]},
                    {"check_id":"Y","check_name":"z","file_path":"b.tf"}
                ]
            }
        }"#;
        let f = parse_checkov_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
        assert_eq!(f[1].line_start, 1);
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"{
            "results":{"failed_checks":[
                {"check_id":"X","check_name":"y","file_path":"a.tf",
                 "file_line_range":[1,1],"severity":"INFORMATIONAL"}
            ]}
        }"#;
        let f = parse_checkov_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn empty_check_name_falls_back_to_id_in_message() {
        let json = r#"{
            "results":{"failed_checks":[
                {"check_id":"CKV_X","check_name":"","file_path":"a.tf",
                 "file_line_range":[1,1]}
            ]}
        }"#;
        let f = parse_checkov_output(json).expect("ok");
        assert!(f[0].message.contains("CKV_X"));
    }
}
