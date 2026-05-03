//! pylint runner. Parses `pylint --output-format=json` output for
//! Python lint findings.
//!
//! pylint covers a different rule set from ruff: stricter design
//! checks (cyclomatic complexity, too-many-arguments, similar-code),
//! deeper semantic analysis (E1101 no-member, R0902 too-many-
//! instance-attributes), and convention rules ruff hasn't ported.
//! Routes alongside ruff/mypy/bandit on `.py` files in the legacy runner set.
//!
//! Output: native JSON, top-level array of `{type, line, column,
//! endLine, endColumn, path, symbol, message, message-id}`. The
//! `type` field tier is fatal/error/warning/convention/refactor/
//! info; we map fatal/error → Error, warning → Warning, the rest
//! → Note. `symbol` (e.g. `missing-docstring`) becomes our
//! human-readable rule_id; we prefer it over `message-id` (C0116
//! etc.) because the symbol surfaces the rule's intent.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "pylint";

#[derive(Debug, Deserialize)]
struct PylintFinding {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    line: u32,
    #[serde(default, rename = "endLine")]
    end_line: Option<u32>,
    #[serde(default)]
    path: String,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    message: String,
}

pub fn parse_pylint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<PylintFinding> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .into_iter()
        .map(|p| {
            let start = p.line.max(1);
            let end = p.end_line.map(|l| l.max(start)).unwrap_or(start);
            Finding {
                source_tool: TOOL.into(),
                rule_id: p.symbol,
                path: p.path,
                line_start: start,
                line_end: end,
                severity: severity_from(&p.kind),
                message: p.message,
            }
        })
        .collect())
}

fn severity_from(kind: &str) -> Severity {
    match kind.to_ascii_lowercase().as_str() {
        "fatal" | "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct PylintRunner;

#[async_trait]
impl LinterRunner for PylintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // pylint exits non-zero on findings; --exit-zero treats
        // them as 0-exit so we can read stdout cleanly.
        // --recursive=y walks subdirs without an explicit list.
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "pylint",
            vec![
                "--output-format=json".into(),
                "--exit-zero".into(),
                "--recursive=y".into(),
                ".".into(),
            ],
            vec![],
        )
        .await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_pylint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_pylint_output() {
        let json = r#"[
            {
                "type": "convention",
                "module": "foo",
                "obj": "Foo.bar",
                "line": 10,
                "column": 4,
                "endLine": 10,
                "endColumn": 14,
                "path": "src/foo.py",
                "symbol": "missing-function-docstring",
                "message": "Missing function or method docstring",
                "message-id": "C0116"
            },
            {
                "type": "error",
                "module": "bar",
                "obj": "",
                "line": 3,
                "column": 0,
                "path": "src/bar.py",
                "symbol": "syntax-error",
                "message": "invalid syntax",
                "message-id": "E0001"
            },
            {
                "type": "warning",
                "module": "baz",
                "line": 7,
                "column": 0,
                "endLine": 9,
                "endColumn": 0,
                "path": "src/baz.py",
                "symbol": "unused-import",
                "message": "Unused import os",
                "message-id": "W0611"
            }
        ]"#;
        let f = parse_pylint_output(json).expect("ok");
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].path, "src/foo.py");
        assert_eq!(f[0].line_start, 10);
        assert_eq!(f[0].rule_id.as_deref(), Some("missing-function-docstring"));
        assert_eq!(f[0].severity, Severity::Note); // convention
        assert_eq!(f[1].severity, Severity::Error); // error
        assert_eq!(f[2].severity, Severity::Warning); // warning
        assert_eq!(f[2].line_end, 9);
    }

    #[test]
    fn fatal_severity_maps_to_error() {
        let json = r#"[
            {"type":"fatal","line":1,"path":"x.py","symbol":"r","message":"m"}
        ]"#;
        let f = parse_pylint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn refactor_and_info_fall_back_to_note() {
        let json = r#"[
            {"type":"refactor","line":1,"path":"x.py","symbol":"r","message":"m"},
            {"type":"information","line":2,"path":"y.py","symbol":"i","message":"m"}
        ]"#;
        let f = parse_pylint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
        assert_eq!(f[1].severity, Severity::Note);
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_pylint_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_pylint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn missing_symbol_drops_rule_id() {
        let json = r#"[
            {"type":"warning","line":1,"path":"x.py","message":"m"}
        ]"#;
        let f = parse_pylint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn missing_end_line_mirrors_start() {
        let json = r#"[
            {"type":"warning","line":42,"path":"x.py","symbol":"r","message":"m"}
        ]"#;
        let f = parse_pylint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].line_end, 42);
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[
            {"type":"warning","line":0,"path":"x.py","symbol":"r","message":"m"}
        ]"#;
        let f = parse_pylint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn end_line_below_start_is_clamped() {
        let json = r#"[
            {"type":"warning","line":50,"endLine":10,"path":"x.py","symbol":"r","message":"m"}
        ]"#;
        let f = parse_pylint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 50);
        assert_eq!(f[0].line_end, 50);
    }
}
