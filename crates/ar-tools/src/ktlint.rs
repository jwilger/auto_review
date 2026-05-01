//! ktlint runner. Parses `ktlint --reporter=json` output for Kotlin
//! style and lint findings.
//!
//! ktlint covers the official Kotlin coding conventions plus a small
//! body of common-mistake rules (unused imports, multi-line if-else,
//! …). Routes on `.kt` / `.kts` files; runs nowhere else.
//!
//! Output structure: top-level array of `{file, errors: [{line,
//! column, message, rule}]}`. ktlint doesn't tier severity in JSON
//! output — every finding maps to Warning. The LLM consuming
//! findings can promote/demote based on rule_id context.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "ktlint";

#[derive(Debug, Deserialize)]
struct KtlintFile {
    file: String,
    #[serde(default)]
    errors: Vec<KtlintError>,
}

#[derive(Debug, Deserialize)]
struct KtlintError {
    #[serde(default)]
    line: u32,
    #[serde(default)]
    message: String,
    #[serde(default)]
    rule: Option<String>,
}

pub fn parse_ktlint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<KtlintFile> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for file in raw {
        for e in file.errors {
            let line = e.line.max(1);
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: e.rule,
                path: file.file.clone(),
                line_start: line,
                line_end: line,
                severity: Severity::Warning,
                message: e.message,
            });
        }
    }
    Ok(out)
}

pub struct KtlintRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for KtlintRunner {
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
        // --reporter=json    structured output
        // --color=never      defensive against ANSI bytes
        // --relative         emit repo-relative paths
        let mut args = vec![
            "--reporter=json".into(),
            "--color=never".into(),
            "--relative".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "ktlint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_ktlint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_ktlint_output() {
        let json = r#"[
            {
                "file": "src/main/kotlin/Foo.kt",
                "errors": [
                    {
                        "line": 5,
                        "column": 1,
                        "message": "Unexpected indentation (4) (should be 0)",
                        "rule": "indent"
                    },
                    {
                        "line": 12,
                        "column": 1,
                        "message": "Unused import",
                        "rule": "unused-imports"
                    }
                ]
            },
            {
                "file": "src/main/kotlin/Clean.kt",
                "errors": []
            }
        ]"#;
        let f = parse_ktlint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "src/main/kotlin/Foo.kt");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].rule_id.as_deref(), Some("indent"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[1].rule_id.as_deref(), Some("unused-imports"));
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_ktlint_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn file_with_no_errors_contributes_nothing() {
        let json = r#"[{"file":"x.kt","errors":[]}]"#;
        let f = parse_ktlint_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_ktlint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_rule_drops_rule_id() {
        let json = r#"[{"file":"x.kt","errors":[{"line":1,"message":"m"}]}]"#;
        let f = parse_ktlint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[{"file":"x.kt","errors":[{"line":0,"message":"m","rule":"R"}]}]"#;
        let f = parse_ktlint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn line_end_mirrors_line_start() {
        let json = r#"[{"file":"x.kt","errors":[{"line":42,"message":"m","rule":"R"}]}]"#;
        let f = parse_ktlint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].line_end, 42);
    }
}
