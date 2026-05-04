use ar_gateway::{build_router, AppState};
use ar_orchestrator::{JobDispatcher, ReviewJob};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

type HmacSha256 = Hmac<Sha256>;

#[derive(Default)]
struct RecordingDispatcher {
    jobs: Mutex<Vec<ReviewJob>>,
}

impl RecordingDispatcher {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn jobs(&self) -> Vec<ReviewJob> {
        self.jobs.lock().expect("jobs lock").clone()
    }
}

#[async_trait]
impl JobDispatcher for RecordingDispatcher {
    async fn dispatch(&self, job: ReviewJob) {
        self.jobs.lock().expect("jobs lock").push(job);
    }
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("valid hmac key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn review_requested_pr_payload(requested_reviewer: &str, draft: bool, state: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "review_requested",
        "number": 7,
        "requested_reviewer": {"login": requested_reviewer, "id": 42},
        "pull_request": {
            "number": 7,
            "title": "x",
            "body": "review the current head",
            "draft": draft,
            "state": state,
            "user": {"login": "u", "id": 1},
            "head": {"ref": "t", "sha": "deadbeef"},
            "base": {"ref": "main", "sha": "cafef00d"}
        },
        "repository": {
            "name": "r", "full_name": "o/r", "default_branch": "main",
            "owner": {"login": "o", "id": 99}
        },
        "sender": {"login": "alice", "id": 1}
    }))
    .expect("valid review requested payload")
}

async fn post_pull_request_webhook(
    dispatcher: Arc<RecordingDispatcher>,
    bot_login: &str,
    body: Vec<u8>,
) -> StatusCode {
    let secret = "s";
    let sig = sign(secret, &body);
    let app = build_router(
        AppState::new(secret, dispatcher as Arc<dyn JobDispatcher>)
            .with_bot_identity(bot_login, bot_login),
    );
    let req = Request::post("/webhooks/forgejo")
        .header("x-forgejo-event", "pull_request")
        .header("x-forgejo-signature", sig)
        .body(Body::from(body))
        .expect("valid webhook request");

    app.oneshot(req).await.expect("webhook response").status()
}

#[tokio::test]
async fn review_requested_for_configured_bot_is_accepted_without_dispatching_review() {
    let body = review_requested_pr_payload("pr-bot", false, "open");
    let recorder = RecordingDispatcher::new();

    let status = post_pull_request_webhook(recorder.clone(), "pr-bot", body).await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(recorder.jobs().is_empty());
}

#[tokio::test]
async fn review_requested_for_non_bot_is_not_dispatched() {
    let body = review_requested_pr_payload("human-reviewer", false, "open");
    let recorder = RecordingDispatcher::new();

    let status = post_pull_request_webhook(recorder.clone(), "auto_review", body).await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(recorder.jobs().is_empty());
}

#[tokio::test]
async fn review_requested_for_draft_or_closed_pr_is_not_dispatched() {
    for (draft, state) in [(true, "open"), (false, "closed")] {
        let body = review_requested_pr_payload("auto_review", draft, state);
        let recorder = RecordingDispatcher::new();

        let status = post_pull_request_webhook(recorder.clone(), "auto_review", body).await;

        assert_eq!(status, StatusCode::ACCEPTED, "draft={draft} state={state}");
        assert!(
            recorder.jobs().is_empty(),
            "draft={draft} state={state} must not dispatch"
        );
    }
}
