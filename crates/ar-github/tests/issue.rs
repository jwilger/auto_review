//! Issue lookups through the GitHub client and both GitHub `ReviewHost`s.

use ar_forge::{IssueSummary, ReviewHost};
use ar_github::{AppReviewHost, Client, GitHubAppJwt, InstallationReviewHost};
use core::error::Error;
use serde_json::json;
use wiremock::matchers::{header, header_regex, method, path};
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
        title: "bug: crash on empty diff".to_owned(),
        body: "Steps to reproduce".to_owned(),
        state: "open".to_owned(),
        is_pull_request,
        html_url: "https://github.example/owner/repo/issues/12".to_owned(),
    }
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
async fn client_get_issue_uses_installation_token_and_maps_issue() -> TestResult {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer installation-token"))
        .and(header("accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt")?;

    let issue = client
        .get_issue("owner", "repo", 12, "installation-token")
        .await?;

    assert_eq!(issue, Some(expected_issue(false)));
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
async fn review_host_get_issue_uses_installation_token() -> TestResult {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt")?;
    let installation_host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &installation_host;

    let issue = host.get_issue("owner", "repo", 12).await?;

    assert_eq!(issue, Some(expected_issue(false)));
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
async fn review_host_get_issue_marks_pull_requests() -> TestResult {
    let github = MockServer::start().await;
    let mut body = issue_json();
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "pull_request".to_owned(),
            json!({"url": "https://api.github.example/repos/owner/repo/pulls/12"}),
        );
    }
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt")?;
    let installation_host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &installation_host;

    let issue = host.get_issue("owner", "repo", 12).await?;

    assert_eq!(issue, Some(expected_issue(true)));
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
async fn review_host_get_issue_returns_none_on_404() -> TestResult {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/404"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt")?;
    let installation_host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &installation_host;

    let issue = host.get_issue("owner", "repo", 404).await?;

    assert!(issue.is_none());
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::non_ascii_literal,
    reason = "the two-byte 'é' is the character boundary this test exercises; an escape would hide the case it checks and edit an asserted literal"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[tokio::test]
async fn review_host_get_issue_caps_error_body_on_a_char_boundary() -> TestResult {
    let github = MockServer::start().await;
    // 1023 ASCII bytes, then a two-byte character spanning bytes 1023..1025.
    let body = format!("{}é{}", "a".repeat(1023), "b".repeat(500));
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(500).set_body_string(body))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt")?;
    let installation_host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &installation_host;

    let err = host
        .get_issue("owner", "repo", 12)
        .await
        .err()
        .ok_or("500 is an error")?;
    let message = err.message();

    assert!(message.contains("500"), "{message}");
    assert!(message.contains(&"a".repeat(1023)), "{message}");
    assert!(!message.contains('é'), "{message}");
    assert!(!message.contains("ab"), "{message}");
    assert!(message.contains("[truncated]"), "{message}");
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
async fn review_host_get_issue_treats_other_4xx_as_errors() -> TestResult {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "Forbidden"})))
        .mount(&github)
        .await;
    let client = Client::new(&github.uri(), "app-jwt")?;
    let installation_host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ReviewHost = &installation_host;

    let err = host
        .get_issue("owner", "repo", 12)
        .await
        .err()
        .ok_or("403 is an error")?;

    assert!(err.message().contains("403"), "{}", err.message());
    Ok(())
}

#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked GitHub payload; type suffixes would edit the fixture without changing the bytes sent"
)]
#[expect(
    clippy::needless_raw_strings,
    reason = "the authorization header_regex matchers stay as written, in the raw-string form the GitHub App host tests use for JWT patterns"
)]
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

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[tokio::test]
async fn app_review_host_get_issue_uses_minted_installation_token() -> TestResult {
    let github = MockServer::start().await;
    mount_installation_token(&github).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .and(header("authorization", "Bearer ghs_minted"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&github)
        .await;
    let app_host = AppReviewHost::new(app_client(&github.uri())?);
    let host: &dyn ReviewHost = &app_host;

    let issue = host.get_issue("owner", "repo", 12).await?;

    assert_eq!(issue, Some(expected_issue(false)));
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
async fn app_review_host_get_issue_returns_none_on_404() -> TestResult {
    let github = MockServer::start().await;
    mount_installation_token(&github).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/404"))
        .and(header("authorization", "Bearer ghs_minted"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
        .expect(1)
        .mount(&github)
        .await;
    let app_host = AppReviewHost::new(app_client(&github.uri())?);
    let host: &dyn ReviewHost = &app_host;

    let issue = host.get_issue("owner", "repo", 404).await?;

    assert!(issue.is_none());
    Ok(())
}
