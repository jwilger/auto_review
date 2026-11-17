extern crate alloc;

use alloc::sync::Arc;
use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use core::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt as _;

#[derive(Default)]
struct CountingHandler {
    calls: AtomicUsize,
}

struct DuplicateIdempotency;

#[async_trait]
impl ar_agentcore::InvocationHandler for CountingHandler {
    async fn handle(
        &self,
        _payload: ar_agentcore::InvocationPayload,
    ) -> Result<ar_agentcore::InvocationOutcome, ar_agentcore::InvocationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ar_agentcore::InvocationOutcome {
            status: "completed".to_owned(),
            message: "review finished".to_owned(),
        })
    }
}

#[async_trait]
impl ar_agentcore::InvocationIdempotency for DuplicateIdempotency {
    async fn claim(&self, _key: &str) -> Result<bool, ar_agentcore::InvocationIdempotencyError> {
        Ok(false)
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
#[expect(
    clippy::default_numeric_fallback,
    reason = "serde_json::json! numbers are untyped JSON wire values, and the restriction group \
              forbids both separated and unseparated literal suffixes"
)]
async fn duplicate_invocation_returns_duplicate_without_rehandling() {
    let handler = Arc::new(CountingHandler::default());
    let idempotency = Arc::new(ar_agentcore::InMemoryInvocationIdempotency::new());
    let app = ar_agentcore::build_router_with_handler_and_idempotency(
        Arc::<CountingHandler>::clone(&handler),
        idempotency,
    );
    let payload = serde_json::json!({
        "provider": "forgejo",
        "kind": "semantic_review",
        "owner": "alice",
        "repo": "widgets",
        "pr_number": 42,
        "head_sha": "0123456789012345678901234567890123456789"
    })
    .to_string();

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/invocations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.clone()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/invocations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(second.status(), StatusCode::OK);
    let body = to_bytes(second.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json,
        serde_json::json!({
            "status": "duplicate",
            "message": "invocation already handled"
        })
    );
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
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
async fn serve_config_router_uses_configured_idempotency_store() {
    let handler = Arc::new(CountingHandler::default());
    let app = ar_agentcore::build_router_from_config(&ar_agentcore::ServeConfig {
        bind: "127.0.0.1:0".to_owned(),
        handler: Some(Arc::<CountingHandler>::clone(&handler)),
        idempotency: Some(Arc::new(DuplicateIdempotency)),
    });
    let payload = serde_json::json!({
        "provider": "forgejo",
        "kind": "semantic_review",
        "owner": "alice",
        "repo": "widgets",
        "pr_number": 42,
        "head_sha": "0123456789012345678901234567890123456789"
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/invocations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
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
            "status": "duplicate",
            "message": "invocation already handled"
        })
    );
    assert_eq!(handler.calls.load(Ordering::SeqCst), 0);
}
