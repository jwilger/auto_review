#![expect(
    clippy::tests_outside_test_module,
    reason = "this integration target is already a test harness"
)]
#![expect(
    clippy::expect_used,
    reason = "fixtures and in-process request plumbing must succeed"
)]

extern crate alloc;

use alloc::sync::Arc;
use ar_forgejo::Client as ForgejoClient;
use ar_gateway::{build_router, AppState};
use ar_orchestrator::{JobDispatcher, ReviewJob};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use std::io;
use std::sync::Mutex;
use tower::ServiceExt as _;
use tracing::subscriber;
use tracing_subscriber::fmt::MakeWriter;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Clone, Default)]
struct RecordingDispatcher {
    jobs: Arc<Mutex<Vec<ReviewJob>>>,
}

impl RecordingDispatcher {
    fn jobs(&self) -> Vec<ReviewJob> {
        self.jobs.lock().expect("dispatcher lock").clone()
    }
}

#[async_trait]
impl JobDispatcher for RecordingDispatcher {
    async fn dispatch(&self, job: ReviewJob) {
        self.jobs.lock().expect("dispatcher lock").push(job);
    }
}

#[derive(Clone, Default)]
struct CapturedLogs {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CapturedLogs {
    fn contents(&self) -> String {
        let bytes = self.bytes.lock().expect("log lock").clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl<'writer> MakeWriter<'writer> for CapturedLogs {
    type Writer = CapturedLogWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        CapturedLogWriter {
            bytes: Arc::clone(&self.bytes),
        }
    }
}

struct CapturedLogWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for CapturedLogWriter {
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "a poisoned capture buffer is a test fixture failure"
    )]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.lock().expect("log lock").extend_from_slice(buf);
        Ok(buf.len())
    }
}

fn ci_review_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "owner": "o",
        "repo": "r",
        "pr_number": 42,
        "head_sha": "deadbeef",
        "force": true,
        "trigger": {"source": "forgejo-actions", "run_id": "123"}
    }))
    .expect("serialize request body")
}

#[tokio::test(flavor = "current_thread")]
async fn unauthorized_ci_review_response_and_logs_do_not_leak_action_tokens() {
    let expected_token = "expected-action-token-visible-regression-secret";
    let rejected_token = "rejected-bearer-token-visible-regression-secret";
    let forgejo_token = "forgejo-token-must-not-be-used";

    let forgejo = MockServer::start().await;
    let pr_number: i32 = 42;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": pr_number,
            "title": "must not be fetched",
            "body": "unauthorized requests stop before Forgejo",
            "head": {"ref": "topic", "sha": "deadbeef"},
            "base": {"ref": "main", "sha": "cafef00d"}
        })))
        .expect(0)
        .mount(&forgejo)
        .await;

    let recorder = Arc::new(RecordingDispatcher::default());
    let client = Arc::new(ForgejoClient::new(&forgejo.uri(), forgejo_token).expect("client"));
    let dispatcher: Arc<dyn JobDispatcher> = Arc::<RecordingDispatcher>::clone(&recorder);
    let app = build_router(
        AppState::new("webhook-secret", dispatcher).with_ci_review_endpoint(expected_token, client),
    );
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _guard = subscriber::set_default(subscriber);

    let req = Request::post("/reviews/ci")
        .header("authorization", format!("Bearer {rejected_token}"))
        .header("content-type", "application/json")
        .body(Body::from(ci_review_body()))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    let log_output = logs.contents();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "unauthorized");
    assert!(recorder.jobs().is_empty());
    assert!(!body.contains(expected_token), "response body was {body:?}");
    assert!(!body.contains(rejected_token), "response body was {body:?}");
    assert!(
        !log_output.contains(expected_token),
        "logs were {log_output:?}"
    );
    assert!(
        !log_output.contains(rejected_token),
        "logs were {log_output:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn forgejo_failure_ci_review_response_and_logs_do_not_leak_upstream_secrets() {
    let action_token = "valid-action-token-visible-regression-secret";
    let forgejo_token = "forgejo-api-token-visible-regression-secret";
    let upstream_sensitive_body = "upstream exploded with password=secret-token-value";

    let forgejo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/42"))
        .and(header("Authorization", format!("token {forgejo_token}")))
        .respond_with(ResponseTemplate::new(500).set_body_string(upstream_sensitive_body))
        .expect(1)
        .mount(&forgejo)
        .await;

    let recorder = Arc::new(RecordingDispatcher::default());
    let client = Arc::new(ForgejoClient::new(&forgejo.uri(), forgejo_token).expect("client"));
    let dispatcher: Arc<dyn JobDispatcher> = Arc::<RecordingDispatcher>::clone(&recorder);
    let app = build_router(
        AppState::new("webhook-secret", dispatcher).with_ci_review_endpoint(action_token, client),
    );
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _guard = subscriber::set_default(subscriber);

    let req = Request::post("/reviews/ci")
        .header("authorization", format!("Bearer {action_token}"))
        .header("content-type", "application/json")
        .body(Body::from(ci_review_body()))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    let log_output = logs.contents();

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, "fetch pull request failed");
    assert!(recorder.jobs().is_empty());
    assert!(!body.contains(forgejo_token), "response body was {body:?}");
    assert!(
        !body.contains(upstream_sensitive_body),
        "response body was {body:?}"
    );
    assert!(
        !log_output.contains(forgejo_token),
        "logs were {log_output:?}"
    );
    assert!(
        !log_output.contains(upstream_sensitive_body),
        "logs were {log_output:?}"
    );
}
