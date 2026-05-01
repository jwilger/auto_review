//! swiftlint runner. Parses `swiftlint --reporter json --quiet`
//! output for Swift code style + common bugs.
//!
//! swiftlint is the de-facto Swift linter — covers force-casting,
//! force-unwrapping, naming conventions, file/function length, etc.
//! Routes on `.swift` files; runs nowhere else.
//!
//! Output structure: a top-level array of records with `{file, line,
//! character, reason, rule_id, severity, type}`. severity is the
//! human-readable string `"Warning"` or `"Error"`; we map both to
//! the corresponding [`Severity`] tier.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "swiftlint";

#[derive(Debug, Deserialize)]
struct SwiftlintFinding {
    file: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    rule_id: Option<String>,
    #[serde(default)]
    severity: String,
}

pub fn parse_swiftlint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<SwiftlintFinding> =
        serde_json::from_str(json).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: e.to_string(),
        })?;
    Ok(raw
        .into_iter()
        .map(|s| {
            let line = s.line.max(1);
            Finding {
                source_tool: TOOL.into(),
                rule_id: s.rule_id,
                path: s.file,
                line_start: line,
                line_end: line,
                severity: severity_from(&s.severity),
                message: s.reason,
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

pub struct SwiftLintRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for SwiftLintRunner {
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
        // `lint` is the default subcommand; --reporter json gives us
        // the structured array. --quiet suppresses progress chatter
        // that would otherwise corrupt the JSON stream.
        let mut args = vec![
            "lint".into(),
            "--reporter".into(),
            "json".into(),
            "--quiet".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "swiftlint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_swiftlint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_swiftlint_output() {
        let json = r#"[
            {
                "character" : 1,
                "file" : "Sources/Auth.swift",
                "reason" : "Force casts should be avoided.",
                "rule_id" : "force_cast",
                "severity" : "Warning",
                "line" : 42,
                "type" : "Force Cast"
            },
            {
                "character" : 9,
                "file" : "Sources/View.swift",
                "reason" : "Force tries should be avoided.",
                "rule_id" : "force_try",
                "severity" : "Error",
                "line" : 7,
                "type" : "Force Try"
            }
        ]"#;
        let f = parse_swiftlint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "Sources/Auth.swift");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].rule_id.as_deref(), Some("force_cast"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("Force casts"));
        assert_eq!(f[1].severity, Severity::Error);
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_swiftlint_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_swiftlint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"[
            {"file":"x.swift","line":1,"reason":"r","rule_id":"r","severity":"info"}
        ]"#;
        let f = parse_swiftlint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_rule_id_drops_field() {
        let json = r#"[
            {"file":"x.swift","line":1,"reason":"r","severity":"warning"}
        ]"#;
        let f = parse_swiftlint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[
            {"file":"x.swift","line":0,"reason":"r","rule_id":"R","severity":"warning"}
        ]"#;
        let f = parse_swiftlint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }
}
