use ar_forgejo::ReviewEvent;
use ar_prompts::ReviewOutput;
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
