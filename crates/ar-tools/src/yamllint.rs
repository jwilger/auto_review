//! yamllint runner. yamllint has no JSON output, so we parse the
//! `--format parsable` text format:
//!
//! `path:line:col: [level] message (rule)`

use crate::finding::{Finding, Severity};
use crate::runner::{LinterRunner, RunnerError};
use async_trait::async_trait;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;
use tokio::process::Command;

const TOOL: &str = "yamllint";

fn parsable_line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Captures: path, line, col, level, message, rule
        Regex::new(r"^(?P<path>[^:]+):(?P<line>\d+):(?P<col>\d+): \[(?P<level>\w+)\] (?P<msg>.+?)(?: \((?P<rule>[^)]+)\))?$")
            .expect("yamllint parsable regex compiles")
    })
}

pub fn parse_yamllint_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let re = parsable_line_regex();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(caps) = re.captures(line) else {
            // Skip non-matching lines (banners, summaries) rather than
            // failing the whole batch.
            continue;
        };
        let path = caps["path"].to_string();
        let line_no: u32 = caps["line"].parse().unwrap_or(0);
        let level = &caps["level"];
        let message = caps["msg"].to_string();
        let rule = caps.name("rule").map(|m| m.as_str().to_string());
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: rule,
            path,
            line_start: line_no,
            line_end: line_no,
            severity: severity_from(level),
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

pub struct YamlLintRunner {
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for YamlLintRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(&self, repo_dir: &Path) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        let output = match Command::new("yamllint")
            .args(["--format", "parsable"])
            .args(&self.files)
            .current_dir(repo_dir)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(RunnerError::Io(e)),
        };
        // yamllint writes findings to stdout in parsable format.
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_yamllint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_parsable_output() {
        let text = "\
config/app.yml:3:1: [warning] missing document start \"---\" (document-start)
config/app.yml:7:81: [error] line too long (98 > 80 characters) (line-length)
deploy/k8s.yaml:12:5: [warning] wrong indentation: expected 4 but found 2 (indentation)
";
        let f = parse_yamllint_output(text).expect("ok");
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].path, "config/app.yml");
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].rule_id.as_deref(), Some("document-start"));
        assert_eq!(f[1].severity, Severity::Error);
        assert_eq!(f[1].rule_id.as_deref(), Some("line-length"));
    }

    #[test]
    fn handles_lines_without_rule_suffix() {
        // Some yamllint messages have no `(rule)` suffix.
        let text = "x.yml:1:1: [warning] generic complaint\n";
        let f = parse_yamllint_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert!(f[0].rule_id.is_none());
        assert_eq!(f[0].message, "generic complaint");
    }

    #[test]
    fn skips_empty_input() {
        let f = parse_yamllint_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn skips_unrecognized_lines() {
        let text = "\
some banner output that doesn't match
config/app.yml:3:1: [warning] msg (rule)
trailing junk
";
        let f = parse_yamllint_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "config/app.yml");
    }

    #[test]
    fn unrecognized_level_is_note() {
        let text = "x:1:1: [info] m (r)\n";
        let f = parse_yamllint_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }
}
