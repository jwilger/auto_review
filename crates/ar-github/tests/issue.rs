//! Issue lookups through the GitHub client and both GitHub `ReviewHost`s.

use ar_forge::{IssueSummary, ReviewHost};
use ar_github::{AppReviewHost, Client, GitHubAppJwt, InstallationReviewHost};
use serde_json::json;
use wiremock::matchers::{header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_private_key() -> &'static str {
    let source = include_str!("app_jwt.rs");
    let start = source
        .find("-----BEGIN PRIVATE KEY-----")
        .expect("key start");
    let end = source.find("-----END PRIVATE KEY-----").expect("key end")
        + "-----END PRIVATE KEY-----".len();
    &source[start..end]
}

fn app_client(base: &str) -> Client {
    let signer = GitHubAppJwt::from_rsa_pem(12345, test_private_key().as_bytes()).expect("signer");
    Client::with_app_signer(base, signer).expect("client")
}

fn issue_json() -> serde_json::Value {
    json!({
        "id": 991,
        "number": 12,
        "title": "bug: crash on empty diff",
        "body": "Steps to reproduce",
        "state": "open",
        "html_url": "https://github.example/owner/repo/issues/12"
    })
}

fn expected_issue(is_pull_request: bool) -> IssueSummary {
    IssueSummary {
        number: 12,
        title: "bug: crash on empty diff".to_string(),
        body: "Steps to reproduce".to_string(),
        state: "open".to_string(),
        is_pull_request,
        html_url: "https://github.example/owner/repo/issues/12".to_string(),
    }
}

#[tokio::test]
async fn client_get_issue_uses_installation_token_and_maps_issue() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");

    let issue = client
        .get_issue("owner", "repo", 12, "installation-token")
        .await
        .expect("issue");

    assert_eq!(issue, Some(expected_issue(false)));
}

#[tokio::test]
async fn review_host_get_issue_uses_installation_token() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &host;

    let issue = host.get_issue("owner", "repo", 12).await.expect("issue");

    assert_eq!(issue, Some(expected_issue(false)));
}

#[tokio::test]
async fn review_host_get_issue_marks_pull_requests() {
    let github = MockServer::start().await;
    let mut body = issue_json();
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "pull_request".to_string(),
            json!({"url": "https://api.github.example/repos/owner/repo/pulls/12"}),
        );
    }
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &host;

    let issue = host.get_issue("owner", "repo", 12).await.expect("issue");

    assert_eq!(issue, Some(expected_issue(true)));
}

#[tokio::test]
async fn review_host_get_issue_returns_none_on_404() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/404"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &host;

    let issue = host
        .get_issue("owner", "repo", 404)
        .await
        .expect("404 is not an error");

    assert!(issue.is_none());
}

#[tokio::test]
async fn review_host_get_issue_caps_error_body_on_a_char_boundary() {
    let github = MockServer::start().await;
    // 1023 ASCII bytes, then a two-byte character spanning bytes 1023..1025.
    let body = format!("{}é{}", "a".repeat(1023), "b".repeat(500));
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(500).set_body_string(body))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &host;

    let err = host
        .get_issue("owner", "repo", 12)
        .await
        .expect_err("500 is an error");
    let message = err.message();

    assert!(message.contains("500"), "{message}");
    assert!(message.contains(&"a".repeat(1023)), "{message}");
    assert!(!message.contains('é'), "{message}");
    assert!(!message.contains("ab"), "{message}");
    assert!(message.contains("[truncated]"), "{message}");
}

#[tokio::test]
async fn review_host_get_issue_treats_other_4xx_as_errors() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "Forbidden"})))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &host;

    let err = host
        .get_issue("owner", "repo", 12)
        .await
        .expect_err("403 is an error");

    assert!(err.message().contains("403"), "{}", err.message());
}

async fn mount_installation_token(github: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/installation"))
        .and(header_regex("authorization", r"^Bearer ey"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 42})))
        .expect(1)
        .mount(github)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/42/access_tokens"))
        .and(header_regex("authorization", r"^Bearer ey"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_minted",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .expect(1)
        .mount(github)
        .await;
}

#[tokio::test]
async fn app_review_host_get_issue_uses_minted_installation_token() {
    let github = MockServer::start().await;
    mount_installation_token(&github).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer ghs_minted"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&github)
        .await;
    let host = AppReviewHost::new(app_client(&github.uri()));
    let host: &dyn ReviewHost = &host;

    let issue = host.get_issue("owner", "repo", 12).await.expect("issue");

    assert_eq!(issue, Some(expected_issue(false)));
}

#[tokio::test]
async fn app_review_host_get_issue_returns_none_on_404() {
    let github = MockServer::start().await;
    mount_installation_token(&github).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/404"))
        .and(header("authorization", "Bearer ghs_minted"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
        .expect(1)
        .mount(&github)
        .await;
    let host = AppReviewHost::new(app_client(&github.uri()));
    let host: &dyn ReviewHost = &host;

    let issue = host
        .get_issue("owner", "repo", 404)
        .await
        .expect("404 is not an error");

    assert!(issue.is_none());
}
