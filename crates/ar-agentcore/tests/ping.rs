use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[tokio::test]
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json, serde_json::json!({ "status": "healthy" }));
}

#[tokio::test]
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"], "invalid_payload");
}

#[derive(Default)]
struct RecordingHandler {
    seen: Mutex<Vec<ar_agentcore::InvocationPayload>>,
}

#[async_trait]
impl ar_agentcore::InvocationHandler for RecordingHandler {
    async fn handle(
        &self,
        payload: ar_agentcore::InvocationPayload,
    ) -> Result<ar_agentcore::InvocationOutcome, ar_agentcore::InvocationError> {
        self.seen.lock().expect("lock").push(payload);
        Ok(ar_agentcore::InvocationOutcome {
            status: "completed".to_string(),
            message: "review finished".to_string(),
        })
    }
}

#[tokio::test]
async fn invocations_execute_through_injected_handler() {
    let handler = Arc::new(RecordingHandler::default());
    let response = ar_agentcore::build_router_with_handler(handler.clone())
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
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
async fn serve_config_router_uses_configured_handler() {
    let handler = Arc::new(RecordingHandler::default());
    let config = ar_agentcore::ServeConfig {
        bind: "127.0.0.1:0".to_string(),
        handler: Some(handler.clone()),
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
