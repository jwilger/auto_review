//! buf runner. Parses `buf lint --error-format=json` output for
//! Protocol Buffers schema lint issues.
//!
//! buf is the standard Protobuf toolchain — covers field naming
//! conventions, package layout, breaking-change detection, and
//! message-shape rules. Routes on `.proto` files; runs nowhere else.
//!
//! Output format: JSON-Lines (one object per line), each shaped
//! `{path, start_line, start_column, end_line, end_column, type,
//! message}`. We treat `type` (e.g. `FIELD_LOWER_SNAKE_CASE`) as the
//! rule_id and surface every line as a Warning — buf doesn't tier
//! severity in its lint output, so promoting/demoting is a job for
//! the LLM consuming the findings.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "buf";

#[derive(Debug, Deserialize)]
struct BufFinding {
    #[serde(default)]
    path: String,
    #[serde(default)]
    start_line: u32,
    #[serde(default)]
    end_line: u32,
    #[serde(default, rename = "type")]
    rule: Option<String>,
    #[serde(default)]
    message: String,
}

pub fn parse_buf_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let f: BufFinding = serde_json::from_str(line).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: format!("line {line:?}: {e}"),
        })?;
        let start = f.start_line.max(1);
        let end = if f.end_line >= start {
            f.end_line
        } else {
            start
        };
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: f.rule,
            path: f.path,
            line_start: start,
            line_end: end,
            severity: Severity::Warning,
            message: f.message,
        });
    }
    Ok(out)
}

pub struct BufRunner;

#[async_trait]
impl LinterRunner for BufRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "buf",
            vec!["lint".into(), "--error-format=json".into()],
            vec![],
        )
        .await?;
        // buf writes findings to stdout; one JSON object per line.
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_buf_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_buf_output() {
        let text = "\
{\"path\":\"foo.proto\",\"start_line\":3,\"start_column\":1,\"end_line\":3,\"end_column\":20,\"type\":\"FIELD_LOWER_SNAKE_CASE\",\"message\":\"Field name 'fooBar' should be lower_snake_case.\"}
{\"path\":\"bar.proto\",\"start_line\":7,\"start_column\":1,\"end_line\":9,\"end_column\":1,\"type\":\"PACKAGE_LOWER_SNAKE_CASE\",\"message\":\"Package 'BarPkg' must be lower_snake_case.\"}
";
        let f = parse_buf_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "foo.proto");
        assert_eq!(f[0].rule_id.as_deref(), Some("FIELD_LOWER_SNAKE_CASE"));
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].line_end, 3);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("lower_snake_case"));
        assert_eq!(f[1].line_end, 9);
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_buf_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn blank_lines_are_tolerated() {
        let text = "\n\n  \n";
        let f = parse_buf_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_line_is_a_parse_error() {
        let err = parse_buf_output("this is not json\n").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_type_field_drops_rule_id() {
        let text = r#"{"path":"x.proto","start_line":1,"end_line":1,"message":"m"}"#;
        let f = parse_buf_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let text = r#"{"path":"x.proto","start_line":0,"end_line":0,"type":"R","message":"m"}"#;
        let f = parse_buf_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn end_line_below_start_line_is_clamped_to_start() {
        // Defensive: shouldn't happen in practice but guards against
        // an empty range yielding a bogus inverted line span.
        let text = r#"{"path":"x.proto","start_line":5,"end_line":2,"type":"R","message":"m"}"#;
        let f = parse_buf_output(text).expect("ok");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].line_end, 5);
    }
}
