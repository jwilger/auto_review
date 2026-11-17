extern crate alloc;

use alloc::sync::Arc;
use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use std::sync::Mutex;
use tower::ServiceExt as _;

#[derive(Default)]
struct RecordingHandler {
    seen: Mutex<Vec<ar_agentcore::InvocationPayload>>,
}

#[async_trait]
impl ar_agentcore::InvocationHandler for RecordingHandler {
    #[expect(
        clippy::expect_used,
        reason = "the trait fixes the error type to InvocationError; a poisoned lock means another \
                  test thread already panicked, so panicking here is the honest test failure"
    )]
    async fn handle(
        &self,
        payload: ar_agentcore::InvocationPayload,
    ) -> Result<ar_agentcore::InvocationOutcome, ar_agentcore::InvocationError> {
        self.seen.lock().expect("lock").push(payload);
        Ok(ar_agentcore::InvocationOutcome {
            status: "completed".to_owned(),
            message: "review finished".to_owned(),
        })
    }
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "a failed request, body read or JSON parse should fail this test at the named step; \
              returning Result instead would trip panic_in_result_fn on the assertions"
)]
async fn ping_returns_agentcore_health_json() {
    let response = ar_agentcore::build_router()
        .oneshot(
            Request::builder()
                .uri("/ping")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json, serde_json::json!({ "status": "healthy" }));
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "a failed request, body read or JSON parse should fail this test at the named step; \
              returning Result instead would trip panic_in_result_fn on the assertions"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "the expected response body is an untyped serde_json::json! literal kept exactly as written"
)]
async fn invocations_accepts_provider_neutral_review_payload() {
    let response = ar_agentcore::build_router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/invocations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": "forgejo",
                        "kind": "semantic_review",
                        "owner": "alice",
                        "repo": "widgets",
                        "pr_number": 42,
                        "head_sha": "0123456789012345678901234567890123456789",
                        "force": true
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json,
        serde_json::json!({
            "status": "accepted",
            "provider": "forgejo",
            "kind": "semantic_review",
            "owner": "alice",
            "repo": "widgets",
            "pr_number": 42,
            "head_sha": "0123456789012345678901234567890123456789",
            "force": true
        })
    );
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "a failed request, body read or JSON parse should fail this test at the named step; \
              returning Result instead would trip panic_in_result_fn on the assertions"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "serde_json::Value indexing returns Null for a missing key instead of panicking; the assertions stay as written"
)]
async fn invocations_returns_structured_json_for_invalid_payload() {
    let response = ar_agentcore::build_router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/invocations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"], "invalid_payload");
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "a failed request, body read or JSON parse should fail this test at the named step; \
              returning Result instead would trip panic_in_result_fn on the assertions"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "the preceding seen.len() assertion proves seen[0] exists; the assertions stay as written"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers are untyped JSON wire values, and the restriction group \
              forbids both separated and unseparated literal suffixes"
)]
async fn invocations_execute_through_injected_handler() {
    let handler = Arc::new(RecordingHandler::default());
    let response =
        ar_agentcore::build_router_with_handler(Arc::<RecordingHandler>::clone(&handler))
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/invocations")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "provider": "github",
                            "kind": "semantic_review",
                            "owner": "alice",
                            "repo": "widgets",
                            "pr_number": 42,
                            "head_sha": "0123456789012345678901234567890123456789",
                            "installation_id": 99
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json,
        serde_json::json!({
            "status": "completed",
            "message": "review finished"
        })
    );

    let seen = handler.seen.lock().expect("lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].provider, ar_agentcore::Provider::Github);
    assert_eq!(seen[0].installation_id, Some(99));
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "a failed request, body read or JSON parse should fail this test at the named step; \
              returning Result instead would trip panic_in_result_fn on the assertions"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers are untyped JSON wire values, and the restriction group \
              forbids both separated and unseparated literal suffixes"
)]
async fn serve_config_router_uses_configured_handler() {
    let handler = Arc::new(RecordingHandler::default());
    let config = ar_agentcore::ServeConfig {
        bind: "127.0.0.1:0".to_owned(),
        handler: Some(Arc::<RecordingHandler>::clone(&handler)),
        idempotency: None,
    };

    let response = ar_agentcore::build_router_from_config(&config)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/invocations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": "forgejo",
                        "kind": "semantic_review",
                        "owner": "alice",
                        "repo": "widgets",
                        "pr_number": 42,
                        "head_sha": "0123456789012345678901234567890123456789"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(handler.seen.lock().expect("lock").len(), 1);
}
