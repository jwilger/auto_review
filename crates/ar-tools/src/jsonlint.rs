//! jsonlint runner. Parses jsonlint's text output for JSON syntax
//! issues — trailing commas, unquoted keys, duplicate keys, missing
//! brackets.
//!
//! Routes on `.json` and `.jsonc` files. Distinct from prettier
//! (which only formats well-formed JSON) and taplo (which handles
//! TOML); jsonlint catches the validity issues that break parsers.
//!
//! There are several `jsonlint` implementations with subtly
//! different output shapes. The most common (jsonlint-py and the
//! Node `jsonlint` v1.x) emit one finding per line as
//! `path:line:col: message` or as a multi-line "Error: Parse error
//! on line N:" block. We parse the simple `path:line:col:` form;
//! repos using a different shape can disable jsonlint via
//! `.auto_review.yaml`.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

const TOOL: &str = "jsonlint";

fn line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `path:line:col: message`. The path can't contain colons
        // (no Windows support) so the first three colons separate
        // the fixed-shape fields.
        Regex::new(r"^(?P<path>[^:]+):(?P<line>\d+):(?P<col>\d+):\s*(?P<msg>.+)$")
            .expect("jsonlint regex compiles")
    })
}

pub fn parse_jsonlint_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let re = line_regex();
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(caps) = re.captures(line) else {
            continue; // banner / multi-line error context
        };
        let path = caps["path"].to_string();
        let line_no: u32 = caps["line"].parse().unwrap_or(0).max(1);
        let message = caps["msg"].to_string();
        out.push(Finding {
            source_tool: TOOL.into(),
            // jsonlint doesn't emit a discrete rule id; tag with a
            // stable label so findings group cleanly.
            rule_id: Some("syntax".into()),
            path,
            line_start: line_no,
            line_end: line_no,
            severity: Severity::Error,
            message,
        });
    }
    Ok(out)
}

pub struct JsonlintRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for JsonlintRunner {
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
        // -c (compact)  flat one-line-per-error output
        // -q (quiet)    suppress success banners
        let mut args = vec!["-c".into(), "-q".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "jsonlint", args, vec![]).await?;
        // jsonlint writes errors to stderr; combine streams so we
        // catch both shapes of the binary.
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        if combined.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_jsonlint_output(&combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_jsonlint_output() {
        let text = "\
package.json:3:5: Expecting property name enclosed in double quotes
config/app.json:12:1: Unexpected end of JSON input
";
        let f = parse_jsonlint_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "package.json");
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].rule_id.as_deref(), Some("syntax"));
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("property name"));
        assert_eq!(f[1].path, "config/app.json");
        assert_eq!(f[1].line_start, 12);
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_jsonlint_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn unparseable_lines_are_silently_skipped() {
        let text = "\
Error: Parse error on line 3:
... { \"foo\": \"bar\",
-----------------^
Expecting STRING, got '}'
";
        // None of these lines match the path:line:col: shape, so
        // they're filtered. Repos hitting the multi-line shape
        // can use a different jsonlint implementation or disable
        // this runner.
        let f = parse_jsonlint_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let text = "x.json:0:1: error\n";
        let f = parse_jsonlint_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn message_preserved_verbatim() {
        let text = "x.json:5:1: A: B: C\n";
        let f = parse_jsonlint_output(text).expect("ok");
        assert_eq!(f[0].message, "A: B: C");
    }

    #[test]
    fn all_findings_severity_error() {
        // jsonlint reports parse errors only — no warning tier,
        // no notes. Always Error so the LLM treats them as
        // blocking.
        let text = "x.json:1:1: m\ny.json:2:1: m\n";
        let f = parse_jsonlint_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| x.severity == Severity::Error));
    }
}
