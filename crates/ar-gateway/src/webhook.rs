use crate::hmac::{verify, HmacError};
use crate::AppState;
use ar_forgejo::{PullRequestAction, PullRequestEvent};
use ar_orchestrator::ReviewJob;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

const SIG_HEADER: &str = "x-forgejo-signature";
const FALLBACK_SIG_HEADER: &str = "x-gitea-signature";
const EVENT_HEADER: &str = "x-forgejo-event";
const FALLBACK_EVENT_HEADER: &str = "x-gitea-event";

/// Top-level webhook handler.
///
/// 1. HMAC-verifies the body against the configured secret.
/// 2. Dispatches by `X-Forgejo-Event`.
/// 3. For `pull_request` opened/synchronized/reopened, decodes the payload
///    and hands a [`ReviewJob`] to the configured dispatcher.
pub async fn handle(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let sig = headers
        .get(SIG_HEADER)
        .or_else(|| headers.get(FALLBACK_SIG_HEADER))
        .and_then(|v| v.to_str().ok());
    let Some(sig) = sig else {
        return reject(StatusCode::UNAUTHORIZED, "missing signature");
    };

    if let Err(e) = verify(&state.webhook_secret, &body, sig) {
        let status = match e {
            HmacError::Mismatch => StatusCode::UNAUTHORIZED,
            HmacError::Missing | HmacError::NotHex => StatusCode::BAD_REQUEST,
            HmacError::InvalidSecret => StatusCode::INTERNAL_SERVER_ERROR,
        };
        return reject(status, &format!("signature: {e}"));
    }

    let event = headers
        .get(EVENT_HEADER)
        .or_else(|| headers.get(FALLBACK_EVENT_HEADER))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match event {
        "pull_request" => handle_pull_request(&state, &body).await,
        "ping" => (StatusCode::OK, "pong").into_response(),
        other => {
            tracing::debug!(event = other, "ignoring webhook event");
            (StatusCode::ACCEPTED, "").into_response()
        }
    }
}

async fn handle_pull_request(state: &AppState, body: &[u8]) -> Response {
    let evt: PullRequestEvent = match serde_json::from_slice(body) {
        Ok(e) => e,
        Err(e) => return reject(StatusCode::BAD_REQUEST, &format!("payload decode: {e}")),
    };
    if !is_actionable(evt.action) {
        tracing::debug!(action = ?evt.action, "ignoring non-review-triggering action");
        return (StatusCode::ACCEPTED, "").into_response();
    }
    if evt.pull_request.draft {
        tracing::debug!(number = evt.number, "ignoring draft PR");
        return (StatusCode::ACCEPTED, "").into_response();
    }
    tracing::info!(
        repo = %evt.repository.full_name,
        number = evt.number,
        action = ?evt.action,
        head = %evt.pull_request.head.sha,
        "accepted PR for review",
    );
    state.dispatcher.dispatch(ReviewJob::from(&evt)).await;
    (StatusCode::ACCEPTED, "").into_response()
}

fn is_actionable(action: PullRequestAction) -> bool {
    matches!(
        action,
        PullRequestAction::Opened
            | PullRequestAction::Synchronized
            | PullRequestAction::Reopened
            | PullRequestAction::ReadyForReview,
    )
}

fn reject(status: StatusCode, msg: &str) -> Response {
    tracing::warn!(%status, msg, "rejecting webhook");
    (status, msg.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_router;
    use ::hmac::{Hmac, Mac};
    use ar_orchestrator::{JobDispatcher, NoOpDispatcher};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use sha2::Sha256;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    type HmacSha256 = Hmac<Sha256>;

    struct RecordingDispatcher {
        seen: Mutex<Vec<ReviewJob>>,
    }

    impl RecordingDispatcher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
            })
        }
        fn jobs(&self) -> Vec<ReviewJob> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: ReviewJob) {
            self.seen.lock().unwrap().push(job);
        }
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn pr_payload(action: &str, draft: bool) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": action,
            "number": 7,
            "pull_request": {
                "number": 7,
                "title": "x",
                "body": "",
                "draft": draft,
                "user": {"login": "u", "id": 1},
                "head": {"ref": "t", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "cafef00d"}
            },
            "repository": {
                "name": "r", "full_name": "o/r", "default_branch": "main",
                "owner": {"login": "o", "id": 99}
            },
            "sender": {"login": "u", "id": 1}
        }))
        .unwrap()
    }

    async fn send(
        secret: &str,
        event: &str,
        body: Vec<u8>,
        sig: Option<&str>,
        dispatcher: Arc<dyn JobDispatcher>,
    ) -> (StatusCode, String) {
        let app = build_router(AppState::new(secret, dispatcher));
        let mut req = Request::post("/webhooks/forgejo").header(EVENT_HEADER, event);
        if let Some(s) = sig {
            req = req.header(SIG_HEADER, s);
        }
        let req = req.body(Body::from(body)).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn missing_signature_is_unauthorized() {
        let (status, _) = send(
            "s",
            "pull_request",
            pr_payload("opened", false),
            None,
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bad_signature_is_unauthorized() {
        let body = pr_payload("opened", false);
        let (status, _) = send(
            "s",
            "pull_request",
            body,
            Some("00"),
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_pr_opened_is_accepted_and_dispatched() {
        let body = pr_payload("opened", false);
        let sig = sign("s", &body);
        let recorder = RecordingDispatcher::new();
        let (status, _) = send("s", "pull_request", body, Some(&sig), recorder.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let jobs = recorder.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].pr_number, 7);
        assert_eq!(jobs[0].owner, "o");
        assert_eq!(jobs[0].repo, "r");
        assert_eq!(jobs[0].head_sha, "deadbeef");
    }

    #[tokio::test]
    async fn draft_pr_is_skipped_and_not_dispatched() {
        let body = pr_payload("opened", true);
        let sig = sign("s", &body);
        let recorder = RecordingDispatcher::new();
        let (status, _) = send("s", "pull_request", body, Some(&sig), recorder.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(recorder.jobs().is_empty());
    }

    #[tokio::test]
    async fn non_actionable_action_is_not_dispatched() {
        let body = pr_payload("closed", false);
        let sig = sign("s", &body);
        let recorder = RecordingDispatcher::new();
        let (status, _) = send("s", "pull_request", body, Some(&sig), recorder.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(recorder.jobs().is_empty());
    }

    #[tokio::test]
    async fn version_endpoint_returns_name_and_version() {
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::get("/version").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["name"], "auto_review");
        assert!(!json["version"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::get("/healthz").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ping_is_pong() {
        let body = b"{}".to_vec();
        let sig = sign("s", &body);
        let (status, body) = send("s", "ping", body, Some(&sig), Arc::new(NoOpDispatcher)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "pong");
    }

    #[tokio::test]
    async fn unknown_event_is_accepted_silently() {
        let body = b"{}".to_vec();
        let sig = sign("s", &body);
        let (status, _) = send(
            "s",
            "issue_comment",
            body,
            Some(&sig),
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
}
