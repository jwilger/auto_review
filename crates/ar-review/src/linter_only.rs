//! Linter-only review mode: build a [`ReviewOutput`] directly from
//! linter findings, no LLM call.
//!
//! Operators opt in via `.auto_review.yaml`'s `mode: linter_only`.
//! The orchestrator still clones the workspace and runs the full
//! linter pipeline; this module just maps the resulting findings
//! to the same review-comment shape the LLM path produces, so
//! everything downstream (verifier, mapper, poster) is unchanged.
//!
//! The verifier (Simple or Agentic) is skipped — there's no LLM
//! output to verify. Linter findings are trusted as-is; if a
//! repo wants noisy linters silenced, that's what `disabled_tools:`
//! is for.

use ar_prompts::{ReviewFinding, ReviewOutput, ReviewSeverity};
use ar_tools::{Finding, Severity};

/// Map a slice of linter [`Finding`]s into a [`ReviewOutput`] with
/// no walkthrough, no Mermaid diagram, and a summary that just
/// states the number of findings.
pub fn build_linter_only_output(findings: &[Finding]) -> ReviewOutput {
    let summary = match findings.len() {
        0 => "auto_review (linter-only mode): no findings.".to_string(),
        1 => "auto_review (linter-only mode): 1 finding from the bundled linters.".to_string(),
        n => format!("auto_review (linter-only mode): {n} findings from the bundled linters."),
    };
    let review_findings = findings.iter().map(finding_to_review_finding).collect();
    ReviewOutput {
        summary,
        walkthrough: String::new(),
        mermaid: String::new(),
        findings: review_findings,
    }
}

fn finding_to_review_finding(f: &Finding) -> ReviewFinding {
    let line_end = if f.line_end > f.line_start {
        Some(f.line_end)
    } else {
        None
    };
    let prefix = match f.rule_id.as_deref() {
        Some(rule) => format!("[{}/{rule}] ", f.source_tool),
        None => format!("[{}] ", f.source_tool),
    };
    ReviewFinding {
        path: f.path.clone(),
        line_start: f.line_start,
        line_end,
        severity: severity_to_review(f.severity),
        message: format!("{prefix}{}", f.message),
    }
}

fn severity_to_review(s: Severity) -> ReviewSeverity {
    match s {
        Severity::Note => ReviewSeverity::Note,
        Severity::Warning => ReviewSeverity::Warning,
        Severity::Error => ReviewSeverity::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(
        tool: &str,
        rule: Option<&str>,
        path: &str,
        line: u32,
        end: u32,
        sev: Severity,
        msg: &str,
    ) -> Finding {
        Finding {
            source_tool: tool.into(),
            rule_id: rule.map(String::from),
            path: path.into(),
            line_start: line,
            line_end: end,
            severity: sev,
            message: msg.into(),
        }
    }

    #[test]
    fn empty_findings_produces_clean_summary_and_no_findings() {
        let out = build_linter_only_output(&[]);
        assert!(out.findings.is_empty());
        assert!(out.summary.contains("no findings"));
        assert!(out.walkthrough.is_empty());
        assert!(out.mermaid.is_empty());
    }

    #[test]
    fn one_finding_produces_singular_summary() {
        let out = build_linter_only_output(&[finding(
            "ruff",
            Some("E501"),
            "src/x.py",
            7,
            7,
            Severity::Warning,
            "line too long",
        )]);
        assert_eq!(out.findings.len(), 1);
        assert!(out.summary.contains("1 finding"));
    }

    #[test]
    fn many_findings_produces_count_summary() {
        let findings = vec![
            finding("ruff", None, "a.py", 1, 1, Severity::Note, "x"),
            finding("ruff", None, "b.py", 2, 2, Severity::Warning, "y"),
            finding("ruff", None, "c.py", 3, 3, Severity::Error, "z"),
        ];
        let out = build_linter_only_output(&findings);
        assert_eq!(out.findings.len(), 3);
        assert!(out.summary.contains("3 findings"));
    }

    #[test]
    fn message_includes_tool_and_rule_prefix() {
        let f = finding(
            "shellcheck",
            Some("SC2086"),
            "build.sh",
            42,
            42,
            Severity::Warning,
            "double-quote variable",
        );
        let out = build_linter_only_output(&[f]);
        let msg = &out.findings[0].message;
        assert!(msg.starts_with("[shellcheck/SC2086]"));
        assert!(msg.contains("double-quote"));
    }

    #[test]
    fn message_omits_rule_when_finding_has_no_rule_id() {
        let f = finding(
            "gitleaks",
            None,
            "config/.env",
            1,
            1,
            Severity::Error,
            "AWS access key found",
        );
        let out = build_linter_only_output(&[f]);
        let msg = &out.findings[0].message;
        assert!(msg.starts_with("[gitleaks]"));
        // No `/` between tool and rule when there's no rule_id.
        assert!(!msg.starts_with("[gitleaks/"));
    }

    #[test]
    fn line_end_collapses_when_equal_to_line_start() {
        let f = finding(
            "eslint",
            Some("no-unused-vars"),
            "src/x.js",
            10,
            10,
            Severity::Note,
            "unused",
        );
        let out = build_linter_only_output(&[f]);
        assert_eq!(out.findings[0].line_start, 10);
        assert!(out.findings[0].line_end.is_none());
    }

    #[test]
    fn line_end_preserved_when_greater_than_line_start() {
        let f = finding(
            "semgrep",
            Some("xss-injection"),
            "src/header.js",
            5,
            8,
            Severity::Error,
            "innerHTML write",
        );
        let out = build_linter_only_output(&[f]);
        assert_eq!(out.findings[0].line_start, 5);
        assert_eq!(out.findings[0].line_end, Some(8));
    }

    #[test]
    fn severity_maps_one_to_one() {
        for (input, expected) in [
            (Severity::Note, ReviewSeverity::Note),
            (Severity::Warning, ReviewSeverity::Warning),
            (Severity::Error, ReviewSeverity::Error),
        ] {
            let f = finding("x", None, "a.rs", 1, 1, input, "m");
            let out = build_linter_only_output(&[f]);
            assert_eq!(out.findings[0].severity, expected);
        }
    }
}
