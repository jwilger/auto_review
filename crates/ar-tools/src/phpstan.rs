//! phpstan runner. Parses `phpstan analyse --error-format=json` output.
//!
//! phpstan is the standard PHP static-analysis tool — used by Symfony,
//! Laravel, WordPress, Drupal, and most modern PHP projects. We invoke
//! it with `--no-progress` to suppress the progress bar (which would
//! otherwise corrupt the JSON stream) and `--error-format=json` for
//! structured output.
//!
//! Output structure: a top-level `{files: {<path>: {messages: [{line,
//! message}]}}}` map. phpstan doesn't tier severity in its JSON output;
//! everything maps to Warning, with the file path going through as-is.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const TOOL: &str = "phpstan";

#[derive(Debug, Deserialize)]
struct PhpstanOutput {
    #[serde(default)]
    files: BTreeMap<String, PhpstanFile>,
}

#[derive(Debug, Deserialize)]
struct PhpstanFile {
    #[serde(default)]
    messages: Vec<PhpstanMessage>,
}

#[derive(Debug, Deserialize)]
struct PhpstanMessage {
    line: u32,
    message: String,
    #[serde(default)]
    identifier: Option<String>,
}

pub fn parse_phpstan_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: PhpstanOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for (path, file) in raw.files {
        for msg in file.messages {
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: msg.identifier,
                path: path.clone(),
                line_start: msg.line,
                line_end: msg.line,
                severity: Severity::Warning,
                message: msg.message,
            });
        }
    }
    Ok(out)
}

pub struct PhpstanRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for PhpstanRunner {
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
        let mut args = vec![
            "analyse".into(),
            "--no-progress".into(),
            "--error-format=json".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "phpstan", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_phpstan_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_phpstan_output() {
        let json = r#"{
            "totals": {"errors": 0, "file_errors": 3},
            "files": {
                "src/Controller/UserController.php": {
                    "errors": 2,
                    "messages": [
                        {
                            "message": "Undefined variable: $usr",
                            "line": 17,
                            "ignorable": true,
                            "identifier": "variable.undefined"
                        },
                        {
                            "message": "Method does not return.",
                            "line": 25,
                            "ignorable": false
                        }
                    ]
                },
                "src/Service/Mailer.php": {
                    "errors": 1,
                    "messages": [
                        {
                            "message": "Type mismatch.",
                            "line": 4,
                            "identifier": "type.mismatch"
                        }
                    ]
                }
            }
        }"#;
        let f = parse_phpstan_output(json).expect("ok");
        assert_eq!(f.len(), 3);
        // BTreeMap sorts keys alphabetically: Controller comes first.
        assert_eq!(f[0].path, "src/Controller/UserController.php");
        assert_eq!(f[0].line_start, 17);
        assert_eq!(f[0].rule_id.as_deref(), Some("variable.undefined"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[1].rule_id.is_none()); // missing identifier
        assert_eq!(f[2].path, "src/Service/Mailer.php");
    }

    #[test]
    fn empty_files_yields_zero_findings() {
        let f = parse_phpstan_output(r#"{"totals":{},"files":{}}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_files_field_decodes_to_empty() {
        let f = parse_phpstan_output(r#"{"totals":{}}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_phpstan_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn line_end_mirrors_line_start() {
        let json = r#"{
            "files": {
                "x.php": {
                    "messages": [{"message": "m", "line": 99}]
                }
            }
        }"#;
        let f = parse_phpstan_output(json).expect("ok");
        assert_eq!(f[0].line_start, 99);
        assert_eq!(f[0].line_end, 99);
    }
}
