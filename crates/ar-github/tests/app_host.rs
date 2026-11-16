//! Long-lived GitHub App review host used by the always-on gateway.

use ar_github::{AppReviewHost, Client, GitHubAppJwt};
use serde_json::json;
use wiremock::matchers::{body_json, header, header_regex, method, path};
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

fn pr_json() -> serde_json::Value {
    json!({
        "number": 7,
        "title": "feat: thing",
        "body": "details",
        "draft": false,
        "state": "open",
        "head": {"ref": "feature", "sha": "head-sha"},
        "base": {"ref": "main", "sha": "base-sha"}
    })
}

#[tokio::test]
async fn app_review_host_resolves_installation_and_reuses_scoped_token() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/installation"))
        .and(header_regex(
            "authorization",
            r"^Bearer ey[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 42})))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/42/access_tokens"))
        .and(header_regex("authorization", r"^Bearer ey"))
        .and(body_json(json!({
            "repositories": ["repo"],
            "permissions": {
                "contents": "read",
                "issues": "write",
                "pull_requests": "write",
                "statuses": "write"
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_installation",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .and(header("authorization", "Bearer ghs_installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_json()))
        .expect(2)
        .mount(&github)
        .await;

    let host = AppReviewHost::new(app_client(&github.uri()));
    let host: &dyn ar_forge::ReviewHost = &host;

    let first = host
        .get_pull_request("owner", "repo", 7)
        .await
        .expect("first");
    let second = host
        .get_pull_request("owner", "repo", 7)
        .await
        .expect("second");

    assert_eq!(first.head.sha, "head-sha");
    assert_eq!(second.title, "feat: thing");
}

#[tokio::test]
async fn app_review_host_uses_webhook_reported_installation_without_lookup() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/installation"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/77/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_from_webhook",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let host = AppReviewHost::new(app_client(&github.uri()));
    host.remember_installation("Owner", "Repo", 77).await;

    let token = host
        .installation_token_for("owner", "repo")
        .await
        .expect("token");

    assert_eq!(token, "ghs_from_webhook");
}

#[tokio::test]
async fn authenticated_app_reports_bot_login_from_slug() {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header_regex("authorization", r"^Bearer ey"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 12345,
            "slug": "auto-review",
            "name": "Auto Review"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let app = app_client(&github.uri())
        .get_authenticated_app()
        .await
        .expect("app");

    assert_eq!(app.slug, "auto-review");
    assert_eq!(app.bot_login(), "auto-review[bot]");
}
