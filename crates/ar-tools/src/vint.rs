//! vint runner. Parses `vint --json` output for Vim-script lint
//! findings.
//!
//! vint covers Vim-script style and common-mistake rules
//! (implicit scope variables, deprecated functions, missing
//! `set nocompatible`, …). Niche but distinct — Vim plugins and
//! `.vimrc` configurations show up in plenty of dotfile repos.
//! Routes on `.vim` files and the bare names `vimrc` / `.vimrc` /
//! `gvimrc` / `.gvimrc`.
//!
//! Output structure: top-level array of `{file_path, line_number,
//! column_number, severity, description, policy_name}`. Severity
//! values: `error` → Error, `warning` → Warning, `style_problem`
//! / anything else → Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "vint";

#[derive(Debug, Deserialize)]
struct VintFinding {
    #[serde(default)]
    file_path: String,
    #[serde(default)]
    line_number: u32,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    policy_name: Option<String>,
}

pub fn parse_vint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<VintFinding> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .into_iter()
        .map(|v| {
            let line = v.line_number.max(1);
            Finding {
                source_tool: TOOL.into(),
                rule_id: v.policy_name,
                path: v.file_path,
                line_start: line,
                line_end: line,
                severity: severity_from(&v.severity),
                message: v.description,
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

pub struct VintRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for VintRunner {
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
        // --json   structured output
        // --no-color  defensive; not strictly needed for JSON
        let mut args = vec!["--json".into(), "--no-color".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "vint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_vint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_vint_output() {
        let json = r#"[
            {
                "file_path": "vimrc",
                "line_number": 7,
                "column_number": 1,
                "severity": "warning",
                "description": "Make the scope explicit like 'g:foo'.",
                "policy_name": "ProhibitImplicitScopeVariable",
                "reference": "..."
            },
            {
                "file_path": "plugin/foo.vim",
                "line_number": 3,
                "column_number": 1,
                "severity": "error",
                "description": "Use 'has' to detect features.",
                "policy_name": "ProhibitMissingScriptEncoding"
            }
        ]"#;
        let f = parse_vint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "vimrc");
        assert_eq!(f[0].line_start, 7);
        assert_eq!(
            f[0].rule_id.as_deref(),
            Some("ProhibitImplicitScopeVariable")
        );
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[1].severity, Severity::Error);
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_vint_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_vint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn style_problem_severity_falls_back_to_note() {
        let json = r#"[
            {"file_path":"x.vim","line_number":1,"severity":"style_problem","description":"d","policy_name":"R"}
        ]"#;
        let f = parse_vint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_policy_name_drops_rule_id() {
        let json = r#"[
            {"file_path":"x.vim","line_number":1,"severity":"warning","description":"d"}
        ]"#;
        let f = parse_vint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[
            {"file_path":"x.vim","line_number":0,"severity":"warning","description":"d","policy_name":"R"}
        ]"#;
        let f = parse_vint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }
}
