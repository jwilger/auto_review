//! staticcheck runner. Parses `staticcheck -f json ./...` output for
//! Go static-analysis findings.
//!
//! staticcheck is a Go-specific analyzer maintained by Dominik Honnef
//! that predates golangci-lint and has its own rule conventions. It
//! catches deprecation, code simplification opportunities, performance
//! pitfalls, and idiom-conformance issues that gosec (security) and
//! golangci-lint (general lint with optional staticcheck inclusion)
//! don't always surface together. Routes alongside golangci-lint
//! and gosec on `.go` files.
//!
//! Output format: JSON Lines (one record per finding), each shaped
//! `{code, severity, location: {file, line}, end: {file, line},
//! message}`. Severity values: `error` → Error, `warning` → Warning,
//! anything else → Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "staticcheck";

#[derive(Debug, Deserialize)]
struct StaticcheckRecord {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    location: StaticcheckLoc,
    #[serde(default)]
    end: StaticcheckLoc,
}

#[derive(Debug, Default, Deserialize)]
struct StaticcheckLoc {
    #[serde(default)]
    file: String,
    #[serde(default)]
    line: u32,
}

pub fn parse_staticcheck_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let r: StaticcheckRecord = serde_json::from_str(line).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: format!("line {line:?}: {e}"),
        })?;
        let start = r.location.line.max(1);
        let end = if r.end.line >= start {
            r.end.line
        } else {
            start
        };
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: r.code,
            path: r.location.file,
            line_start: start,
            line_end: end,
            severity: severity_from(&r.severity),
            message: r.message,
        });
    }
    Ok(out)
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_lowercase().as_str() {
        "error" => Severity::Error,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct StaticcheckRunner;

#[async_trait]
impl LinterRunner for StaticcheckRunner {
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
            "staticcheck",
            vec!["-f".into(), "json".into(), "./...".into()],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_staticcheck_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_staticcheck_output() {
        let text = "\
{\"code\":\"SA1019\",\"severity\":\"warning\",\"location\":{\"file\":\"cmd/main.go\",\"line\":10,\"column\":5},\"end\":{\"file\":\"cmd/main.go\",\"line\":10,\"column\":15},\"message\":\"package crypto/md5 is deprecated\"}
{\"code\":\"S1000\",\"severity\":\"warning\",\"location\":{\"file\":\"util/sel.go\",\"line\":3,\"column\":1},\"end\":{\"file\":\"util/sel.go\",\"line\":7,\"column\":1},\"message\":\"should use a simple channel send/receive instead of select with a single case\"}
";
        let f = parse_staticcheck_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("SA1019"));
        assert_eq!(f[0].path, "cmd/main.go");
        assert_eq!(f[0].line_start, 10);
        assert_eq!(f[0].line_end, 10);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("md5"));
        assert_eq!(f[1].line_end, 7);
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_staticcheck_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn blank_lines_are_tolerated() {
        let f = parse_staticcheck_output("\n\n  \n").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_line_is_a_parse_error() {
        let err = parse_staticcheck_output("this is not json\n").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn error_severity_maps_to_error() {
        let text = r#"{"code":"X","severity":"error","location":{"file":"x.go","line":1},"end":{"file":"x.go","line":1},"message":"m"}"#;
        let f = parse_staticcheck_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let text = r#"{"code":"X","severity":"hint","location":{"file":"x.go","line":1},"end":{"file":"x.go","line":1},"message":"m"}"#;
        let f = parse_staticcheck_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_code_drops_rule_id() {
        let text = r#"{"severity":"warning","location":{"file":"x.go","line":1},"end":{"file":"x.go","line":1},"message":"m"}"#;
        let f = parse_staticcheck_output(text).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let text = r#"{"code":"X","severity":"warning","location":{"file":"x.go","line":0},"end":{"file":"x.go","line":0},"message":"m"}"#;
        let f = parse_staticcheck_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn end_line_below_start_is_clamped() {
        let text = r#"{"code":"X","severity":"warning","location":{"file":"x.go","line":50},"end":{"file":"x.go","line":10},"message":"m"}"#;
        let f = parse_staticcheck_output(text).expect("ok");
        assert_eq!(f[0].line_start, 50);
        assert_eq!(f[0].line_end, 50);
    }
}
