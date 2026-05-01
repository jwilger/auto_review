//! stylelint runner. Parses `stylelint --formatter=json` output for
//! CSS / SCSS / Sass / Less lint findings.
//!
//! stylelint is the de-facto CSS linter — covers naming conventions,
//! at-rule misuse, browser-compat issues, and dead-rule detection.
//! Routes on `.css` / `.scss` / `.sass` / `.less` files.
//!
//! Output structure: a top-level array of `{source, warnings:
//! [{line, column, rule, severity, text}]}`. Severity values:
//! `error` → Error, `warning` → Warning. stylelint emits
//! per-file objects even when the file has no findings (empty
//! `warnings` array); we just skip those.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};

const TOOL: &str = "stylelint";

#[derive(Debug, Deserialize)]
struct StylelintFile {
    #[serde(default)]
    source: String,
    #[serde(default)]
    warnings: Vec<StylelintWarning>,
}

#[derive(Debug, Deserialize)]
struct StylelintWarning {
    #[serde(default)]
    line: u32,
    #[serde(default)]
    rule: Option<String>,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    text: String,
}

pub fn parse_stylelint_output(json: &str, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<StylelintFile> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for file in raw {
        let path = relativize(&file.source, repo_dir);
        for w in file.warnings {
            let line = w.line.max(1);
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: w.rule,
                path: path.clone(),
                line_start: line,
                line_end: line,
                severity: severity_from(&w.severity),
                message: w.text,
            });
        }
    }
    Ok(out)
}

fn relativize(path: &str, repo_dir: &Path) -> String {
    PathBuf::from(path)
        .strip_prefix(repo_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_lowercase().as_str() {
        "error" => Severity::Error,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct StylelintRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for StylelintRunner {
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
        // --formatter=json     structured output
        // --no-color           defensive against ANSI bytes
        // --allow-empty-input  don't error when files have no rules
        let mut args = vec![
            "--formatter=json".into(),
            "--no-color".into(),
            "--allow-empty-input".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "stylelint", args, vec![]).await?;
        // stylelint writes JSON to stdout when --formatter=json,
        // even though it exits non-zero on findings.
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_stylelint_output(&stdout, repo_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_stylelint_output() {
        let json = r#"[
            {
                "source": "/repo/src/styles/main.scss",
                "warnings": [
                    {
                        "line": 10,
                        "column": 3,
                        "rule": "block-no-empty",
                        "severity": "error",
                        "text": "Unexpected empty block"
                    },
                    {
                        "line": 15,
                        "column": 1,
                        "rule": "selector-class-pattern",
                        "severity": "warning",
                        "text": "Expected class selector to match BEM"
                    }
                ],
                "deprecations": [],
                "invalidOptionWarnings": [],
                "errored": true
            },
            {
                "source": "/repo/src/styles/clean.css",
                "warnings": [],
                "errored": false
            }
        ]"#;
        let f = parse_stylelint_output(json, Path::new("/repo")).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "src/styles/main.scss");
        assert_eq!(f[0].line_start, 10);
        assert_eq!(f[0].rule_id.as_deref(), Some("block-no-empty"));
        assert_eq!(f[0].severity, Severity::Error);
        assert_eq!(f[1].severity, Severity::Warning);
        assert_eq!(f[1].rule_id.as_deref(), Some("selector-class-pattern"));
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_stylelint_output("[]", Path::new("/repo")).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn file_with_no_warnings_contributes_nothing() {
        let json = r#"[{"source":"/r/clean.css","warnings":[]}]"#;
        let f = parse_stylelint_output(json, Path::new("/r")).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_stylelint_output("not json", Path::new("/r")).expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"[{"source":"/r/a.css","warnings":[
            {"line":1,"rule":"r","severity":"info","text":"t"}
        ]}]"#;
        let f = parse_stylelint_output(json, Path::new("/r")).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_rule_drops_rule_id() {
        let json = r#"[{"source":"/r/a.css","warnings":[
            {"line":1,"severity":"warning","text":"t"}
        ]}]"#;
        let f = parse_stylelint_output(json, Path::new("/r")).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[{"source":"/r/a.css","warnings":[
            {"line":0,"rule":"R","severity":"warning","text":"t"}
        ]}]"#;
        let f = parse_stylelint_output(json, Path::new("/r")).expect("ok");
        assert_eq!(f[0].line_start, 1);
    }

    #[test]
    fn path_outside_repo_dir_passes_through_unchanged() {
        let json = r#"[{"source":"/elsewhere/x.css","warnings":[
            {"line":1,"rule":"R","severity":"warning","text":"t"}
        ]}]"#;
        let f = parse_stylelint_output(json, Path::new("/repo")).expect("ok");
        assert_eq!(f[0].path, "/elsewhere/x.css");
    }
}
