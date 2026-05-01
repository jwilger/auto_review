//! osv-scanner runner. Parses `osv-scanner --format json` output.
//!
//! osv-scanner queries Google's OSV database for known
//! vulnerabilities in declared dependencies. Trivy already covers
//! similar ground but uses different feeds; running both surfaces
//! CVEs either DB has indexed. Output structure: a top-level
//! `results` array, each entry carrying a `packages[]` array with
//! `{package: {name, ecosystem}, vulnerabilities: [{id, summary,
//! severity[]}]}`.
//!
//! Like trivy, osv-scanner findings don't carry source-line info —
//! we surface them at line 1 of the manifest file with the package
//! name in the rule_id.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "osv-scanner";

#[derive(Debug, Deserialize)]
struct OsvOutput {
    #[serde(default)]
    results: Vec<OsvResult>,
}

#[derive(Debug, Deserialize)]
struct OsvResult {
    #[serde(default)]
    source: Option<OsvSource>,
    #[serde(default)]
    packages: Vec<OsvPackage>,
}

#[derive(Debug, Deserialize)]
struct OsvSource {
    path: String,
}

#[derive(Debug, Deserialize)]
struct OsvPackage {
    package: OsvPackageInfo,
    #[serde(default)]
    vulnerabilities: Vec<OsvVulnerability>,
}

#[derive(Debug, Deserialize)]
struct OsvPackageInfo {
    name: String,
    #[serde(default)]
    ecosystem: String,
}

#[derive(Debug, Deserialize)]
struct OsvVulnerability {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    severity: Vec<OsvSeverity>,
}

#[derive(Debug, Deserialize)]
struct OsvSeverity {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    score: String,
}

pub fn parse_osv_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: OsvOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for result in raw.results {
        let path = result
            .source
            .map(|s| s.path)
            .unwrap_or_else(|| "(unknown manifest)".into());
        for pkg in result.packages {
            for vuln in pkg.vulnerabilities {
                let summary = if vuln.summary.is_empty() {
                    format!("{} affects {}", vuln.id, pkg.package.name)
                } else {
                    vuln.summary.clone()
                };
                out.push(Finding {
                    source_tool: TOOL.into(),
                    rule_id: Some(vuln.id),
                    path: path.clone(),
                    line_start: 1,
                    line_end: 1,
                    severity: severity_from(&vuln.severity),
                    message: format!(
                        "{} ({}/{}): {}",
                        summary, pkg.package.ecosystem, pkg.package.name, "see OSV.dev"
                    ),
                });
            }
        }
    }
    Ok(out)
}

/// OSV severity entries are CVSS strings (e.g. `CVSS_V3` →
/// `CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H`). We don't parse
/// the score; instead, presence of any CVSS_V3/V4 entry is treated
/// as a warning, no entry as a note. Critical/high distinction is
/// left to the LLM via the underlying CVE ID.
fn severity_from(entries: &[OsvSeverity]) -> Severity {
    if entries
        .iter()
        .any(|e| e.kind.starts_with("CVSS") && !e.score.is_empty())
    {
        Severity::Warning
    } else {
        Severity::Note
    }
}

pub struct OsvScannerRunner;

#[async_trait]
impl LinterRunner for OsvScannerRunner {
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
            "osv-scanner",
            vec![
                "scan".into(),
                "source".into(),
                "--format=json".into(),
                "--recursive".into(),
                ".".into(),
            ],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_osv_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_osv_output() {
        let json = r#"{
            "results": [{
                "source": {"path": "/repo/Cargo.lock", "type": "lockfile"},
                "packages": [
                    {
                        "package": {"name": "ring", "version": "0.16.20", "ecosystem": "crates.io"},
                        "vulnerabilities": [
                            {
                                "id": "GHSA-w457-6q6x-cgp9",
                                "summary": "AES key panic in ring",
                                "severity": [{"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H"}]
                            }
                        ]
                    },
                    {
                        "package": {"name": "untracked", "version": "1.0.0", "ecosystem": "crates.io"},
                        "vulnerabilities": [
                            {
                                "id": "RUSTSEC-2024-0001",
                                "summary": "",
                                "severity": []
                            }
                        ]
                    }
                ]
            }]
        }"#;
        let f = parse_osv_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("GHSA-w457-6q6x-cgp9"));
        assert_eq!(f[0].path, "/repo/Cargo.lock");
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("AES key panic"));
        assert!(f[0].message.contains("crates.io/ring"));
        // No severity entry → note
        assert_eq!(f[1].severity, Severity::Note);
        // Empty summary fallback
        assert!(f[1].message.contains("untracked"));
    }

    #[test]
    fn no_results_yields_zero_findings() {
        let json = r#"{"results":[]}"#;
        let f = parse_osv_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_results_field_decodes_to_empty() {
        let json = r#"{}"#;
        let f = parse_osv_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_osv_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_source_falls_back_to_unknown_path() {
        let json = r#"{
            "results": [{
                "packages": [{
                    "package": {"name": "x", "version": "1", "ecosystem": "npm"},
                    "vulnerabilities": [
                        {"id": "GHSA-1", "summary": "s", "severity": []}
                    ]
                }]
            }]
        }"#;
        let f = parse_osv_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "(unknown manifest)");
    }

    #[test]
    fn multiple_packages_in_one_result_each_emit_findings() {
        let json = r#"{
            "results": [{
                "source": {"path": "package-lock.json", "type": "lockfile"},
                "packages": [
                    {
                        "package": {"name": "a", "version": "1", "ecosystem": "npm"},
                        "vulnerabilities": [{"id":"X1","summary":"","severity":[]}]
                    },
                    {
                        "package": {"name": "b", "version": "2", "ecosystem": "npm"},
                        "vulnerabilities": [
                            {"id":"X2","summary":"","severity":[]},
                            {"id":"X3","summary":"","severity":[]}
                        ]
                    }
                ]
            }]
        }"#;
        let f = parse_osv_output(json).expect("ok");
        assert_eq!(f.len(), 3);
        assert_eq!(
            f.iter()
                .map(|f| f.rule_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["X1", "X2", "X3"]
        );
    }
}
