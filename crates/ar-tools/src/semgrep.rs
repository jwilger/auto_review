//! Semgrep runner. Multi-language SAST that scans the whole tree
//! against either a custom ruleset or `--config=auto` (Semgrep's
//! community-curated defaults). Parses the JSON output's `results`
//! array.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "semgrep";

#[derive(Debug, Deserialize)]
struct SemgrepOutput {
    #[serde(default)]
    results: Vec<SemgrepResult>,
}

#[derive(Debug, Deserialize)]
struct SemgrepResult {
    check_id: String,
    path: String,
    start: Position,
    end: Position,
    extra: Extra,
}

#[derive(Debug, Deserialize)]
struct Position {
    line: u32,
}

#[derive(Debug, Deserialize)]
struct Extra {
    message: String,
    severity: String,
}

pub fn parse_semgrep_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: SemgrepOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .results
        .into_iter()
        .map(|r| Finding {
            source_tool: TOOL.into(),
            rule_id: Some(r.check_id),
            path: r.path,
            line_start: r.start.line,
            line_end: r.end.line,
            severity: severity_from(&r.extra.severity),
            message: r.extra.message,
        })
        .collect())
}

fn severity_from(level: &str) -> Severity {
    match level {
        "ERROR" => Severity::Error,
        "WARNING" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct SemgrepRunner;

#[async_trait]
impl LinterRunner for SemgrepRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // --config=auto pulls Semgrep's community-curated default
        // rules; users with a `.semgrep.yml` in the repo will see it
        // get picked up automatically.
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "semgrep",
            vec![
                "scan".into(),
                "--json".into(),
                "--config=auto".into(),
                "--quiet".into(),
                "--metrics=off".into(),
            ],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_semgrep_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_semgrep_output() {
        let json = r#"{
            "version": "1.0.0",
            "results": [
                {
                    "check_id": "rules.unsafe-unwrap",
                    "path": "src/main.rs",
                    "start": {"line": 42, "col": 5},
                    "end": {"line": 42, "col": 30},
                    "extra": {
                        "message": "Avoid unwrap() in production code.",
                        "severity": "ERROR"
                    }
                },
                {
                    "check_id": "rules.todo-left",
                    "path": "src/lib.rs",
                    "start": {"line": 7, "col": 1},
                    "end": {"line": 7, "col": 6},
                    "extra": {
                        "message": "TODO comment left in code.",
                        "severity": "WARNING"
                    }
                }
            ],
            "errors": []
        }"#;
        let f = parse_semgrep_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rule_id.as_deref(), Some("rules.unsafe-unwrap"));
        assert_eq!(f[0].path, "src/main.rs");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].severity, Severity::Error);
        assert_eq!(f[1].severity, Severity::Warning);
    }

    #[test]
    fn empty_results_yields_zero_findings() {
        let json = r#"{"version":"1.0","results":[],"errors":[]}"#;
        let f = parse_semgrep_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_results_field_decodes_to_empty() {
        let json = r#"{"version":"1.0","errors":[]}"#;
        let f = parse_semgrep_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"{
            "results": [{
                "check_id":"r","path":"a","start":{"line":1,"col":1},
                "end":{"line":1,"col":2},
                "extra":{"message":"m","severity":"WHATEVER"}
            }]
        }"#;
        let f = parse_semgrep_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_semgrep_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn end_line_is_preserved_when_different_from_start() {
        let json = r#"{
            "results": [{
                "check_id":"r","path":"a","start":{"line":5,"col":1},
                "end":{"line":9,"col":1},
                "extra":{"message":"m","severity":"WARNING"}
            }]
        }"#;
        let f = parse_semgrep_output(json).expect("ok");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].line_end, 9);
    }
}
