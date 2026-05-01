//! RuboCop runner. Parses `rubocop --format=json` output: a top-level
//! object with a `files` array, each carrying a path and an `offenses`
//! array of {severity, message, cop_name, location}.

use crate::finding::{Finding, Severity};
use crate::runner::{LinterRunner, RunnerError};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;

const TOOL: &str = "rubocop";

#[derive(Debug, Deserialize)]
struct RubocopOutput {
    #[serde(default)]
    files: Vec<RubocopFile>,
}

#[derive(Debug, Deserialize)]
struct RubocopFile {
    path: String,
    #[serde(default)]
    offenses: Vec<RubocopOffense>,
}

#[derive(Debug, Deserialize)]
struct RubocopOffense {
    severity: String,
    message: String,
    cop_name: String,
    location: RubocopLocation,
}

#[derive(Debug, Deserialize)]
struct RubocopLocation {
    #[serde(default, rename = "start_line")]
    start_line: u32,
    #[serde(default, rename = "last_line")]
    last_line: Option<u32>,
}

pub fn parse_rubocop_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: RubocopOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for file in raw.files {
        for off in file.offenses {
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: Some(off.cop_name),
                path: file.path.clone(),
                line_start: off.location.start_line,
                line_end: off.location.last_line.unwrap_or(off.location.start_line),
                severity: severity_from(&off.severity),
                message: off.message,
            });
        }
    }
    Ok(out)
}

fn severity_from(level: &str) -> Severity {
    // RuboCop levels: refactor, convention, warning, error, fatal.
    match level {
        "fatal" | "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct RubocopRunner {
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for RubocopRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(&self, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        let output = match Command::new("rubocop")
            .args(["--format", "json"])
            .args(&self.files)
            .current_dir(repo_dir)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(RunnerError::Io(e)),
        };
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_rubocop_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_rubocop_output() {
        let json = r#"{
            "metadata": {"rubocop_version": "1.0"},
            "files": [
                {
                    "path": "lib/user.rb",
                    "offenses": [
                        {
                            "severity": "convention",
                            "message": "Missing top-level documentation",
                            "cop_name": "Style/Documentation",
                            "location": {"start_line": 3, "last_line": 3, "start_column": 1, "last_column": 5}
                        },
                        {
                            "severity": "error",
                            "message": "Syntax error",
                            "cop_name": "Lint/Syntax",
                            "location": {"start_line": 12, "last_line": 12, "start_column": 1, "last_column": 1}
                        }
                    ]
                },
                {
                    "path": "spec/user_spec.rb",
                    "offenses": [
                        {
                            "severity": "warning",
                            "message": "Use let instead",
                            "cop_name": "RSpec/InstanceVariable",
                            "location": {"start_line": 5, "last_line": 5, "start_column": 3, "last_column": 20}
                        }
                    ]
                }
            ]
        }"#;
        let f = parse_rubocop_output(json).expect("ok");
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].path, "lib/user.rb");
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].rule_id.as_deref(), Some("Style/Documentation"));
        assert_eq!(f[0].severity, Severity::Note); // convention → note
        assert_eq!(f[1].severity, Severity::Error);
        assert_eq!(f[2].path, "spec/user_spec.rb");
        assert_eq!(f[2].severity, Severity::Warning);
    }

    #[test]
    fn empty_files_yields_zero_findings() {
        let json = r#"{"files":[]}"#;
        let f = parse_rubocop_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn fatal_severity_maps_to_error() {
        let json = r#"{
            "files": [{
                "path": "a.rb",
                "offenses": [{
                    "severity": "fatal",
                    "message": "...",
                    "cop_name": "X",
                    "location": {"start_line": 1, "last_line": 1, "start_column": 1, "last_column": 1}
                }]
            }]
        }"#;
        let f = parse_rubocop_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn refactor_severity_falls_back_to_note() {
        let json = r#"{
            "files": [{
                "path": "a.rb",
                "offenses": [{
                    "severity": "refactor",
                    "message": "x",
                    "cop_name": "Y",
                    "location": {"start_line": 1, "last_line": 1, "start_column": 1, "last_column": 1}
                }]
            }]
        }"#;
        let f = parse_rubocop_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_last_line_falls_back_to_start_line() {
        let json = r#"{
            "files": [{
                "path": "a.rb",
                "offenses": [{
                    "severity": "warning",
                    "message": "x",
                    "cop_name": "Y",
                    "location": {"start_line": 7, "start_column": 1, "last_column": 1}
                }]
            }]
        }"#;
        let f = parse_rubocop_output(json).expect("ok");
        assert_eq!(f[0].line_start, 7);
        assert_eq!(f[0].line_end, 7);
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_rubocop_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }
}
