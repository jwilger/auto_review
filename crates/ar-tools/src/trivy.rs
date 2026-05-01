//! Trivy runner. Parses `trivy fs --format json` output to surface:
//! - Vulnerabilities (CVEs in declared dependencies, e.g. Cargo.lock,
//!   package-lock.json, Gemfile.lock)
//! - Misconfigurations (Dockerfile / k8s / Terraform issues)
//! - Secrets (overlap with gitleaks, but Trivy's matchers differ;
//!   surfacing both gives belt-and-braces coverage)
//!
//! Vulnerabilities don't carry source-line info — Trivy reports them
//! at the manifest file. We surface them at line 1 of the target with
//! the package name in the rule_id.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "trivy";

#[derive(Debug, Deserialize)]
struct TrivyOutput {
    #[serde(default, rename = "Results")]
    results: Vec<TrivyResult>,
}

#[derive(Debug, Deserialize)]
struct TrivyResult {
    #[serde(rename = "Target")]
    target: String,
    #[serde(default, rename = "Vulnerabilities")]
    vulnerabilities: Vec<Vulnerability>,
    #[serde(default, rename = "Misconfigurations")]
    misconfigurations: Vec<Misconfiguration>,
    #[serde(default, rename = "Secrets")]
    secrets: Vec<Secret>,
}

#[derive(Debug, Deserialize)]
struct Vulnerability {
    #[serde(rename = "VulnerabilityID")]
    id: String,
    #[serde(rename = "PkgName")]
    pkg_name: String,
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(default, rename = "Title")]
    title: String,
}

#[derive(Debug, Deserialize)]
struct Misconfiguration {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "Title")]
    title: String,
    #[serde(default, rename = "Description")]
    description: String,
    #[serde(default, rename = "CauseMetadata")]
    cause: Option<CauseMetadata>,
}

#[derive(Debug, Deserialize)]
struct CauseMetadata {
    #[serde(default, rename = "StartLine")]
    start_line: Option<u32>,
    #[serde(default, rename = "EndLine")]
    end_line: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct Secret {
    #[serde(rename = "RuleID")]
    rule_id: String,
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "Title")]
    title: String,
    #[serde(default, rename = "StartLine")]
    start_line: u32,
    #[serde(default, rename = "EndLine")]
    end_line: Option<u32>,
}

pub fn parse_trivy_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: TrivyOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for result in raw.results {
        for v in result.vulnerabilities {
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: Some(format!("{}:{}", v.pkg_name, v.id)),
                path: result.target.clone(),
                line_start: 1,
                line_end: 1,
                severity: severity_from(&v.severity),
                message: if v.title.is_empty() {
                    format!("{} in {}", v.id, v.pkg_name)
                } else {
                    format!("{}: {}", v.id, v.title)
                },
            });
        }
        for m in result.misconfigurations {
            let (start, end) = match m.cause {
                Some(c) => (
                    c.start_line.unwrap_or(1),
                    c.end_line.unwrap_or_else(|| c.start_line.unwrap_or(1)),
                ),
                None => (1, 1),
            };
            let message = if m.description.is_empty() {
                m.title
            } else {
                format!("{}: {}", m.title, m.description)
            };
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: Some(m.id),
                path: result.target.clone(),
                line_start: start,
                line_end: end,
                severity: severity_from(&m.severity),
                message,
            });
        }
        for s in result.secrets {
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: Some(s.rule_id),
                path: result.target.clone(),
                line_start: s.start_line.max(1),
                line_end: s.end_line.unwrap_or(s.start_line).max(1),
                severity: severity_from(&s.severity),
                message: format!("Possible secret: {}", s.title),
            });
        }
    }
    Ok(out)
}

fn severity_from(level: &str) -> Severity {
    // Trivy uses CRITICAL / HIGH / MEDIUM / LOW / UNKNOWN for CVEs,
    // CRITICAL / HIGH / MEDIUM / LOW / UNKNOWN for misconfigs too.
    match level {
        "CRITICAL" | "HIGH" => Severity::Error,
        "MEDIUM" | "WARNING" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct TrivyRunner;

#[async_trait]
impl LinterRunner for TrivyRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "trivy",
            vec![
                "fs".into(),
                "--format=json".into(),
                "--severity=MEDIUM,HIGH,CRITICAL".into(),
                "--quiet".into(),
                ".".into(),
            ],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_trivy_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vulnerabilities() {
        let json = r#"{
            "Results": [{
                "Target": "Cargo.lock",
                "Vulnerabilities": [
                    {
                        "VulnerabilityID": "CVE-2024-12345",
                        "PkgName": "shaky-lib",
                        "Severity": "CRITICAL",
                        "Title": "Buffer overflow in shaky-lib"
                    }
                ]
            }]
        }"#;
        let f = parse_trivy_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "Cargo.lock");
        assert_eq!(f[0].rule_id.as_deref(), Some("shaky-lib:CVE-2024-12345"));
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("Buffer overflow"));
    }

    #[test]
    fn parses_misconfigurations_with_line_metadata() {
        let json = r#"{
            "Results": [{
                "Target": "Dockerfile",
                "Misconfigurations": [
                    {
                        "ID": "DS001",
                        "Title": "Image user is root",
                        "Description": "Specify a USER instruction",
                        "Severity": "MEDIUM",
                        "CauseMetadata": {"StartLine": 7, "EndLine": 7}
                    }
                ]
            }]
        }"#;
        let f = parse_trivy_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id.as_deref(), Some("DS001"));
        assert_eq!(f[0].line_start, 7);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("Specify a USER"));
    }

    #[test]
    fn parses_secrets_with_line_range() {
        let json = r#"{
            "Results": [{
                "Target": "config/.env",
                "Secrets": [
                    {
                        "RuleID": "aws-access-key-id",
                        "Severity": "CRITICAL",
                        "Title": "AWS Access Key ID",
                        "StartLine": 3,
                        "EndLine": 3
                    }
                ]
            }]
        }"#;
        let f = parse_trivy_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id.as_deref(), Some("aws-access-key-id"));
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("Possible secret"));
    }

    #[test]
    fn empty_results_yields_zero_findings() {
        let json = r#"{"Results":[]}"#;
        let f = parse_trivy_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_results_decodes_to_empty() {
        let json = r#"{}"#;
        let f = parse_trivy_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_trivy_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn low_severity_falls_back_to_note() {
        let json = r#"{
            "Results": [{
                "Target": "Cargo.lock",
                "Vulnerabilities": [{
                    "VulnerabilityID": "CVE-X",
                    "PkgName": "lib",
                    "Severity": "LOW",
                    "Title": "minor"
                }]
            }]
        }"#;
        let f = parse_trivy_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }
}
