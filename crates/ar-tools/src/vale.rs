//! vale runner. Parses `vale --output=JSON` output for prose
//! linting against project-level style rules.
//!
//! vale is a prose linter — catches grammar, spelling, voice, and
//! style issues in markdown. Distinct from markdownlint (which
//! covers structural / syntactic mistakes); the two are
//! complementary and most docs-heavy repos run both.
//!
//! Routes on `*.md` / `*.markdown`. vale needs a `.vale.ini`
//! configuration file at the repo root to know which style packs
//! to apply; without one it exits cleanly with `{}` and we emit
//! no findings.
//!
//! Output structure: `{<filename>: [{Check, Message, Line,
//! Severity, ...}]}`. Severity maps `error` → Error, `warning` →
//! Warning, anything else → Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const TOOL: &str = "vale";

#[derive(Debug, Deserialize)]
struct ValeAlert {
    #[serde(rename = "Check")]
    #[serde(default)]
    check: Option<String>,
    #[serde(rename = "Message")]
    #[serde(default)]
    message: String,
    #[serde(rename = "Line")]
    #[serde(default)]
    line: u32,
    #[serde(rename = "Severity")]
    #[serde(default)]
    severity: String,
}

pub fn parse_vale_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    // vale's JSON is a flat map: { "path/to/file.md": [alert, ...] }.
    let raw: BTreeMap<String, Vec<ValeAlert>> =
        serde_json::from_str(json).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: e.to_string(),
        })?;
    let mut out = Vec::new();
    for (path, alerts) in raw {
        for a in alerts {
            let line = a.line.max(1);
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: a.check,
                path: path.clone(),
                line_start: line,
                line_end: line,
                severity: severity_from(&a.severity),
                message: a.message,
            });
        }
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

pub struct ValeRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for ValeRunner {
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
        let mut args = vec!["--output=JSON".into(), "--no-exit".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "vale", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_vale_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_vale_output() {
        let json = r#"{
            "docs/intro.md": [
                {
                    "Check": "Vale.Spelling",
                    "Description": "",
                    "Line": 12,
                    "Link": "",
                    "Message": "Did you really mean 'recieve'?",
                    "Severity": "error",
                    "Span": [3, 9],
                    "Match": "recieve"
                },
                {
                    "Check": "write-good.Passive",
                    "Line": 5,
                    "Message": "'is being used' looks like passive voice.",
                    "Severity": "warning"
                }
            ],
            "README.md": [
                {
                    "Check": "Vale.Repetition",
                    "Line": 1,
                    "Message": "'the the' is a repetition.",
                    "Severity": "suggestion"
                }
            ]
        }"#;
        let f = parse_vale_output(json).expect("ok");
        assert_eq!(f.len(), 3);
        // BTreeMap sorts keys alphabetically, so README.md comes first.
        assert_eq!(f[0].path, "README.md");
        assert_eq!(f[0].rule_id.as_deref(), Some("Vale.Repetition"));
        assert_eq!(f[0].severity, Severity::Note); // suggestion → note
        assert_eq!(f[1].path, "docs/intro.md");
        assert_eq!(f[1].severity, Severity::Error);
        assert_eq!(f[1].line_start, 12);
        assert_eq!(f[2].severity, Severity::Warning);
    }

    #[test]
    fn empty_object_yields_zero_findings() {
        let f = parse_vale_output("{}").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_vale_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"{
            "x.md": [{"Check":"R","Line":0,"Message":"m","Severity":"warning"}]
        }"#;
        let f = parse_vale_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line_start, 1);
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"{"x.md":[{"Check":"R","Line":1,"Message":"m","Severity":"hint"}]}"#;
        let f = parse_vale_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_check_drops_rule_id() {
        let json = r#"{"x.md":[{"Line":1,"Message":"m","Severity":"warning"}]}"#;
        let f = parse_vale_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn file_with_empty_alert_array_contributes_nothing() {
        let json = r#"{"clean.md":[]}"#;
        let f = parse_vale_output(json).expect("ok");
        assert!(f.is_empty());
    }
}
