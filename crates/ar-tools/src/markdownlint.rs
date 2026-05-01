//! markdownlint-cli runner. Parses `markdownlint --json` output.

use crate::finding::{Finding, Severity};
use crate::runner::{LinterRunner, RunnerError};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;

const TOOL: &str = "markdownlint";

#[derive(Debug, Deserialize)]
struct MdlDiagnostic {
    #[serde(rename = "fileName")]
    file_name: String,
    #[serde(rename = "lineNumber")]
    line_number: u32,
    #[serde(rename = "ruleNames")]
    rule_names: Vec<String>,
    #[serde(rename = "ruleDescription")]
    rule_description: String,
    #[serde(default, rename = "errorDetail")]
    error_detail: Option<String>,
}

pub fn parse_markdownlint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<MdlDiagnostic> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .into_iter()
        .map(|d| Finding {
            source_tool: TOOL.into(),
            rule_id: d.rule_names.first().cloned(),
            path: d.file_name,
            line_start: d.line_number,
            line_end: d.line_number,
            severity: Severity::Note,
            message: match d.error_detail {
                Some(detail) if !detail.is_empty() => format!("{}: {}", d.rule_description, detail),
                _ => d.rule_description,
            },
        })
        .collect())
}

pub struct MarkdownLintRunner {
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for MarkdownLintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(&self, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        let output = match Command::new("markdownlint")
            .args(["--json"])
            .args(&self.files)
            .current_dir(repo_dir)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(RunnerError::Io(e)),
        };
        // markdownlint writes findings to stderr; stdout is normally empty.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_markdownlint_output(&stderr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_markdownlint_output() {
        let json = r#"[
            {
                "fileName": "README.md",
                "lineNumber": 1,
                "ruleNames": ["MD041", "first-line-heading"],
                "ruleDescription": "First line in a file should be a top-level heading",
                "ruleInformation": null,
                "errorDetail": null,
                "errorContext": null,
                "errorRange": null
            }
        ]"#;
        let f = parse_markdownlint_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "README.md");
        assert_eq!(f[0].rule_id.as_deref(), Some("MD041"));
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn includes_error_detail_when_present() {
        let json = r#"[
            {"fileName":"a.md","lineNumber":2,"ruleNames":["MD013"],
             "ruleDescription":"Line length","errorDetail":"Expected: 80; Actual: 120"}
        ]"#;
        let f = parse_markdownlint_output(json).expect("ok");
        assert!(f[0].message.contains("Line length"));
        assert!(f[0].message.contains("120"));
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_markdownlint_output("[]").expect("ok");
        assert!(f.is_empty());
    }
}
