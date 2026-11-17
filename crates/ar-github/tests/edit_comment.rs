//! Comment editing through both GitHub `ReviewHost` implementations.
//!
//! GitHub edits top-level issue/PR comments through
//! `issues/comments/{id}` and inline review comments through
//! `pulls/comments/{id}`.

use ar_forge::ReviewHost;
use ar_github::{AppReviewHost, Client, GitHubAppJwt, InstallationReviewHost};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUE_PATH: &str = "/repos/o/r/issues/comments/41";
const REVIEW_PATH: &str = "/repos/o/r/pulls/comments/52";

fn test_private_key() -> &'static str {
    let source = include_str!("app_jwt.rs");
    let start = source
        .find("-----BEGIN PRIVATE KEY-----")
        .expect("key start");
    let end = source.find("-----END PRIVATE KEY-----").expect("key end")
        + "-----END PRIVATE KEY-----".len();
    &source[start..end]
}

fn installation_host(github: &MockServer) -> InstallationReviewHost {
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    InstallationReviewHost::new(client, "installation-token")
}

/// Mock the installation lookup and token exchange the same way the
/// existing `AppReviewHost` tests do, then build the host.
async fn app_host(github: &MockServer) -> AppReviewHost {
    Mock::given(method("GET"))
        .and(path("/repos/o/r/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 42})))
        .mount(github)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/42/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_installation",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .mount(github)
        .await;
    let signer = GitHubAppJwt::from_rsa_pem(12345, test_private_key().as_bytes()).expect("signer");
    AppReviewHost::new(Client::with_app_signer(&github.uri(), signer).expect("client"))
}

async fn mount_edit(github: &MockServer, route: &str, token: &str, body: &str) {
    Mock::given(method("PATCH"))
        .and(path(route))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .and(body_json(json!({"body": body})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1, "body": body})))
        .expect(1)
        .mount(github)
        .await;
}

async fn mount_long_error(github: &MockServer, route: &str, status: u16) {
    Mock::given(method("PATCH"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_string("e".repeat(5000)))
        .expect(1)
        .mount(github)
        .await;
}

fn assert_capped(err: &ar_forge::HostError, status: u16) {
    let message = err.message();
    assert!(message.contains(&status.to_string()), "{message}");
    assert!(message.contains("[truncated]"), "{message}");
    assert!(
        message.len() < 1200,
        "body not capped: {} bytes",
        message.len()
    );
}

#[tokio::test]
async fn installation_host_edit_issue_comment_patches_issues_comments() {
    let github = MockServer::start().await;
    mount_edit(&github, ISSUE_PATH, "installation-token", "summary v2").await;
    let host = installation_host(&github);

    host.edit_issue_comment("o", "r", 41, "summary v2")
        .await
        .expect("edited");
}

#[tokio::test]
async fn installation_host_edit_review_comment_patches_pulls_comments() {
    let github = MockServer::start().await;
    mount_edit(&github, REVIEW_PATH, "installation-token", "addressed").await;
    let host = installation_host(&github);

    host.edit_review_comment("o", "r", 52, "addressed")
        .await
        .expect("edited");
}

#[tokio::test]
async fn installation_host_edit_issue_comment_caps_error_body() {
    let github = MockServer::start().await;
    mount_long_error(&github, ISSUE_PATH, 403).await;
    let host = installation_host(&github);

    let err = host
        .edit_issue_comment("o", "r", 41, "x")
        .await
        .expect_err("403");
    assert_capped(&err, 403);
}

#[tokio::test]
async fn installation_host_edit_review_comment_caps_error_body() {
    let github = MockServer::start().await;
    mount_long_error(&github, REVIEW_PATH, 422).await;
    let host = installation_host(&github);

    let err = host
        .edit_review_comment("o", "r", 52, "x")
        .await
        .expect_err("422");
    assert_capped(&err, 422);
}

#[tokio::test]
async fn app_host_edit_issue_comment_patches_issues_comments() {
    let github = MockServer::start().await;
    mount_edit(&github, ISSUE_PATH, "ghs_installation", "summary v2").await;
    let host = app_host(&github).await;

    host.edit_issue_comment("o", "r", 41, "summary v2")
        .await
        .expect("edited");
}

#[tokio::test]
async fn app_host_edit_review_comment_patches_pulls_comments() {
    let github = MockServer::start().await;
    mount_edit(&github, REVIEW_PATH, "ghs_installation", "addressed").await;
    let host = app_host(&github).await;

    host.edit_review_comment("o", "r", 52, "addressed")
        .await
        .expect("edited");
}

#[tokio::test]
async fn app_host_edit_issue_comment_caps_error_body() {
    let github = MockServer::start().await;
    mount_long_error(&github, ISSUE_PATH, 404).await;
    let host = app_host(&github).await;

    let err = host
        .edit_issue_comment("o", "r", 41, "x")
        .await
        .expect_err("404");
    assert_capped(&err, 404);
}

#[tokio::test]
async fn app_host_edit_review_comment_caps_error_body() {
    let github = MockServer::start().await;
    mount_long_error(&github, REVIEW_PATH, 403).await;
    let host = app_host(&github).await;

    let err = host
        .edit_review_comment("o", "r", 52, "x")
        .await
        .expect_err("403");
    assert_capped(&err, 403);
}
