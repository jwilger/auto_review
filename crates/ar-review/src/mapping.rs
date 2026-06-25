use ar_forge::{CreateReviewRequest, ReviewComment, ReviewEvent};
use ar_prompts::{ReviewFinding, ReviewOutput, ReviewSeverity};

/// Map a validated [`ReviewOutput`] to a Forgejo [`CreateReviewRequest`].
///
/// Conventions:
/// - The review body is `summary` plus optional walkthrough/Mermaid
///   sections, each on its own paragraph. Keeps the top-level review
///   readable on its own.
/// - `event` is `RequestChanges` if any finding has severity `Error`,
///   otherwise `Approved`. Advisory findings still become inline comments,
///   but warning-only or note-only reviews must supersede this bot's stale
///   `RequestChanges` reviews and satisfy branch protection.
/// - Each unique finding message/severity pair becomes one inline
///   `ReviewComment` anchored at `new_position = line_start`. Multi-line
///   ranges are rendered as a `**Lines N–M:**` prefix in the body since
///   Forgejo's per-line position schema doesn't carry an end line.
pub fn output_to_review_request(out: &ReviewOutput, head_sha: &str) -> CreateReviewRequest {
    let event = if out
        .findings
        .iter()
        .any(|f| matches!(f.severity, ReviewSeverity::Error))
    {
        ReviewEvent::RequestChanges
    } else {
        ReviewEvent::Approved
    };

    let mut seen_findings = Vec::new();
    let comments = out
        .findings
        .iter()
        .filter(|finding| {
            let key = (finding.severity, finding.message.as_str());
            if seen_findings.contains(&key) {
                false
            } else {
                seen_findings.push(key);
                true
            }
        })
        .map(finding_to_comment)
        .collect::<Vec<_>>();

    CreateReviewRequest {
        body: render_body(out),
        commit_id: head_sha.to_string(),
        event,
        comments,
    }
}

/// Cap on the rendered review body posted to Forgejo. Forgejo
/// accepts large review bodies but a multi-MB markdown blob from
/// a misbehaving LLM (no length cap on the schema's `summary` /
/// `walkthrough` / `mermaid` fields) would either 422 the
/// `create_review` POST or render unreadably. Cap at 32 KiB to
/// fit comfortably under any practical Forgejo limit while
/// holding any reasonable summary + walkthrough + mermaid block.
const REVIEW_BODY_MAX_BYTES: usize = 32 * 1024;

fn render_body(out: &ReviewOutput) -> String {
    let mut body = out.summary.clone();
    if body.starts_with("This PR fixes ") || body.starts_with("This PR addresses ") {
        if let Some((_, rest)) = body.split_once(". ") {
            body = rest.to_string();
        }
    }
    if !out.walkthrough.is_empty() {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("## Walkthrough\n\n");
        body.push_str(&out.walkthrough);
    }
    if !out.mermaid.is_empty() {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("```mermaid\n");
        body.push_str(out.mermaid.trim());
        body.push_str("\n```");
    }
    if body.len() > REVIEW_BODY_MAX_BYTES {
        let mut cut = REVIEW_BODY_MAX_BYTES;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut truncated = body[..cut].to_string();
        if !truncated.ends_with('\n') {
            truncated.push('\n');
        }
        truncated.push_str("\n[review body truncated]\n");
        body = truncated;
    }
    body
}

/// Cap on the `message` text inlined into each review comment's
/// body. The review JSON schema enforces minLength=1 but no max,
/// so a misbehaving LLM could emit multi-KB-per-finding messages.
/// With many findings × huge messages, the create_review payload
/// would either 422 (size limit) or render an unreadable wall of
/// text. 4 KiB per message comfortably holds any actionable
/// reviewer comment.
const FINDING_MESSAGE_MAX_BYTES: usize = 4_096;

fn finding_to_comment(f: &ReviewFinding) -> ReviewComment {
    let message = if f.message.len() > FINDING_MESSAGE_MAX_BYTES {
        let mut cut = FINDING_MESSAGE_MAX_BYTES;
        while cut > 0 && !f.message.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}… [truncated]", &f.message[..cut])
    } else {
        f.message.clone()
    };
    let body = match f.line_end {
        Some(end) if end > f.line_start => {
            let label = severity_label(f.severity);
            format!("{label} **Lines {}–{}:** {message}", f.line_start, end)
        }
        _ => {
            let label = severity_label(f.severity);
            format!("{label} {message}")
        }
    };
    ReviewComment {
        path: f.path.clone(),
        body,
        old_position: None,
        new_position: Some(f.line_start),
    }
}

fn severity_label(s: ReviewSeverity) -> &'static str {
    match s {
        ReviewSeverity::Error => "🔴 **Error:**",
        ReviewSeverity::Warning => "🟡 **Warning:**",
        ReviewSeverity::Note => "💡 **Note:**",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(summary: &str, findings: Vec<ReviewFinding>) -> ReviewOutput {
        ReviewOutput {
            summary: summary.into(),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings,
        }
    }

    fn finding(severity: ReviewSeverity, line_start: u32, line_end: Option<u32>) -> ReviewFinding {
        ReviewFinding {
            path: "src/x.rs".into(),
            line_start,
            line_end,
            severity,
            message: "do the thing".into(),
        }
    }

    #[test]
    fn clean_output_is_an_approved_review_to_supersede_stale_request_changes() {
        let req = output_to_review_request(&output("lgtm", vec![]), "deadbeef");
        assert_eq!(req.event, ReviewEvent::Approved);
        assert!(req.comments.is_empty());
        assert_eq!(req.body, "lgtm");
        assert_eq!(req.commit_id, "deadbeef");
    }

    #[test]
    fn any_error_severity_promotes_to_request_changes() {
        let req = output_to_review_request(
            &output(
                "issues",
                vec![
                    finding(ReviewSeverity::Note, 1, None),
                    finding(ReviewSeverity::Error, 2, None),
                ],
            ),
            "x",
        );
        assert_eq!(req.event, ReviewEvent::RequestChanges);
    }

    #[test]
    fn warning_only_or_note_only_output_is_approved_but_keeps_inline_comments() {
        for severity in [ReviewSeverity::Warning, ReviewSeverity::Note] {
            let req =
                output_to_review_request(&output("minor", vec![finding(severity, 7, None)]), "x");
            assert_eq!(req.event, ReviewEvent::Approved);
            assert_eq!(req.comments.len(), 1);
            assert_eq!(req.comments[0].new_position, Some(7));
            assert!(req.comments[0].body.contains("do the thing"));
        }
    }

    #[test]
    fn line_start_becomes_new_position() {
        let req = output_to_review_request(
            &output("", vec![finding(ReviewSeverity::Note, 17, None)]),
            "x",
        );
        assert_eq!(req.comments[0].new_position, Some(17));
        assert!(req.comments[0].old_position.is_none());
    }

    #[test]
    fn multi_line_range_is_annotated_in_body() {
        let req = output_to_review_request(
            &output("", vec![finding(ReviewSeverity::Warning, 5, Some(9))]),
            "x",
        );
        let body = &req.comments[0].body;
        assert!(body.contains("Lines 5"));
        assert!(body.contains("9"));
    }

    #[test]
    fn single_line_range_does_not_include_range_label() {
        let req = output_to_review_request(
            &output("", vec![finding(ReviewSeverity::Note, 3, Some(3))]),
            "x",
        );
        let body = &req.comments[0].body;
        assert!(!body.contains("Lines"));
    }

    #[test]
    fn finding_body_includes_severity_label() {
        let req = output_to_review_request(
            &output("", vec![finding(ReviewSeverity::Error, 1, None)]),
            "x",
        );
        assert!(req.comments[0].body.to_lowercase().contains("error"));
    }

    #[test]
    fn walkthrough_appears_under_a_walkthrough_heading_in_review_body() {
        let out = ReviewOutput {
            summary: "TL;DR".into(),
            walkthrough: "- File `a.rs`: refactored\n- File `b.rs`: new tests".into(),
            mermaid: String::new(),
            findings: vec![],
        };
        let req = output_to_review_request(&out, "x");
        assert!(req.body.starts_with("TL;DR"));
        assert!(req.body.contains("## Walkthrough"));
        assert!(req.body.contains("a.rs"));
    }

    #[test]
    fn redundant_leading_pr_recap_sentence_is_dropped_from_review_body() {
        for summary in [
            "This PR fixes the flaky review summary. It keeps the actionable reviewer context.",
            "This PR addresses the flaky review summary. It keeps the actionable reviewer context.",
            "This PR fixes several issues, including the flaky review summary. It keeps the actionable reviewer context.",
        ] {
            let out = ReviewOutput {
                summary: summary.into(),
                walkthrough: "- File `mapping.rs`: keeps walkthrough rendering".into(),
                mermaid: String::new(),
                findings: vec![],
            };

            let req = output_to_review_request(&out, "x");

            assert_eq!(
                req.body,
                "It keeps the actionable reviewer context.\n\n## Walkthrough\n\n- File `mapping.rs`: keeps walkthrough rendering"
            );
        }
    }

    #[test]
    fn mermaid_diagram_is_rendered_in_a_fenced_block() {
        let out = ReviewOutput {
            summary: "summary".into(),
            walkthrough: String::new(),
            mermaid: "graph TD\nA-->B".into(),
            findings: vec![],
        };
        let req = output_to_review_request(&out, "x");
        assert!(req.body.contains("```mermaid"));
        assert!(req.body.contains("graph TD"));
        assert!(req.body.contains("A-->B"));
        assert!(req.body.trim_end().ends_with("```"));
    }

    #[test]
    fn empty_summary_with_walkthrough_does_not_double_blank_line() {
        let out = ReviewOutput {
            summary: String::new(),
            walkthrough: "all the details".into(),
            mermaid: String::new(),
            findings: vec![],
        };
        let req = output_to_review_request(&out, "x");
        // No leading blank lines from concatenating "" + "\n\n" + walkthrough.
        assert!(req.body.starts_with("## Walkthrough"));
    }

    #[test]
    fn oversized_body_is_truncated_with_marker() {
        // The schema doesn't bound summary/walkthrough/mermaid
        // length; a misbehaving LLM could emit a multi-MB body
        // that 422s the create_review POST. Cap defensively.
        let out = ReviewOutput {
            summary: "x".repeat(50_000),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings: vec![],
        };
        let req = output_to_review_request(&out, "x");
        assert!(req.body.contains("[review body truncated]"));
        // Bounded by cap + framing.
        assert!(req.body.len() <= REVIEW_BODY_MAX_BYTES + 64);
    }

    #[test]
    fn oversized_finding_message_is_truncated() {
        // Per-finding message length isn't bounded by the schema.
        // A misbehaving LLM emitting 50 findings × 100 KiB message
        // each would 422 the create_review payload.
        let huge = "x".repeat(20_000);
        let f = ReviewFinding {
            path: "src/x.rs".into(),
            line_start: 1,
            line_end: None,
            severity: ReviewSeverity::Note,
            message: huge,
        };
        let req = output_to_review_request(&output("ok", vec![f]), "x");
        let comment_body = &req.comments[0].body;
        assert!(comment_body.contains("[truncated]"));
        assert!(
            comment_body.len() < FINDING_MESSAGE_MAX_BYTES + 64,
            "expected ≤ cap + framing, got {}",
            comment_body.len()
        );
    }

    #[test]
    fn body_under_cap_passes_through_unchanged() {
        let out = ReviewOutput {
            summary: "short summary".into(),
            walkthrough: "brief walkthrough".into(),
            mermaid: String::new(),
            findings: vec![],
        };
        let req = output_to_review_request(&out, "x");
        assert!(!req.body.contains("[review body truncated]"));
        assert!(req.body.contains("short summary"));
    }
}
