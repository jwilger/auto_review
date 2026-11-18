//! Forgejo host capability probing and bot identity.

use ar_forge::{HostCapabilities, ReviewHost};
use ar_forgejo::Client;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[expect(
    clippy::expect_used,
    reason = "a mock-server client that fails to build should abort the calling test at setup"
)]
async fn capabilities_for_server_version(version: &str) -> HostCapabilities {
    let forgejo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": version })))
        .expect(1)
        .mount(&forgejo)
        .await;
    let client = Client::new(&forgejo.uri(), "tok").expect("client");

    let first = client.capabilities().await;
    let second = client.capabilities().await;
    let from_clone = client.clone().capabilities().await;
    assert_eq!(
        (first, second),
        (from_clone, from_clone),
        "cached result must not change"
    );
    first
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn forgejo_15_supports_nothing() {
    assert_eq!(
        capabilities_for_server_version("15.0.1").await,
        HostCapabilities::default()
    );
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn forgejo_16_supports_multi_line_comments_only() {
    assert_eq!(
        capabilities_for_server_version("16.0.7").await,
        HostCapabilities {
            multi_line_comments: true,
            ..HostCapabilities::default()
        }
    );
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn forgejo_17_dev_build_supports_multi_line_comments_only() {
    assert_eq!(
        capabilities_for_server_version("17.0.0-100-2254cb0+gitea-1.22.0").await,
        HostCapabilities {
            multi_line_comments: true,
            ..HostCapabilities::default()
        }
    );
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn forgejo_18_supports_everything() {
    assert_eq!(
        capabilities_for_server_version("18.0.0").await,
        HostCapabilities::all()
    );
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
async fn garbage_version_supports_nothing() {
    assert_eq!(
        capabilities_for_server_version("garbage").await,
        HostCapabilities::default()
    );
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
async fn failed_version_probe_supports_nothing_and_is_retried() {
    let forgejo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(1)
        .mount(&forgejo)
        .await;
    let client = Client::new(&forgejo.uri(), "tok").expect("client");

    assert_eq!(client.capabilities().await, HostCapabilities::default());

    forgejo.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "18.0.0" })))
        .expect(1)
        .mount(&forgejo)
        .await;

    assert_eq!(client.capabilities().await, HostCapabilities::all());
}

#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
)]
async fn mount_user(forgejo: &MockServer, expected_calls: u64) {
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7,
            "login": "review-bot"
        })))
        .expect(expected_calls)
        .mount(forgejo)
        .await;
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
async fn bot_login_comes_from_authenticated_user_once() {
    let forgejo = MockServer::start().await;
    mount_user(&forgejo, 1).await;
    let client = Client::new(&forgejo.uri(), "tok").expect("client");

    let first = ReviewHost::bot_login(&client).await.expect("first login");
    let second = ReviewHost::bot_login(&client).await.expect("second login");

    assert_eq!(
        (first.as_str(), second.as_str()),
        ("review-bot", "review-bot")
    );
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
async fn seeded_bot_login_skips_user_lookup() {
    let forgejo = MockServer::start().await;
    mount_user(&forgejo, 0).await;
    let client = Client::new(&forgejo.uri(), "tok")
        .expect("client")
        .with_bot_login("configured-bot");

    let login = ReviewHost::bot_login(&client).await.expect("login");

    assert_eq!(login, "configured-bot");
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
async fn failed_user_lookup_is_an_error() {
    let forgejo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&forgejo)
        .await;
    let client = Client::new(&forgejo.uri(), "tok").expect("client");

    let error = ReviewHost::bot_login(&client)
        .await
        .expect_err("401 must fail");

    assert!(error.message().contains("401"), "{error}");
}
