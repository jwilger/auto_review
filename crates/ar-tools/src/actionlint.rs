//! actionlint runner for GitHub Actions / Forgejo Actions workflow YAML.
//!
//! actionlint emits JSON via `actionlint -format '{{json .}}'`. The
//! per-error fields are PascalCase (`Filepath`, `Line`, `Column`,
//! `Message`, `Kind`).

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "actionlint";

#[derive(Debug, Deserialize)]
struct ActionlintError {
    #[serde(rename = "Filepath")]
    filepath: String,
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Message")]
    message: String,
    #[serde(default, rename = "Kind")]
    kind: Option<String>,
}

pub fn parse_actionlint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<ActionlintError> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .into_iter()
        .map(|e| Finding {
            source_tool: TOOL.into(),
            rule_id: e.kind,
            path: e.filepath,
            line_start: e.line,
            line_end: e.line,
            severity: Severity::Warning,
            message: e.message,
        })
        .collect())
}

pub struct ActionlintRunner {
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for ActionlintRunner {
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
        let mut args = vec!["-format".into(), "{{json .}}".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "actionlint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_actionlint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_actionlint_output() {
        let json = r#"[
            {
                "Filepath": ".github/workflows/ci.yml",
                "Line": 12,
                "Column": 7,
                "Message": "shellcheck reported issue in this script",
                "Kind": "shellcheck"
            },
            {
                "Filepath": ".github/workflows/release.yml",
                "Line": 3,
                "Column": 1,
                "Message": "unknown action 'foo/bar@v9999'",
                "Kind": "action"
            }
        ]"#;
        let f = parse_actionlint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, ".github/workflows/ci.yml");
        assert_eq!(f[0].line_start, 12);
        assert_eq!(f[0].rule_id.as_deref(), Some("shellcheck"));
        assert_eq!(f[1].rule_id.as_deref(), Some("action"));
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_actionlint_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_actionlint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_kind_defaults_to_none_rule_id() {
        let json = r#"[
            {"Filepath":"a.yml","Line":1,"Column":1,"Message":"m"}
        ]"#;
        let f = parse_actionlint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }
}
