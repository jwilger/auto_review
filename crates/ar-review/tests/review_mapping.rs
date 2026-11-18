use ar_forgejo::ReviewEvent;
use ar_prompts::{ReviewFinding, ReviewOutput, ReviewSeverity};
use ar_review::output_to_review_request;

#[test]
#[expect(
    clippy::assert_is_empty,
    reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
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
#[expect(
    clippy::indexing_slicing,
    reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
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
                title: String::new(),
                category: ar_prompts::FindingCategory::Correctness,
            },
            ReviewFinding {
                path: "src/second.rs".into(),
                line_start: 34,
                line_end: None,
                severity: ReviewSeverity::Warning,
                message: duplicate_message.into(),
                title: String::new(),
                category: ar_prompts::FindingCategory::Correctness,
            },
        ],
    };

    let request = output_to_review_request(&output, "deadbeef");

    assert_eq!(request.comments.len(), 1);
    assert_eq!(request.comments[0].path, "src/first.rs");
    assert!(request.comments[0].body.contains(duplicate_message));
}
