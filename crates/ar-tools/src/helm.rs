//! helm-lint runner. Parses `helm lint` text output for Helm chart
//! issues.
//!
//! helm lint catches Chart.yaml convention violations, template
//! parse errors, schema violations against `values.schema.json`,
//! and missing-but-recommended fields. Routes when a `Chart.yaml`
//! appears in the diff; we invoke `helm lint <chart-dir>` for each
//! distinct chart directory found, since helm operates on chart
//! roots not individual files.
//!
//! Output format: text, one finding per line shaped
//! `[LEVEL] path: message` plus banner / summary lines we skip.
//! Some messages embed inline source-position hints like
//! `(mychart/templates/foo.yaml:5)` — we extract the line number
//! when present, otherwise default to line 1.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

const TOOL: &str = "helm";

fn finding_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `[LEVEL] path: rest of message`
        Regex::new(r"^\[(?P<level>[A-Z]+)\] (?P<path>[^:]+): (?P<msg>.+)$")
            .expect("helm finding regex compiles")
    })
}

fn line_hint_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Inline source hint like `(mychart/templates/foo.yaml:5)`.
        // We pull the trailing :NUMBER) and parse it.
        Regex::new(r":(?P<line>\d+)\)").expect("helm line-hint regex compiles")
    })
}

pub fn parse_helm_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let re = finding_regex();
    let line_re = line_hint_regex();
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with("==>") || line.starts_with("Error:") {
            continue;
        }
        let Some(caps) = re.captures(line) else {
            continue;
        };
        let level = &caps["level"];
        let path = caps["path"].to_string();
        let message = caps["msg"].to_string();
        let line_no = line_re
            .captures(&message)
            .and_then(|c| c.name("line"))
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .map(|n| n.max(1))
            .unwrap_or(1);
        out.push(Finding {
            source_tool: TOOL.into(),
            // helm doesn't have rule ids; surface the level as
            // a simple identifier so the LLM can group findings.
            rule_id: Some(level.to_ascii_lowercase()),
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
        "ERROR" | "FATAL" => Severity::Error,
        "WARNING" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct HelmRunner {
    /// Distinct chart-root directories (parents of Chart.yaml). Empty
    /// means "no charts in the diff; skip".
    pub chart_dirs: Vec<String>,
}

#[async_trait]
impl LinterRunner for HelmRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        if self.chart_dirs.is_empty() {
            return Ok(vec![]);
        }
        // helm lint takes a chart directory and emits findings for
        // that one chart. Iterate over every changed chart and
        // accumulate; if helm is missing the runner returns
        // Ok(empty) per the run_in_sandbox contract.
        let mut combined = String::new();
        for dir in &self.chart_dirs {
            let output = run_in_sandbox(
                sandbox,
                repo_dir,
                "helm",
                vec!["lint".into(), dir.clone()],
                vec![],
            )
            .await?;
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            combined.push('\n');
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            combined.push('\n');
        }
        if combined.trim().is_empty() {
            return Ok(vec![]);
        }
        // Deduplicate identical findings: helm can emit the same
        // INFO across multiple charts in repos with shared
        // conventions. The combination of (path, line, level,
        // message) is unique enough.
        let raw_findings = parse_helm_output(&combined)?;
        let mut seen = BTreeSet::new();
        let mut out = Vec::with_capacity(raw_findings.len());
        for f in raw_findings {
            let key = (
                f.path.clone(),
                f.line_start,
                f.rule_id.clone(),
                f.message.clone(),
            );
            if seen.insert(key) {
                out.push(f);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_helm_output() {
        let text = "\
==> Linting ./mychart
[INFO] Chart.yaml: icon is recommended
[WARNING] templates/deployment.yaml: deprecated apiVersion
[ERROR] templates/service.yaml: parse error at (mychart/templates/service.yaml:12)

Error: 1 chart(s) linted, 1 chart(s) failed
";
        let f = parse_helm_output(text).expect("ok");
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].path, "Chart.yaml");
        assert_eq!(f[0].rule_id.as_deref(), Some("info"));
        assert_eq!(f[0].severity, Severity::Note);
        assert_eq!(f[0].line_start, 1); // no line hint
        assert_eq!(f[1].severity, Severity::Warning);
        assert_eq!(f[2].severity, Severity::Error);
        // Inline hint `(...:12)` is extracted.
        assert_eq!(f[2].line_start, 12);
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_helm_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn only_banners_yields_zero_findings() {
        let text = "\
==> Linting ./chart-a
==> Linting ./chart-b
Error: 0 chart(s) failed
";
        let f = parse_helm_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn unrecognized_lines_are_silently_skipped() {
        let text = "\
random progress noise
[INFO] Chart.yaml: ok-shape note
";
        let f = parse_helm_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "Chart.yaml");
    }

    #[test]
    fn missing_line_hint_falls_back_to_one() {
        let text = "[ERROR] templates/x.yaml: something broke\n";
        let f = parse_helm_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn unknown_level_falls_back_to_note() {
        let text = "[DEBUG] x: y\n";
        let f = parse_helm_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn message_with_internal_colons_is_preserved() {
        let text = "[ERROR] templates/foo.yaml: yaml: line 5: bad token (foo.yaml:5)\n";
        let f = parse_helm_output(text).expect("ok");
        assert_eq!(f[0].path, "templates/foo.yaml");
        assert!(f[0].message.contains("yaml: line 5: bad token"));
        assert_eq!(f[0].line_start, 5);
    }
}
