use ar_forgejo::ReviewEvent;
use ar_prompts::{ReviewFinding, ReviewOutput, ReviewSeverity};
use ar_review::output_to_review_request;

#[test]
fn clean_review_output_posts_approval() {
    let output = ReviewOutput {
        summary: "lgtm".into(),
        walkthrough: String::new(),
        mermaid: String::new(),
        findings: Vec::new(),
    };

    let request = output_to_review_request(&output, "deadbeef");

    assert_eq!(request.event, ReviewEvent::Approved);
    assert_eq!(request.commit_id, "deadbeef");
    assert!(request.comments.is_empty());
}

#[test]
fn duplicate_severity_and_message_across_locations_posts_one_inline_comment() {
    let duplicate_message = "same actionable feedback";
    let output = ReviewOutput {
        summary: "duplicate findings".into(),
        walkthrough: String::new(),
        mermaid: String::new(),
        findings: vec![
            ReviewFinding {
                path: "src/first.rs".into(),
                line_start: 12,
                line_end: None,
                severity: ReviewSeverity::Warning,
                message: duplicate_message.into(),
            },
            ReviewFinding {
                path: "src/second.rs".into(),
                line_start: 34,
                line_end: None,
                severity: ReviewSeverity::Warning,
                message: duplicate_message.into(),
            },
        ],
    };

    let request = output_to_review_request(&output, "deadbeef");

    assert_eq!(request.comments.len(), 1);
    assert_eq!(request.comments[0].path, "src/first.rs");
    assert!(request.comments[0].body.contains(duplicate_message));
}
