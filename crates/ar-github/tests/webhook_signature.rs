//! GitHub webhook signature verification and event payload parsing.

use ar_github::{verify_webhook_signature, WebhookSignatureError};
use core::error::Error;
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn signature(secret: &str, body: &[u8]) -> TestResult<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())?;
    mac.update(body);
    Ok(format!(
        "sha256={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[test]
fn github_signature_256_passes_with_expected_prefix() -> TestResult {
    let body = br#"{"action":"opened"}"#;

    verify_webhook_signature("secret", body, &signature("secret", body)?)?;
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[test]
fn github_signature_256_rejects_missing_prefix() -> TestResult {
    let body = br#"{"action":"opened"}"#;
    let without_prefix = signature("secret", body)?
        .strip_prefix("sha256=")
        .ok_or("prefix")?
        .to_owned();

    let error = verify_webhook_signature("secret", body, &without_prefix)
        .err()
        .ok_or("missing prefix")?;

    assert!(matches!(error, WebhookSignatureError::InvalidFormat));
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[test]
fn github_signature_256_rejects_wrong_secret() -> TestResult {
    let body = br#"{"action":"opened"}"#;

    let error = verify_webhook_signature("secret", body, &signature("other", body)?)
        .err()
        .ok_or("mismatch")?;

    assert!(matches!(error, WebhookSignatureError::Mismatch));
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers here are untyped JSON wire values in the GitHub webhook fixture; type suffixes would edit the fixture without changing the parsed payload"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[test]
fn issue_comment_event_parses_pull_request_comment_with_installation() -> TestResult {
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
    }))?;

    assert_eq!(event.action, "created");
    assert!(event.is_pull_request_comment());
    assert_eq!(event.issue.number, 7);
    assert_eq!(event.comment.id, 555);
    assert_eq!(event.comment.body(), "@auto-review help");
    assert!(!event.sender.is_bot());
    assert_eq!(event.repository.owner.login, "owner");
    assert_eq!(event.installation.map(|i| i.id), Some(42));
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers here are untyped JSON wire values in the GitHub webhook fixture; type suffixes would edit the fixture without changing the parsed payload"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[test]
fn review_comment_event_parses_bot_sender_and_pr_number() -> TestResult {
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
        }))?;

    assert_eq!(event.pull_request.number, 12);
    assert_eq!(event.comment.body(), "");
    assert!(event.sender.is_bot());
    assert!(event.installation.is_none());
    Ok(())
}
