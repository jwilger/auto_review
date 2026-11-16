use ar_github::{verify_webhook_signature, WebhookSignatureError};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("valid key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[test]
fn github_signature_256_passes_with_expected_prefix() {
    let body = br#"{"action":"opened"}"#;

    verify_webhook_signature("secret", body, &signature("secret", body)).expect("valid signature");
}

#[test]
fn github_signature_256_rejects_missing_prefix() {
    let body = br#"{"action":"opened"}"#;
    let without_prefix = signature("secret", body)
        .strip_prefix("sha256=")
        .expect("prefix")
        .to_string();

    let error =
        verify_webhook_signature("secret", body, &without_prefix).expect_err("missing prefix");

    assert!(matches!(error, WebhookSignatureError::InvalidFormat));
}

#[test]
fn github_signature_256_rejects_wrong_secret() {
    let body = br#"{"action":"opened"}"#;

    let error =
        verify_webhook_signature("secret", body, &signature("other", body)).expect_err("mismatch");

    assert!(matches!(error, WebhookSignatureError::Mismatch));
}

#[test]
fn issue_comment_event_parses_pull_request_comment_with_installation() {
    let event: ar_github::IssueCommentEvent = serde_json::from_value(serde_json::json!({
        "action": "created",
        "issue": {
            "number": 7,
            "pull_request": {"url": "https://api.github.com/repos/owner/repo/pulls/7"}
        },
        "comment": {
            "id": 555,
            "body": "@auto-review help",
            "user": {"login": "alice", "type": "User"}
        },
        "repository": {
            "name": "repo",
            "full_name": "owner/repo",
            "owner": {"login": "owner", "type": "Organization"}
        },
        "sender": {"login": "alice", "type": "User"},
        "installation": {"id": 42}
    }))
    .expect("issue_comment payload");

    assert_eq!(event.action, "created");
    assert!(event.is_pull_request_comment());
    assert_eq!(event.issue.number, 7);
    assert_eq!(event.comment.id, 555);
    assert_eq!(event.comment.body(), "@auto-review help");
    assert!(!event.sender.is_bot());
    assert_eq!(event.repository.owner.login, "owner");
    assert_eq!(event.installation.map(|i| i.id), Some(42));
}

#[test]
fn review_comment_event_parses_bot_sender_and_pr_number() {
    let event: ar_github::PullRequestReviewCommentEvent =
        serde_json::from_value(serde_json::json!({
            "action": "created",
            "comment": {
                "id": 9001,
                "body": null,
                "user": {"login": "auto-review[bot]", "type": "Bot"}
            },
            "pull_request": {"number": 12},
            "repository": {
                "name": "repo",
                "full_name": "owner/repo",
                "owner": {"login": "owner"}
            },
            "sender": {"login": "auto-review[bot]", "type": "Bot"}
        }))
        .expect("pull_request_review_comment payload");

    assert_eq!(event.pull_request.number, 12);
    assert_eq!(event.comment.body(), "");
    assert!(event.sender.is_bot());
    assert!(event.installation.is_none());
}
