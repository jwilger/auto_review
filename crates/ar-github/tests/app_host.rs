//! Long-lived GitHub App review host used by the always-on gateway.

use ar_github::{AppReviewHost, Client, GitHubAppJwt};
use core::error::Error;
use serde_json::json;
use wiremock::matchers::{body_json, header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The test RSA key, read from the PEM block embedded in `app_jwt.rs`.
fn test_private_key() -> TestResult<String> {
    const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
    const END: &str = "-----END PRIVATE KEY-----";
    let source = include_str!("app_jwt.rs");
    let (_, after_begin) = source.split_once(BEGIN).ok_or("key start")?;
    let (key_body, _) = after_begin.split_once(END).ok_or("key end")?;
    Ok(format!("{BEGIN}{key_body}{END}"))
}

fn app_client(base: &str) -> TestResult<Client> {
    let signer = GitHubAppJwt::from_rsa_pem(12345, test_private_key()?.as_bytes())?;
    Ok(Client::with_app_signer(base, signer)?)
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

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked GitHub payload; type suffixes would edit the fixture without changing the bytes sent"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[expect(
    clippy::needless_raw_strings,
    reason = "the authorization header_regex matchers stay as written, in the same raw-string form as the escaped JWT pattern beside them"
)]
#[tokio::test]
async fn app_review_host_resolves_installation_and_reuses_scoped_token() -> TestResult {
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

    let app_host = AppReviewHost::new(app_client(&github.uri())?);
    let host: &dyn ar_forge::ReviewHost = &app_host;

    let first = host.get_pull_request("owner", "repo", 7).await?;
    let second = host.get_pull_request("owner", "repo", 7).await?;

    assert_eq!(first.head.sha, "head-sha");
    assert_eq!(second.title, "feat: thing");
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
#[tokio::test]
async fn app_review_host_uses_webhook_reported_installation_without_lookup() -> TestResult {
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

    let host = AppReviewHost::new(app_client(&github.uri())?);
    host.remember_installation("Owner", "Repo", 77).await;

    let token = host.installation_token_for("owner", "repo").await?;

    assert_eq!(token, "ghs_from_webhook");
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked GitHub payload; type suffixes would edit the fixture without changing the bytes sent"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[expect(
    clippy::needless_raw_strings,
    reason = "the authorization header_regex matcher stays as written, in the raw-string form the other JWT matchers in this file use"
)]
#[tokio::test]
async fn authenticated_app_reports_bot_login_from_slug() -> TestResult {
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

    let app = app_client(&github.uri())?.get_authenticated_app().await?;

    assert_eq!(app.slug, "auto-review");
    assert_eq!(app.bot_login(), "auto-review[bot]");
    Ok(())
}
