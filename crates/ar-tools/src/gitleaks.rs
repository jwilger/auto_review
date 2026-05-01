//! gitleaks runner. Parses `gitleaks detect --report-format json` output.
//!
//! gitleaks scans the whole working tree for committed secrets — API keys,
//! tokens, private keys, etc. We always run it when the repo is cloned;
//! secrets can live in any file, so there's no extension-based filter.

use crate::finding::{Finding, Severity};
use crate::runner::{LinterRunner, RunnerError};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;

const TOOL: &str = "gitleaks";

#[derive(Debug, Deserialize)]
struct GitleaksFinding {
    #[serde(rename = "Description")]
    description: String,
    #[serde(rename = "StartLine")]
    start_line: u32,
    #[serde(default, rename = "EndLine")]
    end_line: Option<u32>,
    #[serde(rename = "File")]
    file: String,
    #[serde(rename = "RuleID")]
    rule_id: String,
}

pub fn parse_gitleaks_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<GitleaksFinding> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .into_iter()
        .map(|g| Finding {
            source_tool: TOOL.into(),
            rule_id: Some(g.rule_id.clone()),
            path: g.file,
            line_start: g.start_line,
            line_end: g.end_line.unwrap_or(g.start_line),
            // Every gitleaks hit is a potential secret leak — treat as
            // error, not warning.
            severity: Severity::Error,
            message: format!("Possible secret committed: {}", g.description),
        })
        .collect())
}

pub struct GitleaksRunner;

#[async_trait]
impl LinterRunner for GitleaksRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(&self, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
        // gitleaks exits non-zero when it finds anything; --no-git scans
        // the working tree (we don't want git-history scanning here, we
        // already have a shallow clone).
        let output = match Command::new("gitleaks")
            .args([
                "detect",
                "--no-git",
                "--report-format=json",
                "--report-path=/dev/stdout",
                "--exit-code=0",
            ])
            .current_dir(repo_dir)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(RunnerError::Io(e)),
        };
        // gitleaks writes a banner to stdout before the JSON when
        // --report-path is /dev/stdout. Find the first '[' to trim.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(start) = stdout.find('[') else {
            return Ok(vec![]);
        };
        parse_gitleaks_output(&stdout[start..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_gitleaks_output() {
        let json = r#"[
            {
                "Description": "AWS Access Key",
                "StartLine": 5,
                "EndLine": 5,
                "StartColumn": 1,
                "EndColumn": 30,
                "Match": "AKIA...",
                "Secret": "AKIAIOSFODNN7EXAMPLE",
                "File": "config/.env",
                "RuleID": "aws-access-token",
                "Tags": []
            },
            {
                "Description": "Private SSH key",
                "StartLine": 1,
                "File": "id_rsa",
                "RuleID": "private-key"
            }
        ]"#;
        let f = parse_gitleaks_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("aws-access-token"));
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("AWS"));
        assert_eq!(f[1].path, "id_rsa");
        // Missing EndLine falls back to StartLine.
        assert_eq!(f[1].line_end, 1);
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_gitleaks_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_gitleaks_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn every_finding_is_marked_error_severity() {
        let json = r#"[
            {"Description":"x","StartLine":1,"File":"a","RuleID":"r1"},
            {"Description":"y","StartLine":2,"File":"b","RuleID":"r2"}
        ]"#;
        let f = parse_gitleaks_output(json).expect("ok");
        assert!(f.iter().all(|x| x.severity == Severity::Error));
    }
}
