//! mypy runner. Parses mypy's text output format.
//!
//! mypy is the de-facto Python type checker — distinct from ruff
//! (a linter) and bandit (a security scanner). Routes on `.py`
//! files alongside ruff in the legacy runner set. Without a project-level
//! config (mypy.ini, pyproject.toml [tool.mypy]) mypy emits a lot of false
//! positives.
//!
//! Output format: lines of
//! `path/to/file.py:LINE: severity: message [error-code]`.
//! mypy doesn't have a JSON reporter as of v1.x, so we parse the
//! text format. Lines that don't match (banners, summaries) are
//! silently skipped — mypy emits a trailing summary that has no
//! `path:line:` prefix.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

const TOOL: &str = "mypy";

fn line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Captures: path, line, (optional) col, severity, message,
        // (optional) error code in brackets at end.
        Regex::new(
            r"^(?P<path>[^:]+):(?P<line>\d+)(?::\d+)?: (?P<severity>error|warning|note): (?P<msg>.+?)(?:  \[(?P<code>[a-zA-Z0-9_-]+)\])?$",
        )
        .expect("mypy line regex compiles")
    })
}

pub fn parse_mypy_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let re = line_regex();
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(caps) = re.captures(line) else {
            continue; // banner / summary
        };
        let path = caps["path"].to_string();
        let line_no: u32 = caps["line"].parse().unwrap_or(0);
        let severity_str = &caps["severity"];
        let message = caps["msg"].to_string();
        let rule_id = caps.name("code").map(|m| m.as_str().to_string());
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id,
            path,
            line_start: line_no.max(1),
            line_end: line_no.max(1),
            severity: severity_from(severity_str),
            message,
        });
    }
    Ok(out)
}

fn severity_from(level: &str) -> Severity {
    match level {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct MypyRunner;

#[async_trait]
impl LinterRunner for MypyRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // `--no-error-summary` strips the trailing "Found N errors"
        // line so we don't have to skip it; `--show-error-codes`
        // surfaces the [error-code] suffix that becomes our rule_id.
        // `--no-pretty` flattens multi-line error rendering.
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "mypy",
            vec![
                "--no-error-summary".into(),
                "--show-error-codes".into(),
                "--no-pretty".into(),
                "--no-color-output".into(),
                ".".into(),
            ],
            vec![],
        )
        .await?;
        // mypy writes findings to stdout when given a path argument.
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_mypy_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_mypy_output() {
        let text = "\
src/api.py:25: error: Argument 1 has incompatible type \"int\"; expected \"str\"  [arg-type]
src/api.py:30: warning: Returning Any from function declared to return \"int\"  [no-any-return]
src/util.py:7: note: Revealed type is \"builtins.str\"
";
        let f = parse_mypy_output(text).expect("ok");
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].path, "src/api.py");
        assert_eq!(f[0].line_start, 25);
        assert_eq!(f[0].rule_id.as_deref(), Some("arg-type"));
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("incompatible type"));
        assert_eq!(f[1].severity, Severity::Warning);
        assert_eq!(f[1].rule_id.as_deref(), Some("no-any-return"));
        assert_eq!(f[2].severity, Severity::Note);
        assert!(f[2].rule_id.is_none()); // notes have no error code
    }

    #[test]
    fn parses_lines_with_column_numbers() {
        let text = "src/x.py:7:12: error: Bad value  [arg-type]\n";
        let f = parse_mypy_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line_start, 7);
        assert_eq!(f[0].rule_id.as_deref(), Some("arg-type"));
    }

    #[test]
    fn skips_summary_and_banner_lines() {
        let text = "\
mypy version 1.7.0
src/x.py:1: error: Whatever  [error-code]
Found 1 error in 1 file (checked 23 source files)
";
        let f = parse_mypy_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "src/x.py");
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_mypy_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        // The regex only captures error/warning/note literally, so
        // an unrecognised severity word doesn't match and the line
        // is skipped — that's the safer behaviour than guessing.
        let text = "src/x.py:1: hint: something  [foo]\n";
        let f = parse_mypy_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        // mypy occasionally emits :0: for module-level errors.
        let text = "src/x.py:0: error: Module-level issue  [misc]\n";
        let f = parse_mypy_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn message_without_error_code_drops_rule_id() {
        let text = "src/x.py:5: error: An old-style mypy message\n";
        let f = parse_mypy_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert!(f[0].rule_id.is_none());
        assert_eq!(f[0].message, "An old-style mypy message");
    }
}
