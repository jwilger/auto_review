use ar_forgejo::{CreateReviewRequest, ReviewComment, ReviewEvent};
use ar_prompts::{ReviewFinding, ReviewOutput, ReviewSeverity};

/// Map a validated [`ReviewOutput`] to a Forgejo [`CreateReviewRequest`].
///
/// Conventions:
/// - The review body is `summary` plus optional walkthrough/Mermaid
///   sections, each on its own paragraph. Keeps the top-level review
///   readable on its own.
/// - `event` is `RequestChanges` if any finding has severity `Error`,
///   otherwise `Comment`. Drafts can't be approved by a bot.
/// - Each finding becomes one inline `ReviewComment` anchored at
///   `new_position = line_start`. Multi-line ranges are rendered as a
///   `**Lines N–M:**` prefix in the body since Forgejo's per-line position
///   schema doesn't carry an end line.
pub fn output_to_review_request(out: &ReviewOutput, head_sha: &str) -> CreateReviewRequest {
    let event = if out
        .findings
        .iter()
        .any(|f| matches!(f.severity, ReviewSeverity::Error))
    {
        ReviewEvent::RequestChanges
    } else {
        ReviewEvent::Comment
    };

    let comments = out
        .findings
        .iter()
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

fn finding_to_comment(f: &ReviewFinding) -> ReviewComment {
    let body = match f.line_end {
        Some(end) if end > f.line_start => {
            let label = severity_label(f.severity);
            format!("{label} **Lines {}–{}:** {}", f.line_start, end, f.message)
        }
        _ => {
            let label = severity_label(f.severity);
            format!("{label} {}", f.message)
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
    fn empty_findings_are_a_comment_event_with_no_comments() {
        let req = output_to_review_request(&output("lgtm", vec![]), "deadbeef");
        assert_eq!(req.event, ReviewEvent::Comment);
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
    fn only_warnings_and_notes_stay_comment() {
        let req = output_to_review_request(
            &output(
                "minor",
                vec![
                    finding(ReviewSeverity::Warning, 1, None),
                    finding(ReviewSeverity::Note, 2, None),
                ],
            ),
            "x",
        );
        assert_eq!(req.event, ReviewEvent::Comment);
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
