use ar_github::{Client, InstallationTokenRequest, Permission};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn installation_token_request_uses_app_jwt_and_reuses_cached_token() {
    let github = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/app/installations/42/access_tokens"))
        .and(header("authorization", "Bearer app-jwt"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
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
            "token": "installation-token",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let request = InstallationTokenRequest::for_repository("repo")
        .with_permission("contents", Permission::Read)
        .with_permission("issues", Permission::Write)
        .with_permission("pull_requests", Permission::Write)
        .with_permission("statuses", Permission::Write);

    let first = client
        .installation_token(42, request.clone())
        .await
        .expect("first token");
    let second = client
        .installation_token(42, request)
        .await
        .expect("cached token");

    assert_eq!(first.token, "installation-token");
    assert_eq!(second.token, "installation-token");
}
