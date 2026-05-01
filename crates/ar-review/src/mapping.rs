use ar_forgejo::{CreateReviewRequest, ReviewComment, ReviewEvent};
use ar_prompts::{ReviewFinding, ReviewOutput, ReviewSeverity};

/// Map a validated [`ReviewOutput`] to a Forgejo [`CreateReviewRequest`].
///
/// Conventions:
/// - The review body is the LLM's `summary`. Keeps the top-level review
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
        body: out.summary.clone(),
        commit_id: head_sha.to_string(),
        event,
        comments,
    }
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
        let out = ReviewOutput {
            summary: "lgtm".into(),
            findings: vec![],
        };
        let req = output_to_review_request(&out, "deadbeef");
        assert_eq!(req.event, ReviewEvent::Comment);
        assert!(req.comments.is_empty());
        assert_eq!(req.body, "lgtm");
        assert_eq!(req.commit_id, "deadbeef");
    }

    #[test]
    fn any_error_severity_promotes_to_request_changes() {
        let out = ReviewOutput {
            summary: "issues".into(),
            findings: vec![
                finding(ReviewSeverity::Note, 1, None),
                finding(ReviewSeverity::Error, 2, None),
            ],
        };
        let req = output_to_review_request(&out, "x");
        assert_eq!(req.event, ReviewEvent::RequestChanges);
    }

    #[test]
    fn only_warnings_and_notes_stay_comment() {
        let out = ReviewOutput {
            summary: "minor".into(),
            findings: vec![
                finding(ReviewSeverity::Warning, 1, None),
                finding(ReviewSeverity::Note, 2, None),
            ],
        };
        let req = output_to_review_request(&out, "x");
        assert_eq!(req.event, ReviewEvent::Comment);
    }

    #[test]
    fn line_start_becomes_new_position() {
        let out = ReviewOutput {
            summary: "".into(),
            findings: vec![finding(ReviewSeverity::Note, 17, None)],
        };
        let req = output_to_review_request(&out, "x");
        assert_eq!(req.comments[0].new_position, Some(17));
        assert!(req.comments[0].old_position.is_none());
    }

    #[test]
    fn multi_line_range_is_annotated_in_body() {
        let out = ReviewOutput {
            summary: "".into(),
            findings: vec![finding(ReviewSeverity::Warning, 5, Some(9))],
        };
        let req = output_to_review_request(&out, "x");
        let body = &req.comments[0].body;
        assert!(body.contains("Lines 5"));
        assert!(body.contains("9"));
    }

    #[test]
    fn single_line_range_does_not_include_range_label() {
        let out = ReviewOutput {
            summary: "".into(),
            findings: vec![finding(ReviewSeverity::Note, 3, Some(3))],
        };
        let req = output_to_review_request(&out, "x");
        let body = &req.comments[0].body;
        assert!(!body.contains("Lines"));
    }

    #[test]
    fn finding_body_includes_severity_label() {
        let out = ReviewOutput {
            summary: "".into(),
            findings: vec![finding(ReviewSeverity::Error, 1, None)],
        };
        let req = output_to_review_request(&out, "x");
        assert!(req.comments[0].body.to_lowercase().contains("error"));
    }
}
