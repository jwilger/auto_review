//! Issue lookups through the Forgejo `ReviewHost` implementation.

use ar_forge::{IssueSummary, ReviewHost};
use ar_forgejo::Client;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mock_host() -> (MockServer, Client) {
    let server = MockServer::start().await;
    let client = Client::new(&server.uri(), "test-token").expect("client");
    (server, client)
}

fn issue_json(pull_request: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": 991,
        "number": 12,
        "title": "bug: crash on empty diff",
        "body": "Steps to reproduce",
        "state": "open",
        "html_url": "https://forgejo.example/o/r/issues/12",
        "pull_request": pull_request
    })
}

#[tokio::test]
async fn get_issue_maps_every_field_on_200() {
    let (server, client) = mock_host().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(serde_json::Value::Null)))
        .expect(1)
        .mount(&server)
        .await;
    let host: &dyn ReviewHost = &client;

    let issue = host.get_issue("o", "r", 12).await.expect("ok");

    assert_eq!(
        issue,
        Some(IssueSummary {
            number: 12,
            title: "bug: crash on empty diff".to_string(),
            body: "Steps to reproduce".to_string(),
            state: "open".to_string(),
            is_pull_request: false,
            html_url: "https://forgejo.example/o/r/issues/12".to_string(),
        })
    );
}

#[tokio::test]
async fn get_issue_marks_pull_requests() {
    let (server, client) = mock_host().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(issue_json(serde_json::json!({"merged": false}))),
        )
        .mount(&server)
        .await;
    let host: &dyn ReviewHost = &client;

    let issue = host
        .get_issue("o", "r", 12)
        .await
        .expect("ok")
        .expect("some");

    assert!(issue.is_pull_request);
}

#[tokio::test]
async fn get_issue_without_pull_request_field_is_not_a_pull_request() {
    let (server, client) = mock_host().await;
    let mut body = issue_json(serde_json::Value::Null);
    if let Some(object) = body.as_object_mut() {
        object.remove("pull_request");
        object.insert("body".to_string(), serde_json::Value::Null);
    }
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let host: &dyn ReviewHost = &client;

    let issue = host
        .get_issue("o", "r", 12)
        .await
        .expect("ok")
        .expect("some");

    assert!(!issue.is_pull_request);
    assert_eq!(issue.body, "");
}

#[tokio::test]
async fn get_issue_returns_none_on_404() {
    let (server, client) = mock_host().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/404"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(serde_json::json!({"message": "not found"})),
        )
        .mount(&server)
        .await;
    let host: &dyn ReviewHost = &client;

    let issue = host
        .get_issue("o", "r", 404)
        .await
        .expect("404 is not an error");

    assert!(issue.is_none());
}

#[tokio::test]
async fn get_issue_caps_non_404_error_body_on_a_char_boundary() {
    let (server, client) = mock_host().await;
    // 1023 ASCII bytes, then a two-byte character spanning bytes 1023..1025.
    let body = format!("{}é{}", "a".repeat(1023), "b".repeat(500));
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .respond_with(ResponseTemplate::new(500).set_body_string(body))
        .mount(&server)
        .await;
    let host: &dyn ReviewHost = &client;

    let err = host
        .get_issue("o", "r", 12)
        .await
        .expect_err("500 is an error");
    let message = err.message();

    assert!(message.contains("500"), "{message}");
    assert!(message.contains(&"a".repeat(1023)), "{message}");
    assert!(!message.contains('é'), "{message}");
    assert!(!message.contains("ab"), "{message}");
    assert!(message.contains('…'), "{message}");
}

#[tokio::test]
async fn get_issue_treats_other_4xx_as_errors() {
    let (server, client) = mock_host().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;
    let host: &dyn ReviewHost = &client;

    let err = host
        .get_issue("o", "r", 12)
        .await
        .expect_err("403 is an error");

    assert!(err.message().contains("403"), "{}", err.message());
}
