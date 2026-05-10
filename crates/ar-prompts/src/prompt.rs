/// Inputs for rendering the review user prompt.
#[derive(Debug, Clone)]
pub struct ReviewPromptInputs<'a> {
    pub repo_full_name: &'a str,
    pub pr_number: u64,
    pub pr_title: &'a str,
    pub pr_body: &'a str,
    pub diff: &'a str,
    pub changed_files: &'a [String],
    /// Free-form repo-author guidelines from `.auto_review.yaml`. Rendered
    /// as a top-level section so the model treats them as authoritative
    /// project conventions. Empty when no config is present.
    pub guidelines: &'a str,
    /// RAG-retrieved context: relevant code snippets from the index,
    /// matching learnings, co-change neighbors, etc. Free-form markdown
    /// — the orchestrator decides how to format it. Empty when the
    /// index hasn't been built or returned no matches.
    pub repo_context: &'a str,
    /// Commit SHA that was the head of the last completed review. When
    /// present, the prompt scopes review guidance to the delta since this SHA.
    pub previous_review_sha: Option<&'a str>,
    /// Concise existing PR discussion, formatted by the review pipeline from
    /// Forgejo comments. Empty when the PR has no useful discussion history.
    pub prior_discussion: &'a str,
}

const SYSTEM_PROMPT: &str = "\
You are an expert code reviewer. Your job is to review a pull-request diff \
and produce structured feedback that will be posted as inline comments on a \
Forgejo PR.

Rules:
- Output **only** a JSON object that matches the provided schema. Do not \
  emit prose, markdown fences, or any text outside the JSON.
- `summary`: 1–3 sentences for the top-level review body; do not restate \
  or paraphrase the PR description.
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
- Severity: `error` = bug or security issue; `warning` = likely bug or risky \
  change; `note` = optional improvement.
";

/// The static system prompt that anchors the model's persona and
/// JSON-output contract.
pub fn system_prompt() -> &'static str {
    SYSTEM_PROMPT
}

/// Cap the rendered PR body. Forgejo lets PR descriptions grow to
/// ~64 KiB, but the LLM context budget is dominated by the diff —
/// reserve most of it for code rather than letting a verbose
/// description crowd the diff out. 8 KiB comfortably holds any
/// real PR description; longer ones tend to be auto-generated
/// release notes / template forms that don't add reviewer signal.
const PR_BODY_MAX_BYTES: usize = 8_192;

/// Same justification for PR titles. Forgejo titles are typically
/// short, but a misbehaving caller could pass anything; cap so the
/// prompt header stays compact.
const PR_TITLE_MAX_BYTES: usize = 512;

/// Cap on the rendered guidelines section. Operators write these in
/// `.auto_review.yaml` so the field is operator-controlled, not
/// attacker-controlled — but a copy-pasted style guide can easily
/// hit tens of KB. 8 KiB matches PR body's cap and is generous for
/// real guidance.
const GUIDELINES_MAX_BYTES: usize = 8_192;

/// Cap on the rendered RAG context. The retrieval layer already
/// caps top-K, but each retrieved snippet is the full embedded
/// symbol body — for a repo with large functions plus many
/// learnings retrieved, the combined section can grow without
/// bound. 16 KiB lets us include a meaningful similar-code window
/// without burning the full reasoning-tier context.
const REPO_CONTEXT_MAX_BYTES: usize = 16_384;

/// Cap on prior PR discussion context. Existing conversation is reviewer
/// signal for avoiding duplicate feedback, but it must not crowd out the diff.
const PRIOR_DISCUSSION_MAX_BYTES: usize = 8_192;

/// Render the user-facing prompt the LLM will see. The system prompt is
/// returned separately by [`system_prompt`].
pub fn render_review_prompt(inputs: &ReviewPromptInputs<'_>) -> String {
    let mut out = String::with_capacity(inputs.diff.len() + 512);

    out.push_str("Repository: ");
    out.push_str(inputs.repo_full_name);
    out.push_str("\nPull request: #");
    out.push_str(&inputs.pr_number.to_string());
    out.push_str(" — ");
    push_capped(&mut out, inputs.pr_title, PR_TITLE_MAX_BYTES, "[truncated]");
    out.push('\n');

    if !inputs.guidelines.is_empty() {
        out.push_str("\nRepository guidelines (from .auto_review.yaml):\n");
        push_capped(
            &mut out,
            inputs.guidelines,
            GUIDELINES_MAX_BYTES,
            "\n[guidelines truncated]",
        );
        if !inputs.guidelines.ends_with('\n') {
            out.push('\n');
        }
    }

    if !inputs.pr_body.is_empty() {
        out.push_str("\nPR description:\n");
        push_capped(
            &mut out,
            inputs.pr_body,
            PR_BODY_MAX_BYTES,
            "\n[PR description truncated]",
        );
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

    if !inputs.repo_context.is_empty() {
        out.push_str("\nRepository context (retrieved from index):\n");
        push_capped(
            &mut out,
            inputs.repo_context,
            REPO_CONTEXT_MAX_BYTES,
            "\n[repo context truncated]",
        );
        if !inputs.repo_context.ends_with('\n') {
            out.push('\n');
        }
    }

    if let Some(previous_review_sha) = inputs.previous_review_sha {
        let previous_review_sha = short_sha(previous_review_sha);
        out.push_str("\nIncremental review context:\n");
        out.push_str("- This is an incremental review; review only the delta since ");
        out.push_str(previous_review_sha);
        out.push_str(".\n- In `walkthrough`, use heading `Δ since ");
        out.push_str(previous_review_sha);
        out.push_str(":` and leave `walkthrough` empty when nothing material changed.\n");
    }

    if !inputs.prior_discussion.is_empty() {
        out.push_str("\nPrior PR discussion:\n");
        push_capped(
            &mut out,
            inputs.prior_discussion,
            PRIOR_DISCUSSION_MAX_BYTES,
            "\n[prior PR discussion truncated]",
        );
        if !inputs.prior_discussion.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(
            "- Use this history to avoid re-raising addressed concerns unless new evidence remains in the diff.\n",
        );
    }

    out.push_str(
        "\nCI coverage advisory:\n\
         - When repository context or changed CI files show a relevant missing CI linter/check, emit a finding recommending the specific check and naming the CI gate where it appears absent.\n\
         - Default missing CI linter/check findings to warning severity.\n\
         - If project memory says maintainers declined a check, do not emit warning-level missing-CI-linter recommendations for that declined check; at most mention it as a note when the diff makes it newly relevant.\n\
         - Keep this advisory about CI coverage only; do not claim Auto Review ran the linter/check itself.\n",
    );

    out.push_str("\nUnified diff:\n```diff\n");
    out.push_str(inputs.diff);
    if !inputs.diff.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");

    out.push_str("\nReview the diff above and emit the JSON object described by the schema.\n");
    out
}

/// Append `s` to `out`, capping at `max_bytes`. When the cap fires,
/// the truncated prefix is appended followed by `marker` (no
/// trailing newline — the caller decides how to frame). Walks back
/// to a UTF-8 char boundary so multi-byte codepoints aren't split.
fn push_capped(out: &mut String, s: &str, max_bytes: usize, marker: &str) {
    if s.len() <= max_bytes {
        out.push_str(s);
        return;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    out.push_str(&s[..cut]);
    out.push_str(marker);
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(diff: &'a str, files: &'a [String]) -> ReviewPromptInputs<'a> {
        ReviewPromptInputs {
            repo_full_name: "alice/widgets",
            pr_number: 42,
            pr_title: "fix off-by-one",
            pr_body: "closes #7",
            diff,
            changed_files: files,
            guidelines: "",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        }
    }

    #[test]
    fn includes_repo_and_pr_number() {
        let files = vec!["src/main.rs".to_string()];
        let p = render_review_prompt(&sample("diff body", &files));
        assert!(p.contains("alice/widgets"));
        assert!(p.contains("#42"));
    }

    #[test]
    fn includes_pr_title_and_body() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&sample("d", &files));
        assert!(p.contains("fix off-by-one"));
        assert!(p.contains("closes #7"));
    }

    #[test]
    fn includes_diff_verbatim() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&sample("@@ -1 +1 @@\n-a\n+b\n", &files));
        assert!(p.contains("@@ -1 +1 @@"));
        assert!(p.contains("+b"));
    }

    #[test]
    fn lists_changed_files() {
        let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let p = render_review_prompt(&sample("d", &files));
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
            guidelines: "",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        });
        assert!(p.contains("t"));
    }

    #[test]
    fn includes_repo_context_section_when_provided() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&ReviewPromptInputs {
            repo_full_name: "x/y",
            pr_number: 1,
            pr_title: "t",
            pr_body: "",
            diff: "d",
            changed_files: &files,
            guidelines: "",
            repo_context: "Function `foo` is called by 14 callers in this repo.",
            previous_review_sha: None,
            prior_discussion: "",
        });
        assert!(p.contains("Repository context"));
        assert!(p.contains("14 callers"));
    }

    #[test]
    fn omits_repo_context_when_empty() {
        let p = render_review_prompt(&sample("d", &[]));
        assert!(!p.contains("Repository context"));
    }

    #[test]
    fn includes_repository_guidelines_when_provided() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&ReviewPromptInputs {
            repo_full_name: "x/y",
            pr_number: 1,
            pr_title: "t",
            pr_body: "",
            diff: "d",
            changed_files: &files,
            guidelines: "Always prefer total functions over partial.",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        });
        assert!(p.contains("Repository guidelines"));
        assert!(p.contains("total functions"));
    }

    #[test]
    fn omits_guidelines_section_when_empty() {
        let p = render_review_prompt(&sample("d", &[]));
        assert!(!p.contains("Repository guidelines"));
    }

    #[test]
    fn system_prompt_mentions_json_schema() {
        let s = system_prompt();
        assert!(s.to_lowercase().contains("json"));
    }

    #[test]
    fn system_prompt_tells_summary_not_to_restate_pr_description() {
        let s = system_prompt().to_lowercase();

        assert!(
            s.contains("do not restate") || s.contains("do not paraphrase"),
            "system prompt should tell the model not to restate or paraphrase the PR description in `summary`; prompt was:\n{}",
            system_prompt()
        );
        assert!(
            s.contains("summary") && s.contains("pr description"),
            "anti-restatement instruction should explicitly connect `summary` with the PR description; prompt was:\n{}",
            system_prompt()
        );
    }

    #[test]
    fn omits_findings_section_when_empty() {
        let files: Vec<String> = vec![];
        let p = render_review_prompt(&sample("d", &files));
        assert!(!p.to_lowercase().contains("static-analysis findings"));
    }

    #[test]
    fn pr_body_is_capped_at_8kib() {
        // Forgejo accepts ~64 KiB PR descriptions. Without a cap,
        // a release-notes-style body would crowd out the diff in
        // the LLM's context. Cap at 8 KiB and emit a truncation
        // marker so the model can see the description was abridged.
        let files: Vec<String> = vec![];
        let huge_body = "x".repeat(20_000);
        let inputs = ReviewPromptInputs {
            repo_full_name: "o/r",
            pr_number: 1,
            pr_title: "t",
            pr_body: &huge_body,
            diff: "diff",
            changed_files: &files,
            guidelines: "",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        };
        let p = render_review_prompt(&inputs);
        assert!(p.contains("[PR description truncated]"));
        // Total prompt should be under ~9 KiB plus boilerplate,
        // not the full 20 KiB body.
        assert!(
            p.len() < 12_000,
            "expected capped prompt, got {} bytes",
            p.len()
        );
    }

    #[test]
    fn pr_title_is_capped_at_512_bytes() {
        let files: Vec<String> = vec![];
        let huge_title = "T".repeat(2_000);
        let inputs = ReviewPromptInputs {
            repo_full_name: "o/r",
            pr_number: 1,
            pr_title: &huge_title,
            pr_body: "",
            diff: "diff",
            changed_files: &files,
            guidelines: "",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        };
        let p = render_review_prompt(&inputs);
        assert!(p.contains("[truncated]"));
        // The title should not appear in full.
        assert!(!p.contains(&"T".repeat(1_000)));
    }

    #[test]
    fn guidelines_are_capped_at_8kib() {
        let files: Vec<String> = vec![];
        let huge_guidelines = "g".repeat(20_000);
        let inputs = ReviewPromptInputs {
            repo_full_name: "o/r",
            pr_number: 1,
            pr_title: "t",
            pr_body: "",
            diff: "diff",
            changed_files: &files,
            guidelines: &huge_guidelines,
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        };
        let p = render_review_prompt(&inputs);
        assert!(p.contains("[guidelines truncated]"));
        assert!(p.len() < 12_000);
    }

    #[test]
    fn repo_context_is_capped_at_16kib() {
        let files: Vec<String> = vec![];
        let huge_context = "c".repeat(40_000);
        let inputs = ReviewPromptInputs {
            repo_full_name: "o/r",
            pr_number: 1,
            pr_title: "t",
            pr_body: "",
            diff: "diff",
            changed_files: &files,
            guidelines: "",
            repo_context: &huge_context,
            previous_review_sha: None,
            prior_discussion: "",
        };
        let p = render_review_prompt(&inputs);
        assert!(p.contains("[repo context truncated]"));
        assert!(p.len() < 20_000);
    }

    #[test]
    fn pr_body_under_cap_passes_through_unchanged() {
        let files: Vec<String> = vec![];
        let inputs = ReviewPromptInputs {
            repo_full_name: "o/r",
            pr_number: 1,
            pr_title: "Fix",
            pr_body: "Closes #42 — thanks!",
            diff: "diff",
            changed_files: &files,
            guidelines: "",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        };
        let p = render_review_prompt(&inputs);
        assert!(p.contains("Closes #42 — thanks!"));
        assert!(!p.contains("[PR description truncated]"));
    }

    #[test]
    fn incremental_review_prompt_scopes_walkthrough_to_previous_sha_delta() {
        let files = vec!["src/lib.rs".to_string()];
        let inputs = ReviewPromptInputs {
            repo_full_name: "o/r",
            pr_number: 14,
            pr_title: "update review prompt",
            pr_body: "",
            diff: "@@ -1 +1 @@\n-old\n+new\n",
            changed_files: &files,
            guidelines: "",
            repo_context: "",
            previous_review_sha: Some("8f3c2d1e9a0b4c5d6e7f8a9b0c1d2e3f4a5b6c7d"),
            prior_discussion: "",
        };

        let p = render_review_prompt(&inputs);

        assert!(p.contains("incremental review"));
        assert!(p.contains("8f3c2d1"));
        assert!(p.contains("walkthrough"));
        assert!(p.contains("only the delta since 8f3c2d1"));
        assert!(p.contains("Δ since 8f3c2d1:"));
        assert!(p.contains("leave `walkthrough` empty when nothing material changed"));
    }

    #[test]
    fn full_review_prompt_omits_incremental_walkthrough_guidance() {
        let files = vec!["src/lib.rs".to_string()];
        let inputs = ReviewPromptInputs {
            repo_full_name: "o/r",
            pr_number: 14,
            pr_title: "full review",
            pr_body: "",
            diff: "@@ -1 +1 @@\n-old\n+new\n",
            changed_files: &files,
            guidelines: "",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
        };

        let p = render_review_prompt(&inputs);

        assert!(!p.contains("Incremental review context"));
        assert!(!p.contains("Δ since"));
        assert!(!p.contains("leave `walkthrough` empty when nothing material changed"));
    }
}
