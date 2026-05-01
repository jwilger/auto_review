//! gosec runner. Parses `gosec -fmt=json ./...` output for Go
//! security issues (subprocess injection, unsafe SQL, weak crypto,
//! file-path traversal, hardcoded credentials, …).
//!
//! gosec is distinct from golangci-lint: golangci-lint *can* be
//! configured to include gosec via the `gosec` linter, but most
//! repos don't enable it there. Running gosec standalone catches
//! the security ruleset regardless of the repo's golangci-lint
//! config.
//!
//! Output structure: `{Issues: [{severity, confidence, rule_id,
//! details, file, line, column}]}`. `line` is a string in gosec's
//! JSON output (it can be either a single line or a "start-end"
//! range for spanning issues); we parse the leading number.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "gosec";

#[derive(Debug, Deserialize)]
struct GosecOutput {
    #[serde(default, rename = "Issues")]
    issues: Vec<GosecIssue>,
}

#[derive(Debug, Deserialize)]
struct GosecIssue {
    #[serde(default)]
    severity: String,
    #[serde(default)]
    rule_id: Option<String>,
    #[serde(default)]
    details: String,
    #[serde(default)]
    file: String,
    /// gosec emits line as a JSON string, sometimes "10", sometimes
    /// "10-12" for a multi-line span. We parse the leading number
    /// for both shapes.
    #[serde(default)]
    line: String,
}

pub fn parse_gosec_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: GosecOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .issues
        .into_iter()
        .map(|i| {
            let (start, end) = parse_line_field(&i.line);
            Finding {
                source_tool: TOOL.into(),
                rule_id: i.rule_id,
                path: i.file,
                line_start: start,
                line_end: end,
                severity: severity_from(&i.severity),
                message: i.details,
            }
        })
        .collect())
}

/// Parse gosec's `line` string. Shapes seen in the wild:
/// - `"42"` → start=42, end=42
/// - `"10-15"` → start=10, end=15
/// - `""` / unparseable → start=1, end=1 (safe default)
fn parse_line_field(s: &str) -> (u32, u32) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return (1, 1);
    }
    if let Some((a, b)) = trimmed.split_once('-') {
        let start: u32 = a.trim().parse().unwrap_or(1).max(1);
        let end: u32 = b.trim().parse().unwrap_or(start).max(start);
        return (start, end);
    }
    let n: u32 = trimmed.parse().unwrap_or(1).max(1);
    (n, n)
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_uppercase().as_str() {
        "HIGH" | "CRITICAL" => Severity::Error,
        "MEDIUM" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct GosecRunner;

#[async_trait]
impl LinterRunner for GosecRunner {
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
            "gosec",
            vec![
                "-fmt=json".into(),
                "-no-fail".into(),
                "-quiet".into(),
                "./...".into(),
            ],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_gosec_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_gosec_output() {
        let json = r#"{
            "Issues": [
                {
                    "severity": "HIGH",
                    "confidence": "HIGH",
                    "rule_id": "G204",
                    "details": "Subprocess launched with variable",
                    "file": "cmd/main.go",
                    "line": "42",
                    "column": "5"
                },
                {
                    "severity": "MEDIUM",
                    "confidence": "HIGH",
                    "rule_id": "G401",
                    "details": "Use of weak cryptographic primitive",
                    "file": "crypto/hash.go",
                    "line": "10-15",
                    "column": "1"
                }
            ],
            "Stats": {"files": 12, "lines": 800, "found": 2}
        }"#;
        let f = parse_gosec_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("G204"));
        assert_eq!(f[0].path, "cmd/main.go");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].line_end, 42);
        assert_eq!(f[0].severity, Severity::Error); // HIGH
        assert_eq!(f[1].line_start, 10);
        assert_eq!(f[1].line_end, 15);
        assert_eq!(f[1].severity, Severity::Warning); // MEDIUM
    }

    #[test]
    fn low_severity_falls_back_to_note() {
        let json = r#"{
            "Issues":[{"severity":"LOW","rule_id":"G1","details":"d","file":"x.go","line":"1"}]
        }"#;
        let f = parse_gosec_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn empty_line_string_falls_back_to_one() {
        let json = r#"{
            "Issues":[{"severity":"HIGH","rule_id":"G1","details":"d","file":"x.go","line":""}]
        }"#;
        let f = parse_gosec_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn unparseable_line_string_falls_back_to_one() {
        let json = r#"{
            "Issues":[{"severity":"HIGH","rule_id":"G1","details":"d","file":"x.go","line":"abc"}]
        }"#;
        let f = parse_gosec_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
    }

    #[test]
    fn end_below_start_in_range_is_clamped() {
        let json = r#"{
            "Issues":[{"severity":"HIGH","rule_id":"G1","details":"d","file":"x.go","line":"50-10"}]
        }"#;
        let f = parse_gosec_output(json).expect("ok");
        assert_eq!(f[0].line_start, 50);
        assert_eq!(f[0].line_end, 50);
    }

    #[test]
    fn missing_rule_id_drops_field() {
        let json = r#"{
            "Issues":[{"severity":"HIGH","details":"d","file":"x.go","line":"1"}]
        }"#;
        let f = parse_gosec_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn empty_issues_yields_zero_findings() {
        let f = parse_gosec_output(r#"{"Issues":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_issues_field_decodes_to_empty() {
        let f = parse_gosec_output("{}").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_gosec_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }
}
