//! Comment editing through the Forgejo `ReviewHost` implementation.
//!
//! Forgejo v11+ edits inline code comments through the issue-comment
//! endpoint, so both trait methods must hit `issues/comments/{id}`.

use ar_forge::ReviewHost;
use ar_forgejo::Client;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[expect(
    clippy::expect_used,
    reason = "a mock-server client that fails to build should abort the calling test at setup"
)]
async fn host() -> (MockServer, Client) {
    let server = MockServer::start().await;
    let client = Client::new(&server.uri(), "test-token").expect("client");
    (server, client)
}

fn comment_json(id: u64, body: &str) -> serde_json::Value {
    json!({"id": id, "body": body, "user": {"login": "auto-review"}})
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn edit_issue_comment_patches_issue_comment_endpoint() {
    let (server, client) = host().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/41"))
        .and(header("Authorization", "token test-token"))
        .and(body_json(json!({"body": "updated summary"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(comment_json(41, "updated summary")))
        .expect(1)
        .mount(&server)
        .await;

    let host: &dyn ReviewHost = &client;
    host.edit_issue_comment("o", "r", 41, "updated summary")
        .await
        .expect("edited");
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn edit_review_comment_patches_issue_comment_endpoint() {
    let (server, client) = host().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/52"))
        .and(header("Authorization", "token test-token"))
        .and(body_json(json!({"body": "addressed"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(comment_json(52, "addressed")))
        .expect(1)
        .mount(&server)
        .await;

    let host: &dyn ReviewHost = &client;
    host.edit_review_comment("o", "r", 52, "addressed")
        .await
        .expect("edited");
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn edit_succeeds_when_response_body_is_not_a_comment() {
    let (server, client) = host().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/41"))
        .respond_with(ResponseTemplate::new(201).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let host: &dyn ReviewHost = &client;
    host.edit_issue_comment("o", "r", 41, "x")
        .await
        .expect("2xx other than 204 is success");
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn edit_treats_204_no_content_as_not_edited() {
    let (server, client) = host().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/52"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let host: &dyn ReviewHost = &client;
    let err = host
        .edit_review_comment("o", "r", 52, "x")
        .await
        .expect_err("204 means Forgejo did not edit");
    assert!(err.message().contains("204"), "{err}");
    assert!(err.message().contains("not edited"), "{err}");
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn edit_issue_comment_reports_403_status() {
    let (server, client) = host().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/41"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;

    let host: &dyn ReviewHost = &client;
    let err = host
        .edit_issue_comment("o", "r", 41, "x")
        .await
        .expect_err("403");
    assert!(err.message().contains("403"), "{err}");
    assert!(err.message().contains("forbidden"), "{err}");
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn edit_review_comment_reports_404_status() {
    let (server, client) = host().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/52"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    let host: &dyn ReviewHost = &client;
    let err = host
        .edit_review_comment("o", "r", 52, "x")
        .await
        .expect_err("404");
    assert!(err.message().contains("404"), "{err}");
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
)]
#[expect(
    clippy::non_ascii_literal,
    reason = "the expected suffix is cap_for_error's literal `…` marker; escaping it would edit the assertion"
)]
#[expect(
    clippy::panic,
    reason = "an unexpected Error variant must fail this test, printing the variant it got"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every Error variant other than the asserted one is a test failure, reported by the catch-all panic arm"
)]
async fn edit_error_body_is_capped() {
    let (server, client) = host().await;
    let long = "x".repeat(5000);
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/41"))
        .respond_with(ResponseTemplate::new(502).set_body_string(long))
        .mount(&server)
        .await;

    let err = client
        .edit_issue_comment("o", "r", 41, "x")
        .await
        .expect_err("502");
    match err {
        ar_forgejo::Error::Api { status, body } => {
            assert_eq!(status, 502);
            assert!(body.ends_with("… [+3976 bytes]"), "{body}");
            assert!(body.len() < 1100, "body not capped: {} bytes", body.len());
        }
        other => panic!("unexpected: {other:?}"),
    }
}
