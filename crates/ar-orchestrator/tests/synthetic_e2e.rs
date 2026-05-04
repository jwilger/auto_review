//! Synthetic end-to-end integration test.
//!
//! Drives the full stack — CI-triggered gateway dispatch (`ar-gateway`) →
//! `SpawningDispatcher` → review pipeline (`ar-review`) — in a single in-process
//! tokio test, with wiremock standing in for Forgejo and a canned LLM provider
//! standing in for the reasoning model. The assertion target is the posted review
//! on Forgejo: when wiremock records exactly one
//! `POST /api/v1/repos/o/r/pulls/7/reviews`, the whole pipeline ran to
//! completion.
//!
//! The git-clone phase WILL fail (wiremock is not a git server). The
//! lint phase swallows that failure and the dispatcher continues with
//! empty findings — see `dispatcher.rs::run_review_job`. So this test
//! does not need to provide a working clone.

use ar_forgejo::Client as ForgejoClient;
use ar_gateway::{build_router, AppState};
use ar_llm::{
    types::{CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier},
    Router as LlmRouter,
};
use ar_orchestrator::{InMemoryReviewHistory, JobDispatcher, ReviewHistory, SpawningDispatcher};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const WEBHOOK_SECRET: &str = "synthetic-e2e-secret";
const CI_REVIEW_TOKEN: &str = "synthetic-action-token";

/// Replicated from `ar-review`'s test module so this integration test
/// stays self-contained. Pops responses LIFO; defaults to "no
/// findings" when the stack is empty so the verifier-tier can keep
/// pulling without panicking.
struct CannedProvider {
    responses: Mutex<Vec<String>>,
}

impl CannedProvider {
    fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(String::from).collect()),
        }
    }
}

#[async_trait]
impl LlmProvider for CannedProvider {
    async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| r#"{"summary":"","findings":[]}"#.to_string());
        Ok(CompleteResponse {
            content: next,
            input_tokens: 0,
            output_tokens: 0,
        })
    }
}

#[tokio::test]
async fn ci_endpoint_through_dispatcher_through_pipeline_posts_review() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 7,
            "title": "synthetic e2e",
            "body": "",
            "draft": false,
            "state": "open",
            "head": {"ref": "topic", "sha": "deadbeef"},
            "base": {"ref": "main", "sha": "cafef00d"}
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"filename": "src/x.rs", "status": "modified"}
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7.diff"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("diff --git a/src/x.rs b/src/x.rs\n@@ -1 +1 @@\n-old\n+new\n"),
        )
        .mount(&server)
        .await;

    // Pending + final commit-status posts both go to this URL; we
    // accept both with a single mock.
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/statuses/deadbeef"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    // The assertion target. Mounted with `expect(1)` so wiremock
    // panics on Drop if the dispatcher never reached this endpoint.
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 1234,
            "state": "COMMENT"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let forgejo_client =
        Arc::new(ForgejoClient::new(&server.uri(), "tok").expect("forgejo client"));

    let provider = Arc::new(CannedProvider::new(vec![
        r#"{"summary":"looks fine","findings":[]}"#,
    ]));
    let llm_router = Arc::new(LlmRouter::new().with(ModelTier::Reasoning, provider));

    let history: Arc<dyn ReviewHistory> = Arc::new(InMemoryReviewHistory::new());
    let dispatcher = Arc::new(
        SpawningDispatcher::new(forgejo_client.clone(), llm_router, server.uri(), "tok")
            .with_history(history),
    ) as Arc<dyn JobDispatcher>;

    let app = build_router(
        AppState::new(WEBHOOK_SECRET, dispatcher)
            .with_ci_review_endpoint(CI_REVIEW_TOKEN, forgejo_client),
    );

    let req = Request::post("/reviews/ci")
        .header("authorization", format!("Bearer {CI_REVIEW_TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "owner": "o",
                "repo": "r",
                "pr_number": 7,
                "head_sha": "deadbeef"
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // The dispatcher spawns the review on a background task. Poll
    // wiremock's request log until the review POST lands or we
    // exhaust the budget. Bounded so a regression hangs the test
    // briefly rather than indefinitely.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let posts = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.method == wiremock::http::Method::POST
                    && r.url.path() == "/api/v1/repos/o/r/pulls/7/reviews"
            })
            .count();
        if posts >= 1 {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "dispatcher never POSTed the review within 5s; \
                 received_requests so far: {:?}",
                server
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| format!("{} {}", r.method, r.url.path()))
                    .collect::<Vec<_>>()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let posts = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| {
            r.method == wiremock::http::Method::POST
                && r.url.path() == "/api/v1/repos/o/r/pulls/7/reviews"
        })
        .count();
    assert_eq!(posts, 1, "expected exactly one review POST");
}
