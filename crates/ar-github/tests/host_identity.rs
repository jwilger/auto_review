//! Host capabilities and bot identity for the GitHub review hosts.

use ar_forge::HostCapabilities;
use ar_github::{AppReviewHost, Client, InstallationReviewHost};
use serde_json::json;
use std::time::{Duration, Instant};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mount_app(github: &MockServer, expected_calls: u64) {
    Mock::given(method("GET"))
        .and(path("/app"))
        .and(header("authorization", "Bearer app-jwt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 12345,
            "slug": "auto-review",
            "name": "Auto Review"
        })))
        .expect(expected_calls)
        .mount(github)
        .await;
}

#[tokio::test]
async fn app_review_host_resolves_bot_login_once_from_app_slug() {
    let github = MockServer::start().await;
    mount_app(&github, 1).await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = AppReviewHost::new(client);
    let host: &dyn ar_forge::ReviewHost = &host;

    let first = host.bot_login().await.expect("first login");
    let second = host.bot_login().await.expect("second login");

    assert_eq!(
        (first.as_str(), second.as_str()),
        ("auto-review[bot]", "auto-review[bot]")
    );
}

#[tokio::test]
async fn seeded_app_review_host_never_calls_get_app() {
    let github = MockServer::start().await;
    mount_app(&github, 0).await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = AppReviewHost::new(client).with_bot_login("custom-login[bot]");
    let host: &dyn ar_forge::ReviewHost = &host;

    assert_eq!(host.bot_login().await.expect("login"), "custom-login[bot]");
}

#[tokio::test]
async fn installation_review_host_resolves_bot_login_once_from_app_slug() {
    let github = MockServer::start().await;
    mount_app(&github, 1).await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    let first = host.bot_login().await.expect("first login");
    let second = host.bot_login().await.expect("second login");

    assert_eq!(
        (first.as_str(), second.as_str()),
        ("auto-review[bot]", "auto-review[bot]")
    );
}

#[tokio::test]
async fn seeded_installation_review_host_never_calls_get_app() {
    let github = MockServer::start().await;
    mount_app(&github, 0).await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token")
        .with_bot_login("custom-login[bot]");
    let host: &dyn ar_forge::ReviewHost = &host;

    assert_eq!(host.bot_login().await.expect("login"), "custom-login[bot]");
}

#[tokio::test]
async fn installation_review_host_supports_every_capability() {
    let github = MockServer::start().await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let host: &dyn ar_forge::ReviewHost = &host;

    assert_eq!(host.capabilities().await, HostCapabilities::all());
    assert_eq!(github.received_requests().await.map(|r| r.len()), Some(0));
}

#[tokio::test]
async fn app_review_host_supports_every_capability() {
    let github = MockServer::start().await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = AppReviewHost::new(client);
    let host: &dyn ar_forge::ReviewHost = &host;

    assert_eq!(host.capabilities().await, HostCapabilities::all());
    assert_eq!(github.received_requests().await.map(|r| r.len()), Some(0));
}

const IDENTITY_TIMEOUT: Duration = Duration::from_millis(200);
const SLOW_RESPONSE: Duration = Duration::from_secs(10);

async fn mount_slow_app(github: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": 12345, "slug": "auto-review"}))
                .set_delay(SLOW_RESPONSE),
        )
        .mount(github)
        .await;
}

/// A server that accepts `GET /app` but answers too slowly must make
/// `bot_login()` fail within the timeout, and that failure must not be
/// cached: once the server answers promptly the next call succeeds.
async fn assert_slow_get_app_times_out_uncached(
    github: &MockServer,
    host: &dyn ar_forge::ReviewHost,
) {
    mount_slow_app(github).await;
    let started = Instant::now();
    let first = host.bot_login().await;
    let elapsed = started.elapsed();
    assert!(first.is_err(), "{first:?}");
    assert!(elapsed < SLOW_RESPONSE / 2, "took {elapsed:?}");

    github.reset().await;
    mount_app(github, 1).await;
    assert_eq!(
        host.bot_login().await.expect("login after timeout"),
        "auto-review[bot]"
    );
}

#[tokio::test]
async fn installation_review_host_bounds_a_slow_get_app() {
    let github = MockServer::start().await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = InstallationReviewHost::new(client, "installation-token")
        .with_identity_timeout(IDENTITY_TIMEOUT);

    assert_slow_get_app_times_out_uncached(&github, &host).await;
}

#[tokio::test]
async fn app_review_host_bounds_a_slow_get_app() {
    let github = MockServer::start().await;
    let client = Client::new(&github.uri(), "app-jwt").expect("client");
    let host = AppReviewHost::new(client).with_identity_timeout(IDENTITY_TIMEOUT);

    assert_slow_get_app_times_out_uncached(&github, &host).await;
}
