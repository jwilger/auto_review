use ar_github::{Client, InstallationTokenRequest, Permission};
use core::error::Error;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult = Result<(), Box<dyn Error>>;

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[tokio::test]
async fn installation_token_request_uses_app_jwt_and_reuses_cached_token() -> TestResult {
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

    let client = Client::new(&github.uri(), "app-jwt")?;
    let request = InstallationTokenRequest::for_repository("repo")
        .with_permission("contents", Permission::Read)
        .with_permission("issues", Permission::Write)
        .with_permission("pull_requests", Permission::Write)
        .with_permission("statuses", Permission::Write);

    let first = client.installation_token(42, request.clone()).await?;
    let second = client.installation_token(42, request).await?;

    assert_eq!(first.token, "installation-token");
    assert_eq!(second.token, "installation-token");
    Ok(())
}
