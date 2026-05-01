//! bandit runner. Parses `bandit -r . -f json` output for Python
//! security issues.
//!
//! bandit is a Python-specific security linter — distinct from ruff
//! (general lint) and mypy (type-check). It catches dynamic-code
//! execution, deserialisation of untrusted data, hardcoded passwords,
//! weak crypto, unsafe `subprocess` shell=True, etc. Routes alongside
//! ruff/mypy on `.py` files.
//!
//! Output structure: `{results: [{filename, line_number, test_id,
//! test_name, issue_severity, issue_text}]}`. Severity maps
//! HIGH→Error, MEDIUM→Warning, LOW→Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "bandit";

#[derive(Debug, Deserialize)]
struct BanditOutput {
    #[serde(default)]
    results: Vec<BanditResult>,
}

#[derive(Debug, Deserialize)]
struct BanditResult {
    filename: String,
    line_number: u32,
    #[serde(default)]
    test_id: Option<String>,
    #[serde(default)]
    issue_severity: String,
    #[serde(default)]
    issue_text: String,
}

pub fn parse_bandit_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: BanditOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .results
        .into_iter()
        .map(|r| Finding {
            source_tool: TOOL.into(),
            rule_id: r.test_id,
            path: r.filename,
            line_start: r.line_number.max(1),
            line_end: r.line_number.max(1),
            severity: severity_from(&r.issue_severity),
            message: r.issue_text,
        })
        .collect())
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_uppercase().as_str() {
        "HIGH" | "CRITICAL" => Severity::Error,
        "MEDIUM" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct BanditRunner;

#[async_trait]
impl LinterRunner for BanditRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // -r .       recursive scan from cwd
        // -f json    structured output
        // -q         suppress the human banner so JSON stands alone
        // --exit-zero treat findings as 0-exit so we can read stdout
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "bandit",
            vec![
                "-r".into(),
                ".".into(),
                "-f".into(),
                "json".into(),
                "-q".into(),
                "--exit-zero".into(),
            ],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_bandit_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_bandit_output() {
        let json = r#"{
            "errors": [],
            "metrics": {},
            "results": [
                {
                    "filename": "src/api.py",
                    "line_number": 17,
                    "test_id": "B102",
                    "test_name": "exec_used",
                    "issue_severity": "HIGH",
                    "issue_confidence": "HIGH",
                    "issue_text": "Dynamic code execution detected."
                },
                {
                    "filename": "src/util.py",
                    "line_number": 4,
                    "test_id": "B404",
                    "test_name": "blacklist",
                    "issue_severity": "LOW",
                    "issue_confidence": "HIGH",
                    "issue_text": "Consider possible security implications associated with subprocess."
                }
            ]
        }"#;
        let f = parse_bandit_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "src/api.py");
        assert_eq!(f[0].line_start, 17);
        assert_eq!(f[0].rule_id.as_deref(), Some("B102"));
        assert_eq!(f[0].severity, Severity::Error); // HIGH
        assert!(f[0].message.contains("Dynamic"));
        assert_eq!(f[1].severity, Severity::Note); // LOW
    }

    #[test]
    fn medium_severity_maps_to_warning() {
        let json = r#"{
            "results": [{
                "filename":"x.py","line_number":1,"test_id":"B999",
                "issue_severity":"MEDIUM","issue_text":"m"
            }]
        }"#;
        let f = parse_bandit_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Warning);
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"{
            "results": [{
                "filename":"x.py","line_number":1,"test_id":"B1",
                "issue_severity":"INFO","issue_text":"m"
            }]
        }"#;
        let f = parse_bandit_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_test_id_drops_rule_id() {
        let json = r#"{
            "results": [{
                "filename":"x.py","line_number":1,
                "issue_severity":"HIGH","issue_text":"m"
            }]
        }"#;
        let f = parse_bandit_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn empty_results_yields_zero_findings() {
        let f = parse_bandit_output(r#"{"results":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_results_field_decodes_to_empty() {
        let f = parse_bandit_output("{}").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_bandit_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"{
            "results": [{
                "filename":"x.py","line_number":0,"test_id":"B1",
                "issue_severity":"HIGH","issue_text":"m"
            }]
        }"#;
        let f = parse_bandit_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }
}
