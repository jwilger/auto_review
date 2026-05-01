//! taplo runner. Parses `taplo lint --output-format json` output.
//!
//! taplo is a TOML linter + formatter — picks up issues in
//! `Cargo.toml`, `pyproject.toml`, `pnpm-workspace.toml`, etc.
//! Routes on `.toml` files; runs nowhere else.
//!
//! Output structure: a top-level array of diagnostic objects with
//! `{message, range: {start: {line}, end: {line}}, file, severity}`.
//! taplo emits 0-indexed line numbers; we convert to 1-indexed
//! before emitting Findings.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "taplo";

#[derive(Debug, Deserialize)]
struct TaploDiagnostic {
    #[serde(default)]
    severity: String,
    message: String,
    file: String,
    #[serde(default)]
    range: Option<TaploRange>,
}

#[derive(Debug, Deserialize, Default)]
struct TaploRange {
    #[serde(default)]
    start: Option<TaploPosition>,
    #[serde(default)]
    end: Option<TaploPosition>,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
struct TaploPosition {
    #[serde(default)]
    line: Option<u32>,
}

pub fn parse_taplo_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<TaploDiagnostic> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .into_iter()
        .map(|d| {
            let start_line = d
                .range
                .as_ref()
                .and_then(|r| r.start.as_ref())
                .and_then(|p| p.line)
                .map(|l| l.saturating_add(1))
                .unwrap_or(1);
            let end_line = d
                .range
                .as_ref()
                .and_then(|r| r.end.as_ref())
                .and_then(|p| p.line)
                .map(|l| l.saturating_add(1))
                .unwrap_or(start_line);
            Finding {
                source_tool: TOOL.into(),
                rule_id: None,
                path: d.file,
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

pub struct TaploRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for TaploRunner {
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
        let mut args = vec!["lint".into(), "--output-format".into(), "json".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "taplo", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_taplo_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_taplo_output() {
        // taplo emits 0-indexed lines; we emit 1-indexed.
        let json = r#"[
            {
                "severity": "warning",
                "message": "Key 'foo' is declared more than once.",
                "file": "Cargo.toml",
                "range": {
                    "start": {"line": 4, "character": 0},
                    "end": {"line": 4, "character": 6}
                }
            },
            {
                "severity": "error",
                "message": "Expected '='.",
                "file": "pyproject.toml",
                "range": {
                    "start": {"line": 11, "character": 8},
                    "end": {"line": 13, "character": 0}
                }
            }
        ]"#;
        let f = parse_taplo_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "Cargo.toml");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].line_end, 5);
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[1].line_start, 12);
        assert_eq!(f[1].line_end, 14);
        assert_eq!(f[1].severity, Severity::Error);
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_taplo_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_taplo_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_range_falls_back_to_line_one() {
        let json = r#"[
            {"severity":"warning","message":"m","file":"a.toml"}
        ]"#;
        let f = parse_taplo_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn missing_end_position_mirrors_start() {
        let json = r#"[{
            "severity":"warning","message":"m","file":"a.toml",
            "range":{"start":{"line":3,"character":0}}
        }]"#;
        let f = parse_taplo_output(json).expect("ok");
        assert_eq!(f[0].line_start, 4);
        assert_eq!(f[0].line_end, 4);
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"[
            {"severity":"hint","message":"m","file":"a.toml",
             "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}
        ]"#;
        let f = parse_taplo_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }
}
