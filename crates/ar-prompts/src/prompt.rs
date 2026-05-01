use ar_tools::Finding;

/// Inputs for rendering the review user prompt.
#[derive(Debug, Clone)]
pub struct ReviewPromptInputs<'a> {
    pub repo_full_name: &'a str,
    pub pr_number: u64,
    pub pr_title: &'a str,
    pub pr_body: &'a str,
    pub diff: &'a str,
    pub changed_files: &'a [String],
    /// Pre-computed static-analysis findings to surface to the model so it
    /// can corroborate, expand on, or dismiss them. Empty if no linters ran
    /// or none reported anything.
    pub linter_findings: &'a [Finding],
}

const SYSTEM_PROMPT: &str = "\
You are an expert code reviewer. Your job is to review a pull-request diff \
and produce structured feedback that will be posted as inline comments on a \
Forgejo PR.

Rules:
- Output **only** a JSON object that matches the provided schema. Do not \
  emit prose, markdown fences, or any text outside the JSON.
- `summary`: 1–3 sentences for the top-level review body.
- `walkthrough` (optional): a longer markdown walkthrough of what changed \
  and why it matters. Use bullet lists per file or per theme. Leave empty \
  when the PR is small enough that the summary suffices.
- `mermaid` (optional): a Mermaid diagram source (no fence — the text inside \
  the fence) when control flow or sequence changes meaningfully. Leave \
  empty otherwise.
- `findings`: cite specific lines from the diff using 1-based new-file line \
  numbers. Be concrete and actionable. If you have nothing useful to say, \
  return `findings: []` with a `summary` of why.
- Do not flag style/formatting unless the codebase has explicit conventions \
  in the diff. Do not invent issues to look thorough.
- Static-analysis findings (when present) are mechanical signals — \
  corroborate, expand, or dismiss them with judgment; do not blindly \
  forward them.
- Severity: `error` = bug or security issue; `warning` = likely bug or risky \
  change; `note` = optional improvement.
";

/// The static system prompt that anchors the model's persona and
/// JSON-output contract.
pub fn system_prompt() -> &'static str {
    SYSTEM_PROMPT
}

/// Render the user-facing prompt the LLM will see. The system prompt is
/// returned separately by [`system_prompt`].
pub fn render_review_prompt(inputs: &ReviewPromptInputs<'_>) -> String {
    let mut out = String::with_capacity(inputs.diff.len() + 512);

    out.push_str("Repository: ");
    out.push_str(inputs.repo_full_name);
    out.push_str("\nPull request: #");
    out.push_str(&inputs.pr_number.to_string());
    out.push_str(" — ");
    out.push_str(inputs.pr_title);
    out.push('\n');

    if !inputs.pr_body.is_empty() {
        out.push_str("\nPR description:\n");
        out.push_str(inputs.pr_body);
        out.push('\n');
    }

    if !inputs.changed_files.is_empty() {
        out.push_str("\nChanged files:\n");
        for f in inputs.changed_files {
            out.push_str("- ");
            out.push_str(f);
            out.push('\n');
        }
    }

    if !inputs.linter_findings.is_empty() {
        out.push_str("\nStatic-analysis findings:\n");
        for f in inputs.linter_findings {
            let rule = f.rule_id.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "- [{}/{}] {}:{} ({:?}) {}\n",
                f.source_tool, rule, f.path, f.line_start, f.severity, f.message
            ));
        }
    }

    out.push_str("\nUnified diff:\n```diff\n");
    out.push_str(inputs.diff);
    if !inputs.diff.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");

    out.push_str("\nReview the diff above and emit the JSON object described by the schema.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_tools::Severity;

    fn sample<'a>(
        diff: &'a str,
        files: &'a [String],
        findings: &'a [Finding],
    ) -> ReviewPromptInputs<'a> {
        ReviewPromptInputs {
            repo_full_name: "alice/widgets",
            pr_number: 42,
            pr_title: "fix off-by-one",
            pr_body: "closes #7",
            diff,
            changed_files: files,
            linter_findings: findings,
        }
    }

    #[test]
    fn includes_repo_and_pr_number() {
        let files = vec!["src/main.rs".to_string()];
        let p = render_review_prompt(&sample("diff body", &files, &[]));
        assert!(p.contains("alice/widgets"));
        assert!(p.contains("#42"));
    }

    #[test]
    fn includes_pr_title_and_body() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&sample("d", &files, &[]));
        assert!(p.contains("fix off-by-one"));
        assert!(p.contains("closes #7"));
    }

    #[test]
    fn includes_diff_verbatim() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&sample("@@ -1 +1 @@\n-a\n+b\n", &files, &[]));
        assert!(p.contains("@@ -1 +1 @@"));
        assert!(p.contains("+b"));
    }

    #[test]
    fn lists_changed_files() {
        let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let p = render_review_prompt(&sample("d", &files, &[]));
        assert!(p.contains("src/a.rs"));
        assert!(p.contains("src/b.rs"));
    }

    #[test]
    fn handles_empty_pr_body() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&ReviewPromptInputs {
            repo_full_name: "x/y",
            pr_number: 1,
            pr_title: "t",
            pr_body: "",
            diff: "d",
            changed_files: &files,
            linter_findings: &[],
        });
        assert!(p.contains("t"));
    }

    #[test]
    fn system_prompt_mentions_json_schema() {
        let s = system_prompt();
        assert!(s.to_lowercase().contains("json"));
    }

    #[test]
    fn omits_findings_section_when_empty() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&sample("d", &files, &[]));
        assert!(!p.to_lowercase().contains("static-analysis findings"));
    }

    #[test]
    fn includes_findings_section_when_present() {
        let files: Vec<String> = vec![];
        let findings = vec![
            Finding {
                source_tool: "ruff".into(),
                rule_id: Some("E501".into()),
                path: "src/x.py".into(),
                line_start: 12,
                line_end: 12,
                severity: Severity::Warning,
                message: "Line too long".into(),
            },
            Finding {
                source_tool: "shellcheck".into(),
                rule_id: Some("SC2034".into()),
                path: "scripts/build.sh".into(),
                line_start: 3,
                line_end: 3,
                severity: Severity::Note,
                message: "var unused".into(),
            },
        ];
        let p = render_review_prompt(&sample("d", &files, &findings));
        assert!(p.to_lowercase().contains("static-analysis findings"));
        assert!(p.contains("ruff"));
        assert!(p.contains("E501"));
        assert!(p.contains("src/x.py:12"));
        assert!(p.contains("Line too long"));
        assert!(p.contains("shellcheck"));
        assert!(p.contains("SC2034"));
    }

    #[test]
    fn finding_with_no_rule_id_renders_as_dash() {
        let files: Vec<String> = vec![];
        let findings = vec![Finding {
            source_tool: "custom".into(),
            rule_id: None,
            path: "a".into(),
            line_start: 1,
            line_end: 1,
            severity: Severity::Note,
            message: "m".into(),
        }];
        let p = render_review_prompt(&sample("d", &files, &findings));
        assert!(p.contains("[custom/-]"));
    }
}
