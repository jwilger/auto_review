//! Host capabilities and bot identity for the GitHub review hosts.

use ar_forge::HostCapabilities;
use ar_github::{AppReviewHost, Client, InstallationReviewHost};
use serde_json::json;
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
