//! dotenv-linter runner. Parses `dotenv-linter --quiet --no-color check`
//! output (JSON via `dotenv-linter --output json`).
//!
//! dotenv-linter catches common mistakes in `.env` files: lower-cased
//! keys, missing newlines, duplicated keys, leading whitespace, and
//! similar formatting/conventions issues that none of the other
//! bundled linters cover.
//!
//! Output structure: a top-level array of records with
//! `{path, line, message, name}` (where `name` is the rule code,
//! e.g. `LowercaseKey`). Some versions emit a wrapper object; we
//! tolerate both shapes via serde untagged.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "dotenv-linter";

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DotenvOutput {
    /// Newer versions: array of warnings.
    List(Vec<DotenvWarning>),
    /// Older / wrapper shape: `{warnings: [...]}`.
    Wrapped {
        #[serde(default)]
        warnings: Vec<DotenvWarning>,
    },
}

#[derive(Debug, Deserialize)]
struct DotenvWarning {
    /// Repo-relative path to the `.env` file.
    path: String,
    /// 1-indexed line number from dotenv-linter.
    #[serde(default)]
    line: u32,
    /// Human-readable description.
    #[serde(default)]
    message: String,
    /// Rule code (`LowercaseKey`, `LeadingCharacter`, …). Some
    /// versions emit `name`, others `check_name`; accept both.
    #[serde(default, alias = "check_name")]
    name: Option<String>,
}

pub fn parse_dotenv_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: DotenvOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let warnings = match raw {
        DotenvOutput::List(v) => v,
        DotenvOutput::Wrapped { warnings } => warnings,
    };
    Ok(warnings
        .into_iter()
        .map(|w| {
            let line = if w.line == 0 { 1 } else { w.line };
            Finding {
                source_tool: TOOL.into(),
                rule_id: w.name,
                path: w.path,
                line_start: line,
                line_end: line,
                severity: Severity::Warning,
                message: w.message,
            }
        })
        .collect())
}

pub struct DotenvLinterRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for DotenvLinterRunner {
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
        // `check` is the default subcommand; --output json gives us
        // structured findings; --no-color avoids ANSI bytes leaking
        // into the JSON. dotenv-linter exits non-zero on findings;
        // run_in_sandbox treats stdout as the source of truth so
        // that's fine.
        let mut args = vec![
            "check".into(),
            "--output".into(),
            "json".into(),
            "--no-color".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "dotenv-linter", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_dotenv_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_shape_output() {
        let json = r#"[
            {
                "path": ".env",
                "line": 3,
                "message": "The environment variable 'foo' is lowercase",
                "name": "LowercaseKey"
            },
            {
                "path": ".env.local",
                "line": 7,
                "message": "Trailing whitespace",
                "check_name": "TrailingWhitespace"
            }
        ]"#;
        let f = parse_dotenv_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, ".env");
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].rule_id.as_deref(), Some("LowercaseKey"));
        assert_eq!(f[0].severity, Severity::Warning);
        // `check_name` alias resolves into `name`.
        assert_eq!(f[1].rule_id.as_deref(), Some("TrailingWhitespace"));
    }

    #[test]
    fn parses_wrapped_shape_output() {
        let json = r#"{
            "warnings": [
                {"path":".env","line":1,"message":"m","name":"R1"}
            ]
        }"#;
        let f = parse_dotenv_output(json).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, ".env");
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_dotenv_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn empty_wrapper_yields_zero_findings() {
        let f = parse_dotenv_output(r#"{"warnings":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_dotenv_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[{"path":".env","line":0,"message":"m","name":"R"}]"#;
        let f = parse_dotenv_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn missing_name_drops_rule_id() {
        let json = r#"[{"path":".env","line":1,"message":"m"}]"#;
        let f = parse_dotenv_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }
}
