//! ESLint runner. Parses `eslint --format=json` output.
//!
//! ESLint emits one outer object per file (with a `messages` array of
//! per-line diagnostics) rather than a flat list, so the parser fans the
//! per-file messages out into individual [`Finding`]s.

use crate::finding::{Finding, Severity};
use crate::runner::{LinterRunner, RunnerError};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

const TOOL: &str = "eslint";

#[derive(Debug, Deserialize)]
struct EslintFile {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(default)]
    messages: Vec<EslintMessage>,
}

#[derive(Debug, Deserialize)]
struct EslintMessage {
    #[serde(default, rename = "ruleId")]
    rule_id: Option<String>,
    /// 1 = warning, 2 = error.
    severity: u8,
    message: String,
    line: u32,
    #[serde(default, rename = "endLine")]
    end_line: Option<u32>,
}

pub fn parse_eslint_output(json: &str, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<EslintFile> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for file in raw {
        let rel_path = relativize(&file.file_path, repo_dir);
        for msg in file.messages {
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: msg.rule_id,
                path: rel_path.clone(),
                line_start: msg.line,
                line_end: msg.end_line.unwrap_or(msg.line),
                severity: severity_from(msg.severity),
                message: msg.message,
            });
        }
    }
    Ok(out)
}

fn severity_from(level: u8) -> Severity {
    match level {
        2 => Severity::Error,
        1 => Severity::Warning,
        _ => Severity::Note,
    }
}

fn relativize(path: &str, repo_dir: &Path) -> String {
    PathBuf::from(path)
        .strip_prefix(repo_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

pub struct EslintRunner {
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for EslintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(&self, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        let output = match Command::new("eslint")
            .args(["--format=json", "--no-error-on-unmatched-pattern"])
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
        parse_eslint_output(&stdout, repo_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_eslint_output_across_files() {
        let json = r#"[
            {
                "filePath": "/repo/src/a.js",
                "messages": [
                    {"ruleId":"no-unused-vars","severity":1,"message":"unused",
                     "line":3,"column":7,"endLine":3,"endColumn":12}
                ]
            },
            {
                "filePath": "/repo/src/b.ts",
                "messages": [
                    {"ruleId":"@typescript-eslint/no-explicit-any","severity":2,
                     "message":"avoid any","line":1,"column":1}
                ]
            }
        ]"#;
        let f = parse_eslint_output(json, Path::new("/repo")).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "src/a.js");
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].rule_id.as_deref(), Some("no-unused-vars"));
        assert_eq!(f[1].severity, Severity::Error);
        assert_eq!(f[1].line_end, 1); // missing endLine → fallback to line
    }

    #[test]
    fn empty_messages_are_dropped() {
        let json = r#"[
            {"filePath":"/r/clean.js","messages":[]},
            {"filePath":"/r/noisy.js","messages":[
                {"ruleId":null,"severity":2,"message":"x","line":1,"column":1}
            ]}
        ]"#;
        let f = parse_eslint_output(json, Path::new("/r")).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "noisy.js");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_eslint_output("[]", Path::new("/r")).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_eslint_output("not json", Path::new("/r")).expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"[{
            "filePath":"/r/a.js","messages":[
                {"ruleId":"r","severity":99,"message":"m","line":1,"column":1}
            ]
        }]"#;
        let f = parse_eslint_output(json, Path::new("/r")).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }
}
