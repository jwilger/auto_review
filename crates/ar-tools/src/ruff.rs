//! Ruff Python linter runner. Parses `ruff check --output-format=json`.

use crate::finding::{Finding, Severity};
use crate::runner::{LinterRunner, RunnerError};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

const TOOL: &str = "ruff";

#[derive(Debug, Deserialize)]
struct RuffDiagnostic {
    code: Option<String>,
    message: String,
    location: RuffLocation,
    #[serde(default)]
    end_location: Option<RuffLocation>,
    filename: String,
}

#[derive(Debug, Deserialize)]
struct RuffLocation {
    row: u32,
}

/// Map a ruff JSON output blob to normalized findings, stripping `repo_dir`
/// from absolute paths so output matches the diff's relative paths.
pub fn parse_ruff_output(json: &str, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<RuffDiagnostic> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .into_iter()
        .map(|d| Finding {
            source_tool: TOOL.into(),
            rule_id: d.code,
            path: relativize(&d.filename, repo_dir),
            line_start: d.location.row,
            line_end: d.end_location.map(|l| l.row).unwrap_or(d.location.row),
            severity: Severity::Warning,
            message: d.message,
        })
        .collect())
}

fn relativize(path: &str, repo_dir: &Path) -> String {
    PathBuf::from(path)
        .strip_prefix(repo_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

pub struct RuffRunner;

#[async_trait]
impl LinterRunner for RuffRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(&self, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
        let output = match Command::new("ruff")
            .args(["check", "--output-format=json", "--exit-zero", "."])
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
        parse_ruff_output(&stdout, repo_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_ruff_output() {
        let json = r#"[
            {
                "code": "E501",
                "message": "Line too long",
                "location": {"row": 5, "column": 81},
                "end_location": {"row": 5, "column": 110},
                "filename": "/repo/foo.py"
            },
            {
                "code": "F401",
                "message": "unused import",
                "location": {"row": 1, "column": 1},
                "end_location": {"row": 1, "column": 9},
                "filename": "/repo/sub/bar.py"
            }
        ]"#;
        let f = parse_ruff_output(json, Path::new("/repo")).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "foo.py");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].rule_id.as_deref(), Some("E501"));
        assert_eq!(f[1].path, "sub/bar.py");
    }

    #[test]
    fn empty_output_yields_zero_findings() {
        let f = parse_ruff_output("[]", Path::new("/repo")).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn handles_missing_end_location() {
        let json = r#"[
            {"code":"E1","message":"m","location":{"row":7,"column":1},"filename":"/r/a.py"}
        ]"#;
        let f = parse_ruff_output(json, Path::new("/r")).expect("ok");
        assert_eq!(f[0].line_start, 7);
        assert_eq!(f[0].line_end, 7);
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_ruff_output("not json", Path::new("/r")).expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }
}
