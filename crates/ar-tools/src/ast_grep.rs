//! ast-grep runner. Parses `ast-grep scan --json=stream` output.
//!
//! ast-grep matches structural patterns against tree-sitter ASTs and
//! reports rule violations. We invoke it in scan mode, which expects
//! either a `sgconfig.yml` at the repo root or rule files under
//! `rules/` / `.ast-grep/`. When neither is present, ast-grep exits
//! cleanly with empty output (no rules → no findings) and the runner
//! returns an empty Vec — exactly what we want.
//!
//! Output format: one JSON object per line ("stream" mode) with
//! `{ruleId, severity, message, file, range: {start: {line, column},
//! end: {line, column}}}`. Lines are 0-indexed in ast-grep's JSON;
//! we convert to 1-indexed before emitting.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "ast-grep";

#[derive(Debug, Deserialize)]
struct AstGrepFinding {
    #[serde(rename = "ruleId")]
    rule_id: String,
    #[serde(default)]
    severity: String,
    message: String,
    file: String,
    range: AstGrepRange,
}

#[derive(Debug, Deserialize)]
struct AstGrepRange {
    start: AstGrepPosition,
    end: AstGrepPosition,
}

#[derive(Debug, Deserialize)]
struct AstGrepPosition {
    /// 0-indexed line number in ast-grep's output. We emit 1-indexed.
    line: u32,
}

/// Parse newline-delimited JSON (`--json=stream`). One [`Finding`] per
/// non-empty line. Empty lines and trailing whitespace are tolerated;
/// any malformed line aborts with [`RunnerError::Parse`] so a bad
/// invocation fails loudly rather than silently dropping findings.
pub fn parse_ast_grep_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let f: AstGrepFinding = serde_json::from_str(line).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: format!("line {}: {e}", idx + 1),
        })?;
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: Some(f.rule_id),
            path: f.file,
            line_start: f.range.start.line.saturating_add(1),
            line_end: f.range.end.line.saturating_add(1),
            severity: severity_from(&f.severity),
            message: f.message,
        });
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

pub struct AstGrepRunner;

#[async_trait]
impl LinterRunner for AstGrepRunner {
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
            "ast-grep",
            vec!["scan".into(), "--json=stream".into()],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_ast_grep_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_stream_output() {
        let text = "\
{\"ruleId\":\"no-unwrap\",\"severity\":\"error\",\"message\":\"avoid .unwrap()\",\"file\":\"src/main.rs\",\"range\":{\"start\":{\"line\":4,\"column\":12},\"end\":{\"line\":4,\"column\":20}}}
{\"ruleId\":\"prefer-let-else\",\"severity\":\"warning\",\"message\":\"use let-else\",\"file\":\"src/lib.rs\",\"range\":{\"start\":{\"line\":10,\"column\":4},\"end\":{\"line\":13,\"column\":5}}}
";
        let f = parse_ast_grep_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        // 0-indexed → 1-indexed conversion: line 4 → 5
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].rule_id.as_deref(), Some("no-unwrap"));
        assert_eq!(f[0].severity, Severity::Error);
        assert_eq!(f[0].path, "src/main.rs");
        assert_eq!(f[1].line_start, 11);
        assert_eq!(f[1].line_end, 14);
        assert_eq!(f[1].severity, Severity::Warning);
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_ast_grep_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn blank_lines_are_tolerated() {
        let text = "\n\n  \n";
        let f = parse_ast_grep_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_line_is_a_parse_error() {
        let text = "this is not json\n";
        let err = parse_ast_grep_output(text).expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let text = r#"{"ruleId":"r","severity":"info","message":"m","file":"f","range":{"start":{"line":0,"column":0},"end":{"line":0,"column":1}}}"#;
        let f = parse_ast_grep_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_severity_falls_back_to_note() {
        // serde(default) on the field allows omission; the empty
        // string then routes through severity_from to Note.
        let text = r#"{"ruleId":"r","message":"m","file":"f","range":{"start":{"line":0,"column":0},"end":{"line":0,"column":1}}}"#;
        let f = parse_ast_grep_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn line_zero_becomes_one_after_conversion() {
        // ast-grep uses 0-indexed lines; we emit 1-indexed.
        let text = r#"{"ruleId":"r","severity":"error","message":"m","file":"f","range":{"start":{"line":0,"column":0},"end":{"line":0,"column":3}}}"#;
        let f = parse_ast_grep_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }
}
