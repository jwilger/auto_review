//! nilaway runner. Parses nilaway's text output for Go nil-pointer
//! panics.
//!
//! nilaway is a static analyzer from Uber specifically focused on
//! finding code that can dereference nil at runtime. Distinct from
//! golangci-lint / gosec / staticcheck — those tools have nil-check
//! rules but nilaway's flow-sensitive analysis catches cases the
//! others miss (interface receivers, error-return patterns, map
//! lookups). Routes alongside the other Go linters on `.go` files.
//!
//! Output format: text, one finding per line shaped
//! `path:line:col: error: message`. nilaway always emits findings
//! at severity "error" — there's no convention/warning tier.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

const TOOL: &str = "nilaway";

fn line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `path:line:col: severity: message`. The path may
        // contain colons on Windows but we ship Linux-only;
        // anchor the first three segments by digits then take
        // the rest as the message.
        Regex::new(
            r"^(?P<path>[^:]+):(?P<line>\d+):(?P<col>\d+):\s*(?P<sev>error|warning|note):\s*(?P<msg>.+)$",
        )
        .expect("nilaway line regex compiles")
    })
}

pub fn parse_nilaway_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let re = line_regex();
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(caps) = re.captures(line) else {
            continue; // banner / build progress / other tool noise
        };
        let path = caps["path"].to_string();
        let line_no: u32 = caps["line"].parse().unwrap_or(0).max(1);
        let severity = severity_from(&caps["sev"]);
        let message = caps["msg"].to_string();
        out.push(Finding {
            source_tool: TOOL.into(),
            // nilaway doesn't emit a discrete rule id; everything
            // is a nil-flow finding. Tag with a stable label so
            // findings group cleanly in dashboards.
            rule_id: Some("nil-flow".into()),
            path,
            line_start: line_no,
            line_end: line_no,
            severity,
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

pub struct NilawayRunner;

#[async_trait]
impl LinterRunner for NilawayRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // nilaway analyzes whole packages (it needs type info from
        // go/packages); we point it at the module root and let it
        // discover everything. nilaway exits non-zero on findings;
        // we read both stdout and stderr since nilaway's
        // diagnostic stream historically split between the two.
        let output =
            run_in_sandbox(sandbox, repo_dir, "nilaway", vec!["./...".into()], vec![]).await?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        if combined.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_nilaway_output(&combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_nilaway_output() {
        let text = "\
cmd/main.go:42:5: error: Potential nil panic detected. Observed nil flow from source to dereference point.
util/cache.go:7:1: error: Annotation: Read from a value that may be nil.
";
        let f = parse_nilaway_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "cmd/main.go");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].rule_id.as_deref(), Some("nil-flow"));
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("nil panic"));
        assert_eq!(f[1].path, "util/cache.go");
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_nilaway_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn banner_lines_are_skipped() {
        let text = "\
Analyzing packages...
cmd/main.go:1:1: error: bad
done.
";
        let f = parse_nilaway_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "cmd/main.go");
    }

    #[test]
    fn warning_severity_maps_to_warning() {
        let text = "x.go:1:1: warning: questionable\n";
        let f = parse_nilaway_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Warning);
    }

    #[test]
    fn note_severity_falls_back_to_note() {
        let text = "x.go:1:1: note: hint\n";
        let f = parse_nilaway_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn unrecognized_severity_is_skipped() {
        // Lines without one of error/warning/note in the severity
        // slot don't match the regex and get filtered.
        let text = "x.go:1:1: debug: nothing\n";
        let f = parse_nilaway_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        // Defensive: nilaway shouldn't emit line 0 but be safe.
        let text = "x.go:0:1: error: m\n";
        let f = parse_nilaway_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn message_preserved_verbatim() {
        let text = "x.go:5:1: error: A: B: C: D\n";
        let f = parse_nilaway_output(text).expect("ok");
        assert_eq!(f[0].message, "A: B: C: D");
    }
}
